/// IP layer — port of net/ip.c
///
/// IPv4 header parsing, checksum, and basic routing.

use super::ethernet::{self, ETH_TYPE_IP};
use super::arp;

pub const IP_PROTO_ICMP: u8 = 1;
pub const IP_PROTO_TCP:  u8 = 6;
pub const IP_PROTO_UDP:  u8 = 17;

/// Standard Ethernet Maximum Transmission Unit (bytes).
pub const ETH_MTU: usize = 1500;

/// Minimum IPv4 header size (no options).
pub const IP_HDR_MIN: usize = 20;

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

    // Learn sender IP→MAC from ARP (we already added it when ARP was processed,
    // but also update here from IP source)
    let src_ip = hdr.src;
    let _ = (src_ip, our_mac); // suppress unused warning

    let data = &payload[ihl..total];
    let proto = hdr.protocol;

    match proto {
        IP_PROTO_ICMP => handle_icmp(&hdr.src, &hdr.dst, data, our_mac, our_ip),
        IP_PROTO_UDP  => super::udp::handle(&hdr.src, data),
        IP_PROTO_TCP  => super::tcp::handle(&hdr.src, data),
        _             => {}
    }
}

/// Handle ICMP echo request (ping) — send echo reply.
fn handle_icmp(src_ip: &[u8; 4], _dst_ip: &[u8; 4], data: &[u8], our_mac: &[u8; 6], our_ip: &[u8; 4]) {
    if data.len() < 8 { return; }
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
