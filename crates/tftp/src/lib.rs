//! TFTP client — RFC 1350, pure `no_std` state machine.
//!
//! This crate ships the protocol logic for DEV01 fast-iteration
//! netboot: kernel binary served from a local TFTP server on the
//! dev host, fetched by the board (or QEMU) at boot. Cuts the
//! flash-iteration cycle from ~3–5 min (sd-out / copy / sd-in /
//! boot) down to ~5 s.
//!
//! # Scope
//!
//! - Read-only client (RRQ + DATA + ACK + ERROR).
//! - `octet` (binary) mode only — the only mode we need for kernel
//!   images.
//! - No RFC 2347 option negotiation (OACK); the 512-byte default
//!   block size is fine for our images.
//! - No retransmit / timeout policy here — that's the I/O layer
//!   (kernel UDP wiring or a host-side test harness).
//!
//! # Usage shape
//!
//! ```ignore
//! let mut out = [0u8; TFTP_RRQ_MAX_BYTES];
//! let n = build_rrq("KERN.BIN", &mut out).unwrap();
//! udp.sendto(server, TFTP_PORT, &out[..n]);
//!
//! let mut client = TftpClient::new();
//! loop {
//!     let pkt = udp.recv();  // user-supplied I/O
//!     match parse_packet(&pkt) {
//!         RxOutcome::Data { block, payload, is_last } => {
//!             match client.on_data(block, is_last) {
//!                 ClientAction::AckAndConsume => {
//!                     consume(payload);
//!                     let mut ack = [0u8; TFTP_ACK_BYTES];
//!                     build_ack(block, &mut ack);
//!                     udp.sendto(server, TFTP_PORT, &ack);
//!                 }
//!                 ClientAction::AckIgnore => { /* duplicate */ }
//!                 ClientAction::Complete => break,
//!                 ClientAction::OutOfOrder => return Err(...),
//!             }
//!         }
//!         RxOutcome::Error(code) => return Err(code),
//!         RxOutcome::Malformed => continue,
//!     }
//! }
//! ```

#![no_std]

// ──────────────────────────────────────────────────────────────────────────
// Wire-format constants
// ──────────────────────────────────────────────────────────────────────────

/// Well-known TFTP server port (RFC 1350 §1).
pub const TFTP_PORT: u16 = 69;

/// Default data block size (RFC 1350 §3). We do not negotiate
/// other sizes (RFC 2348/2347), so this is the only block size
/// the state machine recognises.
pub const TFTP_BLOCK_SIZE: usize = 512;

/// Maximum filename bytes accepted by [`build_rrq`]. The on-wire
/// RRQ packet structure is `[opcode u16][filename str][0]
/// [mode str][0]`. With opcode (2) + `"octet"` + 2 NUL terminators
/// = 9 fixed bytes, leaving room for filenames up to 119 bytes
/// inside the conservative 128-byte buffer below. We cap at 128
/// to keep manifests readable and stack frames small.
pub const TFTP_MAX_FILENAME_BYTES: usize = 128;

/// Largest RRQ packet [`build_rrq`] can produce. Equals
/// `TFTP_MAX_FILENAME_BYTES + RRQ_FIXED_OVERHEAD_BYTES`. Sized so
/// callers can stack-allocate a buffer without arithmetic.
pub const TFTP_RRQ_MAX_BYTES: usize =
    TFTP_MAX_FILENAME_BYTES + RRQ_FIXED_OVERHEAD_BYTES;

/// Byte count of an ACK packet (`[opcode u16 BE][block u16 BE]`).
pub const TFTP_ACK_BYTES: usize = 4;

/// Header bytes preceding the data payload in a DATA packet
/// (`[opcode u16 BE][block u16 BE]`).
pub const TFTP_DATA_HEADER_BYTES: usize = 4;

/// Minimum bytes for a parseable ERROR packet
/// (`[opcode u16 BE][err_code u16 BE][msg str][0]`). The msg may
/// be empty, so the floor is 5 bytes (4 fixed + 1 NUL).
pub const TFTP_ERROR_MIN_BYTES: usize = 5;

/// RRQ packet bytes that are not part of the filename:
/// opcode (2) + filename NUL (1) + `"octet"` (5) + NUL (1) = 9.
/// Exposed so callers can size buffers without re-deriving the
/// arithmetic.
pub const RRQ_FIXED_OVERHEAD_BYTES: usize = 9;
const RRQ_MODE_BYTES: &[u8] = b"octet";

// ──────────────────────────────────────────────────────────────────────────
// Opcodes (RFC 1350 §5)
// ──────────────────────────────────────────────────────────────────────────

