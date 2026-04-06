/// UDP layer — port of net/udp.c
///
/// Simple UDP send/receive with 8 bound ports.
/// Ring-buffer per socket (4 packets, 512 bytes each).

use robot_os_sync::SpinLock;
use super::ip;

pub const UDP_MAX_SOCKETS: usize = 8;
const UDP_RX_SLOTS: usize = 4;
const UDP_RX_PKT_MAX: usize = 512;

/// Maximum UDP payload including header (MTU minus IP header).
const UDP_MAX_DGRAM: usize = ip::ETH_MTU - ip::IP_HDR_MIN;

#[repr(C, packed)]
struct UdpHdr {
    src_port: [u8; 2],
    dst_port: [u8; 2],
    length:   [u8; 2],
    checksum: [u8; 2],
}

const UDP_HDR_SIZE: usize = core::mem::size_of::<UdpHdr>();

// ── Per-packet receive slot ──────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct UdpRxPkt {
    data:     [u8; UDP_RX_PKT_MAX],
    len:      usize,
    src_ip:   [u8; 4],
    src_port: u16,
}

impl UdpRxPkt {
    const fn new() -> Self {
        UdpRxPkt { data: [0u8; UDP_RX_PKT_MAX], len: 0, src_ip: [0; 4], src_port: 0 }
    }
}

// ── Ring buffer for received packets ─────────────────────────────────────────

#[derive(Clone, Copy)]
struct UdpRxBuf {
    pkts: [UdpRxPkt; UDP_RX_SLOTS],
    head: usize,   // next slot to read
    tail: usize,   // next slot to write
    count: usize,
}

impl UdpRxBuf {
    const fn new() -> Self {
        UdpRxBuf { pkts: [UdpRxPkt::new(); UDP_RX_SLOTS], head: 0, tail: 0, count: 0 }
    }

    fn push(&mut self, src_ip: [u8; 4], src_port: u16, payload: &[u8]) {
        if self.count >= UDP_RX_SLOTS {
            // Drop oldest packet to make room
            self.head = (self.head + 1) % UDP_RX_SLOTS;
            self.count -= 1;
        }
        let n = payload.len().min(UDP_RX_PKT_MAX);
        let slot = &mut self.pkts[self.tail];
        slot.data[..n].copy_from_slice(&payload[..n]);
        slot.len      = n;
        slot.src_ip   = src_ip;
        slot.src_port = src_port;
        self.tail  = (self.tail + 1) % UDP_RX_SLOTS;
        self.count += 1;
    }

    fn pop(&mut self, buf: &mut [u8], src_ip: &mut [u8; 4], src_port: &mut u16) -> i32 {
        if self.count == 0 { return 0; }
        let slot = &self.pkts[self.head];
        let n = slot.len.min(buf.len());
        buf[..n].copy_from_slice(&slot.data[..n]);
        *src_ip   = slot.src_ip;
        *src_port = slot.src_port;
        self.head  = (self.head + 1) % UDP_RX_SLOTS;
        self.count -= 1;
        n as i32
    }
}

// ── Socket state ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub struct UdpSocket {
    pub local_port:  u16,
    pub remote_ip:   u32,
    pub remote_port: u16,
    pub bound:       bool,
    rx: UdpRxBuf,
}

impl UdpSocket {
    pub const fn new() -> Self {
        UdpSocket {
            local_port:  0,
            remote_ip:   0,
            remote_port: 0,
            bound:       false,
            rx:          UdpRxBuf::new(),
        }
    }
}

// ── Global socket table ──────────────────────────────────────────────────────

pub static UDP_SOCKETS: SpinLock<[UdpSocket; UDP_MAX_SOCKETS]> =
    SpinLock::new([UdpSocket::new(); UDP_MAX_SOCKETS]);

/// Bind a UDP socket to a local port.  Returns socket index or -1.
pub fn bind(port: u16) -> i32 {
    let mut socks = UDP_SOCKETS.lock();
    for i in 0..UDP_MAX_SOCKETS {
        if !socks[i].bound {
            socks[i].bound      = true;
            socks[i].local_port = port;
            socks[i].remote_ip  = 0;
            socks[i].remote_port = 0;
            socks[i].rx         = UdpRxBuf::new();
            return i as i32;
        }
    }
    -1
}

