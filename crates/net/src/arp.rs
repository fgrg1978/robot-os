/// ARP layer — port of net/arp.c
///
/// ARP request/reply + cache with 16 entries (LRU eviction).

use robot_os_sync::SpinLock;
use super::ethernet::{self, ETH_TYPE_ARP, MAC_BROADCAST};
use wcet_macro::wcet;

pub const ARP_CACHE_SIZE: usize = 16;

/// Number of outstanding ARP requests we remember (see `ARP_PENDING`).
/// Sized above `ARP_CACHE_SIZE / 2`: the only in-tree requesters are
/// `ip::send` on a cache miss and `tcp::connect_with_yield`, so a handful of
/// concurrent resolutions is the realistic worst case.
const ARP_PENDING_SIZE: usize = 8;

/// How long an outstanding request stays eligible to be answered
/// (CLINT ticks; the clock is 10 MHz on QEMU/VF2, so this is 5 seconds).
///
/// Bounded on purpose: without expiry, one request for an address would leave
/// a permanent licence for anyone on the segment to overwrite that entry at a
/// moment of their choosing.  5 s is orders of magnitude above a LAN ARP RTT
/// (tens of µs on hardware, low ms under QEMU TCG) and still short enough that
/// the window is closed long before an attacker can react to seeing the
/// request on the wire.
const ARP_PENDING_TTL_TICKS: u64 = 5 * 10_000_000;

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

// ── Outstanding-request table (anti-poisoning) ───────────────────────────────

/// One address we have asked about and not yet had answered.
#[derive(Clone, Copy)]
struct PendingArp {
    ip:    [u8; 4],
    sent:  u64,   // CLINT tick when the request went out
    valid: bool,
}

impl PendingArp {
    const fn new() -> Self {
        PendingArp { ip: [0; 4], sent: 0, valid: false }
    }
}

/// Addresses we have an ARP request outstanding for.
///
/// This is the state the old "replies for IPs we never queried are ignored"
/// comment described but that no code kept: without it, `tpa == our_ip` is
/// true of ANY unsolicited reply sent to us, so a single crafted reply with
/// `spa = <gateway>` rewrote the gateway entry and redirected every packet we
/// route off-link.  A reply is now only learned if it answers a question we
/// actually asked, recently.
static ARP_PENDING: SpinLock<[PendingArp; ARP_PENDING_SIZE]> =
    SpinLock::new([PendingArp::new(); ARP_PENDING_SIZE]);

/// Record that we have just asked for `ip`.
///
/// Re-asking for the same address refreshes its timestamp rather than
/// consuming a second slot (`ip::send` fires a request on every cache miss,
/// so a stalled resolution would otherwise evict every other pending entry).
fn record_pending(ip: &[u8; 4], now: u64) {
    let mut p = ARP_PENDING.lock();
    for i in 0..ARP_PENDING_SIZE {
        if p[i].valid && &p[i].ip == ip {
            p[i].sent = now;
            return;
        }
    }
    // Free slot, else the oldest — an expired or stale question is the one we
    // care least about keeping.
    let mut free = None;
    for i in 0..ARP_PENDING_SIZE {
        if !p[i].valid { free = Some(i); break; }
    }
    let idx = match free {
        Some(i) => i,
        None => {
            let mut best = 0usize;
            for i in 1..ARP_PENDING_SIZE {
                if p[i].sent < p[best].sent { best = i; }
            }
            best
        }
    };
    p[idx] = PendingArp { ip: *ip, sent: now, valid: true };
}

/// Consume an outstanding request for `ip`.
///
/// Returns true only if we asked about `ip` within `ARP_PENDING_TTL_TICKS`.
/// The entry is retired on a match so that the answer cannot be replayed
/// later by a third party — a second, genuine reply for an address already in
/// the cache is simply redundant.
fn take_pending(ip: &[u8; 4], now: u64) -> bool {
    let mut p = ARP_PENDING.lock();
    for i in 0..ARP_PENDING_SIZE {
        if p[i].valid && &p[i].ip == ip {
            let fresh = now.saturating_sub(p[i].sent) < ARP_PENDING_TTL_TICKS;
            p[i].valid = false;
            return fresh;
        }
    }
    false
}

