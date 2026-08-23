//! Minimal SCSI command set for the USB MSC gadget.
//!
//! Decodes the 7 commands a standard PC / Mac / Linux OS issues
//! when mounting a USB drive, and dispatches to a [`BlockDevice`]
//! impl that abstracts the backing store. Sufficient to make the
//! board appear as a read/write USB stick to the host operating
//! system without going near a real on-disk filesystem driver.
//!
//! Reference: SBC-3 (block commands) + SPC-4 (primary cmd set).

/// 6/10-byte SCSI opcodes we implement.
pub const SCSI_OP_TEST_UNIT_READY:   u8 = 0x00;
pub const SCSI_OP_REQUEST_SENSE:     u8 = 0x03;
pub const SCSI_OP_INQUIRY:           u8 = 0x12;
pub const SCSI_OP_MODE_SENSE_6:      u8 = 0x1A;
pub const SCSI_OP_READ_CAPACITY_10:  u8 = 0x25;
pub const SCSI_OP_READ_10:           u8 = 0x28;
pub const SCSI_OP_WRITE_10:          u8 = 0x2A;

/// 512-byte block; the SBC standard size.
pub const BLOCK_SIZE: usize = 512;

/// Inquiry vendor / product / revision strings — fixed 8/16/4
/// padded ASCII per SPC-4. Tweak per deployment if needed.
pub const INQUIRY_VENDOR_ID:   &[u8; 8]  = b"PHANES  ";
pub const INQUIRY_PRODUCT_ID:  &[u8; 16] = b"Robot OS Recover";
pub const INQUIRY_REVISION:    &[u8; 4]  = b"0001";

/// Block backing-store trait.  The kernel impl wraps the FAT32
/// SD-card driver; host tests use an in-memory `Vec`.
pub trait BlockDevice {
    /// Total number of 512-byte blocks. Used by READ_CAPACITY.
    fn block_count(&self) -> u32;

    /// Read one block into `out` (which must be ≥ 512 bytes).
    fn read_block(&self, lba: u32, out: &mut [u8]) -> Result<(), ()>;

    /// Write one block from `data` (which must be ≥ 512 bytes).
    fn write_block(&mut self, lba: u32, data: &[u8]) -> Result<(), ()>;
}

/// Decoded SCSI command — the SCSI handler matches on this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScsiCommand {
    TestUnitReady,
    RequestSense   { allocation_length: u8 },
    Inquiry        { allocation_length: u8 },
    ModeSense6     { allocation_length: u8 },
    ReadCapacity10,
    Read10  { lba: u32, blocks: u16 },
    Write10 { lba: u32, blocks: u16 },
}

/// Parse the CDB into a typed command.  Returns `None` on
/// unknown opcode — the caller stalls the bulk endpoint and
/// reports `STATUS_FAIL` in the CSW.
pub fn parse_scsi_command(cdb: &[u8]) -> Option<ScsiCommand> {
    if cdb.is_empty() {
        return None;
    }
    Some(match cdb[0] {
        SCSI_OP_TEST_UNIT_READY => ScsiCommand::TestUnitReady,
        SCSI_OP_REQUEST_SENSE if cdb.len() >= 6 =>
            ScsiCommand::RequestSense { allocation_length: cdb[4] },
        SCSI_OP_INQUIRY if cdb.len() >= 6 =>
            ScsiCommand::Inquiry { allocation_length: cdb[4] },
        SCSI_OP_MODE_SENSE_6 if cdb.len() >= 6 =>
            ScsiCommand::ModeSense6 { allocation_length: cdb[4] },
        SCSI_OP_READ_CAPACITY_10 if cdb.len() >= 10 =>
            ScsiCommand::ReadCapacity10,
        SCSI_OP_READ_10 if cdb.len() >= 10 => ScsiCommand::Read10 {
            lba:    u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]),
            blocks: u16::from_be_bytes([cdb[7], cdb[8]]),
        },
        SCSI_OP_WRITE_10 if cdb.len() >= 10 => ScsiCommand::Write10 {
            lba:    u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]),
            blocks: u16::from_be_bytes([cdb[7], cdb[8]]),
        },
        _ => return None,
    })
}

/// Result of executing a SCSI command — what to do with the bulk
/// endpoints next. `data_in` carries the device→host bytes for
/// IN commands (INQUIRY, READ_CAPACITY, etc); `expected_data_out`
/// is the number of host→device bytes the WRITE will receive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScsiResponse {
    /// Command done immediately. CSW reports `OK`. `data_in`
    /// slice (if any) goes out on the bulk-IN endpoint before
    /// the CSW.
    Done { data_in_len: usize },
    /// Caller should drain `expected_data_out` bytes from
    /// bulk-OUT into the backing store via `write_block` calls.
    WriteData { expected_data_out: u32 },
    /// Bulk-IN read continues with `expected_data_in` more
    /// bytes (handled by the caller's READ loop).
    ReadData  { expected_data_in: u32 },
    /// Command failed — CSW reports `FAIL`; host will issue
    /// REQUEST_SENSE next.
    Failed,
}

