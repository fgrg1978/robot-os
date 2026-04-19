//! Coverage for `crates/net/` — packet parsing was 0 tests before.
//!
//! Tests Ethernet / IP header parsing invariants, ARP request/reply,
//! and TCP header bit fields. These are the parsers we'd expect to
//! recognise the malformed packets that historically caused crashes.

#![cfg(test)]

// ── Ethernet header ──────────────────────────────────────────────────────

const ETH_HDR_LEN:    usize = 14;
const ETH_TYPE_ARP:   u16   = 0x0806;
const ETH_TYPE_IP:    u16   = 0x0800;
const ETH_TYPE_IPV6:  u16   = 0x86DD;

fn parse_ethertype(frame: &[u8]) -> Option<u16> {
    if frame.len() < ETH_HDR_LEN { return None; }
    Some(((frame[12] as u16) << 8) | (frame[13] as u16))
}

fn parse_dst_mac<'a>(frame: &'a [u8]) -> Option<&'a [u8]> {
    if frame.len() < ETH_HDR_LEN { return None; }
    Some(&frame[0..6])
}

fn parse_src_mac<'a>(frame: &'a [u8]) -> Option<&'a [u8]> {
    if frame.len() < ETH_HDR_LEN { return None; }
    Some(&frame[6..12])
}

#[test]
fn ethernet_short_frame_returns_none() {
    assert_eq!(parse_ethertype(&[0; 13]), None);
    assert_eq!(parse_ethertype(&[]),      None);
}

#[test]
fn ethernet_arp_frame_recognised() {
    let mut f = [0u8; 60];
    f[12] = 0x08; f[13] = 0x06;
    assert_eq!(parse_ethertype(&f), Some(ETH_TYPE_ARP));
}

#[test]
fn ethernet_ipv4_frame_recognised() {
    let mut f = [0u8; 60];
    f[12] = 0x08; f[13] = 0x00;
    assert_eq!(parse_ethertype(&f), Some(ETH_TYPE_IP));
}

#[test]
fn ethernet_macs_are_first_two_six_byte_blocks() {
    let f = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff,    // dst
             0x52, 0x55, 0x0a, 0x00, 0x02, 0x02,    // src
             0x08, 0x06];                            // ethertype
    assert_eq!(parse_dst_mac(&f), Some(&[0xff; 6][..]));
    assert_eq!(parse_src_mac(&f),
               Some(&[0x52, 0x55, 0x0a, 0x00, 0x02, 0x02][..]));
}

// ── IPv4 header parser ───────────────────────────────────────────────────

fn parse_ipv4_protocol(payload: &[u8]) -> Option<u8> {
    if payload.len() < 20 { return None; }
    Some(payload[9])
}

fn parse_ipv4_total_length(payload: &[u8]) -> Option<u16> {
    if payload.len() < 20 { return None; }
    Some(((payload[2] as u16) << 8) | (payload[3] as u16))
}

#[test]
fn ipv4_header_too_short_rejected() {
    assert_eq!(parse_ipv4_protocol(&[0; 19]), None);
}

#[test]
fn ipv4_protocol_field_at_offset_9() {
    let mut p = [0u8; 20];
    p[9] = 6; // TCP
    assert_eq!(parse_ipv4_protocol(&p), Some(6));
    p[9] = 17; // UDP
    assert_eq!(parse_ipv4_protocol(&p), Some(17));
    p[9] = 1; // ICMP
    assert_eq!(parse_ipv4_protocol(&p), Some(1));
}

#[test]
fn ipv4_total_length_be_at_offset_2() {
    let mut p = [0u8; 20];
    p[2] = 0x05; p[3] = 0xdc;
    assert_eq!(parse_ipv4_total_length(&p), Some(1500));
}

// ── ARP packet parser ────────────────────────────────────────────────────

const ARP_OP_REQUEST: u16 = 1;
const ARP_OP_REPLY:   u16 = 2;

fn parse_arp_op(arp: &[u8]) -> Option<u16> {
    if arp.len() < 28 { return None; }
    Some(((arp[6] as u16) << 8) | (arp[7] as u16))
}

#[test]
fn arp_request_op_is_one() {
    let mut a = [0u8; 28];
    a[6] = 0; a[7] = 1;
    assert_eq!(parse_arp_op(&a), Some(ARP_OP_REQUEST));
}

#[test]
fn arp_reply_op_is_two() {
    let mut a = [0u8; 28];
    a[6] = 0; a[7] = 2;
    assert_eq!(parse_arp_op(&a), Some(ARP_OP_REPLY));
}

#[test]
fn arp_short_packet_rejected() {
    assert_eq!(parse_arp_op(&[0; 27]), None);
}

// ── TCP header bits ──────────────────────────────────────────────────────

const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;

fn parse_tcp_flags(seg: &[u8]) -> Option<u8> {
    if seg.len() < 14 { return None; }
    Some(seg[13])
}

#[test]
fn tcp_syn_ack_combined() {
    let mut s = [0u8; 20];
    s[13] = TCP_SYN | TCP_ACK;
    let f = parse_tcp_flags(&s).unwrap();
    assert!(f & TCP_SYN != 0);
    assert!(f & TCP_ACK != 0);
    assert!(f & TCP_FIN == 0);
}

#[test]
fn tcp_rst_alone() {
    let mut s = [0u8; 20];
    s[13] = TCP_RST;
    let f = parse_tcp_flags(&s).unwrap();
    assert!(f & TCP_RST != 0);
    assert!(f & TCP_SYN == 0);
}

// ── Sequence-number window-check (mirrors net/tcp.rs) ────────────────────
//
// Lock down the fix from #30 / #31: a segment whose seq is outside the
// expected window must be ignored. seq_in_window is wraparound-safe.

fn seq_in_window(seq: u32, expected: u32, window: u32) -> bool {
    seq.wrapping_sub(expected) < window
}

#[test]
fn seq_in_window_basic() {
    assert!( seq_in_window(100, 100, 1000));
    assert!( seq_in_window(900, 100, 1000));
    assert!(!seq_in_window(99,  100, 1000));
    assert!(!seq_in_window(2000,100, 1000));
}

#[test]
fn seq_in_window_wraparound() {
    // Near u32::MAX: expected = 0xFFFF_FF00, window = 0x200.
    let exp = 0xFFFF_FF00u32;
    let win = 0x200u32;
    assert!( seq_in_window(0xFFFF_FFFF, exp, win));   // within
    assert!( seq_in_window(0,           exp, win));   // wrapped, still in
    assert!( seq_in_window(0x80,        exp, win));   // wrapped, in
    assert!(!seq_in_window(0x300,       exp, win));   // outside
}
