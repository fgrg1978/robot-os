/// IP layer — port of net/ip.c
///
/// IPv4 header parsing, checksum, and basic routing.

use super::ethernet::{self, ETH_TYPE_IP};
use super::arp;
use wcet_macro::wcet;

pub const IP_PROTO_ICMP: u8 = 1;
pub const IP_PROTO_TCP:  u8 = 6;
pub const IP_PROTO_UDP:  u8 = 17;

/// Standard Ethernet Maximum Transmission Unit (bytes).
pub const ETH_MTU: usize = 1500;

/// Minimum IPv4 header size (no options).
pub const IP_HDR_MIN: usize = 20;

/// More-Fragments flag inside the 16-bit fragment field.
pub const IP_FLAG_MF: u16 = 0x2000;
/// Don't-Fragment flag — a hint to routers, *not* an indication of fragmentation.
pub const IP_FLAG_DF: u16 = 0x4000;
/// Low 13 bits of the fragment field hold the offset (in 8-byte units).
pub const IP_FRAG_OFF_MASK: u16 = 0x1FFF;

#[repr(C, packed)]
pub struct IpHdr {
    pub version_ihl: u8,    // version (4) | IHL (in 32-bit words)
    pub dscp_ecn:    u8,
    pub total_len:   [u8; 2],
    pub id:          [u8; 2],
    pub frag_off:    [u8; 2],
    pub ttl:         u8,
    pub protocol:    u8,
    pub checksum:    [u8; 2],
    pub src:         [u8; 4],
    pub dst:         [u8; 4],
}

impl IpHdr {
    pub const MIN_SIZE: usize = 20;

    pub fn ihl_bytes(&self) -> usize {
        ((self.version_ihl & 0x0F) as usize) * 4
    }

    pub fn total_length(&self) -> u16 {
        u16::from_be_bytes(self.total_len)
    }

    pub fn version(&self) -> u8 {
        (self.version_ihl >> 4) & 0xF
    }

    /// Raw fragmentation field (flags + offset) in host order.
    pub fn frag_field(&self) -> u16 {
        u16::from_be_bytes(self.frag_off)
    }

    /// True if this datagram is a fragment: MF set, or a non-zero offset.
    /// DF (`IP_FLAG_DF`) is deliberately excluded — it is a routing hint and
    /// says nothing about whether *this* packet is fragmented.
    pub fn is_fragment(&self) -> bool {
        let f = self.frag_field();
        (f & IP_FLAG_MF) != 0 || (f & IP_FRAG_OFF_MASK) != 0
    }
}

/// Internet checksum (RFC 1071).
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
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

use core::sync::atomic::{AtomicU16, Ordering};

static IP_COUNTER: AtomicU16 = AtomicU16::new(0);

/// Build an IPv4 header into `buf[0..IpHdr::MIN_SIZE]`.
/// Caller must fill the payload after the header.
pub fn build_header(
    buf:      &mut [u8; 20],
    proto:    u8,
    src:      &[u8; 4],
    dst:      &[u8; 4],
    data_len: u16,
) {
    let total = IpHdr::MIN_SIZE as u16 + data_len;
    let id    = IP_COUNTER.fetch_add(1, Ordering::Relaxed);

    buf[0]  = 0x45;                          // version=4, IHL=5 (20 bytes)
    buf[1]  = 0;
    let tl  = total.to_be_bytes();
    buf[2]  = tl[0];
    buf[3]  = tl[1];
    let ib  = id.to_be_bytes();
    buf[4]  = ib[0];
    buf[5]  = ib[1];
    buf[6]  = 0;
    buf[7]  = 0;
    buf[8]  = 64;   // TTL
    buf[9]  = proto;
    buf[10] = 0;    // checksum placeholder
    buf[11] = 0;
    buf[12..16].copy_from_slice(src);
    buf[16..20].copy_from_slice(dst);
    let cs = checksum(buf);
    let cb = cs.to_be_bytes();
    buf[10] = cb[0];
    buf[11] = cb[1];
}

