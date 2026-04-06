//! DHCP client — auto-configuration via DHCP DISCOVER/OFFER/REQUEST/ACK.
//!
//! Uses the UDP module for transport (port 68 client, port 67 server).
//! Simplified: fixed 300-byte message buffer, no lease renewal.

use crate::udp;

// DHCP ports
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_SERVER_PORT: u16 = 67;

// DHCP message types (option 53)
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER:    u8 = 2;
const DHCP_REQUEST:  u8 = 3;
const DHCP_ACK:      u8 = 5;

// DHCP option codes
const OPT_SUBNET_MASK:  u8 = 1;
const OPT_ROUTER:       u8 = 3;
const OPT_DNS:          u8 = 6;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_MSG_TYPE:     u8 = 53;
const OPT_SERVER_ID:    u8 = 54;
const OPT_END:          u8 = 255;

// Magic cookie (DHCP over BOOTP)
const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

// Fixed transaction ID
const XID: u32 = 0x5243_4F53; // "RCOS"

const DHCP_MSG_SIZE: usize = 300;

/// UDP header size (bytes).
const UDP_HDR_SIZE: usize = 8;

// DHCP/BOOTP header offsets (RFC 2131).
const OFF_OP:     usize = 0;
const OFF_HTYPE:  usize = 1;
const OFF_HLEN:   usize = 2;
const OFF_HOPS:   usize = 3;
const OFF_XID:    usize = 4;
const OFF_SECS:   usize = 8;
const OFF_FLAGS:  usize = 10;
const OFF_CIADDR: usize = 12;
const OFF_YIADDR: usize = 16;
const OFF_SIADDR: usize = 20;
const OFF_GIADDR: usize = 24;
const OFF_CHADDR: usize = 28;
const OFF_SNAME:  usize = 44;
const OFF_FILE:   usize = 108;
const OFF_COOKIE: usize = 236;
const OFF_OPTS:   usize = 240;

use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum DhcpState {
    Idle        = 0,
    Discovering = 1,
    Requesting  = 2,
    Bound       = 3,
}

static DHCP_STATE: AtomicU8 = AtomicU8::new(DhcpState::Idle as u8);

fn set_dhcp_state(s: DhcpState) { DHCP_STATE.store(s as u8, Ordering::Release); }

fn get_dhcp_state() -> DhcpState {
    match DHCP_STATE.load(Ordering::Acquire) {
        1 => DhcpState::Discovering,
        2 => DhcpState::Requesting,
        3 => DhcpState::Bound,
        _ => DhcpState::Idle,
    }
}

/// Parsed DHCP offer/ack fields.
struct DhcpInfo {
    offered_ip: [u8; 4],
    server_ip:  [u8; 4],
    subnet:     [u8; 4],
    gateway:    [u8; 4],
    dns:        [u8; 4],
    msg_type:   u8,
}

impl DhcpInfo {
    const fn new() -> Self {
        DhcpInfo {
            offered_ip: [0; 4],
            server_ip:  [0; 4],
            subnet:     [255, 255, 255, 0],
            gateway:    [0; 4],
            dns:        [0; 4],
            msg_type:   0,
        }
    }
}

/// Build a base DHCP message with common fields filled in.
fn build_base_msg(buf: &mut [u8; DHCP_MSG_SIZE], mac: &[u8; 6]) {
    // Zero the entire buffer
    let mut i = 0;
    while i < DHCP_MSG_SIZE { buf[i] = 0; i += 1; }

    buf[OFF_OP]    = 1;   // BOOTREQUEST
    buf[OFF_HTYPE] = 1;   // Ethernet
    buf[OFF_HLEN]  = 6;   // MAC length
    buf[OFF_HOPS]  = 0;
    let xid = XID.to_be_bytes();
    buf[OFF_XID..OFF_XID + 4].copy_from_slice(&xid);
    buf[OFF_SECS]  = 0;
    buf[OFF_SECS + 1] = 0;
    // FLAGS: broadcast bit set (0x8000)
    buf[OFF_FLAGS]     = 0x80;
    buf[OFF_FLAGS + 1] = 0x00;
    // CHADDR: our MAC in first 6 bytes (rest zero)
    buf[OFF_CHADDR..OFF_CHADDR + 6].copy_from_slice(mac);
    // Magic cookie
    buf[OFF_COOKIE..OFF_COOKIE + 4].copy_from_slice(&MAGIC_COOKIE);
}