const OP_RRQ: u16 = 1;
#[allow(dead_code)]
const OP_WRQ: u16 = 2;
const OP_DATA: u16 = 3;
const OP_ACK: u16 = 4;
const OP_ERROR: u16 = 5;

// ──────────────────────────────────────────────────────────────────────────
// Errors
// ──────────────────────────────────────────────────────────────────────────

/// Encode-time failures from [`build_rrq`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TftpEncodeError {
    /// `filename` is empty.
    EmptyFilename,
    /// `filename.len() > TFTP_MAX_FILENAME_BYTES`.
    FilenameTooLong,
    /// `filename` contains a NUL byte (would terminate the wire
    /// string field prematurely).
    FilenameHasNul,
    /// Output buffer too small for the encoded RRQ.
    BufferTooSmall,
}

/// Protocol-level failures the [`TftpClient`] state machine can
/// surface to its caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TftpClientError {
    /// Server sent a DATA block whose number jumped past the
    /// expected one. Minimal client does not retry / reorder.
    OutOfOrderBlock {
        expected: u16,
        received: u16,
    },
    /// Server sent an ERROR packet. `code` is the RFC 1350 error
    /// code; the message (if any) is discarded to keep the state
    /// machine allocation-free.
    ServerError(u16),
    /// Caller declared `is_last = true` but state machine was
    /// already in `Complete`.
    DataAfterComplete,
}

// ──────────────────────────────────────────────────────────────────────────
// RxOutcome — output of [`parse_packet`]
// ──────────────────────────────────────────────────────────────────────────

/// Result of parsing a single incoming UDP datagram.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RxOutcome<'a> {
    /// A valid DATA packet. `is_last` is `true` iff `payload.len()
    /// < TFTP_BLOCK_SIZE` — the RFC 1350 EOF signal.
    Data {
        block: u16,
        payload: &'a [u8],
        is_last: bool,
    },
    /// A valid ERROR packet — the contained `code` is the RFC 1350
    /// error code.
    Error(u16),
    /// Packet is not a recognised TFTP server-to-client frame
    /// (unknown opcode, truncated, or otherwise malformed). The
    /// caller should drop it and keep listening — the I/O layer
    /// re-transmits the most recent ACK if needed.
    Malformed,
}

// ──────────────────────────────────────────────────────────────────────────
// Builders
// ──────────────────────────────────────────────────────────────────────────

/// Build a Read Request (RRQ) packet for `filename` in `octet`
/// mode into `out`. Returns the number of bytes written.
///
/// Wire format: `[opcode=1 u16 BE][filename][0]["octet"][0]`.
pub fn build_rrq(filename: &str, out: &mut [u8]) -> Result<usize, TftpEncodeError> {
    let bytes = filename.as_bytes();
    if bytes.is_empty() {
        return Err(TftpEncodeError::EmptyFilename);
    }
    if bytes.len() > TFTP_MAX_FILENAME_BYTES {
        return Err(TftpEncodeError::FilenameTooLong);
    }
    if bytes.contains(&0) {
        return Err(TftpEncodeError::FilenameHasNul);
    }
    let needed = bytes.len() + RRQ_FIXED_OVERHEAD_BYTES;
    if out.len() < needed {
        return Err(TftpEncodeError::BufferTooSmall);
    }
    out[0..2].copy_from_slice(&OP_RRQ.to_be_bytes());
    let mut cursor = 2;
    out[cursor..cursor + bytes.len()].copy_from_slice(bytes);
    cursor += bytes.len();
    out[cursor] = 0;
    cursor += 1;
    out[cursor..cursor + RRQ_MODE_BYTES.len()].copy_from_slice(RRQ_MODE_BYTES);
    cursor += RRQ_MODE_BYTES.len();
    out[cursor] = 0;
    cursor += 1;
    Ok(cursor)
}

/// Build an ACK packet for `block` into `out`. The caller must
/// supply at least [`TFTP_ACK_BYTES`] of space.
///
/// Wire format: `[opcode=4 u16 BE][block u16 BE]`.
pub fn build_ack(block: u16, out: &mut [u8]) {
    debug_assert!(out.len() >= TFTP_ACK_BYTES);
    out[0..2].copy_from_slice(&OP_ACK.to_be_bytes());
    out[2..4].copy_from_slice(&block.to_be_bytes());
}

// ──────────────────────────────────────────────────────────────────────────
// Parser
// ──────────────────────────────────────────────────────────────────────────

