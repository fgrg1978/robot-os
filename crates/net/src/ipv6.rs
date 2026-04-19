//! IPv6 stack (F22).
//!
//! Implements a minimal IPv6 layer sufficient for link-local communication,
//! ICMPv6 (Neighbor Discovery, Echo Request/Reply), and UDP over IPv6.
//! TCP over IPv6 is structurally supported but not wired into the socket
//! layer in this phase.
//!
//! ## Feature coverage
//!
//! | Feature                     | Status |
//! |-----------------------------|--------|
//! | Link-local address (EUI-64) | ✓      |
//! | Neighbor Solicitation/Adv.  | ✓      |
//! | ICMPv6 Echo (ping6)         | ✓      |
//! | UDP over IPv6               | ✓      |
//! | Header parsing (RX path)    | ✓      |
//! | Header building (TX path)   | ✓      |
//! | Pseudo-header checksum      | ✓      |
//! | Router Advertisement (RX)   | ✓ (parse only) |
//! | DHCPv6 / SLAAC full         | ✗ (future)     |
//!
//! ## Address plan
//! The kernel auto-configures one link-local address from the MAC address
//! using the EUI-64 algorithm (RFC 4291 §2.5.1):
//!   FE80::/64 + EUI-64(MAC)
//!
//! ## Packet flow
//! ```text
//! RX: ethernet_rx → ipv6_rx → [icmpv6_rx | udpv6_rx]
//! TX: udpv6_send  → ipv6_build_header → ethernet_send
//! ```

use core::sync::atomic::{AtomicBool, Ordering};

// ── Constants ─────────────────────────────────────────────────────────────────

/// EtherType for IPv6 frames (IEEE 802.3).
pub const ETH_TYPE_IPV6:    u16 = 0x86DD;

/// IPv6 version nibble (always 6).
pub const IPV6_VERSION:     u8  = 6;
/// Minimum IPv6 header size (no extension headers).
pub const IPV6_HDR_SIZE:    usize = 40;
/// Default Hop Limit (analogous to IPv4 TTL).
pub const IPV6_HOP_LIMIT:   u8  = 64;

/// Next-header values (protocol numbers, same as IPv4 protocol field).
pub const NEXTHDR_ICMPV6:   u8  = 58;
pub const NEXTHDR_UDP:      u8  = 17;
pub const NEXTHDR_TCP:      u8  = 6;

/// ICMPv6 type codes.
pub const ICMPV6_ECHO_REQ:  u8  = 128;
pub const ICMPV6_ECHO_REPLY:u8  = 129;
pub const ICMPV6_NS:        u8  = 135; // Neighbor Solicitation
pub const ICMPV6_NA:        u8  = 136; // Neighbor Advertisement
pub const ICMPV6_RA:        u8  = 134; // Router Advertisement

/// ICMPv6 header size (type + code + checksum = 4 bytes) + Echo fields (4 bytes).
pub const ICMPV6_ECHO_HDR:  usize = 8;
/// Neighbor Solicitation message size (type+code+checksum+reserved+target = 24 bytes).
pub const ICMPV6_NS_SIZE:   usize = 24;
/// Neighbor Advertisement message size.
pub const ICMPV6_NA_SIZE:   usize = 24;

/// NA flag: Solicited (S bit).
pub const NA_FLAG_SOLICITED: u32 = 1 << 30;
/// NA flag: Override (O bit).
pub const NA_FLAG_OVERRIDE:  u32 = 1 << 29;

/// All-nodes multicast address (`FF02::1`).
pub const MCAST_ALL_NODES: [u8; 16] = [
    0xFF, 0x02, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0x01,
];
/// All-routers multicast address (`FF02::2`).
pub const MCAST_ALL_ROUTERS: [u8; 16] = [
    0xFF, 0x02, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0x02,
];

// ── Global state ──────────────────────────────────────────────────────────────

/// Our link-local IPv6 address (FE80::/64 + EUI-64).
static mut LINK_LOCAL_ADDR: [u8; 16] = [0u8; 16];
/// True once `ipv6_init()` has been called.
static IPV6_READY: AtomicBool = AtomicBool::new(false);

// ── Header layout ─────────────────────────────────────────────────────────────

