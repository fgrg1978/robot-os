#![no_std]

//! Robot OS network stack — port of kernel/net/
//! Ethernet → ARP / IPv4 → ICMP / UDP / TCP → BSD socket API.
//! Polling-based (call net_poll() from timer tick or shell).

pub mod ethernet;
pub mod arp;
pub mod ip;
pub mod ipv6;
pub mod udp;
pub mod tcp;
pub mod socket;
#[allow(dead_code)]
pub mod dhcp;
pub mod dns;
pub mod ntp;
// E02 — multi-link transport (WiFi/LoRa/RF failover).
pub mod multilink;
pub mod lora;
pub mod rf;

// DEV01.2 — boot-time TFTP fetch (RFC 1350) wired over UDP.
pub mod tftp_client;

pub use multilink::{
    Transport, TransportError, MultiLinkTransport,
    MAX_LINKS,
    TRANSPORT_MAX_CONSEC_FAILURES,
    TRANSPORT_FAILOVER_TIMEOUT_TICKS,
    LINK_PROBE_INTERVAL_TICKS,
    LINK_QUALITY_DOWN, LINK_QUALITY_GOOD, LINK_QUALITY_UNKNOWN,
};
pub use lora::LoRaTransport;
pub use rf::{RfTransport, RF_MAX_PAYLOAD};

pub use socket::{
    socket_create, socket_bind, socket_connect,
    socket_listen, socket_listen_bound, socket_accept,
    socket_send, socket_recv, socket_close,
    SockAddr, AF_INET, SOCK_STREAM, SOCK_DGRAM, IPPROTO_TCP, IPPROTO_UDP,
    // Per-task socket ownership. `socket_create_owned` / `socket_accept_owned`
    // stamp the owning TID; `socket_owner` is what the syscall layer's gate
    // reads; `socket_release_all` is the task-exit hook. See `socket.rs`.
    socket_create_owned, socket_accept_owned, socket_owner,
    socket_release_all, SOCK_OWNER_KERNEL, MAX_SOCKETS,
};

pub use ipv6::{
    ipv6_init, ipv6_link_local, ipv6_ready, udpv6_send,
    eui64_link_local, pseudo_checksum,
    ETH_TYPE_IPV6, IPV6_HDR_SIZE,
};

// ── Network configuration ─────────────────────────────────────────────────────

/// Default QEMU virt network (10.0.2.x/24 NAT).
pub const DEFAULT_IP:      [u8; 4] = [10, 0, 2, 15];
pub const DEFAULT_GATEWAY: [u8; 4] = [10, 0, 2, 2];
pub const DEFAULT_MASK:    [u8; 4] = [255, 255, 255, 0];

use robot_os_sync::SpinLock;

struct NetConfig {
    ip:      [u8; 4],
    mask:    [u8; 4],
    gateway: [u8; 4],
    mac:     [u8; 6],
    ready:   bool,
}

impl NetConfig {
    const fn new() -> Self {
        NetConfig {
            ip:      DEFAULT_IP,
            mask:    DEFAULT_MASK,
            gateway: DEFAULT_GATEWAY,
            mac:     [0; 6],
            ready:   false,
        }
    }
}

static NET_CFG: SpinLock<NetConfig> = SpinLock::new(NetConfig::new());

// ── Transport abstraction ─────────────────────────────────────────────────────
//
// On QEMU we use VirtIO net; on VF2 we use the Cadence MACB Ethernet driver.
// These two functions hide the difference from the rest of the stack.

/// Send a raw Ethernet frame via the active transport.
///
/// No separate `is_ready()` pre-check for the VirtIO path: `send()` already
/// fails cleanly on an uninitialized device, and the pre-check cost an extra
/// `NET.lock()` round-trip on every single frame of the hot path.
pub fn net_raw_send(frame: &[u8]) -> i32 {
    if robot_os_drivers::eth::eth_is_ready() {
        return robot_os_drivers::eth::eth_send(frame);
    }
    match robot_os_drivers::virtio::net::send(frame) {
        Ok(()) => frame.len() as i32,
        Err(()) => -1,
    }
}

/// Receive a raw Ethernet frame from the active transport.
/// Returns the number of bytes received, or 0 if none available.
///
/// Same rationale as `net_raw_send`: `poll_recv()` guards on device readiness
/// itself, so the previous `is_ready()` call here doubled the lock traffic of
/// the busiest loop in the stack (`net_poll` calls this up to 64x per tick).
fn net_raw_recv(buf: &mut [u8]) -> usize {
    if robot_os_drivers::eth::eth_is_ready() {
        let n = robot_os_drivers::eth::eth_recv(buf);
        return if n > 0 { n as usize } else { 0 };
    }
    robot_os_drivers::virtio::net::poll_recv(buf)
}