/// Execute a parsed SCSI command. Writes any immediate IN
/// response into `data_in_buf` (returns the number of bytes
/// produced in `Done.data_in_len`).
pub fn execute_scsi(
    cmd: ScsiCommand,
    blk: &dyn BlockDevice,
    data_in_buf: &mut [u8],
) -> ScsiResponse {
    match cmd {
        ScsiCommand::TestUnitReady => ScsiResponse::Done { data_in_len: 0 },

        ScsiCommand::RequestSense { allocation_length } => {
            // 18-byte fixed sense format, NO SENSE.
            let need = (allocation_length as usize).min(18);
            if data_in_buf.len() < need {
                return ScsiResponse::Failed;
            }
            for b in &mut data_in_buf[..need] {
                *b = 0;
            }
            // Guard each field write: a host may request fewer than 8 bytes,
            // and `need - 8` would underflow (usize) → abort. Mirrors Inquiry.
            if need >= 1 { data_in_buf[0] = 0x70; }          // response code
            if need >= 3 { data_in_buf[2] = 0x00; }          // sense key = NO SENSE
            if need >= 8 { data_in_buf[7] = (need - 8) as u8; } // additional length
            ScsiResponse::Done { data_in_len: need }
        }

        ScsiCommand::Inquiry { allocation_length } => {
            // 36-byte standard inquiry response.
            let need = (allocation_length as usize).min(36);
            if data_in_buf.len() < need {
                return ScsiResponse::Failed;
            }
            for b in &mut data_in_buf[..need] {
                *b = 0;
            }
            // Guard each field write against `need`, exactly as RequestSense
            // above does. `need` is `min(allocation_length, 36)` and
            // `allocation_length` is byte 4 of a host-supplied CDB — the host
            // is free to send 0. The `data_in_buf.len() < need` check above
            // does NOT cover these five stores: with `allocation_length = 0`
            // it reduces to `len < 0`, which passes for every buffer
            // including an empty one, and then `data_in_buf[0] = 0x00` is an
            // out-of-bounds index. With `panic = "abort"` that is not a bad
            // INQUIRY response, it is a board reset — a physical-safety event
            // on a robot, reachable from one USB control transfer.
            //
            // Not reachable through `dispatch_cbw` today (it hands in a
            // 64-byte scratch buffer, and the §6.7 check now rejects the
            // mismatched CBW anyway), but `execute_scsi` is `pub` and takes
            // the buffer from its caller, so the guard belongs with the
            // stores rather than with any one call site.
            //
            // The `>= 16 / >= 32 / >= 36` thresholds below are left as they
            // are: they are wire-format decisions about when to include the
            // vendor / product / revision strings, not the missing
            // bounds check.
            if need >= 1 { data_in_buf[0] = 0x00; } // peripheral type = direct access
            if need >= 2 { data_in_buf[1] = 0x80; } // RMB=1 (removable)
            if need >= 3 { data_in_buf[2] = 0x06; } // version = SPC-4
            if need >= 4 { data_in_buf[3] = 0x02; } // response data format
            if need >= 5 { data_in_buf[4] = 0x1F; } // additional length = 31
            if need >= 16 {
                let n = (need - 8).min(8);
                data_in_buf[8..8 + n].copy_from_slice(&INQUIRY_VENDOR_ID[..n]);
            }
            if need >= 32 {
                let n = (need - 16).min(16);
                data_in_buf[16..16 + n].copy_from_slice(&INQUIRY_PRODUCT_ID[..n]);
            }
            if need >= 36 {
                data_in_buf[32..36].copy_from_slice(INQUIRY_REVISION);
            }
            ScsiResponse::Done { data_in_len: need }
        }

        ScsiCommand::ModeSense6 { allocation_length } => {
            // 4-byte mode parameter header: length=3, medium type=0,
            // device-specific=0, block-descriptor length=0.
            let need = (allocation_length as usize).min(4);
            if data_in_buf.len() < need {
                return ScsiResponse::Failed;
            }
            for b in &mut data_in_buf[..need] {
                *b = 0;
            }
            if need >= 1 {
                data_in_buf[0] = 3;
            }
            ScsiResponse::Done { data_in_len: need }
        }

        ScsiCommand::ReadCapacity10 => {
            // 8 bytes: last LBA (big-endian u32) + block size.
            if data_in_buf.len() < 8 {
                return ScsiResponse::Failed;
            }
            let last_lba = blk.block_count().saturating_sub(1);
            data_in_buf[0..4].copy_from_slice(&last_lba.to_be_bytes());
            data_in_buf[4..8].copy_from_slice(&(BLOCK_SIZE as u32).to_be_bytes());
            ScsiResponse::Done { data_in_len: 8 }
        }

        ScsiCommand::Read10  { blocks, .. } => ScsiResponse::ReadData {
            expected_data_in: (blocks as u32) * (BLOCK_SIZE as u32),
        },

        ScsiCommand::Write10 { blocks, .. } => ScsiResponse::WriteData {
            expected_data_out: (blocks as u32) * (BLOCK_SIZE as u32),
        },
    }
}
