/// TCP layer — hardened TCP stack (RFC 793 + RFC 6298 + RFC 6528 + Reno)
///
/// Simplified TCP state machine with 8 connections.
/// Supports listen/connect/send/recv/close with:
///   - ISN randomization (RFC 6528)
///   - Retransmission timer (RFC 6298)
///   - Keep-alive probes
///   - Sequence number validation
///   - Congestion control (Reno)
///   - MSS negotiation

use robot_os_sync::SpinLock;
use super::ip;

// ---------------------------------------------------------------------------
// Connection limits
// ---------------------------------------------------------------------------

pub const TCP_MAX_CONNS: usize = 8;

/// Per-connection receive ring buffer size (bytes).
/// Must be a power of two for efficient modular arithmetic.
/// Sized to keep OTA / large transfers flowing without window-stalls: a
/// 4 KB buffer fills in ~3 segments at MSS=1460 and stalls the sender if
/// the consumer task can't drain instantly. 128 KB gives the OTA recv task
/// generous breathing room across FAT32 write latencies and burst arrivals.
const TCP_BUF_SIZE: usize = 131072;

/// Ring buffer index mask — used instead of modulo for power-of-two buffers.
const TCP_BUF_MASK: usize = TCP_BUF_SIZE - 1;

/// TCP Maximum Segment Size — max payload bytes per segment.
/// Standard Ethernet MTU (1500) minus IP header (20) minus TCP header (20).
const TCP_MSS: usize = 1460;

/// Maximum TCP segment buffer size (MSS + max TCP header with options).
const TCP_SEGMENT_BUF_SIZE: usize = TCP_MSS + TCP_HDR_MAX;

/// TCP advertised receive window size (bytes), capped at u16::MAX since
/// the TCP header window field is 16 bits and we don't implement window
/// scaling (RFC 7323). With a buffer ≥ 64 KiB we always advertise the max
/// 65535-byte window; the actual free-space ACK on each segment then
/// reflects the live free count saturated to u16.
const TCP_WINDOW_SIZE: u16 = if TCP_BUF_SIZE > u16::MAX as usize {
    u16::MAX
} else {
    TCP_BUF_SIZE as u16
};

/// Cap any advertised free-space value at u16::MAX without window scaling.
const fn window_clamp(free: usize) -> u16 {
    if free > u16::MAX as usize { u16::MAX } else { free as u16 }
}

// ---------------------------------------------------------------------------
// TCP header constants
// ---------------------------------------------------------------------------

/// Minimum TCP header length in bytes (5 × 4-byte words).
const TCP_HDR_MIN: usize = 20;

/// Maximum TCP header length with MSS option (6 × 4-byte words = 24 bytes).
const TCP_HDR_MAX: usize = 24;

/// TCP data offset for a minimal 20-byte header (5 × 4-byte words),
/// encoded in the upper 4 bits of the data_off field.
const TCP_DATA_OFF_MIN: u8 = 0x50;

/// TCP data offset for a 24-byte header with MSS option (6 × 4-byte words).
const TCP_DATA_OFF_MSS: u8 = 0x60;

// ---------------------------------------------------------------------------
// TCP flags
// ---------------------------------------------------------------------------

const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
#[allow(dead_code)]
const TCP_RST: u8 = 0x04;
const TCP_ACK: u8 = 0x10;

// ---------------------------------------------------------------------------
// MSS option
// ---------------------------------------------------------------------------

/// TCP option kind for Maximum Segment Size.
const TCP_OPT_MSS: u8 = 2;

/// TCP option length for MSS (kind + len + 2-byte value).
const TCP_OPT_MSS_LEN: u8 = 4;

/// TCP option kind: End of Options List.
const TCP_OPT_EOL: u8 = 0;

/// TCP option kind: No-Operation (padding).
const TCP_OPT_NOP: u8 = 1;

/// Default remote MSS when peer does not advertise one (RFC 879).
const TCP_DEFAULT_REMOTE_MSS: u16 = 536;

// ---------------------------------------------------------------------------
// ISN generation (RFC 6528)
// ---------------------------------------------------------------------------

/// Compile-time secret seed for ISN generation.
const ISN_SECRET: u32 = 0xA5F0_3C7B;

/// FNV-1a offset basis (32-bit).
const FNV_OFFSET_BASIS: u32 = 0x811C_9DC5;

/// FNV-1a prime (32-bit).
const FNV_PRIME: u32 = 0x0100_0193;

// ---------------------------------------------------------------------------
// Retransmission timer (RFC 6298)
// ---------------------------------------------------------------------------

/// Initial retransmission timeout in milliseconds.
const RTO_INITIAL_MS: u64 = 1000;

/// Minimum retransmission timeout in milliseconds.
const RTO_MIN_MS: u64 = 200;

/// Maximum retransmission timeout in milliseconds (60 seconds).
const RTO_MAX_MS: u64 = 60_000;

/// Maximum retransmission attempts before connection is considered dead.
const RETX_MAX_ATTEMPTS: u8 = 8;

/// Timer ticks per millisecond (10 MHz clock).
const TICKS_PER_MS: u64 = 10_000;

// ---------------------------------------------------------------------------
// Keep-alive
// ---------------------------------------------------------------------------

/// Keep-alive interval: 30 seconds in ticks (30 × 10,000,000).
const KEEPALIVE_INTERVAL_TICKS: u64 = 30 * 10_000_000;

/// Maximum keep-alive probes before declaring connection dead.
const KEEPALIVE_MAX_PROBES: u8 = 3;

// ---------------------------------------------------------------------------
// Congestion control (Reno)
// ---------------------------------------------------------------------------

/// Initial congestion window: 2 segments.
const CWND_INITIAL: u32 = TCP_MSS as u32 * 2;

/// Initial slow-start threshold: equals receive buffer size.
const SSTHRESH_INITIAL: u32 = TCP_BUF_SIZE as u32;

/// Fast retransmit threshold: 3 duplicate ACKs.
const DUP_ACK_THRESHOLD: u8 = 3;

// ---------------------------------------------------------------------------
// Out-of-order reassembly (F01)
// ---------------------------------------------------------------------------

/// Maximum number of out-of-order segments buffered per connection.
const OOO_MAX_SEGMENTS: usize = 4;

/// Maximum data bytes per out-of-order segment (save memory).
const OOO_SEGMENT_MAX_LEN: usize = 256;

// ---------------------------------------------------------------------------
// FIN state machine (F01)
// ---------------------------------------------------------------------------

/// TIME-WAIT duration in milliseconds (2 × MSL, MSL = 1 second for LAN).
const TIME_WAIT_MS: u64 = 2_000;

// ---------------------------------------------------------------------------
// RTT fixed-point scaling
// ---------------------------------------------------------------------------

/// Fixed-point multiplier for SRTT/RTTVAR (×1000).
const RTT_SCALE: u64 = 1000;

/// SRTT smoothing factor: alpha = 1/8, so (1 - alpha) = 7/8.
const SRTT_ALPHA_INV: u64 = 8;