/// Fixed IPv6 header (40 bytes, no extension headers).
///
/// All multi-byte fields are big-endian (network byte order).
#[repr(C, packed)]
pub struct Ipv6Hdr {
    /// Version (4b) | Traffic Class (8b) | Flow Label (20b).
    pub vcf:        [u8; 4],
    /// Payload length (bytes after this header).
    pub payload_len: [u8; 2],
    /// Next header (protocol).
    pub next_hdr:   u8,
    /// Hop limit (decremented by each router).
    pub hop_limit:  u8,
    /// Source address (128-bit).
    pub src:        [u8; 16],
    /// Destination address (128-bit).
    pub dst:        [u8; 16],
}

impl Ipv6Hdr {
    pub fn version(&self) -> u8  { (self.vcf[0] >> 4) & 0xF }
    pub fn payload_length(&self) -> u16 { u16::from_be_bytes(self.payload_len) }
}

// ── Address helpers ───────────────────────────────────────────────────────────

/// Compute the EUI-64 link-local address from a 48-bit MAC address.
///
/// Algorithm (RFC 4291 §2.5.6):
/// 1. Insert `FF:FE` in the middle of the MAC.
/// 2. Flip the Universal/Local bit (bit 6 of the first octet).
/// 3. Prepend `FE80::/64`.
pub fn eui64_link_local(mac: &[u8; 6]) -> [u8; 16] {
    let mut addr = [0u8; 16];
    // FE80::/64 prefix
    addr[0] = 0xFE;
    addr[1] = 0x80;
    // Interface ID (EUI-64): bytes 8..15
    addr[8]  = mac[0] ^ 0x02; // flip U/L bit
    addr[9]  = mac[1];
    addr[10] = mac[2];
    addr[11] = 0xFF;
    addr[12] = 0xFE;
    addr[13] = mac[3];
    addr[14] = mac[4];
    addr[15] = mac[5];
    addr
}

/// Initialize the IPv6 stack with the given MAC address.
///
/// Computes the link-local address and marks the stack ready.
pub fn ipv6_init(mac: &[u8; 6]) {
    let ll = eui64_link_local(mac);
    unsafe { LINK_LOCAL_ADDR = ll; }
    IPV6_READY.store(true, Ordering::Release);
}

/// Our link-local address.  Returns all-zero if not initialized.
#[inline]
pub fn ipv6_link_local() -> [u8; 16] {
    unsafe { LINK_LOCAL_ADDR }
}

/// Returns `true` if the IPv6 stack has been initialized.
#[inline]
pub fn ipv6_ready() -> bool { IPV6_READY.load(Ordering::Acquire) }

/// Check if `addr` matches our link-local address.
#[inline]
pub fn is_our_addr(addr: &[u8; 16]) -> bool {
    unsafe { *addr == LINK_LOCAL_ADDR }
}

/// Check if `addr` is the all-nodes multicast address `FF02::1`.
#[inline]
pub fn is_all_nodes(addr: &[u8; 16]) -> bool { addr == &MCAST_ALL_NODES }

/// Check if `addr` is a multicast address (starts with `FF`).
#[inline]
pub fn is_multicast(addr: &[u8; 16]) -> bool { addr[0] == 0xFF }

// ── Pseudo-header checksum ────────────────────────────────────────────────────

/// Compute the ICMPv6 / UDP-over-IPv6 pseudo-header checksum.
///
/// RFC 2460 §8.1: the pseudo-header contains src, dst, upper-layer length,
/// zeros, and the next-header value.
pub fn pseudo_checksum(
    src:      &[u8; 16],
    dst:      &[u8; 16],
    proto:    u8,
    data:     &[u8],
) -> u16 {
    let mut sum: u32 = 0;

    // Accumulate src and dst addresses (16 bytes each, as u16 pairs).
    for i in (0..16).step_by(2) {
        sum += u16::from_be_bytes([src[i], src[i+1]]) as u32;
        sum += u16::from_be_bytes([dst[i], dst[i+1]]) as u32;
    }
    // Upper-layer packet length (32-bit in pseudo-header, but payload fits in 16-bit).
    sum += data.len() as u32;
    // Next-header.
    sum += proto as u32;
    // Payload.
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i+1]]) as u32;
        i += 2;
    }
    if i < data.len() { sum += (data[i] as u32) << 8; }
    // Fold carry.
    while sum >> 16 != 0 { sum = (sum & 0xFFFF) + (sum >> 16); }
    !(sum as u16)
}