/// Send DHCP DISCOVER via broadcast.
pub fn dhcp_discover() {
    let mac = crate::net_get_mac();
    let mut msg = [0u8; DHCP_MSG_SIZE];
    build_base_msg(&mut msg, &mac);

    // Options
    let mut o = OFF_OPTS;
    // Option 53: DHCP Message Type = DISCOVER
    msg[o] = OPT_MSG_TYPE; msg[o + 1] = 1; msg[o + 2] = DHCP_DISCOVER; o += 3;
    // End
    msg[o] = OPT_END;

    // Send broadcast: src 0.0.0.0:68 → dst 255.255.255.255:67
    let src_ip  = [0u8; 4];
    let dst_ip  = [255u8; 4];
    let dst_mac = [0xFFu8; 6];

    // Build and send raw (bypass ARP — broadcast)
    use super::ethernet;
    use super::ip;
    let udp_len = UDP_HDR_SIZE + DHCP_MSG_SIZE;
    let ip_total = ip::IP_HDR_MIN + udp_len;
    let frame_len = ethernet::EthHdr::SIZE + ip_total;

    let mut frame = [0u8; ethernet::ETH_FRAME_MAX];
    if frame_len > frame.len() { return; }

    // IP header
    let mut ip_hdr = [0u8; ip::IP_HDR_MIN];
    ip::build_header(&mut ip_hdr, ip::IP_PROTO_UDP, &src_ip, &dst_ip, udp_len as u16);

    // UDP header
    let mut udp_buf = [0u8; UDP_HDR_SIZE];
    udp_buf[0..2].copy_from_slice(&DHCP_CLIENT_PORT.to_be_bytes());
    udp_buf[2..4].copy_from_slice(&DHCP_SERVER_PORT.to_be_bytes());
    udp_buf[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    udp_buf[6..8].copy_from_slice(&[0, 0]); // checksum optional

    // Assemble IP payload: IP hdr + UDP hdr + DHCP msg
    let mut ip_payload = [0u8; ip::ETH_MTU];
    ip_payload[..ip::IP_HDR_MIN].copy_from_slice(&ip_hdr);
    ip_payload[ip::IP_HDR_MIN..ip::IP_HDR_MIN + UDP_HDR_SIZE].copy_from_slice(&udp_buf);
    ip_payload[ip::IP_HDR_MIN + UDP_HDR_SIZE..ip::IP_HDR_MIN + UDP_HDR_SIZE + DHCP_MSG_SIZE].copy_from_slice(&msg);

    ethernet::build(&mut frame[..frame_len], &dst_mac, &mac,
                    ethernet::ETH_TYPE_IP, &ip_payload[..ip_total]);

    let _ = super::net_raw_send(&frame[..frame_len]);
    set_dhcp_state(DhcpState::Discovering);
    robot_os_drivers::kprintln!("[DHCP] DISCOVER sent");
}

/// Parse a DHCP response (OFFER or ACK) from raw UDP payload.
fn parse_response(data: &[u8]) -> Option<DhcpInfo> {
    if data.len() < OFF_OPTS + 4 { return None; }
    // Verify op=2 (BOOTREPLY) and magic cookie
    if data[OFF_OP] != 2 { return None; }
    if data[OFF_COOKIE..OFF_COOKIE + 4] != MAGIC_COOKIE { return None; }
    // Verify XID
    let xid = u32::from_be_bytes([data[OFF_XID], data[OFF_XID+1], data[OFF_XID+2], data[OFF_XID+3]]);
    if xid != XID { return None; }

    let mut info = DhcpInfo::new();
    info.offered_ip.copy_from_slice(&data[OFF_YIADDR..OFF_YIADDR + 4]);
    info.server_ip.copy_from_slice(&data[OFF_SIADDR..OFF_SIADDR + 4]);

    // Parse options
    let mut o = OFF_OPTS;
    while o < data.len() {
        let opt = data[o];
        if opt == OPT_END { break; }
        if opt == 0 { o += 1; continue; } // padding
        if o + 1 >= data.len() { break; }
        let len = data[o + 1] as usize;
        if o + 2 + len > data.len() { break; }
        let val = &data[o + 2..o + 2 + len];
        match opt {
            OPT_MSG_TYPE    if len >= 1 => { info.msg_type = val[0]; }
            OPT_SUBNET_MASK if len >= 4 => { info.subnet.copy_from_slice(&val[..4]); }
            OPT_ROUTER      if len >= 4 => { info.gateway.copy_from_slice(&val[..4]); }
            OPT_DNS         if len >= 4 => { info.dns.copy_from_slice(&val[..4]); }
            OPT_SERVER_ID   if len >= 4 => { info.server_ip.copy_from_slice(&val[..4]); }
            _ => {}
        }
        o += 2 + len;
    }
    Some(info)
}

/// Handle a DHCP OFFER: extract and return offered IP + server IP.
pub fn dhcp_handle_offer(data: &[u8]) -> Option<([u8; 4], [u8; 4])> {
    let info = parse_response(data)?;
    if info.msg_type != DHCP_OFFER { return None; }
    robot_os_drivers::kprintln!(
        "[DHCP] OFFER: {}.{}.{}.{} from server {}.{}.{}.{}",
        info.offered_ip[0], info.offered_ip[1], info.offered_ip[2], info.offered_ip[3],
        info.server_ip[0], info.server_ip[1], info.server_ip[2], info.server_ip[3],
    );
    Some((info.offered_ip, info.server_ip))
}

/// Send DHCP REQUEST for the offered IP.
pub fn dhcp_request(offered_ip: [u8; 4], server_ip: [u8; 4]) {
    let mac = crate::net_get_mac();
    let mut msg = [0u8; DHCP_MSG_SIZE];
    build_base_msg(&mut msg, &mac);

    // Options
    let mut o = OFF_OPTS;
    // Option 53: DHCP Message Type = REQUEST
    msg[o] = OPT_MSG_TYPE; msg[o + 1] = 1; msg[o + 2] = DHCP_REQUEST; o += 3;
    // Option 50: Requested IP
    msg[o] = OPT_REQUESTED_IP; msg[o + 1] = 4;
    msg[o + 2..o + 6].copy_from_slice(&offered_ip); o += 6;
    // Option 54: Server Identifier
    msg[o] = OPT_SERVER_ID; msg[o + 1] = 4;
    msg[o + 2..o + 6].copy_from_slice(&server_ip); o += 6;
    // End
    msg[o] = OPT_END;

    // Send broadcast (still using 0.0.0.0 as src until ACK)
    let src_ip  = [0u8; 4];
    let dst_ip  = [255u8; 4];
    let dst_mac = [0xFFu8; 6];

    use super::ethernet;
    use super::ip;
    let udp_len = UDP_HDR_SIZE + DHCP_MSG_SIZE;
    let ip_total = ip::IP_HDR_MIN + udp_len;
    let frame_len = ethernet::EthHdr::SIZE + ip_total;

    let mut frame = [0u8; ethernet::ETH_FRAME_MAX];
    if frame_len > frame.len() { return; }

    let mut ip_hdr = [0u8; ip::IP_HDR_MIN];
    ip::build_header(&mut ip_hdr, ip::IP_PROTO_UDP, &src_ip, &dst_ip, udp_len as u16);

    let mut udp_buf = [0u8; UDP_HDR_SIZE];
    udp_buf[0..2].copy_from_slice(&DHCP_CLIENT_PORT.to_be_bytes());
    udp_buf[2..4].copy_from_slice(&DHCP_SERVER_PORT.to_be_bytes());
    udp_buf[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    udp_buf[6..8].copy_from_slice(&[0, 0]);

    let mut ip_payload = [0u8; ip::ETH_MTU];
    ip_payload[..ip::IP_HDR_MIN].copy_from_slice(&ip_hdr);
    ip_payload[ip::IP_HDR_MIN..ip::IP_HDR_MIN + UDP_HDR_SIZE].copy_from_slice(&udp_buf);
    ip_payload[ip::IP_HDR_MIN + UDP_HDR_SIZE..ip::IP_HDR_MIN + UDP_HDR_SIZE + DHCP_MSG_SIZE].copy_from_slice(&msg);

    ethernet::build(&mut frame[..frame_len], &dst_mac, &mac,
                    ethernet::ETH_TYPE_IP, &ip_payload[..ip_total]);

    let _ = super::net_raw_send(&frame[..frame_len]);
    set_dhcp_state(DhcpState::Requesting);
    robot_os_drivers::kprintln!("[DHCP] REQUEST sent");
}

/// Handle a DHCP ACK: apply the IP configuration.  Returns true on success.
pub fn dhcp_handle_ack(data: &[u8]) -> bool {
    let info = match parse_response(data) {
        Some(i) => i,
        None    => return false,
    };
    if info.msg_type != DHCP_ACK { return false; }

    crate::net_set_ip(info.offered_ip, info.subnet, info.gateway);
    // F05: Apply DNS server from DHCP lease
    if info.dns != [0, 0, 0, 0] {
        crate::dns::set_dns_server(info.dns);
        robot_os_drivers::kprintln!(
            "[DHCP] DNS server: {}.{}.{}.{}",
            info.dns[0], info.dns[1], info.dns[2], info.dns[3],
        );
    }
    set_dhcp_state(DhcpState::Bound);
    robot_os_drivers::kprintln!(
        "[DHCP] ACK: bound to {}.{}.{}.{}  mask {}.{}.{}.{}  gw {}.{}.{}.{}",
        info.offered_ip[0], info.offered_ip[1], info.offered_ip[2], info.offered_ip[3],
        info.subnet[0], info.subnet[1], info.subnet[2], info.subnet[3],
        info.gateway[0], info.gateway[1], info.gateway[2], info.gateway[3],
    );
    true
}

/// Run the full DHCP handshake: DISCOVER → OFFER → REQUEST → ACK.
/// Yields between steps to allow net_poll() to process packets.
/// `yield_fn` is called to yield the CPU (e.g., `robot_os_sched::task_yield`).
/// Returns true if an IP was obtained, false on timeout.
pub fn dhcp_start(yield_fn: fn()) -> bool {
    // Bind to port 68 (DHCP client)
    let sock = udp::bind(DHCP_CLIENT_PORT);
    if sock < 0 {
        robot_os_drivers::kprintln!("[DHCP] ERROR: cannot bind port 68");
        return false;
    }

    // Phase 1: send DISCOVER
    dhcp_discover();

    // Wait for OFFER (up to ~200 yields)
    use super::ip;
    let mut buf = [0u8; ip::ETH_MTU];
    let mut src_ip   = [0u8; 4];
    let mut src_port = 0u16;
    let mut got_offer = false;
    let mut offered_ip = [0u8; 4];
    let mut server_ip  = [0u8; 4];

    for _ in 0..200 {
        yield_fn();
        crate::net_poll();
        let n = udp::recvfrom(sock, &mut buf, &mut src_ip, &mut src_port);
        if n > 0 {
            if let Some((oip, sip)) = dhcp_handle_offer(&buf[..n as usize]) {
                offered_ip = oip;
                server_ip  = sip;
                got_offer  = true;
                break;
            }
        }
    }

    if !got_offer {
        robot_os_drivers::kprintln!("[DHCP] TIMEOUT: no OFFER received");
        udp::close(sock);
        return false;
    }

    // Phase 2: send REQUEST
    dhcp_request(offered_ip, server_ip);

    // Wait for ACK
    let mut got_ack = false;
    for _ in 0..200 {
        yield_fn();
        crate::net_poll();
        let n = udp::recvfrom(sock, &mut buf, &mut src_ip, &mut src_port);
        if n > 0 {
            if dhcp_handle_ack(&buf[..n as usize]) {
                got_ack = true;
                break;
            }
        }
    }

    udp::close(sock);

    if !got_ack {
        robot_os_drivers::kprintln!("[DHCP] TIMEOUT: no ACK received");
        return false;
    }

    robot_os_drivers::kprintln!("[DHCP] Configuration complete");
    true
}

/// Return the current DHCP state.
pub fn dhcp_state() -> DhcpState {
    get_dhcp_state()
}