/// Parse a single TFTP server-to-client packet. Recognises DATA
/// and ERROR; everything else (including server-side RRQ/ACK and
/// any truncated frame) returns [`RxOutcome::Malformed`].
pub fn parse_packet(pkt: &[u8]) -> RxOutcome<'_> {
    if pkt.len() < 2 {
        return RxOutcome::Malformed;
    }
    let opcode = u16::from_be_bytes([pkt[0], pkt[1]]);
    match opcode {
        OP_DATA => parse_data(pkt),
        OP_ERROR => parse_error(pkt),
        _ => RxOutcome::Malformed,
    }
}

fn parse_data(pkt: &[u8]) -> RxOutcome<'_> {
    if pkt.len() < TFTP_DATA_HEADER_BYTES {
        return RxOutcome::Malformed;
    }
    let block = u16::from_be_bytes([pkt[2], pkt[3]]);
    let payload = &pkt[TFTP_DATA_HEADER_BYTES..];
    if payload.len() > TFTP_BLOCK_SIZE {
        // A peer sending oversized blocks violates the
        // un-negotiated 512-byte block size.
        return RxOutcome::Malformed;
    }
    let is_last = payload.len() < TFTP_BLOCK_SIZE;
    RxOutcome::Data {
        block,
        payload,
        is_last,
    }
}

fn parse_error(pkt: &[u8]) -> RxOutcome<'_> {
    if pkt.len() < TFTP_ERROR_MIN_BYTES {
        return RxOutcome::Malformed;
    }
    let code = u16::from_be_bytes([pkt[2], pkt[3]]);
    RxOutcome::Error(code)
}

// ──────────────────────────────────────────────────────────────────────────
// Client state machine
// ──────────────────────────────────────────────────────────────────────────

/// Action the caller should take on a [`TftpClient::on_data`] result.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClientAction {
    /// Block matched `expected_block`: caller should consume the
    /// payload AND send an ACK with this block number.
    AckAndConsume,
    /// Duplicate of the previously-acked block (`expected_block -
    /// 1`): caller should re-ACK without re-consuming. RFC 1350
    /// §6 — the canonical "Sorcerer's Apprentice" guard.
    AckIgnore,
    /// `is_last` was set on this DATA — caller should consume
    /// (already happens via `AckAndConsume` if not a duplicate),
    /// send the final ACK, and treat the transfer as finished.
    Complete,
    /// Block number jumped past `expected_block`. Caller decides
    /// whether to abort or wait for retransmit (this minimal
    /// state machine does not buffer out-of-order blocks).
    OutOfOrder {
        expected: u16,
        received: u16,
    },
}

/// Minimal client state for one RRQ transfer.
#[derive(Clone, Copy, Debug)]
pub struct TftpClient {
    /// Block number we expect to receive next. Starts at 1 per
    /// RFC 1350 §4; wraps to 0 after 65535 for very long files
    /// (RFC 7440 OACK negotiates a wider field but we do not).
    expected_block: u16,
    /// `true` after a DATA with `is_last == true` has been
    /// accepted; further DATA on this client is an error.
    complete: bool,
}

impl TftpClient {
    /// Construct a fresh client. The first valid DATA block will
    /// be number `1`.
    pub const fn new() -> Self {
        Self {
            expected_block: 1,
            complete: false,
        }
    }

    /// Returns the block number this client expects next, or the
    /// last block consumed if the transfer is complete.
    pub const fn expected_block(&self) -> u16 {
        self.expected_block
    }

    /// `true` after the terminating short block has been consumed.
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// Feed in the block number + `is_last` of an incoming DATA
    /// packet (as returned by [`parse_packet`]). Returns the
    /// caller action; on `AckAndConsume` / `Complete` the client
    /// internal expected_block advances.
    pub fn on_data(&mut self, block: u16, is_last: bool) -> ClientAction {
        if self.complete {
            // We've already accepted the final block — treat any
            // further DATA as a duplicate-final retransmit and
            // re-ACK the last block we consumed.
            return ClientAction::AckIgnore;
        }
        if block == self.expected_block {
            // Advance. u16 wraps naturally; that matches what
            // common TFTP servers do for files > 32 MiB.
            self.expected_block = self.expected_block.wrapping_add(1);
            if is_last {
                self.complete = true;
                ClientAction::Complete
            } else {
                ClientAction::AckAndConsume
            }
        } else if block == self.expected_block.wrapping_sub(1) {
            // Duplicate of the previously-acked block — re-ACK.
            ClientAction::AckIgnore
        } else {
            ClientAction::OutOfOrder {
                expected: self.expected_block,
                received: block,
            }
        }
    }
}

impl Default for TftpClient {
    fn default() -> Self {
        Self::new()
    }
}
