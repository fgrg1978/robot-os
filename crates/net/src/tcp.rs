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
pub use robot_os_limits::TCP_MAX_CONNS;
use wcet_macro::wcet;

// ---------------------------------------------------------------------------
// Connection limits
// ---------------------------------------------------------------------------

/// Per-connection receive ring buffer size (bytes).
/// Must be a power of two for efficient modular arithmetic.
/// Sized to keep OTA / large transfers flowing without window-stalls: a
/// 4 KB buffer fills in ~3 segments at MSS=1460 and stalls the sender if
/// the consumer task can't drain instantly. 128 KB gives the OTA recv task
/// generous breathing room across FAT32 write latencies and burst arrivals.
const TCP_BUF_SIZE: usize = robot_os_limits::TCP_BUF_SIZE;

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

/// Runtime-seeded secret for ISN generation. Was previously a compile-time
/// constant; an attacker with the binary could predict every initial sequence
/// number and trivially hijack a TCP session (RFC 6528 explicitly warns
/// against this). Seeded once at `net_init()` from the CLINT cycle counter
/// (and any other entropy available — the current bare-metal target lacks a
/// dedicated TRNG crate, so we mix the boot mtime into a static atomic; this
/// is not strong randomness but it is unpredictable from a static analysis of
/// the binary alone, which is the immediate threat).
static ISN_SECRET: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0xA5F0_3C7B);

/// Seed the ISN secret from a runtime entropy source. Called once during
/// `net_init`. Safe to call multiple times — last seed wins.
pub fn isn_secret_seed(entropy: u32) {
    // XOR-mix to preserve any prior seed contribution.
    let prev = ISN_SECRET.load(core::sync::atomic::Ordering::Relaxed);
    ISN_SECRET.store(prev ^ entropy, core::sync::atomic::Ordering::Relaxed);
}

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

// ---------------------------------------------------------------------------
// Handshake (half-open) timer
// ---------------------------------------------------------------------------

/// Interval between SYN / SYN-ACK retransmissions, in milliseconds.
///
/// Deliberately a fixed interval and NOT `rto_ticks`: backing off the
/// connection's RTO during the handshake would carry an inflated value into
/// `Established`, where nothing resets it until the first RTT sample, and the
/// first lost data segment would then wait seconds instead of ~1 s.
const SYN_RETRY_INTERVAL_MS: u64 = 1_000;