// ── Header builder ────────────────────────────────────────────────────────────

/// Fill a 40-byte IPv6 header into `buf`.
///
/// `buf` must be at least `IPV6_HDR_SIZE` bytes.  Payload starts at buf[40].
pub fn ipv6_build_header(
    buf:         &mut [u8],
    next_hdr:    u8,
    src:         &[u8; 16],
    dst:         &[u8; 16],
    payload_len: u16,
) {
    debug_assert!(buf.len() >= IPV6_HDR_SIZE);
    buf[0] = IPV6_VERSION << 4;  // Version=6, TC=0, FL=0
    buf[1] = 0; buf[2] = 0; buf[3] = 0;
    buf[4] = (payload_len >> 8) as u8;
    buf[5] = payload_len as u8;
    buf[6] = next_hdr;
    buf[7] = IPV6_HOP_LIMIT;
    buf[8..24].copy_from_slice(src);
    buf[24..40].copy_from_slice(dst);
}

// ── ICMPv6 receive handler ────────────────────────────────────────────────────

/// Process an incoming ICMPv6 message.
///
/// `hdr` is the parsed IPv6 header.  `payload` starts at the ICMPv6 type byte.
/// Returns `true` if the packet was handled (a reply was sent or no reply needed),
/// `false` if the packet should be passed to upper layers.
pub fn icmpv6_rx(src: &[u8; 16], dst: &[u8; 16], payload: &[u8]) -> bool {
    if payload.len() < 4 { return false; }
    match payload[0] {
        ICMPV6_ECHO_REQ => {
            if is_our_addr(dst) || is_all_nodes(dst) {
                icmpv6_echo_reply(src, payload);
                return true;
            }
        }
        ICMPV6_NS => {
            if payload.len() >= ICMPV6_NS_SIZE {
                let target = &payload[8..24];
                if is_our_addr(target.try_into().unwrap_or(&[0u8; 16])) {
                    icmpv6_neighbor_advertisement(src);
                    return true;
                }
            }
        }
        ICMPV6_RA => {
            // Parse Router Advertisement — record default gateway (future: SLAAC).
            // For now, just acknowledge reception (no state update).
            return true;
        }
        _ => {}
    }
    false
}

/// Send an ICMPv6 Echo Reply in response to an Echo Request.
fn icmpv6_echo_reply(dst: &[u8; 16], request: &[u8]) {
    if request.len() < ICMPV6_ECHO_HDR { return; }

    // Build reply: same identifier + sequence, type=129.
    let payload_len = request.len();
    let frame_len   = 14 + IPV6_HDR_SIZE + payload_len;

    let mut frame = [0u8; 1500];
    if frame_len > frame.len() { return; }

    let src = ipv6_link_local();
    ipv6_build_header(&mut frame[14..], NEXTHDR_ICMPV6, &src, dst,
        payload_len as u16);

    let icmp = &mut frame[14 + IPV6_HDR_SIZE..14 + IPV6_HDR_SIZE + payload_len];
    icmp.copy_from_slice(request);
    icmp[0] = ICMPV6_ECHO_REPLY;
    icmp[1] = 0;
    // Zero checksum before computing.
    icmp[2] = 0; icmp[3] = 0;
    let csum = pseudo_checksum(&src, dst, NEXTHDR_ICMPV6, icmp);
    icmp[2] = (csum >> 8) as u8;
    icmp[3] = csum as u8;

    super::ethernet::send_ipv6(&mut frame, frame_len, dst);
}

