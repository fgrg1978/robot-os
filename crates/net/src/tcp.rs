/// TCP layer — port of net/tcp.c
///
/// Simplified TCP state machine with 8 connections.
/// Supports listen/connect/send/recv/close.

use robot_os_sync::SpinLock;
use super::ip;

pub const TCP_MAX_CONNS: usize = 8;

/// Per-connection receive ring buffer size (bytes).
/// Must be a power of two for efficient modular arithmetic.
const TCP_BUF_SIZE: usize = 4096;

/// TCP Maximum Segment Size — max payload bytes per segment.
/// Standard Ethernet MTU (1500) minus IP header (20) minus TCP header (20).
const TCP_MSS: usize = 1460;

/// Maximum TCP segment buffer size (MSS + TCP header).
const TCP_SEGMENT_BUF_SIZE: usize = TCP_MSS + TCP_HDR_MIN;

/// TCP advertised receive window size (bytes).
/// Matches TCP_BUF_SIZE so the peer doesn't overrun our ring buffer.
const TCP_WINDOW_SIZE: u16 = TCP_BUF_SIZE as u16;

/// TCP data offset for a minimal 20-byte header (5 × 4-byte words),
/// encoded in the upper 4 bits of the data_off field.
const TCP_DATA_OFF_MIN: u8 = 0x50;

/// Initial Sequence Number for outgoing connections.
/// TODO: replace with a proper ISN generator (RFC 6528).
const TCP_INITIAL_SEQ_OUT: u32 = 0x1234_5678;

/// Initial Sequence Number for accepted (incoming) connections.
/// TODO: replace with a proper ISN generator (RFC 6528).
const TCP_INITIAL_SEQ_IN: u32 = 0xDEAD_BEEF;

// TCP flags
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
#[allow(dead_code)]
const TCP_RST: u8 = 0x04;
const TCP_ACK: u8 = 0x10;

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

const TCP_HDR_MIN: usize = 20;

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
        }
    }

    pub fn rx_available(&self) -> usize {
        if self.rx_tail >= self.rx_head {
            self.rx_tail - self.rx_head
        } else {
            TCP_BUF_SIZE - self.rx_head + self.rx_tail
        }
    }
}

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
    let seq = TCP_INITIAL_SEQ_OUT;
    t.conns[idx].state       = TcpState::SynSent;
    t.conns[idx].local_ip    = ip;
    t.conns[idx].local_port  = src_port;
    t.conns[idx].remote_ip   = dst_ip;
    t.conns[idx].remote_port = dst_port;
    t.conns[idx].seq         = seq;
    t.conns[idx].ack         = 0;

    // Send SYN
    send_segment(&mac, &ip, &dst_ip, src_port, dst_port, TCP_SYN, seq, 0, &[]);
    idx as i32
}

/// Send data on an established connection.
pub fn send_data(idx: usize, data: &[u8]) -> i32 {
    if idx >= TCP_MAX_CONNS { return -1; }
    let (mac, ip, seq, dst_ip, src_port, dst_port, state) = {
        let t = TCP.lock();
        let c = &t.conns[idx];
        (t.our_mac, t.our_ip, c.seq, c.remote_ip, c.local_port, c.remote_port, c.state)
    };
    if state != TcpState::Established { return -1; }

    // Split into segments if needed (simplification: send all in one shot)
    let result = send_segment(&mac, &ip, &dst_ip, src_port, dst_port,
                              TCP_ACK, seq, 0, data);

    if result == 0 {
        let mut t = TCP.lock();
        t.conns[idx].seq = t.conns[idx].seq.wrapping_add(data.len() as u32);
        data.len() as i32
    } else {
        -1
    }
}

/// Read received data from a connection.  Returns bytes read, 0 if none.
pub fn recv(idx: usize, buf: &mut [u8]) -> i32 {
    if idx >= TCP_MAX_CONNS { return -1; }
    let mut t = TCP.lock();
    let c = &mut t.conns[idx];
    let avail = c.rx_available();
    if avail == 0 { return 0; }
    let n = avail.min(buf.len());
    for i in 0..n {
        buf[i] = c.rx_buf[c.rx_head % TCP_BUF_SIZE];
        c.rx_head = (c.rx_head + 1) % TCP_BUF_SIZE;
    }
    n as i32
}

