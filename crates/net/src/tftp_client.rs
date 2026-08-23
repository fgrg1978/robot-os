//! Kernel-side TFTP fetch — wires the pure `robot_os_tftp` state
//! machine to the kernel's UDP socket layer.
//!
//! Intended for **boot-time use** (before the scheduler starts):
//! the function is busy-polling and synchronous. It picks an
//! ephemeral local UDP port, sends a Read Request to
//! `server_ip:69`, then drives the state machine through DATA /
//! ACK exchanges until the transfer completes or a bounded poll
//! budget is exhausted.
//!
//! # Phase 1 limitations
//!
//! - Blocking poll loop. Not safe to call after `sched::start()`
//!   from a non-driver task — the calling CPU stalls.
//! - No RFC 1350 §4 retransmit-on-timeout. If the server drops a
//!   packet the fetch fails fast.
//! - No congestion / windowing — just request / data / ack.
//! - Server's `tid` (the ephemeral port it picks for replies) is
//!   captured on the first DATA and used for all subsequent ACKs.

use core::sync::atomic::{AtomicU16, Ordering};

use robot_os_tftp::{
    build_ack, build_rrq, parse_packet, ClientAction, RxOutcome, TftpClient,
    TftpEncodeError, TFTP_ACK_BYTES, TFTP_BLOCK_SIZE, TFTP_DATA_HEADER_BYTES,
    TFTP_PORT, TFTP_RRQ_MAX_BYTES,
};

use crate::udp;

// ──────────────────────────────────────────────────────────────────────────
// Tunables — every "magic number" lives here.
// ──────────────────────────────────────────────────────────────────────────

/// Receive buffer for one UDP datagram (header + payload). One
/// TFTP DATA carries at most `TFTP_DATA_HEADER_BYTES +
/// TFTP_BLOCK_SIZE` payload bytes.
const TFTP_RX_BUF_BYTES: usize = TFTP_DATA_HEADER_BYTES + TFTP_BLOCK_SIZE;

/// Maximum total iterations of the boot-time poll loop, summed
/// across all blocks. At ~1 poll per cycle this bounds the fetch
/// runtime regardless of server behaviour — a hung server can
/// still fail the boot in finite time rather than spin forever.
pub const TFTP_FETCH_MAX_POLLS: u32 = 5_000_000;

/// Polls to spend warming up the ARP cache + re-trying a send
/// that returned -1 (typical reason: ARP cache miss on first
/// send; the IP layer fires an ARP request and expects the
/// caller to retry once a reply lands — see `ip::send` comment).
/// Each iteration pumps `net_poll()` to drain incoming packets.
pub const TFTP_SEND_RETRY_POLLS: u32 = 200_000;

/// Ephemeral local source port allocator. We start from this base
/// and bump per fetch so two back-to-back fetches don't collide
/// inside a session. Range chosen above the IANA ephemeral floor
/// (49152) so we don't tread on well-known assignments.
const TFTP_EPHEMERAL_PORT_BASE: u16 = 49152;
static TFTP_EPHEMERAL_PORT: AtomicU16 = AtomicU16::new(TFTP_EPHEMERAL_PORT_BASE);

