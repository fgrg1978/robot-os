/// Ethernet layer — port of net/eth.c
///
/// Parses and builds Ethernet II frames.

pub const ETH_ALEN:    usize = 6;
pub const ETH_TYPE_IP:   u16 = 0x0800;
pub const ETH_TYPE_ARP:  u16 = 0x0806;
pub const ETH_TYPE_IPV6: u16 = 0x86DD;

/// Maximum Ethernet II frame size (header + MTU): 14 + 1500 = 1514 bytes.
pub const ETH_FRAME_MAX: usize = 1514;

/// Ethernet frame header (14 bytes, packed big-endian).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EthHdr {
    pub dst:  [u8; 6],
    pub src:  [u8; 6],
    pub typ:  [u8; 2],   // Big-endian EtherType
}

impl EthHdr {
    pub const SIZE: usize = core::mem::size_of::<EthHdr>();

    pub fn ethertype(&self) -> u16 {
        u16::from_be_bytes(self.typ)
    }
}

/// Parse a raw Ethernet frame. Returns header + payload slice, or None on error.
pub fn parse(frame: &[u8]) -> Option<(&EthHdr, &[u8])> {
    if frame.len() < EthHdr::SIZE { return None; }
    let hdr = unsafe { &*(frame.as_ptr() as *const EthHdr) };
    Some((hdr, &frame[EthHdr::SIZE..]))
}

/// Build an Ethernet frame into `out`. Returns bytes written.
/// `out` must be large enough for ETH header + payload.
pub fn build(out: &mut [u8], dst: &[u8; 6], src: &[u8; 6], ethertype: u16, payload: &[u8]) -> usize {
    let total = EthHdr::SIZE + payload.len();
    if out.len() < total { return 0; }
    out[0..6].copy_from_slice(dst);
    out[6..12].copy_from_slice(src);
    let et = ethertype.to_be_bytes();
    out[12] = et[0];
    out[13] = et[1];
    out[EthHdr::SIZE..total].copy_from_slice(payload);
    total
}

/// Broadcast MAC address.
pub const MAC_BROADCAST: [u8; 6] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

/// Send an IPv6 frame (`frame[14..]` already filled with IPv6 header + payload).
///
/// Resolves the destination MAC from the IPv6 address:
/// - Multicast → map to 33:33:xx:xx:xx:xx (RFC 2464)
/// - Unicast   → use NDP cache (stub: broadcast for now)
///
/// Returns `true` if the frame was handed to the driver.
pub fn send_ipv6(frame: &mut [u8], frame_len: usize, dst_ipv6: &[u8; 16]) -> bool {
    if frame_len > frame.len() { return false; }

    let our_mac = super::net_get_mac();

    // Destination MAC: multicast → 33:33:LL:LL:LL:LL; unicast → broadcast stub.
    let dst_mac = if dst_ipv6[0] == 0xFF {
        // RFC 2464: map FF02::xxxx → 33:33:xx:xx:xx:xx
        [0x33, 0x33, dst_ipv6[12], dst_ipv6[13], dst_ipv6[14], dst_ipv6[15]]
    } else {
        // Unicast: TODO NDP cache — use broadcast for now.
        MAC_BROADCAST
    };

    // Fill Ethernet header (14 bytes).
    frame[0..6].copy_from_slice(&dst_mac);
    frame[6..12].copy_from_slice(&our_mac);
    let et = ETH_TYPE_IPV6.to_be_bytes();
    frame[12] = et[0];
    frame[13] = et[1];

    super::net_raw_send(&frame[..frame_len]) > 0
}