/// RTTVAR smoothing factor: beta = 1/4, so (1 - beta) = 3/4.
const RTTVAR_BETA_INV: u64 = 4;

/// Clock granularity for RTO lower bound (1 ms in ticks).
const CLOCK_GRANULARITY_TICKS: u64 = TICKS_PER_MS;

// ---------------------------------------------------------------------------
// TCP checksum field offset in header
// ---------------------------------------------------------------------------

/// Byte offset of the checksum field within the TCP header.
const TCP_CHECKSUM_OFFSET: usize = 16;

/// Byte offset + 1 of the checksum field (second byte).
const TCP_CHECKSUM_OFFSET_HI: usize = 17;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum TcpState {
    Closed     = 0,
    Listen     = 1,
    SynSent    = 2,
    SynRcvd    = 3,
    Established= 4,
    FinWait1   = 5,
    FinWait2   = 6,
    CloseWait  = 7,
    LastAck    = 8,
    TimeWait   = 9,
}

#[repr(C, packed)]
struct TcpHdr {
    src_port: [u8; 2],
    dst_port: [u8; 2],
    seq:      [u8; 4],
    ack:      [u8; 4],
    data_off: u8,     // header length in 32-bit words (high 4 bits)
    flags:    u8,
    window:   [u8; 2],
    checksum: [u8; 2],
    urgent:   [u8; 2],
}

// ---------------------------------------------------------------------------
// Out-of-order segment buffer
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct OooSegment {
    seq:   u32,
    len:   u16,
    data:  [u8; OOO_SEGMENT_MAX_LEN],
    valid: bool,
}

impl OooSegment {
    const fn empty() -> Self {
        Self { seq: 0, len: 0, data: [0u8; OOO_SEGMENT_MAX_LEN], valid: false }
    }
}

// ---------------------------------------------------------------------------
// TcpConn
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct TcpConn {
    pub state:        TcpState,
    pub local_ip:     [u8; 4],
    pub local_port:   u16,
    pub remote_ip:    [u8; 4],
    pub remote_port:  u16,
    pub seq:          u32,
    pub ack:          u32,
    pub was_accepted: bool,   // socket_accept() has claimed this slot
    pub rx_buf:       [u8; TCP_BUF_SIZE],
    pub rx_head:      usize,
    pub rx_tail:      usize,

    // --- Retransmission (RFC 6298) ---
    retx_buf:         [u8; TCP_MSS],  // last sent segment data
    retx_len:         usize,          // bytes in retx_buf
    retx_seq:         u32,            // sequence number of retx data
    retx_time:        u64,            // tick when segment was sent
    rto_ticks:        u64,            // current RTO in ticks
    srtt:             u64,            // smoothed RTT (×1000 fixed-point)
    rttvar:           u64,            // RTT variance (×1000 fixed-point)
    retx_count:       u8,             // retransmission attempts
    unacked:          bool,           // has un-ACKed data in retx_buf

    // --- Keep-alive ---
    last_activity:    u64,            // last rx/tx timestamp (ticks)
    keepalive_probes: u8,             // probes sent without reply

    // --- Congestion control (Reno) ---
    cwnd:             u32,            // congestion window (bytes)
    ssthresh:         u32,            // slow-start threshold
    dup_ack_count:    u8,             // duplicate ACK counter
    last_ack_recv:    u32,            // last ACK value received (for dup detection)

    // --- MSS negotiation ---
    remote_mss:       u16,            // MSS advertised by remote peer

    // --- Peer's advertised receive window (RFC 793 SND.WND) ---
    /// Last advertised receive window from the peer. Bound on how many
    /// bytes we can have in flight. Updated on every inbound ACK.
    /// Pre-fix: not stored at all; sender ignored the peer's window
    /// and could overshoot it, causing the peer to drop segments.
    remote_window:    u16,

    // --- Out-of-order reassembly (F01) ---
    ooo_buf:          [OooSegment; OOO_MAX_SEGMENTS],
    ooo_count:        u8,

    // --- FIN state machine (F01) ---
    fin_seq:          u32,            // sequence number of our FIN
    time_wait_start:  u64,            // tick when TimeWait began
}

impl TcpConn {
    pub const fn new() -> Self {
        TcpConn {
            state:        TcpState::Closed,
            local_ip:     [0; 4],
            local_port:   0,
            remote_ip:    [0; 4],
            remote_port:  0,
            seq:          0,
            ack:          0,
            was_accepted: false,
            rx_buf:       [0u8; TCP_BUF_SIZE],
            rx_head:      0,
            rx_tail:      0,

            retx_buf:     [0u8; TCP_MSS],
            retx_len:     0,
            retx_seq:     0,
            retx_time:    0,
            rto_ticks:    RTO_INITIAL_MS * TICKS_PER_MS,
            srtt:         0,
            rttvar:       0,
            retx_count:   0,
            unacked:      false,

            last_activity:    0,
            keepalive_probes: 0,

            cwnd:          CWND_INITIAL,
            ssthresh:      SSTHRESH_INITIAL,
            dup_ack_count: 0,
            last_ack_recv: 0,

            remote_mss:    TCP_DEFAULT_REMOTE_MSS,
            remote_window: TCP_WINDOW_SIZE,  // assume peer's window = ours


            ooo_buf:       [OooSegment::empty(); OOO_MAX_SEGMENTS],
            ooo_count:     0,

            fin_seq:       0,
            time_wait_start: 0,
        }
    }

    pub fn rx_available(&self) -> usize {
        if self.rx_tail >= self.rx_head {
            self.rx_tail - self.rx_head
        } else {
            TCP_BUF_SIZE - self.rx_head + self.rx_tail
        }
    }

    /// Reset congestion and retransmission state for a fresh connection.
    fn reset_conn_state(&mut self) {
        self.retx_len         = 0;
        self.retx_seq         = 0;
        self.retx_time        = 0;
        self.rto_ticks        = RTO_INITIAL_MS * TICKS_PER_MS;
        self.srtt             = 0;
        self.rttvar           = 0;
        self.retx_count       = 0;
        self.unacked          = false;
        self.last_activity    = robot_os_drivers::clint::get_time();
        self.keepalive_probes = 0;
        self.cwnd             = CWND_INITIAL;
        self.ssthresh         = SSTHRESH_INITIAL;
        self.dup_ack_count    = 0;
        self.last_ack_recv    = 0;
        self.remote_mss       = TCP_DEFAULT_REMOTE_MSS;
        self.rx_head          = 0;
        self.rx_tail          = 0;
    }
}

// ---------------------------------------------------------------------------
// TcpLayer
// ---------------------------------------------------------------------------

struct TcpLayer {
    conns:        [TcpConn; TCP_MAX_CONNS],
    our_mac:      [u8; 6],
    our_ip:       [u8; 4],
}

impl TcpLayer {
    const fn new() -> Self {
        TcpLayer {
            conns:   [TcpConn::new(); TCP_MAX_CONNS],
            our_mac: [0; 6],
            our_ip:  [0; 4],
        }
    }