/// Close a connection.
pub fn close(idx: usize) {
    if idx >= TCP_MAX_CONNS { return; }
    let (mac, ip, seq, ack, dst_ip, src_port, dst_port) = {
        let t = TCP.lock();
        let c = &t.conns[idx];
        (t.our_mac, t.our_ip, c.seq, c.ack, c.remote_ip, c.local_port, c.remote_port)
    };
    send_segment(&mac, &ip, &dst_ip, src_port, dst_port, TCP_FIN | TCP_ACK, seq, ack, &[]);
    let mut t = TCP.lock();
    t.conns[idx].state = TcpState::Closed;
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
    let _ack_num  = u32::from_be_bytes(hdr.ack);
    let flags     = hdr.flags;
    let off       = ((hdr.data_off >> 4) as usize) * 4;
    // TCP header is at least 20 bytes (data_off >= 5); reject malformed segments.
    let off       = if off < TCP_HDR_MIN { TCP_HDR_MIN } else { off };
    let payload   = if off <= data.len() { &data[off..] } else { &[] };

    let (mac, ip) = { let t = TCP.lock(); (t.our_mac, t.our_ip) };

    // Find an existing connection
    let idx_opt = { TCP.lock().find_conn(dst_port, src_ip, src_port) };

    if let Some(idx) = idx_opt {
        let state = { TCP.lock().conns[idx].state };
        match state {
            TcpState::SynSent if flags & TCP_SYN != 0 && flags & TCP_ACK != 0 => {
                // SYN-ACK received → send ACK, move to Established
                {
                    let mut t = TCP.lock();
                    t.conns[idx].ack   = seq.wrapping_add(1);
                    t.conns[idx].state = TcpState::Established;
                }
                let (seq_n, ack_n) = { let t = TCP.lock(); (t.conns[idx].seq, t.conns[idx].ack) };
                send_segment(&mac, &ip, src_ip, dst_port, src_port, TCP_ACK, seq_n, ack_n, &[]);
            }
            TcpState::SynRcvd if flags & TCP_ACK != 0 && flags & TCP_SYN == 0 => {
                // Client's ACK completing the 3-way handshake → Established
                let mut t = TCP.lock();
                t.conns[idx].seq   = t.conns[idx].seq.wrapping_add(1);
                t.conns[idx].state = TcpState::Established;
            }
            TcpState::Established => {
                if !payload.is_empty() {
                    // Enqueue received data
                    let mut t = TCP.lock();
                    let c = &mut t.conns[idx];
                    for &b in payload {
                        let next = (c.rx_tail + 1) % TCP_BUF_SIZE;
                        if next != c.rx_head {
                            c.rx_buf[c.rx_tail] = b;
                            c.rx_tail = next;
                        }
                    }
                    c.ack = seq.wrapping_add(payload.len() as u32);
                    let (seq_n, ack_n) = (c.seq, c.ack);
                    drop(t);
                    send_segment(&mac, &ip, src_ip, dst_port, src_port, TCP_ACK, seq_n, ack_n, &[]);
                }
                if flags & TCP_FIN != 0 {
                    let mut t = TCP.lock();
                    t.conns[idx].state = TcpState::CloseWait;
                }
            }
            _ => {}
        }
        return;
    }

    // Check if we have a listener
    let listener_idx = { TCP.lock().find_listener(dst_port) };
    if let Some(listen_idx) = listener_idx {
        if flags & TCP_SYN != 0 {
            // Accept: create new connection for this peer
            let new_idx_opt = { TCP.lock().alloc() };
            if let Some(new_idx) = new_idx_opt {
                let our_seq = TCP_INITIAL_SEQ_IN;
                {
                    let mut t = TCP.lock();
                    t.conns[new_idx].state       = TcpState::SynRcvd;
                    t.conns[new_idx].local_ip    = ip;
                    t.conns[new_idx].local_port  = dst_port;
                    t.conns[new_idx].remote_ip   = *src_ip;
                    t.conns[new_idx].remote_port = src_port;
                    t.conns[new_idx].seq         = our_seq;
                    t.conns[new_idx].ack         = seq.wrapping_add(1);
                    let _ = listen_idx; // keep listener alive
                }
                // Send SYN-ACK
                let ack_n = { TCP.lock().conns[new_idx].ack };
                send_segment(&mac, &ip, src_ip, dst_port, src_port,
                             TCP_SYN | TCP_ACK, our_seq, ack_n, &[]);
            }
        }
    }
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
    buf[16] = cb[0];
    buf[17] = cb[1];

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

/// Return connection state for a given index.
pub fn conn_state(idx: usize) -> TcpState {
    if idx >= TCP_MAX_CONNS { return TcpState::Closed; }
    TCP.lock().conns[idx].state
}
