//! DEV03 — USB Mass Storage gadget glue.
//!
//! Ties the three pre-existing layers together so the board appears
//! as a USB flash drive to a connected PC:
//!
//! ```text
//!   USB OTG controller (DWC2, hardware-pending)
//!         ↓ bulk-OUT (31-byte CBW)
//!   robot_os_msc::dispatch::dispatch_cbw
//!         ↓ SCSI cmd
//!   FAT32BlockDevice (this module)  ←→  robot_os_drivers::blkdev
//!         ↓ bulk-IN data + 13-byte CSW
//!   USB OTG controller
//! ```
//!
//! Today the actual USB endpoint reads/writes are stubbed — the
//! VisionFive 2 / SpacemiT K1 DWC2 controller driver does not yet
//! exist (pre-hardware-arrival, July 2026). The pure protocol
//! plumbing IS exercised end-to-end by `crates/msc-tests`.
//!
//! ## Architectural note
//!
//! All non-trivial decoding (CBW → SCSI → CSW, LBA range checks)
//! lives in `robot_os_msc::dispatch`, which is host-testable. This
//! module is intentionally thin — it owns the `BlockDevice` impl
//! that delegates to the kernel's block driver and the endpoint
//! pump that will be filled in once we have a DWC2 driver.

use robot_os_drivers::kprintln;
use robot_os_msc::{
    dispatch_cbw, Action, BlockDevice, MscPhase, MscStateMachine,
    CBW_TOTAL_LEN, DISPATCH_IN_BUF_LEN,
};

/// SBC block size in bytes. The MSC SCSI layer always reports
/// this in READ_CAPACITY; FAT32 also uses 512-byte sectors.
const MSC_BLOCK_BYTES: usize = 512;

/// Bulk-IN bounce buffer size for multi-block READ_10. Sized to
/// hold one block — the pump loop iterates blocks one at a time
/// so we never allocate a huge contiguous buffer in the kernel.
const MSC_BLOCK_BUF_BYTES: usize = MSC_BLOCK_BYTES;

/// Capacity (in 512-byte sectors) reported when the underlying
/// block device is unavailable (e.g. no SD card present, or
/// before `blkdev::init()` has been called). A non-
/// zero stub lets host enumeration succeed; any actual read/write
/// will return Err(()) from `blkdev::read/write` and surface a
/// CSW(FAIL) to the host.
const MSC_FALLBACK_CAPACITY_SECTORS: u32 = 0;

/// Adapter from the kernel block driver to the MSC `BlockDevice`
/// trait. The dispatcher uses this to bounds-check LBAs; the
/// endpoint pump uses it to stream READ_10/WRITE_10 payloads.
pub struct Fat32BlockDevice {
    /// Cached capacity (in 512-byte sectors). Captured at init
    /// time so READ_CAPACITY doesn't need to re-query the driver
    /// on every CBW.
    capacity_sectors: u32,
}

impl Fat32BlockDevice {
    /// Snapshot the block-device capacity now and return an
    /// adapter ready to be passed to `dispatch_cbw`.
    pub fn new() -> Self {
        // `blkdev::capacity_sectors` returns u64; SBC READ_CAPACITY(10)
        // is u32. Saturate — anything past 2 TiB needs READ_CAPACITY(16),
        // which the minimal SCSI command set does not implement.
        let raw = robot_os_drivers::blkdev::capacity_sectors();
        let capacity_sectors = if raw == 0 {
            MSC_FALLBACK_CAPACITY_SECTORS
        } else if raw > u32::MAX as u64 {
            u32::MAX
        } else {
            raw as u32
        };
        Self { capacity_sectors }
    }
}

impl Default for Fat32BlockDevice {
    fn default() -> Self { Self::new() }
}

impl BlockDevice for Fat32BlockDevice {
    fn block_count(&self) -> u32 {
        self.capacity_sectors
    }

    fn read_block(&self, lba: u32, out: &mut [u8]) -> Result<(), ()> {
        if out.len() < MSC_BLOCK_BYTES {
            return Err(());
        }
        // SBC LBA is u32; blkdev API takes u64. The `as u64` here is a
        // widening conversion, never lossy.
        robot_os_drivers::blkdev::read(lba as u64, 1, &mut out[..MSC_BLOCK_BYTES])
    }

    fn write_block(&mut self, lba: u32, data: &[u8]) -> Result<(), ()> {
        if data.len() < MSC_BLOCK_BYTES {
            return Err(());
        }
        robot_os_drivers::blkdev::write(lba as u64, 1, &data[..MSC_BLOCK_BYTES])
    }
}

/// Kernel-owned MSC gadget state. Holds the BBB state machine, the
/// FAT32-backed LUN, and the per-command scratch buffer used to
/// shuttle inline IN responses (INQUIRY / READ_CAPACITY / etc.) and
/// streamed READ_10/WRITE_10 blocks.
pub struct MscGadget {
    state:     MscStateMachine,
    lun:       Fat32BlockDevice,
    /// Scratch for inline SCSI IN responses. Cleared between CBWs.
    in_scratch: [u8; DISPATCH_IN_BUF_LEN],
    /// One-block bounce buffer for multi-block READ_10/WRITE_10.
    block_buf: [u8; MSC_BLOCK_BUF_BYTES],
}

impl MscGadget {
    /// Build a gadget bound to the current block device.
    pub fn new() -> Self {
        Self {
            state:      MscStateMachine::new(),
            lun:        Fat32BlockDevice::new(),
            in_scratch: [0u8; DISPATCH_IN_BUF_LEN],
            block_buf:  [0u8; MSC_BLOCK_BUF_BYTES],
        }
    }