/// Unbind (close) a UDP socket.
pub fn unbind(idx: usize) {
    if idx >= UDP_MAX_SOCKETS { return; }
    let mut socks = UDP_SOCKETS.lock();
    socks[idx].bound = false;
}

/// Send a UDP datagram to `dst_ip:dst_port`.
/// Returns 0 on success, -1 on error.
pub fn sendto(
    sock:     i32,
    dst_ip:   &[u8; 4],
    dst_port: u16,
    data:     &[u8],
) -> i32 {
    if sock < 0 || sock as usize >= UDP_MAX_SOCKETS { return -1; }
    let idx = sock as usize;
    let (bound, src_port) = {
        let socks = UDP_SOCKETS.lock();
        (socks[idx].bound, socks[idx].local_port)
    };
    if !bound { return -1; }

    let (mac, ip) = (crate::net_get_mac(), crate::net_get_ip());
    send_raw(&mac, &ip, dst_ip, src_port, dst_port, data)
}

/// Send a raw UDP datagram (used internally and by DHCP).
pub fn send_raw(
    our_mac:  &[u8; 6],
    our_ip:   &[u8; 4],
    dst_ip:   &[u8; 4],
    src_port: u16,
    dst_port: u16,
    data:     &[u8],
) -> i32 {
    let total_len = UDP_HDR_SIZE + data.len();
    if total_len > UDP_MAX_DGRAM { return -1; }

    let mut buf = [0u8; UDP_MAX_DGRAM];
    let hdr = unsafe { &mut *(buf.as_mut_ptr() as *mut UdpHdr) };
    hdr.src_port = src_port.to_be_bytes();
    hdr.dst_port = dst_port.to_be_bytes();
    hdr.length   = (total_len as u16).to_be_bytes();
    hdr.checksum = [0, 0];  // Optional for IPv4 UDP
    buf[UDP_HDR_SIZE..total_len].copy_from_slice(data);

    ip::send(our_mac, our_ip, dst_ip, ip::IP_PROTO_UDP, &buf[..total_len])
}

/// Receive a datagram from a bound socket (non-blocking).
/// Returns bytes copied (>0), 0 if nothing pending, -1 on error.
/// Fills `src_ip` and `src_port` with the sender's address.
pub fn recvfrom(
    sock:     i32,
    buf:      &mut [u8],
    src_ip:   &mut [u8; 4],
    src_port: &mut u16,
) -> i32 {
    if sock < 0 || sock as usize >= UDP_MAX_SOCKETS { return -1; }
    let idx = sock as usize;
    let mut socks = UDP_SOCKETS.lock();
    if !socks[idx].bound { return -1; }
    socks[idx].rx.pop(buf, src_ip, src_port)
}

/// Simple recv (no source address).  Kept for backward compat with socket layer.
pub fn recv(idx: usize, buf: &mut [u8]) -> i32 {
    if idx >= UDP_MAX_SOCKETS { return -1; }
    let mut socks = UDP_SOCKETS.lock();
    if !socks[idx].bound { return -1; }
    let mut _ip  = [0u8; 4];
    let mut _port = 0u16;
    socks[idx].rx.pop(buf, &mut _ip, &mut _port)
}

/// Close a UDP socket by index.
pub fn close(sock: i32) {
    if sock < 0 { return; }
    unbind(sock as usize);
}

/// Handle an incoming UDP segment (called from ip::handle).
/// Dispatches payload to the correct bound socket's ring buffer.
/// DNS client port — intercept DNS responses before socket dispatch.
const DNS_CLIENT_PORT: u16 = 5353;

pub fn handle(src_ip: &[u8; 4], data: &[u8]) {
    if data.len() < UDP_HDR_SIZE { return; }
    let hdr = unsafe { &*(data.as_ptr() as *const UdpHdr) };
    let dst_port = u16::from_be_bytes(hdr.dst_port);
    let src_port = u16::from_be_bytes(hdr.src_port);
    let payload  = &data[UDP_HDR_SIZE..];

    // F05: Intercept DNS responses
    if dst_port == DNS_CLIENT_PORT {
        super::dns::handle_response(payload);
        return;
    }

    let mut socks = UDP_SOCKETS.lock();
    for i in 0..UDP_MAX_SOCKETS {
        if socks[i].bound && socks[i].local_port == dst_port {
            socks[i].rx.push(*src_ip, src_port, payload);
            return;
        }
    }
}
