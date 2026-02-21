/// Ethernet layer — port of net/eth.c
///
/// Parses and builds Ethernet II frames.

pub const ETH_ALEN:    usize = 6;
pub const ETH_TYPE_IP: u16   = 0x0800;
pub const ETH_TYPE_ARP: u16  = 0x0806;

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