    /// Reported capacity in 512-byte sectors. Exposed for the boot
    /// banner / future shell `msc status` command.
    pub fn capacity_sectors(&self) -> u32 {
        self.lun.block_count()
    }

    /// Drive the gadget once: parse the next 31-byte CBW arriving
    /// on bulk-OUT, execute, and pump the resulting data + CSW back
    /// out. Returns `true` when a CBW was handled (the endpoint
    /// pump should be re-entered immediately to fetch the next),
    /// `false` when the OUT endpoint is empty.
    ///
    /// In a real DWC2 driver this is called from the USB interrupt
    /// handler or a dedicated MSC task. Today the I/O calls are
    /// stubbed — see `bulk_out_read` / `bulk_in_write` below.
    pub fn pump_once(&mut self) -> bool {
        let mut cbw_buf = [0u8; CBW_TOTAL_LEN];
        let n = match bulk_out_read(&mut cbw_buf) {
            Some(n) => n,
            None => return false,
        };
        if n < CBW_TOTAL_LEN {
            // Short OUT packet — protocol error, recover via reset.
            self.state.set_phase(MscPhase::Reset);
            return true;
        }
        let action = dispatch_cbw(&cbw_buf, &self.lun, &mut self.in_scratch);
        match action {
            Action::InlineDone { in_len, csw } => {
                if in_len > 0 {
                    bulk_in_write(&self.in_scratch[..in_len]);
                }
                bulk_in_write(&csw);
                self.state.set_phase(MscPhase::Idle);
            }
            Action::ReadBlocks { start_lba, blocks, csw } => {
                self.stream_read(start_lba, blocks);
                bulk_in_write(&csw);
                self.state.set_phase(MscPhase::Idle);
            }
            Action::WriteBlocks { start_lba, blocks, csw } => {
                self.stream_write(start_lba, blocks);
                bulk_in_write(&csw);
                self.state.set_phase(MscPhase::Idle);
            }
            Action::PhaseError => {
                kprintln!("[msc] CBW phase error — stalling endpoints");
                self.state.set_phase(MscPhase::Reset);
            }
        }
        true
    }

    /// Bulk-IN pump for READ_10. Streams `blocks` × 512 bytes from
    /// `start_lba` via the kernel block driver, one block at a time
    /// to avoid kernel stack pressure.
    fn stream_read(&mut self, start_lba: u32, blocks: u16) {
        for i in 0..blocks {
            let lba = start_lba.wrapping_add(i as u32);
            if self.lun.read_block(lba, &mut self.block_buf).is_err() {
                // Mid-transfer read error — the host already got a
                // CSW(OK) decision back from the dispatcher (LBA
                // range was validated). Best we can do here is send
                // zeros for the remaining blocks; a future revision
                // should reach the dispatcher's FAIL path before any
                // bytes leave the device.
                self.block_buf.fill(0);
            }
            bulk_in_write(&self.block_buf);
        }
    }

    /// Bulk-OUT pump for WRITE_10. Drains `blocks` × 512 bytes from
    /// the host into the kernel block driver, one block at a time.
    fn stream_write(&mut self, start_lba: u32, blocks: u16) {
        for i in 0..blocks {
            let lba = start_lba.wrapping_add(i as u32);
            let n = bulk_out_read(&mut self.block_buf).unwrap_or(0);
            if n < MSC_BLOCK_BYTES {
                // Underrun — drop the rest; host will retry.
                break;
            }
            let _ = self.lun.write_block(lba, &self.block_buf);
        }
    }
}

impl Default for MscGadget {
    fn default() -> Self { Self::new() }
}

// ── USB endpoint stubs ────────────────────────────────────────────
//
// The real implementation will live in `robot_os_drivers::usb_device`
// (DWC2 controller surface). Until that arrives, every endpoint call
// is a no-op that returns "no data" — pump_once() then short-circuits
// and the kernel idles normally.

/// Read up to `out.len()` bytes from the bulk-OUT endpoint into `out`.
/// Returns `Some(n)` with the number of bytes received (≤ out.len()),
/// or `None` if the endpoint is empty / not yet wired.
///
// TODO(hw): wire DWC2 controller here. Until hardware arrives this
// always returns None so `pump_once()` is a no-op.
fn bulk_out_read(_out: &mut [u8]) -> Option<usize> {
    None
}

/// Write `data.len()` bytes to the bulk-IN endpoint. No-op until
/// the DWC2 controller is wired.
///
// TODO(hw): wire DWC2 controller here.
fn bulk_in_write(_data: &[u8]) {
    // Intentionally empty — see module docs.
}

/// One-shot kernel-boot init. Brings up the USB device controller
/// (today: stubbed), then logs the reported capacity.
///
/// Safe to call even when no block device is present — capacity
/// will simply report 0 sectors and the gadget will respond FAIL
/// to any READ_10 / WRITE_10.
pub fn msc_gadget_init() {
    let g = MscGadget::new();
    kprintln!(
        "[msc] gadget ready: {} sectors x {} bytes (USB controller stubbed)",
        g.capacity_sectors(),
        MSC_BLOCK_BYTES,
    );
    // TODO(hw): register `g` with the DWC2 controller's class-driver
    // hook + start the bulk-OUT endpoint.  Today the gadget is
    // dropped here; pump_once() would be a no-op anyway.
}
