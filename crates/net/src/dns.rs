//! DNS resolver — minimal A-record lookup over UDP (F05).
//!
//! Sends a DNS query to the configured DNS server (from DHCP) and waits
//! for a response. Caches the last successful result.

use super::udp;
use robot_os_sync::SpinLock;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// DNS server port.
const DNS_SERVER_PORT: u16 = 53;

/// DNS client source port.
const DNS_CLIENT_PORT: u16 = 5353;

/// Maximum DNS response size we handle.
const DNS_MAX_RESPONSE: usize = 512;

/// DNS query type A (IPv4 address).
const DNS_TYPE_A: u16 = 1;

/// DNS query class IN (Internet).
const DNS_CLASS_IN: u16 = 1;

/// Maximum hostname length.
const DNS_MAX_NAME_LEN: usize = 63;

/// DNS header size in bytes.
const DNS_HDR_SIZE: usize = 12;

/// Maximum number of cached DNS entries.
const DNS_CACHE_SIZE: usize = 4;

/// DNS cache entry TTL in CLINT ticks (5 minutes).
const DNS_CACHE_TTL_TICKS: u64 = 5 * 60 * 10_000_000;

// ---------------------------------------------------------------------------
// DNS header flags
// ---------------------------------------------------------------------------

/// QR bit: this is a response.
const DNS_FLAG_QR: u16 = 1 << 15;
/// RD bit: recursion desired.
const DNS_FLAG_RD: u16 = 1 << 8;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// DNS server IP (set by DHCP or manually).
static mut DNS_SERVER: [u8; 4] = [8, 8, 8, 8]; // default: Google DNS

/// Cached DNS entries.
struct DnsCacheEntry {
    name: [u8; DNS_MAX_NAME_LEN],
    name_len: u8,
    ip: [u8; 4],
    timestamp: u64,
    valid: bool,
}

impl DnsCacheEntry {
    const fn empty() -> Self {
        Self {
            name: [0; DNS_MAX_NAME_LEN],
            name_len: 0,
            ip: [0; 4],
            timestamp: 0,
            valid: false,
        }
    }
}

static DNS_CACHE: SpinLock<[DnsCacheEntry; DNS_CACHE_SIZE]> = SpinLock::new({
    const EMPTY: DnsCacheEntry = DnsCacheEntry::empty();
    [EMPTY; DNS_CACHE_SIZE]
});

/// Transaction ID counter.
static mut DNS_TX_ID: u16 = 0x1234;

/// Last received DNS response (filled by handle()).
static mut DNS_RESPONSE: [u8; DNS_MAX_RESPONSE] = [0u8; DNS_MAX_RESPONSE];
static mut DNS_RESPONSE_LEN: usize = 0;
static mut DNS_RESPONSE_READY: bool = false;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Set the DNS server IP address (called by DHCP on lease).
pub fn set_dns_server(ip: [u8; 4]) {
    unsafe { DNS_SERVER = ip; }
}

/// Get the currently configured DNS server IP.
pub fn get_dns_server() -> [u8; 4] {
    unsafe { DNS_SERVER }
}

/// Resolve a hostname to an IPv4 address.
/// Returns the IP or None on failure/timeout.
///
/// Checks cache first, then sends UDP query and polls for response.
pub fn resolve(hostname: &str) -> Option<[u8; 4]> {
    if hostname.is_empty() || hostname.len() > DNS_MAX_NAME_LEN {
        return None;
    }

    let now = robot_os_drivers::clint::get_time();

    // Check cache
    {
        let cache = DNS_CACHE.lock();
        for entry in cache.iter() {
            if entry.valid
                && entry.name_len as usize == hostname.len()
                && &entry.name[..entry.name_len as usize] == hostname.as_bytes()
                && now.saturating_sub(entry.timestamp) < DNS_CACHE_TTL_TICKS
            {
                return Some(entry.ip);
            }
        }
    }

    // Build and send DNS query
    let tx_id = unsafe {
        DNS_TX_ID = DNS_TX_ID.wrapping_add(1);
        DNS_TX_ID
    };

    let mut query_buf = [0u8; 128];
    let query_len = build_query(&mut query_buf, tx_id, hostname)?;

    let server_ip = unsafe { DNS_SERVER };
    let (mac, our_ip) = (super::net_get_mac(), super::net_get_ip());

    // Clear response flag
    unsafe {
        DNS_RESPONSE_READY = false;
        DNS_RESPONSE_LEN = 0;
    }

    // Send query via raw UDP (don't need a socket)
    udp::send_raw(&mac, &our_ip, &server_ip,
                   DNS_CLIENT_PORT, DNS_SERVER_PORT,
                   &query_buf[..query_len]);

    // Poll for response (up to 2 seconds)
    let timeout_ticks = 2 * 10_000_000u64; // 2 seconds
    let deadline = now + timeout_ticks;
    while robot_os_drivers::clint::get_time() < deadline {
        super::net_poll();
        if unsafe { DNS_RESPONSE_READY } {
            // Parse response
            let result = unsafe {
                parse_response(&DNS_RESPONSE[..DNS_RESPONSE_LEN], tx_id)
            };
            if let Some(ip) = result {
                // Cache it
                cache_result(hostname, ip, now);
                return Some(ip);
            }
            return None;
        }
        core::hint::spin_loop();
    }

    None // timeout
}