/// Initialize the network stack.
///
/// Must be called after the transport driver is ready (VirtIO net or MACB eth).
pub fn net_init() {
    // Seed the TCP ISN secret from runtime entropy so an attacker who has the
    // binary cannot predict every initial sequence number (RFC 6528). This
    // bare-metal target has no dedicated TRNG crate today, so we mix the
    // CLINT cycle counter at boot (unpredictable from static analysis) plus
    // each transport MAC. Once a real TRNG lands, replace this seed source.
    let boot_time = robot_os_drivers::clint::get_time();
    let seed = (boot_time as u32) ^ ((boot_time >> 32) as u32);
    tcp::isn_secret_seed(seed);

    // Determine which transport is available and get its MAC.
    let (mac, ready) = if robot_os_drivers::eth::eth_is_ready() {
        (robot_os_drivers::eth::eth_mac_addr(), true)
    } else if robot_os_drivers::virtio::net::is_ready() {
        (robot_os_drivers::virtio::net::get_mac(), true)
    } else {
        ([0u8; 6], false)
    };

    {
        let mut cfg = NET_CFG.lock();
        cfg.mac   = mac;
        cfg.ready = ready;
    }
    if ready {
        let cfg = NET_CFG.lock();
        tcp::init(cfg.mac, cfg.ip);
        // F22: IPv6 — compute link-local address from MAC (EUI-64).
        ipv6::ipv6_init(&cfg.mac);
        let ll = ipv6::ipv6_link_local();
        robot_os_drivers::kprintln!(
            "[NET] Stack ready — IP: {}.{}.{}.{}, GW: {}.{}.{}.{}",
            cfg.ip[0], cfg.ip[1], cfg.ip[2], cfg.ip[3],
            cfg.gateway[0], cfg.gateway[1], cfg.gateway[2], cfg.gateway[3],
        );
        robot_os_drivers::kprintln!(
            "[NET] IPv6 link-local: {:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:\
             {:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}",
            ll[0],ll[1],ll[2],ll[3],ll[4],ll[5],ll[6],ll[7],
            ll[8],ll[9],ll[10],ll[11],ll[12],ll[13],ll[14],ll[15]
        );
    }
}

/// Poll for incoming packets and process them.
/// Should be called periodically (e.g. from timer handler or shell loop).
///
/// Drains the entire RX queue per call (up to `MAX_DRAIN_PER_CALL` frames)
/// instead of just one — otherwise the kernel falls behind under load.
pub fn net_poll() {
    /// Bound the drain so we don't starve other tasks if the device is
    /// flooding (e.g. broadcast storm). 64 ≈ one Ethernet line-rate burst.
    const MAX_DRAIN_PER_CALL: usize = 64;

    let (mac, ip) = {
        let cfg = NET_CFG.lock();
        (cfg.mac, cfg.ip)
    };

    let mut buf = [0u8; ethernet::ETH_FRAME_MAX];
    for _ in 0..MAX_DRAIN_PER_CALL {
        let n = net_raw_recv(&mut buf);
        if n == 0 { break; }
        if let Some((hdr, payload)) = ethernet::parse(&buf[..n]) {
            match hdr.ethertype() {
                ethernet::ETH_TYPE_ARP  => arp::handle(payload, &mac, &ip),
                ethernet::ETH_TYPE_IP   => ip::handle(payload, &mac, &ip),
                ethernet::ETH_TYPE_IPV6 => ipv6::ipv6_rx(payload, payload.len()),
                _                       => {}
            }
        }
    }
}

/// Send an ICMP ping to `dst_ip`.  Returns 0 on success, -1 on ARP miss.
pub fn net_ping(dst_ip: [u8; 4]) -> i32 {
    let (mac, ip) = {
        let cfg = NET_CFG.lock();
        (cfg.mac, cfg.ip)
    };

    // ICMP echo request (type=8, code=0)
    let mut icmp = [0u8; 16];
    icmp[0] = 8;  // type: echo request
    icmp[4] = 0;  // id hi
    icmp[5] = 1;  // id lo
    icmp[6] = 0;  // seq hi
    icmp[7] = 1;  // seq lo
    icmp[8..16].copy_from_slice(b"RobotOS!");
    let cs = ip::checksum(&icmp);
    let cb = cs.to_be_bytes();
    icmp[2] = cb[0];
    icmp[3] = cb[1];

    ip::send(&mac, &ip, &dst_ip, ip::IP_PROTO_ICMP, &icmp)
}

/// Print network interface information.
pub fn net_info() {
    let cfg = NET_CFG.lock();
    let mac = &cfg.mac;
    robot_os_drivers::kprintln!(
        "[NET] eth0: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    robot_os_drivers::kprintln!(
        "[NET]       inet {}.{}.{}.{}  mask {}.{}.{}.{}",
        cfg.ip[0], cfg.ip[1], cfg.ip[2], cfg.ip[3],
        cfg.mask[0], cfg.mask[1], cfg.mask[2], cfg.mask[3],
    );
    robot_os_drivers::kprintln!(
        "[NET]       gw   {}.{}.{}.{}",
        cfg.gateway[0], cfg.gateway[1], cfg.gateway[2], cfg.gateway[3],
    );
    if !cfg.ready {
        robot_os_drivers::kprintln!("[NET]       (not ready — no VirtIO net)");
    }
}

/// Set a static IP configuration.
pub fn net_set_ip(ip: [u8; 4], mask: [u8; 4], gw: [u8; 4]) {
    {
        let mut cfg = NET_CFG.lock();
        cfg.ip      = ip;
        cfg.mask    = mask;
        cfg.gateway = gw;
    }
    // TCP caches its own copy for the checksum pseudo-header, and used to be
    // written only by `tcp::init`. Anything that changed the address later —
    // DHCP above all — left TCP checksumming against the old one, which drops
    // every segment silently. Done after releasing NET_CFG so the two locks are
    // never held at once.
    tcp::set_our_ip(ip);
}

/// Get current IP address.
pub fn net_get_ip() -> [u8; 4] {
    NET_CFG.lock().ip
}

/// Get current gateway address.
pub fn net_get_gateway() -> [u8; 4] {
    NET_CFG.lock().gateway
}

/// Get current network mask.
pub fn net_get_mask() -> [u8; 4] {
    NET_CFG.lock().mask
}

/// Get current MAC address.
pub fn net_get_mac() -> [u8; 6] {
    NET_CFG.lock().mac
}

/// Get full network config: (ip, mask, gateway).
pub fn net_get_config() -> ([u8; 4], [u8; 4], [u8; 4]) {
    let cfg = NET_CFG.lock();
    (cfg.ip, cfg.mask, cfg.gateway)
}