/// SYN / SYN-ACK retransmissions before a half-open slot is abandoned.
///
/// This is what bounds the lifetime of a half-open connection: 4 retries at
/// `SYN_RETRY_INTERVAL_MS` means a `SynSent` / `SynRcvd` slot lives at most
/// ~5 s without progress, then returns to `Closed`.
///
/// Before this existed nothing reaped those slots at all — `tcp_tick`'s only
/// reaper was gated on `unacked`, which neither `connect()` nor the listener
/// path ever sets — so `MAX_HALF_OPEN_PER_LISTENER`'s comment about slots
/// being "reaped on RTO" described a mechanism that did not exist.  Four
/// SYNs that were never completed silenced a listener permanently, and eight
/// consumed `TCP_MAX_CONNS`, so the robot could not dial out either.  Both
/// conditions survived until reboot.
const SYN_MAX_RETRIES: u8 = 4;

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
            rto_ticks:    0,
            srtt:         0,
            rttvar:       0,
            retx_count:   0,
            unacked:      false,

            last_activity:    0,
            keepalive_probes: 0,

            cwnd:          0,
            ssthresh:      0,
            dup_ack_count: 0,
            last_ack_recv: 0,

            remote_mss:    0,
            remote_window: 0,


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
    ///
    /// Called at slot creation on both open paths (`connect` and the listener's
    /// `SynRcvd`), so `retx_time` doubles as the slot's creation timestamp:
    /// `tcp_tick` uses it to age out half-open connections.  It is seeded to
    /// *now* rather than 0 for exactly that reason — a zero would read as
    /// "sent at boot" and make the first tick retransmit immediately.
    fn reset_conn_state(&mut self) {
        self.retx_len         = 0;
        self.retx_seq         = 0;
        self.retx_time        = robot_os_drivers::clint::get_time();
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

    // Hash secret seed (4 bytes). Loaded at runtime — see `ISN_SECRET` doc.
    let secret = ISN_SECRET
        .load(core::sync::atomic::Ordering::Relaxed)
        .to_le_bytes();
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
                    let mss = u16::from_be_bytes([opts[i + 2], opts[i + 3]]);
                    // Floor the peer's value: `send_data` sizes every segment
                    // by `remote_mss`, so an advertised MSS of 0 makes it
                    // return 0 forever and `send_all_with_yield` burns its
                    // whole yield budget without progress — a peer-inflicted
                    // stall. 64 is the same defensive floor Linux applies
                    // (tcp_min_snd_mss) for exactly this attack.
                    return mss.max(64);
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

/// Next expected sequence number after accepting a segment that carries FIN.
///
/// A FIN occupies one sequence number of its own, immediately after any
/// payload the same segment carried — so the acknowledgement the peer expects
/// is `seq + payload_len + 1`, not `seq + payload_len`. Getting this wrong by
/// one leaves the peer retransmitting a FIN we believe we already
/// acknowledged, and the connection sits in TimeWait on our side while the
/// peer never reaches CLOSED.
///
/// Wrapping throughout: sequence numbers are modulo 2^32, and with
/// `overflow-checks = true` a bare `+` here would turn a connection that
/// happens to straddle the wrap point into a board reset.
fn fin_next_ack(seq: u32, payload_len: usize) -> u32 {
    seq.wrapping_add(payload_len as u32).wrapping_add(1)
}

/// RFC 793 §3.3 acceptability test for a segment on a synchronised connection.
///
/// Every inbound segment must pass this — not only the ones carrying data.
/// The window check used to sit inside `if !payload.is_empty()`, so a
/// payload-free segment skipped it entirely and still reached the ACK
/// processing that stores the peer's advertised window: a single spoofed bare
/// ACK advertising window 0 stopped the transmit side forever (`send_data`
/// returns 0 on a closed window) while the connection went on looking healthy.
/// The same unchecked path drove `dup_ack_count` and the congestion window.
///
/// With this in place, an off-path attacker who has already guessed the
/// 4-tuple must additionally land `seq` inside a 64 KiB window out of the
/// 2^32 sequence space.
///
/// The window used is the constant `TCP_WINDOW_SIZE`, not the live advertised
/// window: it is a superset of what we have really advertised, which keeps
/// legitimate peers (retransmissions, segments in flight when our window
/// shrank) from being rejected while still costing a blind attacker ~2^16.
///
/// **Keep-alive exception** (RFC 1122 §4.2.3.6): a zero-length segment at
/// `RCV.NXT - 1` is the standard keep-alive probe, and `tcp_tick` sends
/// exactly that shape itself — so in a two-node deployment of this stack, both
/// ends must accept it or each silently drops the other's probes and the
/// connection is torn down by the keep-alive timer.
fn segment_acceptable(seq_num: u32, seg_len: usize, rcv_nxt: u32) -> bool {
    if seq_in_window(seq_num, rcv_nxt, TCP_WINDOW_SIZE as u32) {
        return true;
    }
    seg_len == 0 && seq_num == rcv_nxt.wrapping_sub(1)
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

/// Re-point the TCP layer at a new local address.
///
/// TCP keeps its own copy of the address because every segment needs it for the
/// pseudo-header. `init` used to be the only writer, so anything that changed
/// the address afterwards — DHCP is the one that matters — left TCP building
/// and verifying checksums against the OLD address while the rest of the stack
/// had moved on. Symptom: every TCP segment silently fails checksum and the
/// connection never establishes, with no error anywhere.
///
/// Called from `net_set_ip`, so callers configure the address in one place.
/// Changing the address with connections live is not meaningful — existing
/// slots keep the endpoints they were opened with — but at boot, which is when
/// DHCP runs, there are none.
pub fn set_our_ip(ip: [u8; 4]) {
    let mut t = TCP.lock();
    t.our_ip = ip;
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

/// Maximum number of `yield_fn` calls `connect_with_yield` will spin while
/// waiting for the ARP cache to resolve the destination MAC before sending
/// the first SYN.  Sized for a few wall-ms under hardware (ARP reply
/// arrives in tens of µs) and a fraction of a second under QEMU TCG SMP
/// (the slowest case observed).
pub const CONNECT_ARP_MAX_YIELDS: u32 = 10_000;

/// Initiate an outgoing TCP connection.  Returns connection index or -1.
/// Connection is not established until state == Established.
///
/// **First-SYN behavior**: this function does NOT block on ARP — if the
/// destination MAC is not cached, `ip::send` returns -1, the SYN is dropped
/// silently, and the caller must retry (typically via the brain-task's
/// outer reconnect loop).  Use `connect_with_yield` instead for a
/// blocking-with-yield variant that issues the SYN only once ARP has
/// resolved, eliminating the "first attempt always stalls" pattern under
/// SLIRP/QEMU.
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

/// Like `connect`, but yields until the ARP cache has the destination MAC
/// before issuing the SYN.  This eliminates the "first SYN dropped silently
/// due to ARP miss" pattern that otherwise costs an entire RTO + close-and-
/// reconnect cycle (~1.5-2 s observed under SLIRP/QEMU).
///
/// The first `ip::send` inside `crate::send_syn_segment` would normally
/// trigger an ARP request and return -1.  Here we issue the ARP request up
/// front, yield-poll the cache for resolution, then call the standard
/// `connect` (which now hits the populated cache and sends the SYN in one
/// pass).
///
/// `yield_fn` is injected to keep `crates/net` scheduler-agnostic — same
/// pattern as `send_all_with_yield`.  Falls back to the non-blocking
/// `connect` after `CONNECT_ARP_MAX_YIELDS` to bound worst-case latency.
pub fn connect_with_yield<F: FnMut()>(
    dst_ip:   [u8; 4],
    dst_port: u16,
    src_port: u16,
    mut yield_fn: F,
) -> i32 {
    // Snapshot our addresses to issue the ARP request without holding TCP.lock.
    let (our_mac, our_ip) = { let t = TCP.lock(); (t.our_mac, t.our_ip) };

    // If the cache already has the entry (subsequent connects, gateway hot),
    // skip the resolution step entirely.
    if crate::arp::lookup(&dst_ip).is_none() {
        crate::arp::send_request(&our_mac, &our_ip, &dst_ip);
        let mut yields: u32 = 0;
        while crate::arp::lookup(&dst_ip).is_none() && yields < CONNECT_ARP_MAX_YIELDS {
            yield_fn();
            yields += 1;
        }
        // If we timed out, fall through to plain `connect` anyway — it'll
        // fire another ARP and return SynSent; caller's reconnect loop is
        // still the last-resort fallback.
    }

    connect(dst_ip, dst_port, src_port)
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
        .min(TCP_MSS)               // never emit a segment larger than our own MSS
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

/// Maximum number of `yield_fn` calls `send_all_with_yield` will spin before
/// giving up.  Acts as a soft timeout — if the peer never advances cwnd /
/// never ACKs, we don't loop forever.  Sized for tens of seconds under QEMU
/// TCG (where each yield may cost ~milliseconds) and a few hundred ms on
/// real hardware.
pub const SEND_ALL_MAX_YIELDS: u32 = 10_000;

/// Read the unacked flag for a connection (true while a sent segment is
/// awaiting ACK).  Exposed so multi-segment senders can wait for an in-flight
/// segment to clear before overwriting `retx_buf`.
pub fn is_unacked(idx: usize) -> bool {
    if idx >= TCP_MAX_CONNS { return false; }
    let t = TCP.lock();
    t.conns[idx].unacked
}

/// Send all bytes of `data`, looping over partial `send_data` calls.
///
/// `send_data` returns at most one TCP segment's worth (bounded by cwnd,
/// remote MSS, or remote window — whichever is smallest).  Callers that
/// trust the first return value drop every byte past the segment boundary
/// (root cause of #39 / sensor-pump pt2).
///
/// This helper loops until all `data.len()` bytes have been handed to the
/// stack OR the connection drops OR the yield budget is exhausted (timeout).
/// Between segments it waits for the previous segment's ACK so the next
/// `send_data` call doesn't overwrite `retx_buf` while the previous segment
/// is still in flight.
///
/// `yield_fn` is injected (instead of pulling in `crates/sched`) to keep the
/// `net` crate scheduler-agnostic.  Kernel callers pass
/// `robot_os_sched::task_yield`; userspace can pass a syscall yield.
///
/// Returns bytes successfully sent (≤ `data.len()`).  Callers should check
/// `sent < data.len()` to detect partial completion (rare — peer stall).
pub fn send_all_with_yield<F: FnMut()>(
    idx: usize,
    data: &[u8],
    mut yield_fn: F,
) -> usize {
    let mut sent_total: usize = 0;
    let mut yields: u32 = 0;
    while sent_total < data.len() && yields < SEND_ALL_MAX_YIELDS {
        // Wait for any in-flight segment to ACK before sending more — see
        // is_unacked rationale above.
        while is_unacked(idx) && yields < SEND_ALL_MAX_YIELDS {
            yield_fn();
            yields += 1;
        }
        if yields >= SEND_ALL_MAX_YIELDS { break; }

        let n = send_data(idx, &data[sent_total..]);
        if n < 0 {
            // Connection dropped or fd invalid — caller will observe partial.
            break;
        }
        if n == 0 {
            // Peer window closed / cwnd not yet open — yield and retry.
            yield_fn();
            yields += 1;
            continue;
        }
        sent_total += n as usize;
    }
    sent_total
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

/// Handle an incoming TCP segment with receive-side checksum validation.
///
/// Called from `ip::handle`, which forwards both IP endpoints.
///
/// Requires both endpoints because the TCP checksum covers the IPv4
/// pseudo-header (src, dst, proto, length).  This is the ONLY ingress path —
/// the segment processor below is private, so no future caller can reach it
/// without passing through this validation.
///
/// Unlike UDP the TCP checksum is mandatory (RFC 793 §3.1): there is no
/// "sender opted out" encoding, so any mismatch is an unconditional drop.
pub fn handle_checked(src_ip: &[u8; 4], dst_ip: &[u8; 4], data: &[u8]) {
    if data.len() < TCP_HDR_MIN { return; }
    // TCP carries no length field of its own — the segment length comes from
    // the IP total length, i.e. exactly what `ip::handle` sliced for us.
    let tcp_len = match u16::try_from(data.len()) { Ok(n) => n, Err(_) => return };

    // Sum the segment *including* the stored checksum; a correct segment folds
    // to 0xFFFF, so the complement must be zero.
    let pseudo = ip::pseudo_checksum(src_ip, dst_ip, ip::IP_PROTO_TCP, tcp_len);
    if tcp_checksum(pseudo, data) != 0 { return; }

    handle(src_ip, data);
}

/// Process a TCP segment whose checksum has already been verified.
///
/// Private by design: `handle_checked` is the only way in, so this cannot be
/// reached without the mandatory checksum validation having run first.
fn handle(src_ip: &[u8; 4], data: &[u8]) {
    if data.len() < TCP_HDR_MIN { return; }
    let hdr = unsafe { &*(data.as_ptr() as *const TcpHdr) };
    let dst_port  = u16::from_be_bytes(hdr.dst_port);
    let src_port  = u16::from_be_bytes(hdr.src_port);
    let seq       = u32::from_be_bytes(hdr.seq);
    let ack_num   = u32::from_be_bytes(hdr.ack);
    let flags     = hdr.flags;

    // Sensor-pump (#39) trace probe: log every inbound TCP segment with
    // flags + seq + ack + payload length so we can compare against what
    // the brain claims to send.  Gated on `--features qemu` so production
    // builds pay nothing.
    #[cfg(feature = "qemu")]
    {
        let off_hdr = ((hdr.data_off >> 4) as usize) * 4;
        let off_hdr = if off_hdr < TCP_HDR_MIN { TCP_HDR_MIN } else { off_hdr };
        let pl_len = data.len().saturating_sub(off_hdr);
        robot_os_drivers::kprintln!(
            "[TCP-RX] src={}.{}.{}.{}:{} -> :{} flags=0x{:02x} seq={} ack={} pl={}B",
            src_ip[0], src_ip[1], src_ip[2], src_ip[3], src_port,
            dst_port, flags, seq, ack_num, pl_len
        );
    }
    // Peer's advertised receive window — we cap our outbound size by
    // this so we don't overshoot and force the peer to drop segments.
    let peer_win  = u16::from_be_bytes(hdr.window);
    let off       = ((hdr.data_off >> 4) as usize) * 4;
    // RFC 793: data_off >= 5 (20-byte minimum header) and the header cannot
    // extend past the segment end.  Both are malformed — DROP, don't guess.
    // The previous clamp (off < 20 → treat as 20) reinterpreted option bytes
    // as payload; the `off > len → payload = &[]` fallback processed flags
    // from a header that claims bytes the segment doesn't contain.
    if off < TCP_HDR_MIN || off > data.len() { return; }
    let payload   = &data[off..];

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
            // Every state validates the RST before acting on it. The previous
            // version exempted SynSent/SynRcvd with the comment "validated
            // elsewhere" — there was no elsewhere, so a RST bearing any
            // sequence number closed a connecting socket.
            let (st, rcv_nxt, iss) = {
                let t = TCP.lock();
                (t.conns[idx].state, t.conns[idx].ack, t.conns[idx].seq)
            };
            let acceptable = match st {
                // RFC 793 §3.4: a RST is only meaningful here if it
                // acknowledges our SYN, i.e. it came from a host that actually
                // saw it. Without the ACK check the ISN randomisation buys
                // nothing on this path: any RST at all killed the connect.
                TcpState::SynSent => {
                    flags & TCP_ACK != 0 && ack_num == iss.wrapping_add(1)
                }
                // RFC 793: ignore a RST while listening. `find_conn` can match
                // a Listen slot (its remote endpoint is 0.0.0.0:0), so a
                // spoofed segment from 0.0.0.0:0 would otherwise destroy the
                // listener — the server socket vanishes with no other symptom.
                TcpState::Listen | TcpState::Closed => false,
                // SynRcvd and every synchronised state: RFC 5961 — the
                // sequence must fall in the receive window, or an off-path
                // attacker who knows the 4-tuple tears the flow down with one
                // spoofed segment.
                _ => seq_in_window(seq, rcv_nxt, TCP_WINDOW_SIZE as u32),
            };
            if !acceptable {
                return; // unacceptable RST — ignore (do not tear down)
            }
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
                // RFC 793 §3.4: a SYN-ACK is only ours if it acknowledges our
                // SYN — SEG.ACK must equal ISS+1. `c.seq` still holds the ISS
                // here (it is incremented on the transition below).
                //
                // This arm used to match on flags alone and never read
                // `ack_num`, which made the RFC 6528 ISN randomisation
                // decorative on the active-open path: an off-path attacker who
                // guessed the 4-tuple (16 tries, given the deterministic
                // ephemeral port) could complete our handshake without ever
                // seeing the SYN-ACK, and hand the application a connection to
                // a host of their choosing.
                {
                    let t = TCP.lock();
                    if ack_num != t.conns[idx].seq.wrapping_add(1) { return; }
                }
                // SYN-ACK received → parse MSS, send ACK, move to Established
                let remote_mss = parse_mss_option(data, off);
                {
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    // Our SYN consumed one sequence number (RFC 793 §3.3): the
                    // peer ACKs ISN+1, so SND.NXT must advance to ISN+1 before
                    // the first data byte. The passive-open path (SynRcvd→ACK
                    // below) already does this; the active-open path did not,
                    // leaving c.seq one BEHIND. Effect: the first data segment
                    // started at ISN, whose first byte the peer treats as the
                    // already-ACKed SYN slot and discards — so the app received
                    // (len-1) bytes of the FIRST post-connect segment. Streaming
                    // senders (sensor pump) self-realign after one frame and the
                    // brain MAGIC-resyncs past it, which is why this stayed
                    // latent; a one-shot request/response (the RFC-0019 handshake
                    // reply, #34/#74) has no second frame to recover, so it
                    // surfaced as "stub reads 65 of 66 bytes" → handshake stall.
                    c.seq   = c.seq.wrapping_add(1);
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
                // Client's ACK completing the 3-way handshake → Established.
                //
                // RFC 793 §3.4 / RFC 6528: the ACK must acknowledge the SYN-ACK
                // we sent, i.e. SEG.ACK == our ISS + 1. Without this the
                // handshake completes on an ACK the peer could not have
                // computed: an off-path attacker spoofs a SYN from a victim
                // address, then immediately spoofs a bare ACK without ever
                // seeing our SYN-ACK, and `accept()` hands the application a
                // connection that appears to come from the victim. Checking
                // the ISN on the way back in is the entire reason for
                // generating it unpredictably.
                let mut t = TCP.lock();
                let c = &mut t.conns[idx];
                if ack_num != c.seq.wrapping_add(1) { return; }
                c.seq   = c.seq.wrapping_add(1);
                c.state = TcpState::Established;
                c.remote_window = peer_win;
                c.last_activity = now;
                c.keepalive_probes = 0;
            }
            TcpState::Established => {
                // --- Sequence number validation ---
                //
                // Applied to EVERY segment, data-bearing or not. See
                // `segment_acceptable`: the check used to be nested inside the
                // payload branch, so a bare ACK reached the window/congestion
                // bookkeeping below with no validation at all.
                let expected_ack = { TCP.lock().conns[idx].ack };
                if !segment_acceptable(seq, payload.len(), expected_ack) {
                    // Out-of-window segment — drop silently
                    return;
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
                    // Only honour a FIN whose sequence is in the receive window
                    // (RFC 5961). The check at the top of this arm is broader —
                    // it also admits the RFC 1122 keep-alive shape at
                    // `RCV.NXT - 1`, which must never be read as a connection
                    // teardown — so a FIN is re-tested against the window here.
                    if !seq_in_window(seq, expected_ack, TCP_WINDOW_SIZE as u32) {
                        return;
                    }
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    // The FIN occupies sequence number seq + payload_len.  Only
                    // consume it once every byte before it has actually landed
                    // in the rx ring (c.ack caught up to it).  Two failure
                    // modes otherwise: (a) ring filled mid-segment — c.ack
                    // advanced only by `stored`, and +1 here would acknowledge
                    // a data byte we dropped, silently losing the tail of the
                    // stream; (b) the FIN segment arrived out of order (its
                    // payload went to the OOO buffer) — +1 would desync the
                    // flow entirely.  In both cases just skip: the earlier ACK
                    // reported what we stored, and the peer retransmits the
                    // remaining payload + FIN.
                    if c.ack != seq.wrapping_add(payload.len() as u32) {
                        return;
                    }
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
                        // Same rule as the branch below: the FIN must fall in
                        // the receive window before we consume it, or a
                        // spoofed FIN bearing any sequence at all pushes us
                        // into TimeWait. An out-of-window FIN is not ours to
                        // consume — the ACK above is still valid, so fall
                        // through to FinWait2 rather than dropping it.
                        if flags & TCP_FIN != 0
                            && seq_in_window(seq, c.ack, TCP_WINDOW_SIZE as u32)
                        {
                            // Simultaneous close: FIN+ACK → TimeWait
                            c.ack = fin_next_ack(seq, payload.len());
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
                    // Peer FIN before our FIN is ACKed → simultaneous close.
                    // Unvalidated, any spoofed FIN carrying any sequence at all
                    // pushed us into TimeWait and desynchronised the flow.
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    if !seq_in_window(seq, c.ack, TCP_WINDOW_SIZE as u32) { return; }
                    c.ack = fin_next_ack(seq, payload.len());
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
                    // The FIN must fall in the receive window. This state
                    // previously accepted a FIN bearing any sequence number
                    // whatsoever, which both closed the connection on command
                    // and left `c.ack` pointing somewhere arbitrary.
                    if !seq_in_window(seq, c.ack, TCP_WINDOW_SIZE as u32) { return; }
                    c.ack = fin_next_ack(seq, payload.len());
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
                    let c = &mut t.conns[idx];
                    // Only the ACK of OUR FIN closes the connection
                    // (SEG.ACK == FIN sequence + 1). Any ACK used to do it,
                    // including one carrying a stale or forged acknowledgement
                    // number, so the slot could be freed while the peer still
                    // considered the connection open — and reused for a new
                    // peer while the old one's segments were still arriving.
                    if ack_num != c.fin_seq.wrapping_add(1) { return; }
                    c.state = TcpState::Closed;
                    c.unacked = false;
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
                // Drop the SYN — the peer will retransmit. A legitimate peer
                // gets in once a half-open slot completes or is reaped: the
                // handshake timer in `tcp_tick` forces any SynRcvd slot back
                // to Closed after `SYN_MAX_RETRIES` × `SYN_RETRY_INTERVAL_MS`
                // (~5 s), so this cap can only ever delay a connection, never
                // deafen the listener permanently.
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
        // Evict the entry with the highest sequence number (farthest from
        // c.ack — least likely to become contiguous soon).  Track the best
        // candidate explicitly: the previous version seeded max_seq = 0, and
        // `0.wrapping_sub(c.ack)` is ~u32::MAX for any nonzero ack, so no
        // real in-window entry ever beat the seed and slot 0 was always the
        // one evicted, regardless of its sequence.
        let mut best: Option<(usize, u32)> = None;
        for i in 0..OOO_MAX_SEGMENTS {
            if !c.ooo_buf[i].valid { continue; }
            let off = c.ooo_buf[i].seq.wrapping_sub(c.ack);
            if best.map_or(true, |(_, b)| off > b) {
                best = Some((i, off));
            }
        }
        if let Some((i, _)) = best {
            slot = Some(i);
            c.ooo_count = c.ooo_count.saturating_sub(1);
        }
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
             retx_seq, retx_len, conn_seq, ack_val, lp, rp, rip) = {
            let t = TCP.lock();
            let c = &t.conns[idx];
            (c.state, c.unacked, c.retx_time, c.rto_ticks, c.retx_count,
             c.last_activity, c.keepalive_probes,
             c.retx_seq, c.retx_len, c.seq, c.ack, c.local_port, c.remote_port, c.remote_ip)
        };

        if state == TcpState::Closed || state == TcpState::Listen {
            continue;
        }

        // --- Handshake (half-open) timer ---
        //
        // The retransmission branch below is gated on `unacked`, which is
        // false for the whole handshake (neither `connect` nor the listener's
        // SynRcvd path arms it — a SYN carries no data to put in `retx_buf`).
        // That is why half-open slots previously lived forever: this was the
        // reaper `MAX_HALF_OPEN_PER_LISTENER` assumed existed. Four SYNs that
        // were opened and abandoned deafened a listener until reboot; eight
        // took every slot in the table.
        //
        // `retx_time` is seeded to the creation instant by `reset_conn_state`,
        // so it serves as both "when the last SYN went out" and "when this
        // slot was born", and `retx_count` — unused while `unacked` is false —
        // counts the retries. Retransmitting rather than only reaping is also
        // the correct behaviour: a dropped SYN-ACK now recovers in ~1 s
        // instead of stalling until the peer gives up.
        if state == TcpState::SynSent || state == TcpState::SynRcvd {
            let interval = SYN_RETRY_INTERVAL_MS * TICKS_PER_MS;
            if now.saturating_sub(retx_time) >= interval {
                if retx_count >= SYN_MAX_RETRIES {
                    // Budget spent — free the slot.
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    c.state        = TcpState::Closed;
                    c.unacked      = false;
                    c.retx_len     = 0;
                    c.retx_count   = 0;
                    c.was_accepted = false;
                    continue;
                }
                // Re-send our SYN (active open) or SYN-ACK (passive open).
                // `conn_seq` is still the ISS in both states: it is only
                // advanced on the transition to Established.
                let (syn_flags, syn_ack) = if state == TcpState::SynSent {
                    (TCP_SYN, 0)
                } else {
                    (TCP_SYN | TCP_ACK, ack_val)
                };
                send_syn_segment(&mac, &ip, &rip, lp, rp, syn_flags, conn_seq, syn_ack);

                let mut t = TCP.lock();
                let c = &mut t.conns[idx];
                c.retx_count = c.retx_count.saturating_add(1);
                c.retx_time  = now;
            }
            // Nothing below applies to a half-open connection.
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
#[wcet(200_us)]
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

    // Sensor-pump (#39) trace probe: log every outbound TCP segment.
    // Pairs with the [TCP-RX] probe in `handle()` so we have full
    // visibility into the wire conversation under `--features qemu`.
    // Also dump the first 16 bytes of payload for non-trivial segments
    // so we can see WHAT got serialised vs what the peer reads.
    #[cfg(feature = "qemu")]
    {
        robot_os_drivers::kprintln!(
            "[TCP-TX] :{} -> {}.{}.{}.{}:{} flags=0x{:02x} seq={} ack={} pl={}B",
            src_port,
            dst_ip[0], dst_ip[1], dst_ip[2], dst_ip[3], dst_port,
            flags, seq, ack, data.len()
        );
        if !data.is_empty() {
            let n = data.len().min(16);
            // Pre-format 16 bytes (zero-padded if shorter) so kprintln doesn't
            // need a slice-formatting helper.
            let mut hex = [0u8; 32];
            const HEX: &[u8; 16] = b"0123456789abcdef";
            for i in 0..n {
                hex[2*i]   = HEX[(data[i] >> 4) as usize];
                hex[2*i+1] = HEX[(data[i] & 0x0f) as usize];
            }
            robot_os_drivers::kprintln!(
                "[TCP-TX]   first {}B: {}",
                n,
                core::str::from_utf8(&hex[..2*n]).unwrap_or("?")
            );
        }
    }

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

/// Internet checksum seeded with a pseudo-header partial sum.
/// Used on the TCP TX path, and on both the TCP and UDP RX paths for
/// verification (a valid segment yields 0).  `udp.rs` reuses this rather than
/// carrying a second copy of the fold loop.
pub(crate) fn tcp_checksum(pseudo_sum: u32, data: &[u8]) -> u16 {
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