    fn find_conn(&self, local_port: u16, remote_ip: &[u8; 4], remote_port: u16) -> Option<usize> {
        for i in 0..TCP_MAX_CONNS {
            let c = &self.conns[i];
            if c.state != TcpState::Closed
                && c.local_port == local_port
                && &c.remote_ip == remote_ip
                && c.remote_port == remote_port
            {
                return Some(i);
            }
        }
        None
    }

    fn find_listener(&self, port: u16) -> Option<usize> {
        for i in 0..TCP_MAX_CONNS {
            if self.conns[i].state == TcpState::Listen && self.conns[i].local_port == port {
                return Some(i);
            }
        }
        None
    }

    fn alloc(&self) -> Option<usize> {
        for i in 0..TCP_MAX_CONNS {
            if self.conns[i].state == TcpState::Closed { return Some(i); }
        }
        None
    }
}

static TCP: SpinLock<TcpLayer> = SpinLock::new(TcpLayer::new());

// ---------------------------------------------------------------------------
// ISN generation (RFC 6528)
// ---------------------------------------------------------------------------

/// Simple ISN generator using connection 4-tuple and a static secret.
/// ISN = FNV-1a(src_ip, dst_ip, src_port, dst_port, secret) + time_ticks
fn generate_isn(src_ip: &[u8; 4], dst_ip: &[u8; 4], src_port: u16, dst_port: u16) -> u32 {
    let mut h: u32 = FNV_OFFSET_BASIS;

    // Hash source IP
    let mut i = 0;
    while i < 4 {
        h ^= src_ip[i] as u32;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }

    // Hash destination IP
    i = 0;
    while i < 4 {
        h ^= dst_ip[i] as u32;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }

    // Hash source port (2 bytes, big-endian)
    let sp = src_port.to_be_bytes();
    h ^= sp[0] as u32;
    h = h.wrapping_mul(FNV_PRIME);
    h ^= sp[1] as u32;
    h = h.wrapping_mul(FNV_PRIME);

    // Hash destination port (2 bytes, big-endian)
    let dp = dst_port.to_be_bytes();
    h ^= dp[0] as u32;
    h = h.wrapping_mul(FNV_PRIME);
    h ^= dp[1] as u32;
    h = h.wrapping_mul(FNV_PRIME);

    // Hash secret seed (4 bytes)
    let secret = ISN_SECRET.to_le_bytes();
    i = 0;
    while i < 4 {
        h ^= secret[i] as u32;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }

    // Add time component for uniqueness across reboots
    let ticks = robot_os_drivers::clint::get_time();
    h.wrapping_add(ticks as u32)
}

// ---------------------------------------------------------------------------
// MSS option parsing
// ---------------------------------------------------------------------------

/// Parse TCP options from the header to extract the remote MSS.
/// Returns the advertised MSS, or TCP_DEFAULT_REMOTE_MSS if not present.
fn parse_mss_option(data: &[u8], hdr_len: usize) -> u16 {
    if hdr_len <= TCP_HDR_MIN || data.len() < hdr_len {
        return TCP_DEFAULT_REMOTE_MSS;
    }
    let opts = &data[TCP_HDR_MIN..hdr_len];
    let mut i = 0;
    while i < opts.len() {
        match opts[i] {
            TCP_OPT_EOL => break,
            TCP_OPT_NOP => { i += 1; }
            TCP_OPT_MSS => {
                if i + (TCP_OPT_MSS_LEN as usize) <= opts.len()
                    && opts[i + 1] == TCP_OPT_MSS_LEN
                {
                    return u16::from_be_bytes([opts[i + 2], opts[i + 3]]);
                }
                break;
            }
            _ => {
                // Unknown option — skip using length byte
                if i + 1 >= opts.len() { break; }
                let opt_len = opts[i + 1] as usize;
                if opt_len < 2 { break; } // malformed
                i += opt_len;
                continue;
            }
        }
    }
    TCP_DEFAULT_REMOTE_MSS
}

// ---------------------------------------------------------------------------
// Sequence number validation
// ---------------------------------------------------------------------------

/// Check if a sequence number falls within the receive window.
/// Window is [win_start, win_start + win_size) using wrapping arithmetic.
fn seq_in_window(seq_num: u32, win_start: u32, win_size: u32) -> bool {
    // seq_num - win_start (wrapping) should be < win_size
    let offset = seq_num.wrapping_sub(win_start);
    offset < win_size
}

// ---------------------------------------------------------------------------
// RTT / RTO update (RFC 6298)
// ---------------------------------------------------------------------------

