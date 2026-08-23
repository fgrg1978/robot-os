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

/// DNS client port — intercept DNS responses before socket dispatch.
const DNS_CLIENT_PORT: u16 = 5353;
/// NTP client source port — intercept NTP responses before socket dispatch.
const NTP_CLIENT_PORT: u16 = 1123;

/// Validate the UDP length field against what IP actually delivered and return
/// the datagram trimmed to it.
///
/// RFC 768: `length` covers header + payload and is never below the 8-byte
/// header.  IP may hand us trailing padding (minimum Ethernet frame size), and
/// a crafted packet may declare a length longer than the bytes received — both
/// must be rejected/trimmed *before* any range is derived from the field, or
/// the slice below panics (kernel halt under `panic = "abort"`).
fn udp_segment(data: &[u8]) -> Option<&[u8]> {
    if data.len() < UDP_HDR_SIZE { return None; }
    let hdr = unsafe { &*(data.as_ptr() as *const UdpHdr) };
    let len = u16::from_be_bytes(hdr.length) as usize;
    if len < UDP_HDR_SIZE || len > data.len() { return None; }
    Some(&data[..len])
}

/// Handle an incoming UDP datagram with receive-side checksum validation.
/// Called from `ip::handle`, which forwards both IP endpoints.
///
/// Requires both endpoints because the UDP checksum covers the IPv4
/// pseudo-header (src, dst, proto, length).  This is the ONLY ingress path:
/// there is deliberately no unvalidated entry point, so a future caller cannot
/// accidentally bypass the checks below.
pub fn handle_checked(src_ip: &[u8; 4], dst_ip: &[u8; 4], data: &[u8]) {
    let seg = match udp_segment(data) { Some(s) => s, None => return };

    // RFC 768: for IPv4 the UDP checksum is OPTIONAL, and an all-zero field
    // means "the sender did not compute one".  Such a datagram must be
    // accepted unvalidated, not dropped — `dhcp.rs` transmits this way and so
    // do many real DHCP/TFTP servers, so rejecting it here would silently
    // break address acquisition.
    let hdr  = unsafe { &*(seg.as_ptr() as *const UdpHdr) };
    let csum = u16::from_be_bytes(hdr.checksum);
    if csum != 0 {
        // Verification sums the segment *including* the stored checksum; a
        // correct datagram folds to 0xFFFF, so the complement must be zero.
        // (A computed zero is transmitted as 0xFFFF, which verifies the same.)
        let pseudo = ip::pseudo_checksum(src_ip, dst_ip, ip::IP_PROTO_UDP, seg.len() as u16);
        if super::tcp::tcp_checksum(pseudo, seg) != 0 { return; }
    }

    dispatch(src_ip, seg);
}

/// Route a length-validated datagram to its interceptor or bound socket.
/// `seg` is guaranteed to be at least `UDP_HDR_SIZE` bytes by `udp_segment`.
fn dispatch(src_ip: &[u8; 4], seg: &[u8]) {
    let hdr = unsafe { &*(seg.as_ptr() as *const UdpHdr) };
    let dst_port = u16::from_be_bytes(hdr.dst_port);
    let src_port = u16::from_be_bytes(hdr.src_port);
    let payload  = &seg[UDP_HDR_SIZE..];

    // F05 / F05.2: intercept DNS and NTP responses.
    //
    // Both handlers receive the source endpoint, not just the payload. Routing
    // on `dst_port` alone tells you only which of OUR ports a datagram was
    // aimed at — which is public knowledge and exactly what an off-path
    // forgery targets. Only the source, checked against the server the query
    // actually went to, distinguishes an answer from an injection, and neither
    // handler can perform that check without these two arguments.
    if dst_port == DNS_CLIENT_PORT {
        super::dns::handle_response(src_ip, src_port, payload);
        return;
    }

    if dst_port == NTP_CLIENT_PORT {
        super::ntp::handle_response(src_ip, src_port, payload);
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

/// Receive a UDP-over-IPv6 datagram.
///
/// Called from `ipv6::ipv6_rx` when `next_hdr == NEXTHDR_UDP`.
/// Routes the payload to the matching bound socket by destination port.
///
/// # Source address and the v4 socket table
///
/// There is no AF_INET6 socket API in this stack, so a v6 datagram has to be
/// delivered — if at all — through the same table the IPv4 path uses, whose
/// `src_ip` field is four bytes wide. This function used to fill that field
/// with the last four bytes of the IPv6 source address. That is a forgery
/// primitive: those four bytes are entirely under a remote sender's control,
/// so any consumer that authenticates a peer by comparing `src_ip` (the
/// boot-time TFTP fetch in `tftp_client.rs` is the in-tree example) could be
/// handed a datagram that claims to be from the server while coming from an
/// arbitrary IPv6 host — and unlike the IPv4 path, nothing on the way here
/// checked it against a real v4 peer.
///
/// The source is therefore reported as the unspecified address `0.0.0.0`,
/// which is never a valid unicast peer, so every equality check against a real
/// server address fails closed. A future AF_INET6 socket layer should carry
/// the full 128-bit source instead of narrowing it; until one exists, refusing
/// to synthesize is the honest answer.
pub fn udpv6_rx(src_ipv6: &[u8; 16], dst_ipv6: &[u8; 16], data: &[u8]) {
    // Same rule as the IPv4 path: trim to the declared UDP length before any
    // range is derived from it, and reject a length below the header size or
    // beyond what IP actually delivered.
    let seg = match udp_segment(data) { Some(s) => s, None => return };
    let hdr = unsafe { &*(seg.as_ptr() as *const UdpHdr) };

    // RFC 8200 §8.1 INVERTS the IPv4 rule: over IPv6 the UDP checksum is
    // mandatory.  A zero checksum field is not "sender opted out" — it is
    // malformed and MUST be dropped.  A non-zero checksum must verify against
    // the IPv6 pseudo-header (both 128-bit addresses, UDP length, next-header).
    if u16::from_be_bytes(hdr.checksum) == 0 { return; }
    // `ipv6::pseudo_checksum` returns the RFC-1071 complement over the
    // pseudo-header plus `seg` (checksum field included); a valid datagram
    // folds to 0xFFFF, so the complement must be zero.
    if super::ipv6::pseudo_checksum(src_ipv6, dst_ipv6, super::ipv6::NEXTHDR_UDP, seg) != 0 {
        return;
    }

    let dst_port = u16::from_be_bytes(hdr.dst_port);
    let src_port = u16::from_be_bytes(hdr.src_port);
    let payload  = &seg[UDP_HDR_SIZE..];

    // Do NOT narrow the IPv6 source into the v4 field — see the note above.
    // 0.0.0.0 is the unspecified address: no peer legitimately has it, so a
    // consumer comparing it against an expected server address rejects.
    const SRC_UNSPECIFIED: [u8; 4] = [0, 0, 0, 0];
    let src_ip4 = SRC_UNSPECIFIED;

    let mut socks = UDP_SOCKETS.lock();
    for i in 0..UDP_MAX_SOCKETS {
        if socks[i].bound && socks[i].local_port == dst_port {
            socks[i].rx.push(src_ip4, src_port, payload);
            return;
        }
    }
}
