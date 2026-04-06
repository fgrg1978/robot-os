#![no_std]

//! Robot OS network stack — port of kernel/net/
//! Ethernet → ARP / IPv4 → ICMP / UDP / TCP → BSD socket API.
//! Polling-based (call net_poll() from timer tick or shell).

pub mod ethernet;
pub mod arp;
pub mod ip;
pub mod udp;
pub mod tcp;
pub mod socket;
#[allow(dead_code)]
pub mod dhcp;
pub mod dns;

pub use socket::{
    socket_create, socket_bind, socket_connect,
    socket_listen, socket_listen_bound, socket_accept,
    socket_send, socket_recv, socket_close,
    SockAddr, AF_INET, SOCK_STREAM, SOCK_DGRAM, IPPROTO_TCP, IPPROTO_UDP,
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
pub fn net_raw_send(frame: &[u8]) -> i32 {
    if robot_os_drivers::eth::eth_is_ready() {
        return robot_os_drivers::eth::eth_send(frame);
    }
    if robot_os_drivers::virtio::net::is_ready() {
        return match robot_os_drivers::virtio::net::send(frame) {
            Ok(()) => frame.len() as i32,
            Err(()) => -1,
        };
    }
    -1
}

/// Receive a raw Ethernet frame from the active transport.
/// Returns the number of bytes received, or 0 if none available.
fn net_raw_recv(buf: &mut [u8]) -> usize {
    if robot_os_drivers::eth::eth_is_ready() {
        let n = robot_os_drivers::eth::eth_recv(buf);
        return if n > 0 { n as usize } else { 0 };
    }
    if robot_os_drivers::virtio::net::is_ready() {
        return robot_os_drivers::virtio::net::poll_recv(buf);
    }
    0
}

/// Initialize the network stack.
///
/// Must be called after the transport driver is ready (VirtIO net or MACB eth).
pub fn net_init() {
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
        robot_os_drivers::kprintln!(
            "[NET] Stack ready — IP: {}.{}.{}.{}, GW: {}.{}.{}.{}",
            cfg.ip[0], cfg.ip[1], cfg.ip[2], cfg.ip[3],
            cfg.gateway[0], cfg.gateway[1], cfg.gateway[2], cfg.gateway[3],
        );
    }
}

/// Poll for incoming packets and process them.
/// Should be called periodically (e.g. from timer handler or shell loop).
pub fn net_poll() {
    let mut buf = [0u8; ethernet::ETH_FRAME_MAX];
    let n = net_raw_recv(&mut buf);
    if n == 0 { return; }

    let (mac, ip) = {
        let cfg = NET_CFG.lock();
        (cfg.mac, cfg.ip)
    };

    if let Some((hdr, payload)) = ethernet::parse(&buf[..n]) {
        match hdr.ethertype() {
            ethernet::ETH_TYPE_ARP => arp::handle(payload, &mac, &ip),
            ethernet::ETH_TYPE_IP  => ip::handle(payload, &mac, &ip),
            _                      => {}
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
    let mut cfg = NET_CFG.lock();
    cfg.ip      = ip;
    cfg.mask    = mask;
    cfg.gateway = gw;
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