/// Handle an incoming DNS response (called from udp::handle for port 5353).
pub fn handle_response(data: &[u8]) {
    if data.len() < DNS_HDR_SIZE || data.len() > DNS_MAX_RESPONSE {
        return;
    }
    unsafe {
        DNS_RESPONSE[..data.len()].copy_from_slice(data);
        DNS_RESPONSE_LEN = data.len();
        DNS_RESPONSE_READY = true;
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build a DNS A-record query. Returns query length or None.
fn build_query(buf: &mut [u8; 128], tx_id: u16, hostname: &str) -> Option<usize> {
    // Header: ID, flags(RD=1), QDCOUNT=1, ANCOUNT=0, NSCOUNT=0, ARCOUNT=0
    buf[0..2].copy_from_slice(&tx_id.to_be_bytes());
    buf[2..4].copy_from_slice(&DNS_FLAG_RD.to_be_bytes());
    buf[4..6].copy_from_slice(&1u16.to_be_bytes()); // QDCOUNT = 1
    // ANCOUNT, NSCOUNT, ARCOUNT = 0 (already zeroed)

    // Question: encode hostname as labels
    let mut pos = DNS_HDR_SIZE;
    for label in hostname.split('.') {
        if label.is_empty() || label.len() > 63 || pos + 1 + label.len() >= 128 {
            return None;
        }
        buf[pos] = label.len() as u8;
        pos += 1;
        buf[pos..pos + label.len()].copy_from_slice(label.as_bytes());
        pos += label.len();
    }
    buf[pos] = 0; // null terminator
    pos += 1;

    // QTYPE = A (1), QCLASS = IN (1)
    buf[pos..pos + 2].copy_from_slice(&DNS_TYPE_A.to_be_bytes());
    pos += 2;
    buf[pos..pos + 2].copy_from_slice(&DNS_CLASS_IN.to_be_bytes());
    pos += 2;

    Some(pos)
}

/// Parse a DNS response and extract the first A record IP.
fn parse_response(data: &[u8], expected_tx_id: u16) -> Option<[u8; 4]> {
    if data.len() < DNS_HDR_SIZE { return None; }

    let tx_id = u16::from_be_bytes([data[0], data[1]]);
    if tx_id != expected_tx_id { return None; }

    let flags = u16::from_be_bytes([data[2], data[3]]);
    if flags & DNS_FLAG_QR == 0 { return None; } // not a response
    let rcode = flags & 0x000F;
    if rcode != 0 { return None; } // error

    let ancount = u16::from_be_bytes([data[6], data[7]]);
    if ancount == 0 { return None; }

    // Skip question section
    let mut pos = DNS_HDR_SIZE;
    // Skip name (labels until null or pointer)
    pos = skip_name(data, pos)?;
    pos += 4; // QTYPE + QCLASS

    // Parse answer records
    for _ in 0..ancount {
        if pos >= data.len() { return None; }
        pos = skip_name(data, pos)?;
        if pos + 10 > data.len() { return None; }
        let rtype = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let rdlength = u16::from_be_bytes([data[pos + 8], data[pos + 9]]);
        pos += 10;
        if rtype == DNS_TYPE_A && rdlength == 4 && pos + 4 <= data.len() {
            return Some([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        }
        pos += rdlength as usize;
    }

    None
}

/// Skip a DNS name (handles both labels and compression pointers).
fn skip_name(data: &[u8], mut pos: usize) -> Option<usize> {
    let max_jumps = 10; // prevent infinite loops
    let mut jumps = 0;
    let mut first_end = None;

    loop {
        if pos >= data.len() || jumps > max_jumps { return None; }
        let b = data[pos];
        if b == 0 {
            pos += 1;
            break;
        }
        if b & 0xC0 == 0xC0 {
            // Compression pointer
            if first_end.is_none() {
                first_end = Some(pos + 2);
            }
            if pos + 1 >= data.len() { return None; }
            pos = ((b as usize & 0x3F) << 8) | data[pos + 1] as usize;
            jumps += 1;
        } else {
            pos += 1 + b as usize;
        }
    }

    Some(first_end.unwrap_or(pos))
}

/// Cache a DNS result.
fn cache_result(hostname: &str, ip: [u8; 4], now: u64) {
    let mut cache = DNS_CACHE.lock();
    // Find oldest or free slot
    let mut oldest_idx = 0;
    let mut oldest_ts = u64::MAX;
    for i in 0..DNS_CACHE_SIZE {
        if !cache[i].valid {
            oldest_idx = i;
            break;
        }
        if cache[i].timestamp < oldest_ts {
            oldest_ts = cache[i].timestamp;
            oldest_idx = i;
        }
    }
    let entry = &mut cache[oldest_idx];
    let name_len = hostname.len().min(DNS_MAX_NAME_LEN);
    entry.name[..name_len].copy_from_slice(&hostname.as_bytes()[..name_len]);
    entry.name_len = name_len as u8;
    entry.ip = ip;
    entry.timestamp = now;
    entry.valid = true;
}