fn next_ephemeral_port() -> u16 {
    // Wrap back to the IANA floor at u16::MAX.
    let next = TFTP_EPHEMERAL_PORT.fetch_add(1, Ordering::Relaxed);
    if next < TFTP_EPHEMERAL_PORT_BASE {
        TFTP_EPHEMERAL_PORT_BASE
    } else {
        next
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Errors
// ──────────────────────────────────────────────────────────────────────────

/// Failure modes of [`tftp_fetch`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TftpFetchError {
    /// Filename rejected by [`build_rrq`].
    Encode(TftpEncodeError),
    /// `dst` is too small for the transferred file.
    BufferOverflow,
    /// Could not allocate a UDP socket (table full).
    SocketBindFailed,
    /// `sendto` of the RRQ or an ACK returned a hardware error.
    SendFailed,
    /// Server's first reply did not arrive within
    /// [`TFTP_FETCH_MAX_POLLS`].
    NoReply,
    /// Block jumped past the expected number — RFC 1350 §4 retry
    /// would be needed; not implemented in Phase 1.
    OutOfOrderBlock { expected: u16, received: u16 },
    /// Server returned an ERROR packet; the contained `code` is
    /// the RFC 1350 error code.
    ServerError(u16),
    /// Poll loop hit [`TFTP_FETCH_MAX_POLLS`] without finishing
    /// the transfer.
    PollBudgetExhausted,
}

impl From<TftpEncodeError> for TftpFetchError {
    fn from(e: TftpEncodeError) -> Self {
        Self::Encode(e)
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Public entry point
// ──────────────────────────────────────────────────────────────────────────

/// Fetch `filename` from `server_ip:69` into `dst`. Returns the
/// number of bytes written.
///
/// **Boot-time only**: busy-polls; not safe from a runnable task.
pub fn tftp_fetch(
    server_ip: [u8; 4],
    filename: &str,
    dst: &mut [u8],
) -> Result<usize, TftpFetchError> {
    // ── Set up the local UDP socket. ────────────────────────────────
    let local_port = next_ephemeral_port();
    let sock = udp::bind(local_port);
    if sock < 0 {
        return Err(TftpFetchError::SocketBindFailed);
    }

    let result = fetch_inner(server_ip, filename, dst, sock);

    // Always release the socket — match-and-return would skip this.
    udp::unbind(sock as usize);
    result
}

fn fetch_inner(
    server_ip: [u8; 4],
    filename: &str,
    dst: &mut [u8],
    sock: i32,
) -> Result<usize, TftpFetchError> {
    // ── Build + send the RRQ. ───────────────────────────────────────
    let mut rrq_buf = [0u8; TFTP_RRQ_MAX_BYTES];
    let rrq_len = build_rrq(filename, &mut rrq_buf)?;
    // The first send to a new destination almost always returns -1
    // because the IP layer has to fire an ARP request and wait for
    // a reply. Pump `net_poll()` between retries so the reply can
    // land in the ARP cache; the comment in `ip::send` literally
    // says "caller can retry after delay".
    sendto_with_arp_retry(sock, &server_ip, TFTP_PORT, &rrq_buf[..rrq_len])?;

    // ── Poll for DATA, ACK, accumulate. ─────────────────────────────
    let mut client = TftpClient::new();
    let mut written: usize = 0;
    // The server picks a fresh ephemeral port (RFC 1350 calls it
    // the TID) and replies from it. We learn it from the first
    // DATA and direct subsequent ACKs there.
    let mut server_tid: u16 = TFTP_PORT;
    let mut server_tid_locked = false;
    let mut rx = [0u8; TFTP_RX_BUF_BYTES];
    let mut from_ip = [0u8; 4];
    let mut from_port: u16 = 0;

    for _poll in 0..TFTP_FETCH_MAX_POLLS {
        // Pump the device once per iter so the RX ring fills.
        crate::net_poll();

        let n = udp::recvfrom(sock, &mut rx, &mut from_ip, &mut from_port);
        if n <= 0 {
            // Nothing yet — keep polling within the budget.
            continue;
        }

        // Defensive: drop packets from unrelated peers. The first
        // accepted DATA locks the server's TID for the rest of
        // the transfer.
        if from_ip != server_ip {
            continue;
        }
        if server_tid_locked && from_port != server_tid {
            continue;
        }

        match parse_packet(&rx[..n as usize]) {
            RxOutcome::Data { block, payload, is_last } => {
                if !server_tid_locked {
                    server_tid = from_port;
                    server_tid_locked = true;
                }
                match client.on_data(block, is_last) {
                    ClientAction::AckAndConsume => {
                        if written + payload.len() > dst.len() {
                            return Err(TftpFetchError::BufferOverflow);
                        }
                        dst[written..written + payload.len()]
                            .copy_from_slice(payload);
                        written += payload.len();
                        send_ack(sock, &server_ip, server_tid, block)?;
                    }
                    ClientAction::AckIgnore => {
                        // Duplicate — re-ACK the prior block.
                        send_ack(sock, &server_ip, server_tid, block)?;
                    }
                    ClientAction::Complete => {
                        if written + payload.len() > dst.len() {
                            return Err(TftpFetchError::BufferOverflow);
                        }
                        dst[written..written + payload.len()]
                            .copy_from_slice(payload);
                        written += payload.len();
                        send_ack(sock, &server_ip, server_tid, block)?;
                        return Ok(written);
                    }
                    ClientAction::OutOfOrder { expected, received } => {
                        return Err(TftpFetchError::OutOfOrderBlock {
                            expected,
                            received,
                        });
                    }
                }
            }
            RxOutcome::Error(code) => {
                return Err(TftpFetchError::ServerError(code));
            }
            RxOutcome::Malformed => {
                // Drop and keep polling — the I/O layer's job is
                // to ignore garbage, not to escalate it.
                continue;
            }
        }
    }
    if !server_tid_locked {
        Err(TftpFetchError::NoReply)
    } else {
        Err(TftpFetchError::PollBudgetExhausted)
    }
}

fn send_ack(
    sock: i32,
    server_ip: &[u8; 4],
    server_tid: u16,
    block: u16,
) -> Result<(), TftpFetchError> {
    let mut ack = [0u8; TFTP_ACK_BYTES];
    build_ack(block, &mut ack);
    // After the first round-trip the ARP cache is warm and sends
    // succeed in one shot — but go through the retry helper anyway
    // so a transient ARP eviction doesn't sink the transfer.
    sendto_with_arp_retry(sock, server_ip, server_tid, &ack)
}

/// `udp::sendto` plus ARP-warmup retries. Returns `Ok(())` as
/// soon as the underlying send returns `0`, or
/// `Err(NoReply | SendFailed)` after `TFTP_SEND_RETRY_POLLS`.
fn sendto_with_arp_retry(
    sock: i32,
    dst_ip: &[u8; 4],
    dst_port: u16,
    data: &[u8],
) -> Result<(), TftpFetchError> {
    let mut last_rc = udp::sendto(sock, dst_ip, dst_port, data);
    if last_rc == 0 {
        return Ok(());
    }
    for _ in 0..TFTP_SEND_RETRY_POLLS {
        crate::net_poll();
        last_rc = udp::sendto(sock, dst_ip, dst_port, data);
        if last_rc == 0 {
            return Ok(());
        }
    }
    // After the retry budget, distinguish the "no ARP reply at
    // all" case from a deeper send error. The IP layer returns
    // -1 in both cases, so we can only report the budget exhaustion.
    Err(TftpFetchError::NoReply)
}
