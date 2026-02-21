/// ARP layer — port of net/arp.c
///
/// ARP request/reply + cache with 16 entries (LRU eviction).

use robot_os_sync::SpinLock;
use super::ethernet::{self, ETH_TYPE_ARP, MAC_BROADCAST};

pub const ARP_CACHE_SIZE: usize = 16;

const ARP_HARDWARE_ETHERNET: u16 = 1;
const ARP_PROTO_IP:          u16 = 0x0800;
const ARP_HLEN_ETH:          u8  = 6;
const ARP_PLEN_IP:           u8  = 4;
const ARP_OP_REQUEST:        u16 = 1;
const ARP_OP_REPLY:          u16 = 2;

#[repr(C, packed)]
struct ArpPkt {
    htype:    [u8; 2],
    ptype:    [u8; 2],
    hlen:     u8,
    plen:     u8,
    oper:     [u8; 2],
    sha:      [u8; 6],  // sender MAC
    spa:      [u8; 4],  // sender IP
    tha:      [u8; 6],  // target MAC
    tpa:      [u8; 4],  // target IP
}

const ARP_PKT_SIZE: usize = core::mem::size_of::<ArpPkt>();

#[derive(Clone, Copy)]
pub struct ArpEntry {
    pub ip:    [u8; 4],
    pub mac:   [u8; 6],
    pub valid: bool,
    pub age:   u32,
}

impl ArpEntry {
    pub const fn new() -> Self {
        ArpEntry { ip: [0; 4], mac: [0; 6], valid: false, age: 0 }
    }
}

struct ArpCache {
    entries: [ArpEntry; ARP_CACHE_SIZE],
    tick:    u32,
}

impl ArpCache {
    const fn new() -> Self {
        ArpCache { entries: [ArpEntry::new(); ARP_CACHE_SIZE], tick: 0 }
    }

    fn find(&self, ip: &[u8; 4]) -> Option<usize> {
        for i in 0..ARP_CACHE_SIZE {
            if self.entries[i].valid && &self.entries[i].ip == ip {
                return Some(i);
            }
        }
        None
    }

    fn insert(&mut self, ip: [u8; 4], mac: [u8; 6]) {
        // Check if already exists → update
        if let Some(i) = self.find(&ip) {
            self.entries[i].mac = mac;
            self.entries[i].age = self.tick;
            return;
        }
        // Find free slot or oldest entry
        let idx = (0..ARP_CACHE_SIZE)
            .find(|&i| !self.entries[i].valid)
            .unwrap_or_else(|| {
                // Evict oldest
                (0..ARP_CACHE_SIZE).min_by_key(|&i| self.entries[i].age).unwrap_or(0)
            });
        self.entries[idx] = ArpEntry { ip, mac, valid: true, age: self.tick };
        self.tick = self.tick.wrapping_add(1);
    }
}

static ARP_TABLE: SpinLock<ArpCache> = SpinLock::new(ArpCache::new());

/// Look up a MAC address for an IP in the ARP cache.
pub fn lookup(ip: &[u8; 4]) -> Option<[u8; 6]> {
    let t = ARP_TABLE.lock();
    t.find(ip).map(|i| t.entries[i].mac)
}

/// Insert or update an ARP cache entry.
pub fn insert(ip: [u8; 4], mac: [u8; 6]) {
    ARP_TABLE.lock().insert(ip, mac);
}

/// Handle an incoming ARP packet (called after stripping ETH header).
/// `our_mac` and `our_ip` are this host's addresses.
pub fn handle(payload: &[u8], our_mac: &[u8; 6], our_ip: &[u8; 4]) {
    if payload.len() < ARP_PKT_SIZE { return; }
    let pkt = unsafe { &*(payload.as_ptr() as *const ArpPkt) };

    let op   = u16::from_be_bytes(pkt.oper);
    let tpa  = &pkt.tpa;
    let spa  = &pkt.spa;
    let sha  = &pkt.sha;

    // Learn sender IP→MAC
    ARP_TABLE.lock().insert(*spa, *sha);

    if tpa != our_ip { return; }  // Not for us

    if op == ARP_OP_REQUEST {
        // Send ARP reply
        send_reply(our_mac, our_ip, sha, spa);
    }
}

/// Send an ARP request for `target_ip`.
pub fn send_request(our_mac: &[u8; 6], our_ip: &[u8; 4], target_ip: &[u8; 4]) {
    let mut arp_buf = [0u8; ARP_PKT_SIZE];
    let pkt = unsafe { &mut *(arp_buf.as_mut_ptr() as *mut ArpPkt) };
    pkt.htype = ARP_HARDWARE_ETHERNET.to_be_bytes();
    pkt.ptype = ARP_PROTO_IP.to_be_bytes();
    pkt.hlen  = ARP_HLEN_ETH;
    pkt.plen  = ARP_PLEN_IP;
    pkt.oper  = ARP_OP_REQUEST.to_be_bytes();
    pkt.sha   = *our_mac;
    pkt.spa   = *our_ip;
    pkt.tha   = [0u8; 6];
    pkt.tpa   = *target_ip;

    let mut frame = [0u8; ethernet::EthHdr::SIZE + ARP_PKT_SIZE];
    ethernet::build(&mut frame, &MAC_BROADCAST, our_mac, ETH_TYPE_ARP, &arp_buf);
    let _ = super::net_raw_send(&frame);
}

fn send_reply(our_mac: &[u8; 6], our_ip: &[u8; 4], dst_mac: &[u8; 6], dst_ip: &[u8; 4]) {
    let mut arp_buf = [0u8; ARP_PKT_SIZE];
    let pkt = unsafe { &mut *(arp_buf.as_mut_ptr() as *mut ArpPkt) };
    pkt.htype = ARP_HARDWARE_ETHERNET.to_be_bytes();
    pkt.ptype = ARP_PROTO_IP.to_be_bytes();
    pkt.hlen  = ARP_HLEN_ETH;
    pkt.plen  = ARP_PLEN_IP;
    pkt.oper  = ARP_OP_REPLY.to_be_bytes();
    pkt.sha   = *our_mac;
    pkt.spa   = *our_ip;
    pkt.tha   = *dst_mac;
    pkt.tpa   = *dst_ip;

    let mut frame = [0u8; ethernet::EthHdr::SIZE + ARP_PKT_SIZE];
    ethernet::build(&mut frame, dst_mac, our_mac, ETH_TYPE_ARP, &arp_buf);
    let _ = super::net_raw_send(&frame);
}

/// Print ARP cache.
pub fn dump() {
    let t = ARP_TABLE.lock();
    robot_os_drivers::kprintln!("[ARP] Cache:");
    for i in 0..ARP_CACHE_SIZE {
        if t.entries[i].valid {
            let ip  = &t.entries[i].ip;
            let mac = &t.entries[i].mac;
            robot_os_drivers::kprintln!(
                "[ARP]   {}.{}.{}.{} -> {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                ip[0], ip[1], ip[2], ip[3],
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            );
        }
    }
}
