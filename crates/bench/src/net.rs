//! Network subsystem microbenchmarks.
//!
//! Today: ARP cache lookup + insert.  Skips IP send (allocates ethernet
//! frame + writes virtio descriptor, not pure CPU) and TCP loopback
//! (needs a server task already listening — depends on workload).

use crate::{BenchResult, report};
use robot_os_drivers::wcet::read_cycles;
use robot_os_net::{arp, ip, ethernet};

const TEST_IP: [u8; 4] = [10, 0, 2, 99]; // unused IP in SLIRP subnet
const TEST_MAC: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

/// `arp::lookup` hit path — IP pre-inserted into the cache.  Measures
/// the table scan + hit return.
pub fn bench_arp_lookup_hit(iters: u64) -> BenchResult {
    // Pre-populate cache so every lookup hits.
    arp::insert(TEST_IP, TEST_MAC);

    let start = read_cycles();
    for _ in 0..iters {
        let _ = arp::lookup(&TEST_IP);
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

/// `arp::lookup` miss path — IP NOT in cache.  Measures full scan +
/// negative return.
pub fn bench_arp_lookup_miss(iters: u64) -> BenchResult {
    // IP guaranteed-absent: TEST_NET (192.0.2/24) per RFC 5737.
    let absent: [u8; 4] = [192, 0, 2, 1];

    let start = read_cycles();
    for _ in 0..iters {
        let _ = arp::lookup(&absent);
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

/// `arp::insert` — table update + LRU bookkeeping.
pub fn bench_arp_insert(iters: u64) -> BenchResult {
    let start = read_cycles();
    for i in 0..iters {
        // Vary IP so we exercise LRU evict over time.
        let ip: [u8; 4] = [10, 0, 2, ((i & 0xFF) as u8)];
        arp::insert(ip, TEST_MAC);
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

/// Internet checksum (RFC 1071) over a 20-byte IP header.  Per-packet TX
/// cost on the header; the sum-fold loop is the hot part.
pub fn bench_ip_checksum_20B(iters: u64) -> BenchResult {
    let data = [0x42u8; 20];

    let start = read_cycles();
    for _ in 0..iters {
        let _ = core::hint::black_box(ip::checksum(&data));
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

/// Internet checksum over a full 1500-byte MTU payload — worst-case
/// per-packet checksum cost.  Scales linearly with payload length.
pub fn bench_ip_checksum_1500B(iters: u64) -> BenchResult {
    let data = [0x42u8; ip::ETH_MTU];

    let start = read_cycles();
    for _ in 0..iters {
        let _ = core::hint::black_box(ip::checksum(&data));
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

/// `ip::build_header` — fill the 20-byte IPv4 header incl. checksum.  The
/// per-packet header-assembly cost (one checksum + scalar writes).
pub fn bench_ip_build_header(iters: u64) -> BenchResult {
    let mut buf = [0u8; 20];
    let src = [10, 0, 2, 15];
    let dst = [10, 0, 2, 2];

    let start = read_cycles();
    for _ in 0..iters {
        ip::build_header(&mut buf, ip::IP_PROTO_UDP, &src, &dst, 64);
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

/// `ip::pseudo_checksum` — TCP/UDP pseudo-header partial sum.  Fixed
/// constant-cost accumulation; computed once per L4 segment.
pub fn bench_ip_pseudo_checksum(iters: u64) -> BenchResult {
    let src = [10, 0, 2, 15];
    let dst = [10, 0, 2, 2];

    let start = read_cycles();
    for _ in 0..iters {
        let _ = core::hint::black_box(
            ip::pseudo_checksum(&src, &dst, ip::IP_PROTO_TCP, 64),
        );
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

/// `ethernet::build` — 14-byte header + 64-byte payload memcpy.  The
/// per-frame framing cost at L2.
pub fn bench_ethernet_build_64B(iters: u64) -> BenchResult {
    let payload = [0x42u8; 64];
    let mut out = [0u8; ethernet::EthHdr::SIZE + 64];
    let dst = [0xAAu8; 6];
    let src = [0xBBu8; 6];

    let start = read_cycles();
    for _ in 0..iters {
        let _ = ethernet::build(&mut out, &dst, &src, ethernet::ETH_TYPE_IP, &payload);
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

/// `ethernet::parse` — header cast + bounds check on a 78-byte frame.  The
/// per-frame RX demux fast path.
pub fn bench_ethernet_parse(iters: u64) -> BenchResult {
    let payload = [0x42u8; 64];
    let mut frame = [0u8; ethernet::EthHdr::SIZE + 64];
    let _ = ethernet::build(&mut frame, &[0xAAu8; 6], &[0xBBu8; 6],
                            ethernet::ETH_TYPE_IP, &payload);

    let start = read_cycles();
    for _ in 0..iters {
        let _ = core::hint::black_box(ethernet::parse(&frame));
    }
    let end = read_cycles();
    BenchResult::from_total(start, end, iters)
}

pub fn run(iters: u64) -> u32 {
    let mut n = 0u32;
    report("net.arp_lookup_hit",   &bench_arp_lookup_hit(iters));   n += 1;
    report("net.arp_lookup_miss",  &bench_arp_lookup_miss(iters));  n += 1;
    report("net.arp_insert",       &bench_arp_insert(iters));       n += 1;
    report("net.ip_checksum_20B",  &bench_ip_checksum_20B(iters));  n += 1;
    report("net.ip_checksum_1500B", &bench_ip_checksum_1500B(iters)); n += 1;
    report("net.ip_build_header",  &bench_ip_build_header(iters));  n += 1;
    report("net.ip_pseudo_checksum", &bench_ip_pseudo_checksum(iters)); n += 1;
    report("net.ethernet_build_64B", &bench_ethernet_build_64B(iters)); n += 1;
    report("net.ethernet_parse",   &bench_ethernet_parse(iters));   n += 1;
    n
}