/// Look up a MAC address for an IP in the ARP cache.
#[wcet(10_us)]
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
///
/// Cache-poisoning hardening: only learn (spa→sha) if the packet is
/// genuinely related to us — either a request asking about our IP
/// (legitimate neighbour discovery) or a reply to a request WE sent and
/// have not yet had answered (`ARP_PENDING`).
///
/// The `tpa == our_ip` test alone is not that property: every unsolicited
/// reply an attacker addresses to us satisfies it.  That is the hole this
/// function's comment used to claim was closed while the code left it open.
pub fn handle(payload: &[u8], our_mac: &[u8; 6], our_ip: &[u8; 4]) {
    if payload.len() < ARP_PKT_SIZE { return; }
    let pkt = unsafe { &*(payload.as_ptr() as *const ArpPkt) };

    // Fixed-header validation (RFC 826).  These four fields define what the
    // address fields *mean*; parsing `sha`/`spa` as a MAC/IPv4 pair without
    // checking them means trusting the sender's word for the layout.  Anything
    // that is not Ethernet/IPv4 with the canonical lengths is not something
    // this cache can represent, so it is dropped rather than reinterpreted.
    if u16::from_be_bytes(pkt.htype) != ARP_HARDWARE_ETHERNET { return; }
    if u16::from_be_bytes(pkt.ptype) != ARP_PROTO_IP          { return; }
    if pkt.hlen != ARP_HLEN_ETH || pkt.plen != ARP_PLEN_IP    { return; }

    let op   = u16::from_be_bytes(pkt.oper);
    let tpa  = &pkt.tpa;
    let spa  = &pkt.spa;
    let sha  = &pkt.sha;

    // Reject obviously bogus senders: 0.0.0.0 and 255.255.255.255 must
    // never be cached, and the broadcast/multicast MAC bits must be 0
    // (an ARP source-MAC with the multicast bit is RFC-illegal).
    let zero_ip       = [0u8; 4];
    let broadcast_ip  = [0xff; 4];
    let mcast_mac_bit = sha[0] & 0x01;
    if *spa == zero_ip || *spa == broadcast_ip || mcast_mac_bit != 0 {
        return;
    }

    // Only learn the sender if:
    //   - they're requesting OUR IP (legitimate question — we'll reply,
    //     so we want their MAC to send the reply), OR
    //   - they're answering a question we asked: the reply is addressed to us
    //     AND `spa` matches a live entry in `ARP_PENDING`.
    // Gratuitous ARPs, and replies for IPs we never queried, are ignored —
    // and now that is enforced, not merely asserted.
    let is_request_for_us = op == ARP_OP_REQUEST && tpa == our_ip;
    let is_reply_to_us    = op == ARP_OP_REPLY   && tpa == our_ip;
    if is_request_for_us {
        ARP_TABLE.lock().insert(*spa, *sha);
    } else if is_reply_to_us {
        // `take_pending` also retires the request, so the same answer cannot
        // be replayed later to overwrite the entry.
        let now = robot_os_drivers::clint::get_time();
        if take_pending(spa, now) {
            ARP_TABLE.lock().insert(*spa, *sha);
        }
    }

    if tpa != our_ip { return; }  // Not for us

    if op == ARP_OP_REQUEST {
        send_reply(our_mac, our_ip, sha, spa);
    }
}

/// Send an ARP request for `target_ip`.
///
/// Also records the question in `ARP_PENDING`: `handle` will only learn a
/// reply for an address that appears there, so every requester must come
/// through this function for resolution to work.
pub fn send_request(our_mac: &[u8; 6], our_ip: &[u8; 4], target_ip: &[u8; 4]) {
    record_pending(target_ip, robot_os_drivers::clint::get_time());

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