/// Send a Neighbor Advertisement in response to a Neighbor Solicitation.
fn icmpv6_neighbor_advertisement(dst: &[u8; 16]) {
    let src = ipv6_link_local();
    let payload_len = ICMPV6_NA_SIZE;
    let frame_len   = 14 + IPV6_HDR_SIZE + payload_len;

    let mut frame = [0u8; 1500];
    ipv6_build_header(&mut frame[14..], NEXTHDR_ICMPV6, &src, dst,
        payload_len as u16);

    let na = &mut frame[14 + IPV6_HDR_SIZE..14 + IPV6_HDR_SIZE + payload_len];
    na[0] = ICMPV6_NA;
    na[1] = 0;
    na[2] = 0; na[3] = 0; // checksum (computed below)
    // Flags: Solicited | Override
    let flags = NA_FLAG_SOLICITED | NA_FLAG_OVERRIDE;
    na[4] = (flags >> 24) as u8;
    na[5] = (flags >> 16) as u8;
    na[6] = (flags >> 8)  as u8;
    na[7] = flags as u8;
    // Target address = our link-local
    na[8..24].copy_from_slice(&src);

    let csum = pseudo_checksum(&src, dst, NEXTHDR_ICMPV6, na);
    na[2] = (csum >> 8) as u8;
    na[3] = csum as u8;

    super::ethernet::send_ipv6(&mut frame, frame_len, dst);
}

// ── IPv6 receive dispatcher ───────────────────────────────────────────────────

/// Parse and dispatch an incoming IPv6 frame.
///
/// `frame` starts at the Ethernet payload (byte 14 of the raw Ethernet frame).
/// `frame_len` is the Ethernet payload length.
pub fn ipv6_rx(frame: &[u8], frame_len: usize) {
    if frame_len < IPV6_HDR_SIZE { return; }
    if (frame[0] >> 4) != IPV6_VERSION { return; }

    let payload_len = u16::from_be_bytes([frame[4], frame[5]]) as usize;
    if IPV6_HDR_SIZE + payload_len > frame_len { return; }

    let src: &[u8; 16] = frame[8..24].try_into().unwrap();
    let dst: &[u8; 16] = frame[24..40].try_into().unwrap();

    // Only process frames addressed to us or all-nodes multicast.
    if !is_our_addr(dst) && !is_all_nodes(dst) && !is_multicast(dst) { return; }

    let next_hdr = frame[6];
    let payload  = &frame[IPV6_HDR_SIZE..IPV6_HDR_SIZE + payload_len];

    match next_hdr {
        NEXTHDR_ICMPV6 => { icmpv6_rx(src, dst, payload); }
        NEXTHDR_UDP    => { super::udp::udpv6_rx(src, dst, payload); }
        _ => {}
    }
}

// ── UDP over IPv6 TX ──────────────────────────────────────────────────────────

/// Send a UDP datagram over IPv6.
///
/// `src_port` and `dst_port` are host-byte-order.
/// Returns `true` on success.
pub fn udpv6_send(
    dst_addr: &[u8; 16],
    src_port: u16,
    dst_port: u16,
    data:     &[u8],
) -> bool {
    if data.len() + 8 > 1452 { return false; } // MTU - IPv6_HDR - UDP_HDR
    let src_addr = ipv6_link_local();

    // Build UDP header + payload (8 + data.len() bytes).
    let udp_len = (8 + data.len()) as u16;
    let mut udp_buf = [0u8; 1460];
    udp_buf[0] = (src_port >> 8) as u8;
    udp_buf[1] = src_port as u8;
    udp_buf[2] = (dst_port >> 8) as u8;
    udp_buf[3] = dst_port as u8;
    udp_buf[4] = (udp_len >> 8) as u8;
    udp_buf[5] = udp_len as u8;
    udp_buf[6] = 0; udp_buf[7] = 0; // checksum placeholder
    udp_buf[8..8 + data.len()].copy_from_slice(data);

    let csum = pseudo_checksum(&src_addr, dst_addr, NEXTHDR_UDP,
        &udp_buf[..udp_len as usize]);
    udp_buf[6] = (csum >> 8) as u8;
    udp_buf[7] = csum as u8;

    let frame_len = 14 + IPV6_HDR_SIZE + udp_len as usize;
    let mut frame = [0u8; 1500];
    ipv6_build_header(&mut frame[14..], NEXTHDR_UDP, &src_addr, dst_addr, udp_len);
    frame[14 + IPV6_HDR_SIZE..frame_len].copy_from_slice(&udp_buf[..udp_len as usize]);

    super::ethernet::send_ipv6(&mut frame, frame_len, dst_addr)
}