/// Send an IP packet.  Resolves `dst` via ARP cache; returns -1 if no MAC found.
#[wcet(150_us)]
pub fn send(
    our_mac:  &[u8; 6],
    our_ip:   &[u8; 4],
    dst_ip:   &[u8; 4],
    proto:    u8,
    payload:  &[u8],
) -> i32 {
    let dst_mac = match arp::lookup(dst_ip) {
        Some(m) => m,
        None    => {
            // Send ARP request and return error; caller can retry after delay
            arp::send_request(our_mac, our_ip, dst_ip);
            return -1;
        }
    };

    let total_len = IpHdr::MIN_SIZE + payload.len();
    // MTU limit
    if total_len > ETH_MTU { return -1; }

    let frame_len = ethernet::EthHdr::SIZE + total_len;
    // Use a fixed-size stack frame (max Ethernet frame)
    let mut frame = [0u8; ethernet::ETH_FRAME_MAX];
    if frame_len > frame.len() { return -1; }

    // Build IP header
    let mut ip_hdr = [0u8; IP_HDR_MIN];
    build_header(&mut ip_hdr, proto, our_ip, dst_ip, payload.len() as u16);

    // Assemble: ETH + IP hdr + payload
    let mut ip_payload = [0u8; ETH_MTU];
    ip_payload[..IP_HDR_MIN].copy_from_slice(&ip_hdr);
    ip_payload[IP_HDR_MIN..IP_HDR_MIN + payload.len()].copy_from_slice(payload);

    ethernet::build(&mut frame[..frame_len], &dst_mac, our_mac, ETH_TYPE_IP,
                    &ip_payload[..total_len]);

    if super::net_raw_send(&frame[..frame_len]) > 0 { 0 } else { -1 }
}

/// Process an incoming IP packet (payload after ETH header).
pub fn handle(payload: &[u8], our_mac: &[u8; 6], our_ip: &[u8; 4]) {
    if payload.len() < IpHdr::MIN_SIZE { return; }
    let hdr = unsafe { &*(payload.as_ptr() as *const IpHdr) };
    if hdr.version() != 4 { return; }

    let ihl = hdr.ihl_bytes();
    if ihl < IpHdr::MIN_SIZE || payload.len() < ihl { return; }

    let total = hdr.total_length() as usize;
    if total > payload.len() { return; }
    // Crafted packets can carry `total_length < ihl`. The slice below is
    // `&payload[ihl..total]`; without this guard that's a reversed range
    // which Rust panics on (kernel halt in release with `panic = "abort"`).
    if total < ihl { return; }

    // Reject fragments. This stack performs NO reassembly, so the only safe
    // action is to drop: handing fragment N>0 to udp/tcp::handle would parse
    // payload bytes as a fresh L4 header, and an attacker could smuggle data
    // past any L4-level check simply by fragmenting it. Real reassembly needs
    // buffer pools, per-datagram timers and an eviction policy — deliberately
    // out of scope here. Note DF alone is not fragmentation (see is_fragment).
    if hdr.is_fragment() { return; }

    // Verify the header checksum before acting on any field beyond the length
    // bounds above. A well-formed header sums to 0xFFFF including its own
    // checksum word, so RFC-1071 one's complement of that is 0. `ihl` is
    // already bounded by `payload.len()`, so this slice cannot panic.
    if checksum(&payload[..ihl]) != 0 { return; }

    // Destination filter. Without it the stack processed every IPv4 packet the
    // NIC delivered, regardless of addressee. Harmless on a point-to-point link
    // (SLIRP only hands us our own traffic), but on shared media — a real LAN,
    // or QEMU's `socket` backend where both guests see every frame — we would
    // ingest the peer's packets, feed them to TCP (whose connection lookup can
    // match on ports alone), and burn checksum work on traffic that was never
    // ours. Accept:
    //   * our unicast address,
    //   * the limited broadcast 255.255.255.255 (DHCP OFFER/ACK arrive here),
    //   * the subnet directed broadcast (e.g. 10.0.0.255 for a /24),
    //   * anything while we are unconfigured (0.0.0.0) — a DHCP client must be
    //     able to hear the server before it has an address (RFC 2131 §4.1).
    let dst = hdr.dst;
    if *our_ip != [0, 0, 0, 0] && dst != *our_ip && dst != [0xff; 4] {
        let mask = super::net_get_mask();
        let is_subnet_bcast = (0..4).all(|i| dst[i] == (our_ip[i] & mask[i]) | !mask[i]);
        if !is_subnet_bcast { return; }
    }

    // Learn sender IP→MAC from ARP (we already added it when ARP was processed,
    // but also update here from IP source)
    let src_ip = hdr.src;
    let _ = (src_ip, our_mac); // suppress unused warning

    let data = &payload[ihl..total];
    let proto = hdr.protocol;

    // Both L4 handlers take the destination address too: the TCP/UDP checksum
    // covers the IPv4 pseudo-header (src, dst, proto, length), so it cannot be
    // verified without it. Passing `&hdr.dst` here is what makes RX checksum
    // validation possible at all — see `udp::handle_checked` / `tcp::handle_checked`.
    match proto {
        IP_PROTO_ICMP => handle_icmp(&hdr.src, &hdr.dst, data, our_mac, our_ip),
        IP_PROTO_UDP  => super::udp::handle_checked(&hdr.src, &hdr.dst, data),
        IP_PROTO_TCP  => super::tcp::handle_checked(&hdr.src, &hdr.dst, data),
        _             => {}
    }
}