/// Update SRTT, RTTVAR, and RTO from a measured RTT sample.
fn update_rtt(conn: &mut TcpConn, measured_ticks: u64) {
    let r = measured_ticks * RTT_SCALE; // scale to fixed-point

    if conn.srtt == 0 {
        // First measurement
        conn.srtt   = r;
        conn.rttvar = r / 2;
    } else {
        // RTTVAR = (1 - beta) * RTTVAR + beta * |SRTT - R|
        let diff = if conn.srtt > r { conn.srtt - r } else { r - conn.srtt };
        conn.rttvar = (conn.rttvar * (RTTVAR_BETA_INV - 1) + diff) / RTTVAR_BETA_INV;

        // SRTT = (1 - alpha) * SRTT + alpha * R
        conn.srtt = (conn.srtt * (SRTT_ALPHA_INV - 1) + r) / SRTT_ALPHA_INV;
    }

    // RTO = SRTT + max(G, 4 * RTTVAR)
    let rttvar_component = conn.rttvar * 4 / RTT_SCALE;
    let g = CLOCK_GRANULARITY_TICKS;
    let k_rttvar = if rttvar_component > g { rttvar_component } else { g };
    let rto = conn.srtt / RTT_SCALE + k_rttvar;

    // Clamp to [RTO_MIN, RTO_MAX]
    let rto_min = RTO_MIN_MS * TICKS_PER_MS;
    let rto_max = RTO_MAX_MS * TICKS_PER_MS;
    conn.rto_ticks = if rto < rto_min {
        rto_min
    } else if rto > rto_max {
        rto_max
    } else {
        rto
    };
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Configure the TCP layer with our network addresses.
pub fn init(mac: [u8; 6], ip: [u8; 4]) {
    let mut t = TCP.lock();
    t.our_mac = mac;
    t.our_ip  = ip;
}

/// Listen on a port.  Returns connection slot index or -1 on failure.
pub fn listen(port: u16) -> i32 {
    let (mac, ip) = { let t = TCP.lock(); (t.our_mac, t.our_ip) };
    let _ = mac;
    let mut t = TCP.lock();
    let idx = match t.alloc() {
        Some(i) => i,
        None    => return -1,
    };
    t.conns[idx].state      = TcpState::Listen;
    t.conns[idx].local_ip   = ip;
    t.conns[idx].local_port = port;
    t.conns[idx].remote_ip  = [0; 4];
    t.conns[idx].remote_port = 0;
    idx as i32
}

/// Initiate an outgoing TCP connection.  Returns connection index or -1.
/// Connection is not established until state == Established.
pub fn connect(dst_ip: [u8; 4], dst_port: u16, src_port: u16) -> i32 {
    let (mac, ip) = { let t = TCP.lock(); (t.our_mac, t.our_ip) };
    let _ = mac;
    let mut t = TCP.lock();
    let idx = match t.alloc() {
        Some(i) => i,
        None    => return -1,
    };
    let seq = generate_isn(&ip, &dst_ip, src_port, dst_port);
    t.conns[idx].state       = TcpState::SynSent;
    t.conns[idx].local_ip    = ip;
    t.conns[idx].local_port  = src_port;
    t.conns[idx].remote_ip   = dst_ip;
    t.conns[idx].remote_port = dst_port;
    t.conns[idx].seq         = seq;
    t.conns[idx].ack         = 0;
    t.conns[idx].reset_conn_state();
    t.conns[idx].seq         = seq; // restore after reset_conn_state clears it

    // Send SYN with MSS option
    send_syn_segment(&mac, &ip, &dst_ip, src_port, dst_port, TCP_SYN, seq, 0);
    idx as i32
}

/// Send data on an established connection.
pub fn send_data(idx: usize, data: &[u8]) -> i32 {
    if idx >= TCP_MAX_CONNS { return -1; }
    let (mac, ip, seq, ack_val, dst_ip, src_port, dst_port,
         state, cwnd, remote_mss, remote_window) = {
        let t = TCP.lock();
        let c = &t.conns[idx];
        (t.our_mac, t.our_ip, c.seq, c.ack, c.remote_ip, c.local_port, c.remote_port,
         c.state, c.cwnd, c.remote_mss, c.remote_window)
    };
    if state != TcpState::Established { return -1; }

    // Limit send size by congestion window, remote MSS, AND the peer's
    // advertised receive window. Without the third bound the kernel could
    // overshoot the peer's window, the peer drops the segments, and we
    // burn round-trips retransmitting until cwnd catches up to reality.
    if remote_window == 0 {
        // Peer's window is closed — caller should retry later.
        return 0;
    }
    let max_send = (cwnd as usize)
        .min(remote_mss as usize)
        .min(remote_window as usize)
        .min(data.len());
    if max_send == 0 { return 0; }
    let send_data_slice = &data[..max_send];

    let result = send_segment(&mac, &ip, &dst_ip, src_port, dst_port,
                              TCP_ACK, seq, ack_val, send_data_slice);

    if result == 0 {
        let now = robot_os_drivers::clint::get_time();
        let mut t = TCP.lock();
        let c = &mut t.conns[idx];
        c.seq = c.seq.wrapping_add(max_send as u32);

        // Capture for retransmission
        let copy_len = max_send.min(TCP_MSS);
        c.retx_buf[..copy_len].copy_from_slice(&send_data_slice[..copy_len]);
        c.retx_len  = copy_len;
        c.retx_seq  = seq;
        c.retx_time = now;
        c.retx_count = 0;
        c.unacked    = true;
        c.last_activity = now;

        max_send as i32
    } else {
        -1
    }
}

/// Read received data from a connection.  Returns bytes read, 0 if none.
pub fn recv(idx: usize, buf: &mut [u8]) -> i32 {
    if idx >= TCP_MAX_CONNS { return -1; }
    let (n, send_window_update, params) = {
        let mut t = TCP.lock();
        let c = &mut t.conns[idx];
        let avail = c.rx_available();
        if avail == 0 {
            if matches!(c.state, TcpState::CloseWait | TcpState::LastAck
                        | TcpState::TimeWait | TcpState::Closed) {
                return -1;
            }
            return 0;
        }
        let n = avail.min(buf.len());
        for i in 0..n {
            buf[i] = c.rx_buf[c.rx_head & TCP_BUF_MASK];
            c.rx_head = (c.rx_head + 1) & TCP_BUF_MASK;
        }
        // Send a window-update ACK on every successful read. Without this,
        // after the peer fills our window it stalls forever — there's no
        // other trigger to advertise the now-free space (the only ACK path
        // is on inbound segments, but the peer stops sending when window=0).
        let free_after  = rx_free_space(c);
        let send_update = n > 0;
        let remote_ip   = c.remote_ip;
        let local_port  = c.local_port;
        let remote_port = c.remote_port;
        let seq         = c.seq;
        let ack         = c.ack;
        let our_mac     = t.our_mac;
        let our_ip      = t.our_ip;
        let params = if send_update {
            Some((our_mac, our_ip, remote_ip,
                  local_port, remote_port,
                  seq, ack, window_clamp(free_after)))
        } else { None };
        (n, send_update, params)
    };
    if send_window_update {
        if let Some((mac, ip, dst_ip, sp, dp, seq, ack, win)) = params {
            send_segment_with_window(&mac, &ip, &dst_ip, sp, dp,
                                     TCP_ACK, seq, ack, &[], win);
        }
    }
    n as i32
}

/// Close a connection (proper FIN state machine — F01).
///
/// Established → send FIN → FinWait1 (active close)
/// CloseWait  → send FIN → LastAck   (passive close response)
pub fn close(idx: usize) {
    if idx >= TCP_MAX_CONNS { return; }
    let (mac, ip, state, seq, ack_val, dst_ip, src_port, dst_port) = {
        let t = TCP.lock();
        let c = &t.conns[idx];
        (t.our_mac, t.our_ip, c.state, c.seq, c.ack, c.remote_ip, c.local_port, c.remote_port)
    };
    match state {
        TcpState::Established => {
            send_segment(&mac, &ip, &dst_ip, src_port, dst_port, TCP_FIN | TCP_ACK, seq, ack_val, &[]);
            let mut t = TCP.lock();
            t.conns[idx].fin_seq = seq;
            t.conns[idx].state = TcpState::FinWait1;
        }
        TcpState::CloseWait => {
            send_segment(&mac, &ip, &dst_ip, src_port, dst_port, TCP_FIN | TCP_ACK, seq, ack_val, &[]);
            let mut t = TCP.lock();
            t.conns[idx].fin_seq = seq;
            t.conns[idx].state = TcpState::LastAck;
        }
        _ => {
            // For other states, force close
            let mut t = TCP.lock();
            t.conns[idx].state   = TcpState::Closed;
            t.conns[idx].unacked = false;
        }
    }
}

/// Accept an established TCP connection on `local_port`.
/// Returns the connection slot index or -1 if none is ready.
/// Marks the slot as accepted to prevent double-accept.
pub fn accept(local_port: u16) -> i32 {
    let mut t = TCP.lock();
    for i in 0..TCP_MAX_CONNS {
        let c = &mut t.conns[i];
        if c.state == TcpState::Established
            && c.local_port == local_port
            && !c.was_accepted
        {
            c.was_accepted = true;
            return i as i32;
        }
    }
    -1
}

/// Handle an incoming TCP segment (called from ip::handle).
pub fn handle(src_ip: &[u8; 4], data: &[u8]) {
    if data.len() < TCP_HDR_MIN { return; }
    let hdr = unsafe { &*(data.as_ptr() as *const TcpHdr) };
    let dst_port  = u16::from_be_bytes(hdr.dst_port);
    let src_port  = u16::from_be_bytes(hdr.src_port);
    let seq       = u32::from_be_bytes(hdr.seq);
    let ack_num   = u32::from_be_bytes(hdr.ack);
    let flags     = hdr.flags;
    // Peer's advertised receive window — we cap our outbound size by
    // this so we don't overshoot and force the peer to drop segments.
    let peer_win  = u16::from_be_bytes(hdr.window);
    let off       = ((hdr.data_off >> 4) as usize) * 4;
    // TCP header is at least 20 bytes (data_off >= 5); reject malformed segments.
    let off       = if off < TCP_HDR_MIN { TCP_HDR_MIN } else { off };
    let payload   = if off <= data.len() { &data[off..] } else { &[] };

    let (mac, ip) = { let t = TCP.lock(); (t.our_mac, t.our_ip) };
    let now = robot_os_drivers::clint::get_time();

    // Find an existing connection
    let idx_opt = { TCP.lock().find_conn(dst_port, src_ip, src_port) };

    if let Some(idx) = idx_opt {
        // RFC 793 §3.4 — RST handling: in any synchronised state, a valid
        // RST closes the connection immediately. We must release retx
        // state and free the slot so resources don't leak. Without this
        // an attacker that sees a flow can inject one RST and the kernel
        // keeps the slot allocated forever (resource-exhaustion DoS), and
        // the unacked retx_buf keeps the bogus RTT samples driving cwnd.
        if flags & TCP_RST != 0 {
            let mut t = TCP.lock();
            let c = &mut t.conns[idx];
            c.state          = TcpState::Closed;
            c.unacked        = false;
            c.retx_len       = 0;
            c.retx_count     = 0;
            c.dup_ack_count  = 0;
            c.was_accepted   = false;
            return;
        }
        let state = { TCP.lock().conns[idx].state };
        match state {
            TcpState::SynSent if flags & TCP_SYN != 0 && flags & TCP_ACK != 0 => {
                // SYN-ACK received → parse MSS, send ACK, move to Established
                let remote_mss = parse_mss_option(data, off);
                {
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    c.ack   = seq.wrapping_add(1);
                    c.state = TcpState::Established;
                    c.remote_mss    = remote_mss;
                    c.remote_window = peer_win;
                    c.last_activity = now;
                    c.keepalive_probes = 0;
                }
                let (seq_n, ack_n) = { let t = TCP.lock(); (t.conns[idx].seq, t.conns[idx].ack) };
                send_segment(&mac, &ip, src_ip, dst_port, src_port, TCP_ACK, seq_n, ack_n, &[]);
            }
            TcpState::SynRcvd if flags & TCP_ACK != 0 && flags & TCP_SYN == 0 => {
                // Client's ACK completing the 3-way handshake → Established
                let mut t = TCP.lock();
                let c = &mut t.conns[idx];
                c.seq   = c.seq.wrapping_add(1);
                c.state = TcpState::Established;
                c.remote_window = peer_win;
                c.last_activity = now;
                c.keepalive_probes = 0;
            }
            TcpState::Established => {
                // --- Sequence number validation ---
                let expected_ack = { TCP.lock().conns[idx].ack };
                if !payload.is_empty() {
                    if !seq_in_window(seq, expected_ack, TCP_WINDOW_SIZE as u32) {
                        // Out-of-window segment — drop silently
                        return;
                    }
                }

                // --- ACK processing ---
                if flags & TCP_ACK != 0 {
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    c.last_activity = now;
                    c.keepalive_probes = 0;
                    // Track the peer's advertised window — every ACK
                    // carries an updated value, used by send_data() to
                    // avoid overshooting the peer's receive buffer.
                    c.remote_window = peer_win;

                    if c.unacked && is_ack_advancing(ack_num, c.retx_seq, c.retx_len) {
                        // New ACK — data acknowledged
                        let rtt_sample = now.saturating_sub(c.retx_time);
                        // Only update RTT if this is not a retransmission (Karn's algorithm)
                        if c.retx_count == 0 {
                            update_rtt(c, rtt_sample);
                        }
                        c.unacked       = false;
                        c.retx_len      = 0;
                        c.retx_count    = 0;
                        c.dup_ack_count = 0;
                        c.last_ack_recv = ack_num;

                        // Congestion control: increase cwnd
                        if c.cwnd < c.ssthresh {
                            // Slow start: increase by one MSS per ACK
                            c.cwnd = c.cwnd.saturating_add(TCP_MSS as u32);
                        } else {
                            // Congestion avoidance: increase by MSS*MSS/cwnd per ACK
                            let increment = (TCP_MSS as u32)
                                .saturating_mul(TCP_MSS as u32)
                                / c.cwnd.max(1);
                            c.cwnd = c.cwnd.saturating_add(increment.max(1));
                        }
                    } else if ack_num == c.last_ack_recv && c.unacked {
                        // Duplicate ACK
                        c.dup_ack_count = c.dup_ack_count.saturating_add(1);

                        if c.dup_ack_count == DUP_ACK_THRESHOLD {
                            // Fast retransmit
                            c.ssthresh = (c.cwnd / 2).max(CWND_INITIAL);
                            c.cwnd = c.ssthresh
                                .saturating_add(DUP_ACK_THRESHOLD as u32 * TCP_MSS as u32);

                            // Retransmit the lost segment
                            let retx_seq  = c.retx_seq;
                            let retx_len  = c.retx_len;
                            let ack_val   = c.ack;
                            let lp        = c.local_port;
                            let rp        = c.remote_port;
                            let rip       = c.remote_ip;
                            // Copy retx data to local buffer before drop
                            let mut retx_copy = [0u8; TCP_MSS];
                            retx_copy[..retx_len].copy_from_slice(&c.retx_buf[..retx_len]);
                            c.retx_time  = now;
                            c.retx_count = c.retx_count.saturating_add(1);
                            drop(t);
                            send_segment(&mac, &ip, &rip, lp, rp,
                                         TCP_ACK, retx_seq, ack_val,
                                         &retx_copy[..retx_len]);
                            // Re-lock not needed; we return below or continue
                            let mut t2 = TCP.lock();
                            t2.conns[idx].last_activity = now;
                            // Skip payload processing after fast retransmit
                            if payload.is_empty() && flags & TCP_FIN == 0 {
                                return;
                            }
                            // Fall through for payload/FIN handling below
                            drop(t2);
                        } else {
                            drop(t);
                        }
                    } else {
                        c.last_ack_recv = ack_num;
                        drop(t);
                    }
                }

                // --- Payload processing with OOO reassembly (F01) ---
                if !payload.is_empty() {
                    if seq == expected_ack {
                        // In-order segment — write to rx_buf directly.
                        // CRITICAL: we MUST only ACK bytes we actually stored.
                        // The previous version ACKed `payload.len()` even when
                        // the rx ring was full, silently dropping bytes —
                        // the sender then advanced its window thinking the
                        // data arrived, causing the connection to "complete"
                        // with missing bytes and OTA payload CRC mismatch.
                        let mut t = TCP.lock();
                        let c = &mut t.conns[idx];
                        let mut stored: u32 = 0;
                        for &b in payload {
                            let next = (c.rx_tail + 1) & TCP_BUF_MASK;
                            if next == c.rx_head {
                                // Ring full — stop here, do NOT ACK the rest.
                                // Peer will retransmit once we drain + open the window.
                                break;
                            }
                            c.rx_buf[c.rx_tail] = b;
                            c.rx_tail = next;
                            stored += 1;
                        }
                        c.ack = seq.wrapping_add(stored);
                        c.last_activity = now;

                        // Flush any OOO segments that are now contiguous
                        flush_ooo_segments(c);

                        // Send ACK with actual free window (flow control — F01)
                        let free = rx_free_space(c);
                        let (seq_n, ack_n) = (c.seq, c.ack);
                        drop(t);
                        send_segment_with_window(&mac, &ip, src_ip, dst_port, src_port,
                                                 TCP_ACK, seq_n, ack_n, &[], window_clamp(free));
                    } else if seq.wrapping_sub(expected_ack) < TCP_WINDOW_SIZE as u32 {
                        // Out-of-order but within window — buffer it (F01)
                        let mut t = TCP.lock();
                        let c = &mut t.conns[idx];
                        store_ooo_segment(c, seq, payload);
                        c.last_activity = now;
                        // Send duplicate ACK (signals missing data to sender)
                        let (seq_n, ack_n) = (c.seq, c.ack);
                        drop(t);
                        send_segment(&mac, &ip, src_ip, dst_port, src_port,
                                     TCP_ACK, seq_n, ack_n, &[]);
                    }
                    // else: outside window, already filtered above
                }

                // --- FIN handling ---
                if flags & TCP_FIN != 0 {
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    c.ack = c.ack.wrapping_add(1); // FIN consumes one sequence number
                    c.state = TcpState::CloseWait;
                    c.last_activity = now;
                    let (seq_n, ack_n) = (c.seq, c.ack);
                    drop(t);
                    // ACK the FIN
                    send_segment(&mac, &ip, src_ip, dst_port, src_port,
                                 TCP_ACK, seq_n, ack_n, &[]);
                }
            }
            // --- FIN state machine states (F01) ---
            TcpState::FinWait1 => {
                if flags & TCP_ACK != 0 {
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    // Our FIN has been ACKed
                    if ack_num == c.fin_seq.wrapping_add(1) {
                        if flags & TCP_FIN != 0 {
                            // Simultaneous close: FIN+ACK → TimeWait
                            c.ack = c.ack.wrapping_add(1);
                            c.state = TcpState::TimeWait;
                            c.time_wait_start = now;
                            let (seq_n, ack_n) = (c.seq, c.ack);
                            drop(t);
                            send_segment(&mac, &ip, src_ip, dst_port, src_port,
                                         TCP_ACK, seq_n, ack_n, &[]);
                        } else {
                            c.state = TcpState::FinWait2;
                        }
                    }
                } else if flags & TCP_FIN != 0 {
                    // Peer FIN before our FIN is ACKed → simultaneous close
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    c.ack = c.ack.wrapping_add(1);
                    c.state = TcpState::TimeWait;
                    c.time_wait_start = now;
                    let (seq_n, ack_n) = (c.seq, c.ack);
                    drop(t);
                    send_segment(&mac, &ip, src_ip, dst_port, src_port,
                                 TCP_ACK, seq_n, ack_n, &[]);
                }
            }
            TcpState::FinWait2 => {
                if flags & TCP_FIN != 0 {
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    c.ack = c.ack.wrapping_add(1);
                    c.state = TcpState::TimeWait;
                    c.time_wait_start = now;
                    let (seq_n, ack_n) = (c.seq, c.ack);
                    drop(t);
                    send_segment(&mac, &ip, src_ip, dst_port, src_port,
                                 TCP_ACK, seq_n, ack_n, &[]);
                }
            }
            TcpState::LastAck => {
                if flags & TCP_ACK != 0 {
                    let mut t = TCP.lock();
                    t.conns[idx].state = TcpState::Closed;
                    t.conns[idx].unacked = false;
                }
            }
            TcpState::TimeWait => {
                // Ignore packets in TimeWait; timer in tcp_tick() will close
            }
            _ => {}
        }
        return;
    }

    // Check if we have a listener
    let listener_idx = { TCP.lock().find_listener(dst_port) };
    if let Some(listen_idx) = listener_idx {
        if flags & TCP_SYN != 0 {
            // Anti-SYN-flood: cap the number of half-open connections
            // (SynRcvd) per listener at half the connection table. With
            // TCP_MAX_CONNS=8 we allow at most 4 half-open SYNs at any
            // time; further SYNs are silently dropped. Without this an
            // attacker could fill the table with SYN_RECV slots in a few
            // packets and starve legitimate clients.
            const MAX_HALF_OPEN_PER_LISTENER: usize = TCP_MAX_CONNS / 2;
            let half_open = {
                let t = TCP.lock();
                let mut n = 0usize;
                for c in t.conns.iter() {
                    if c.state == TcpState::SynRcvd && c.local_port == dst_port {
                        n += 1;
                    }
                }
                n
            };
            if half_open >= MAX_HALF_OPEN_PER_LISTENER {
                // Drop the SYN — the peer will retransmit. If they were
                // legitimate, eventually one of the half-open slots will
                // either complete or be reaped on RTO.
                return;
            }
            // Accept: create new connection for this peer
            let new_idx_opt = { TCP.lock().alloc() };
            if let Some(new_idx) = new_idx_opt {
                let our_seq = generate_isn(&ip, src_ip, dst_port, src_port);
                let remote_mss = parse_mss_option(data, off);
                {
                    let mut t = TCP.lock();
                    let c = &mut t.conns[new_idx];
                    c.state       = TcpState::SynRcvd;
                    c.local_ip    = ip;
                    c.local_port  = dst_port;
                    c.remote_ip   = *src_ip;
                    c.remote_port = src_port;
                    c.seq         = our_seq;
                    c.ack         = seq.wrapping_add(1);
                    c.remote_mss  = remote_mss;
                    c.reset_conn_state();
                    // Restore values that reset_conn_state cleared
                    c.seq = our_seq;
                    c.ack = seq.wrapping_add(1);
                    c.remote_mss = remote_mss;
                    let _ = listen_idx; // keep listener alive
                }
                // Send SYN-ACK with MSS option
                let ack_n = { TCP.lock().conns[new_idx].ack };
                send_syn_segment(&mac, &ip, src_ip, dst_port, src_port,
                             TCP_SYN | TCP_ACK, our_seq, ack_n);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// OOO reassembly helpers (F01)
// ---------------------------------------------------------------------------

/// Store an out-of-order segment in the connection's OOO buffer.
fn store_ooo_segment(c: &mut TcpConn, seg_seq: u32, data: &[u8]) {
    let copy_len = data.len().min(OOO_SEGMENT_MAX_LEN);
    if copy_len == 0 { return; }

    // Check if we already have this segment
    for i in 0..OOO_MAX_SEGMENTS {
        if c.ooo_buf[i].valid && c.ooo_buf[i].seq == seg_seq {
            return; // duplicate
        }
    }

    // Find a free slot (or evict the highest seq — keeps lower seqs which are more useful)
    let mut slot = None;
    for i in 0..OOO_MAX_SEGMENTS {
        if !c.ooo_buf[i].valid {
            slot = Some(i);
            break;
        }
    }
    if slot.is_none() && (c.ooo_count as usize) >= OOO_MAX_SEGMENTS {
        // Evict the entry with the highest sequence number
        let mut max_seq = 0u32;
        let mut max_idx = 0;
        for i in 0..OOO_MAX_SEGMENTS {
            if c.ooo_buf[i].valid && c.ooo_buf[i].seq.wrapping_sub(c.ack) > max_seq.wrapping_sub(c.ack) {
                max_seq = c.ooo_buf[i].seq;
                max_idx = i;
            }
        }
        slot = Some(max_idx);
        c.ooo_count = c.ooo_count.saturating_sub(1);
    }

    if let Some(s) = slot {
        c.ooo_buf[s].seq = seg_seq;
        c.ooo_buf[s].len = copy_len as u16;
        c.ooo_buf[s].data[..copy_len].copy_from_slice(&data[..copy_len]);
        c.ooo_buf[s].valid = true;
        c.ooo_count = c.ooo_count.saturating_add(1);
    }
}

/// Flush OOO segments that are now contiguous with conn.ack.
fn flush_ooo_segments(c: &mut TcpConn) {
    // Repeat until no more contiguous segments found
    let mut flushed = true;
    while flushed {
        flushed = false;
        for i in 0..OOO_MAX_SEGMENTS {
            if !c.ooo_buf[i].valid { continue; }
            if c.ooo_buf[i].seq == c.ack {
                // This segment is now contiguous — write to rx_buf.
                // Same correctness rule as the in-order path: only advance
                // c.ack by the bytes we actually stored. If the ring fills
                // mid-segment, leave the OOO entry valid so the peer is
                // forced to retransmit (or we re-flush after a drain).
                let len = c.ooo_buf[i].len as usize;
                let mut stored: u32 = 0;
                let mut full = false;
                for j in 0..len {
                    let next = (c.rx_tail + 1) & TCP_BUF_MASK;
                    if next == c.rx_head { full = true; break; }
                    c.rx_buf[c.rx_tail] = c.ooo_buf[i].data[j];
                    c.rx_tail = next;
                    stored += 1;
                }
                c.ack = c.ack.wrapping_add(stored);
                if !full {
                    // Whole segment landed — retire the OOO slot.
                    c.ooo_buf[i].valid = false;
                    c.ooo_count = c.ooo_count.saturating_sub(1);
                    flushed = true;
                } else {
                    // Couldn't fit the rest. Don't loop again or we'd spin.
                    flushed = false;
                    break;
                }
                break; // restart scan since ack changed
            }
        }
    }
}

/// Calculate free space in the receive buffer (for flow control — F01).
fn rx_free_space(c: &TcpConn) -> usize {
    let used = c.rx_available();
    TCP_BUF_SIZE.saturating_sub(used).saturating_sub(1) // -1 to avoid full==empty ambiguity
}

// ---------------------------------------------------------------------------
// send_segment with custom window (F01 flow control)
// ---------------------------------------------------------------------------

/// Like send_segment but with a custom advertised window (for flow control).
fn send_segment_with_window(
    our_mac: &[u8; 6], our_ip: &[u8; 4], dst_ip: &[u8; 4],
    src_port: u16, dst_port: u16, flags: u8,
    seq: u32, ack: u32, data: &[u8], window: u16,
) {
    let tcp_len = TCP_HDR_MIN + data.len();
    if tcp_len > TCP_SEGMENT_BUF_SIZE { return; }

    let mut buf = [0u8; TCP_SEGMENT_BUF_SIZE];
    let hdr = unsafe { &mut *(buf.as_mut_ptr() as *mut TcpHdr) };
    hdr.src_port = src_port.to_be_bytes();
    hdr.dst_port = dst_port.to_be_bytes();
    hdr.seq      = seq.to_be_bytes();
    hdr.ack      = ack.to_be_bytes();
    hdr.data_off = TCP_DATA_OFF_MIN;
    hdr.flags    = flags;
    hdr.window   = window.to_be_bytes(); // custom window (flow control)
    hdr.checksum = [0, 0];
    hdr.urgent   = [0, 0];
    buf[TCP_HDR_MIN..tcp_len].copy_from_slice(data);

    let pseudo = ip::pseudo_checksum(our_ip, dst_ip, ip::IP_PROTO_TCP, tcp_len as u16);
    let cs = tcp_checksum(pseudo, &buf[..tcp_len]);
    buf[TCP_CHECKSUM_OFFSET]    = (cs >> 8) as u8;
    buf[TCP_CHECKSUM_OFFSET_HI] = (cs & 0xff) as u8;

    ip::send(our_mac, our_ip, dst_ip, ip::IP_PROTO_TCP, &buf[..tcp_len]);
}

/// Periodic tick — call from timer interrupt to drive retransmissions and keep-alive.
pub fn tcp_tick() {
    let now = robot_os_drivers::clint::get_time();
    let (mac, ip) = { let t = TCP.lock(); (t.our_mac, t.our_ip) };

    for idx in 0..TCP_MAX_CONNS {
        let (state, unacked, retx_time, rto, retx_count, last_act, ka_probes,
             retx_seq, retx_len, ack_val, lp, rp, rip) = {
            let t = TCP.lock();
            let c = &t.conns[idx];
            (c.state, c.unacked, c.retx_time, c.rto_ticks, c.retx_count,
             c.last_activity, c.keepalive_probes,
             c.retx_seq, c.retx_len, c.ack, c.local_port, c.remote_port, c.remote_ip)
        };

        if state == TcpState::Closed || state == TcpState::Listen {
            continue;
        }

        // --- Retransmission timer ---
        if unacked && now.saturating_sub(retx_time) >= rto {
            if retx_count >= RETX_MAX_ATTEMPTS {
                // Connection is dead — close it
                let mut t = TCP.lock();
                t.conns[idx].state   = TcpState::Closed;
                t.conns[idx].unacked = false;
                continue;
            }

            // Copy retransmission data
            let mut retx_copy = [0u8; TCP_MSS];
            {
                let t = TCP.lock();
                let c = &t.conns[idx];
                retx_copy[..retx_len].copy_from_slice(&c.retx_buf[..retx_len]);
            }

            // Retransmit
            send_segment(&mac, &ip, &rip, lp, rp,
                         TCP_ACK, retx_seq, ack_val, &retx_copy[..retx_len]);

            // Exponential backoff (RFC 6298 §5.5)
            let mut t = TCP.lock();
            let c = &mut t.conns[idx];
            c.retx_count = c.retx_count.saturating_add(1);
            c.retx_time  = now;
            let rto_max  = RTO_MAX_MS * TICKS_PER_MS;
            c.rto_ticks  = if c.rto_ticks.saturating_mul(2) > rto_max {
                rto_max
            } else {
                c.rto_ticks.saturating_mul(2)
            };

            // Congestion response: set ssthresh, reset cwnd
            c.ssthresh = (c.cwnd / 2).max(CWND_INITIAL);
            c.cwnd     = TCP_MSS as u32; // collapse to 1 segment
            continue;
        }

        // --- TIME-WAIT timer (F01) ---
        if state == TcpState::TimeWait {
            let tw_start = { TCP.lock().conns[idx].time_wait_start };
            let tw_duration = TIME_WAIT_MS * TICKS_PER_MS;
            if now.saturating_sub(tw_start) >= tw_duration {
                let mut t = TCP.lock();
                t.conns[idx].state = TcpState::Closed;
                t.conns[idx].unacked = false;
            }
            continue;
        }

        // --- Keep-alive (Established only) ---
        if state == TcpState::Established && !unacked {
            if now.saturating_sub(last_act) >= KEEPALIVE_INTERVAL_TICKS {
                if ka_probes >= KEEPALIVE_MAX_PROBES {
                    // No response — close connection
                    let mut t = TCP.lock();
                    t.conns[idx].state   = TcpState::Closed;
                    t.conns[idx].unacked = false;
                    continue;
                }

                // Send keep-alive probe: ACK with seq = snd.nxt - 1
                let seq_val = {
                    let t = TCP.lock();
                    t.conns[idx].seq.wrapping_sub(1)
                };
                send_segment(&mac, &ip, &rip, lp, rp, TCP_ACK, seq_val, ack_val, &[]);

                let mut t = TCP.lock();
                t.conns[idx].keepalive_probes = t.conns[idx].keepalive_probes.saturating_add(1);
                t.conns[idx].last_activity    = now;
            }
        }
    }
}

/// Return connection state for a given index.
pub fn conn_state(idx: usize) -> TcpState {
    if idx >= TCP_MAX_CONNS { return TcpState::Closed; }
    TCP.lock().conns[idx].state
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Check if an incoming ACK advances past our retransmission buffer.
/// Returns true if ack_num acknowledges data at [retx_seq, retx_seq + retx_len).
fn is_ack_advancing(ack_num: u32, retx_seq: u32, retx_len: usize) -> bool {
    if retx_len == 0 { return false; }
    let end = retx_seq.wrapping_add(retx_len as u32);
    // ack_num should be > retx_seq (wrapping) and <= end (wrapping)
    let past_start = ack_num.wrapping_sub(retx_seq) > 0
        && ack_num.wrapping_sub(retx_seq) <= retx_len as u32;
    // Or exactly at end
    past_start || ack_num == end
}

/// Build and send a TCP segment with MSS option (for SYN and SYN-ACK).
fn send_syn_segment(
    our_mac:  &[u8; 6],
    our_ip:   &[u8; 4],
    dst_ip:   &[u8; 4],
    src_port: u16,
    dst_port: u16,
    flags:    u8,
    seq:      u32,
    ack:      u32,
) -> i32 {
    let tcp_len = TCP_HDR_MAX; // 24 bytes: 20-byte header + 4-byte MSS option

    let mut buf = [0u8; TCP_SEGMENT_BUF_SIZE];
    let hdr = unsafe { &mut *(buf.as_mut_ptr() as *mut TcpHdr) };
    hdr.src_port = src_port.to_be_bytes();
    hdr.dst_port = dst_port.to_be_bytes();
    hdr.seq      = seq.to_be_bytes();
    hdr.ack      = ack.to_be_bytes();
    hdr.data_off = TCP_DATA_OFF_MSS;
    hdr.flags    = flags;
    hdr.window   = TCP_WINDOW_SIZE.to_be_bytes();
    hdr.checksum = [0, 0];
    hdr.urgent   = [0, 0];

    // Write MSS option after the 20-byte header
    buf[TCP_HDR_MIN]     = TCP_OPT_MSS;
    buf[TCP_HDR_MIN + 1] = TCP_OPT_MSS_LEN;
    let mss_bytes = (TCP_MSS as u16).to_be_bytes();
    buf[TCP_HDR_MIN + 2] = mss_bytes[0];
    buf[TCP_HDR_MIN + 3] = mss_bytes[1];

    // TCP checksum over pseudo-header
    let pseudo = ip::pseudo_checksum(our_ip, dst_ip, ip::IP_PROTO_TCP, tcp_len as u16);
    let cs = tcp_checksum(pseudo, &buf[..tcp_len]);
    let cb = cs.to_be_bytes();
    buf[TCP_CHECKSUM_OFFSET]    = cb[0];
    buf[TCP_CHECKSUM_OFFSET_HI] = cb[1];

    ip::send(our_mac, our_ip, dst_ip, ip::IP_PROTO_TCP, &buf[..tcp_len])
}

/// Build and send a TCP segment.  Returns 0 on success.
fn send_segment(
    our_mac:  &[u8; 6],
    our_ip:   &[u8; 4],
    dst_ip:   &[u8; 4],
    src_port: u16,
    dst_port: u16,
    flags:    u8,
    seq:      u32,
    ack:      u32,
    data:     &[u8],
) -> i32 {
    let tcp_len = TCP_HDR_MIN + data.len();
    if tcp_len > TCP_SEGMENT_BUF_SIZE { return -1; }

    let mut buf = [0u8; TCP_SEGMENT_BUF_SIZE];
    let hdr = unsafe { &mut *(buf.as_mut_ptr() as *mut TcpHdr) };
    hdr.src_port = src_port.to_be_bytes();
    hdr.dst_port = dst_port.to_be_bytes();
    hdr.seq      = seq.to_be_bytes();
    hdr.ack      = ack.to_be_bytes();
    hdr.data_off = TCP_DATA_OFF_MIN;
    hdr.flags    = flags;
    hdr.window   = TCP_WINDOW_SIZE.to_be_bytes();
    hdr.checksum = [0, 0];
    hdr.urgent   = [0, 0];
    buf[TCP_HDR_MIN..tcp_len].copy_from_slice(data);

    // TCP checksum over pseudo-header
    let pseudo = ip::pseudo_checksum(our_ip, dst_ip, ip::IP_PROTO_TCP, tcp_len as u16);
    let cs = tcp_checksum(pseudo, &buf[..tcp_len]);
    let cb = cs.to_be_bytes();
    buf[TCP_CHECKSUM_OFFSET]    = cb[0];
    buf[TCP_CHECKSUM_OFFSET_HI] = cb[1];

    ip::send(our_mac, our_ip, dst_ip, ip::IP_PROTO_TCP, &buf[..tcp_len])
}

fn tcp_checksum(pseudo_sum: u32, data: &[u8]) -> u16 {
    let mut sum = pseudo_sum;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}