/// Handle ICMP echo request (ping) — send echo reply.
///
/// Replies ONLY to our unicast address.  The destination filter in `handle`
/// deliberately admits the limited and subnet broadcasts (DHCP needs to hear
/// them), which means broadcast echo requests reach this function — and
/// answering those makes the robot a smurf reflector: an attacker pings the
/// broadcast address with the victim's source, every host on the segment
/// replies to the victim, and the robot's link is spent on someone else's
/// attack.  RFC 1122 §3.2.2.6 makes replying to broadcast echo optional; for a
/// device whose radio link is a safety resource it is simply refused.
fn handle_icmp(src_ip: &[u8; 4], dst_ip: &[u8; 4], data: &[u8], our_mac: &[u8; 6], our_ip: &[u8; 4]) {
    if dst_ip != our_ip { return; }
    if data.len() < 8 { return; }
    // Same RFC 1071 identity as the IP header: a valid ICMP message sums to
    // 0xFFFF including its own checksum field. Echoing a corrupt request would
    // vouch for payload bytes we never verified.
    if checksum(data) != 0 { return; }
    let icmp_type = data[0];
    if icmp_type != 8 { return; }  // Only echo request

    // Build echo reply (type=0, same code/id/seq, same data)
    let mut reply = [0u8; 1480];
    let rlen = data.len().min(reply.len());
    reply[..rlen].copy_from_slice(&data[..rlen]);
    reply[0] = 0;  // echo reply
    reply[2] = 0;  // zero checksum
    reply[3] = 0;
    let cs = checksum(&reply[..rlen]);
    let cb = cs.to_be_bytes();
    reply[2] = cb[0];
    reply[3] = cb[1];

    send(our_mac, our_ip, src_ip, IP_PROTO_ICMP, &reply[..rlen]);
}

/// Pseudo-header checksum for TCP/UDP.
pub fn pseudo_checksum(src: &[u8; 4], dst: &[u8; 4], proto: u8, len: u16) -> u32 {
    let mut sum: u32 = 0;
    sum += u16::from_be_bytes([src[0], src[1]]) as u32;
    sum += u16::from_be_bytes([src[2], src[3]]) as u32;
    sum += u16::from_be_bytes([dst[0], dst[1]]) as u32;
    sum += u16::from_be_bytes([dst[2], dst[3]]) as u32;
    sum += proto as u32;
    sum += len as u32;
    sum
}
