//! VirtIO Block Device Driver.
//!
//! Direct port of kernel/drivers/virtio_blk.c + kernel/include/virtio_blk.h.
//! Scans MMIO addresses for a block device, initializes it, and provides
//! sector-level read/write via the VirtIO request protocol.

use super::{
    VirtioDev, Virtq,
    VIRTIO_DEV_BLOCK, VIRTIO_STATUS_DRIVER_OK, VIRTIO_MMIO_STATUS,
    VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE,
};
use super::{mmio_read, mmio_write, probe, init as virtio_init, virtq_init,
            virtq_alloc_desc, virtq_free_desc, virtq_submit, virtq_poll,
            read_config64};
use crate::kprintln;
use robot_os_sync::SpinLock;

// ---- Constants (from virtio_blk.h) ----

const VIRTIO_MMIO_BASE:  usize = 0x1000_1000; // QEMU virt first VirtIO slot
const VIRTIO_MMIO_STEP:  usize = 0x1000;      // each slot is 0x1000 apart
const VIRTIO_MMIO_COUNT: usize = 8;           // scan up to 8 slots

pub const SECTOR_SIZE: usize = 512;

// Request types
const BLK_T_IN:  u32 = 0; // read from device
const BLK_T_OUT: u32 = 1; // write to device

// Status codes
const BLK_S_OK: u8 = 0;

// ---- Request structures (repr(C, packed) to match device ABI) ----

#[repr(C, packed)]
struct BlkReqHdr {
    req_type: u32,
    reserved: u32,
    sector:   u64,
}

// ---- Global device state ----

struct BlkDev {
    vdev:     VirtioDev,
    vq:       Virtq,
    capacity: u64,   // sectors
    readonly: bool,
    ready:    bool,  // init() completed; queue is usable
    failed:   bool,  // latched dead after a timeout — never cleared
}

static mut BLK_DEV: BlkDev = BlkDev {
    vdev:     VirtioDev::zeroed(),
    vq:       Virtq::zeroed(),
    capacity: 0,
    readonly: false,
    ready:    false,
    failed:   false,
};

/// Serializes every path that touches `BLK_DEV`, the virtqueue, or the DMA
/// staging statics below. The driver is a strictly one-request-at-a-time
/// protocol over shared `static mut` state; without this lock two harts
/// calling `read`/`write` concurrently interleave stagings into the single
/// `BLK_DMA_BUF`, corrupt the free-descriptor accounting, and race the
/// header/status statics mid-DMA. (The syscall layer serializes per-CPU
/// only, which is not enough on SMP.)
///
/// Held across the whole multi-chunk transfer, including the bounded
/// busy-wait for completion — the same discipline as the C kernel, just made
/// explicit.
static BLK_LOCK: SpinLock<()> = SpinLock::new(());

// ---- DMA staging area ----
//
// The device is NEVER handed a pointer into caller memory. A VirtIO request
// cannot be cancelled or recalled: once the chain is in the avail ring the
// device owns those buffers until it posts a used-ring entry, and if we give
// up waiting (see the timeout path in `blk_rw`) it may still DMA into them
// arbitrarily later. A caller buffer is very often a stack frame that has been
// popped and reused by then, so a late write would silently corrupt unrelated
// state far from the call site.
//
// Every buffer in a request therefore lives in driver-owned `static` storage
// whose lifetime is the lifetime of the kernel. Transfers larger than the
// staging area are split into chunks by the public `read`/`write` wrappers.
const BLK_DMA_SECTORS: usize = 8;
const BLK_DMA_BYTES:   usize = BLK_DMA_SECTORS * SECTOR_SIZE; // 4 KiB

#[repr(C, align(512))]
struct DmaBuf([u8; BLK_DMA_BYTES]);

// Static request buffers (aligned, single-request protocol like the C kernel)
static mut BLK_REQ_HDR: BlkReqHdr = BlkReqHdr { req_type: 0, reserved: 0, sector: 0 };
static mut BLK_STATUS:  u8         = 0xFF;
static mut BLK_DMA_BUF: DmaBuf     = DmaBuf([0u8; BLK_DMA_BYTES]);

// ---- virtio_blk_init (port of virtio_blk_init in virtio_blk.c) ----

/// Initialize the VirtIO block device.
/// Scans MMIO slots 0x10001000–0x10008000 for a block device.
pub fn init() -> Result<(), ()> {
    let _guard = BLK_LOCK.lock();

    let dev = unsafe { &mut *(&raw mut BLK_DEV) };

    // After a timeout the driver is latched dead: the device misbehaved once
    // and three descriptors are quarantined. Re-running init() would re-arm
    // that same device (DRIVER_OK) while `failed` still blocks all I/O —
    // an inconsistent half-alive state. Refuse instead.
    if dev.failed {
        kprintln!("[VIRTIO-BLK] init refused: driver latched dead after timeout");
        return Err(());
    }
    // Idempotent: a second init would leak the ring pages (the PMM cannot
    // reclaim them) and reset a live device mid-flight.
    if dev.ready {
        return Ok(());
    }

    kprintln!("[VIRTIO-BLK] Scanning for block devices...");

    let mut found = false;

    for i in 0..VIRTIO_MMIO_COUNT {
        let addr = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STEP;
        unsafe {
            if probe(addr, &mut dev.vdev).is_ok() && dev.vdev.device_id == VIRTIO_DEV_BLOCK {
                kprintln!("[VIRTIO-BLK] Found block device at {:#x}", addr);
                found = true;
                break;
            }
        }
    }

    if !found {
        kprintln!("[VIRTIO-BLK] No block device found");
        return Err(());
    }

    unsafe {
        // Initialize device (feature negotiation, status handshake)
        virtio_init(&mut dev.vdev).map_err(|_| {
            kprintln!("[VIRTIO-BLK] Failed to initialize VirtIO device");
        })?;

        // Read capacity from device config (offset 0 = uint64 capacity in sectors)
        dev.capacity = read_config64(&dev.vdev, 0);
        dev.readonly = false; // TODO: check VIRTIO_BLK_F_RO feature bit

        // `capacity` is device-supplied and untrusted: a bogus value near
        // u64::MAX would overflow the byte conversion, and overflow-checks are
        // on in release (panic == board reset). Saturate instead.
        kprintln!("[VIRTIO-BLK] Disk: {} sectors ({} MB)",
            dev.capacity,
            dev.capacity.saturating_mul(SECTOR_SIZE as u64) / (1024 * 1024));

        // Initialize the request queue (queue 0)
        virtq_init(&mut dev.vdev, 0, &mut dev.vq).map_err(|_| {
            kprintln!("[VIRTIO-BLK] Failed to initialize queue");
        })?;

        // Mark device as DRIVER_OK
        let s = mmio_read(dev.vdev.base, VIRTIO_MMIO_STATUS);
        mmio_write(dev.vdev.base, VIRTIO_MMIO_STATUS, s | VIRTIO_STATUS_DRIVER_OK);

        dev.ready = true;
    }

    kprintln!("[VIRTIO-BLK] Block device ready");
    Ok(())
}

// ---- virtio_blk_rw (port of virtio_blk_rw in virtio_blk.c) ----

/// Internal read/write of one chunk — submits a 3-descriptor chain
/// (header | data | status). The data descriptor always points at the
/// driver-owned staging buffer `BLK_DMA_BUF`, never at caller memory;
/// `read`/`write` copy in and out around this call.
///
/// `count` must be in `1..=BLK_DMA_SECTORS`; anything else is rejected.
///
/// Caller must hold `BLK_LOCK`.
unsafe fn blk_rw(sector: u64, count: u32, write: bool) -> Result<(), ()> {
    let dev = unsafe { &mut *(&raw mut BLK_DEV) };

    // A previous request timed out: the device was reset and may still have
    // been mid-DMA into the statics above. Submitting anything else would
    // reuse buffers the device might still be writing, so the driver stays
    // dead until reboot. See the timeout path below.
    if dev.failed || !dev.ready { return Err(()); }

    let vq = &mut dev.vq;

    // Queue must be live: a null table would be a null deref below, and
    // `virtq_submit` divides by `vq.num`.
    if vq.desc.is_null() || vq.num == 0 { return Err(()); }

    if count == 0 || count as usize > BLK_DMA_SECTORS { return Err(()); }

    // `sector + count` can overflow with a hostile LBA; overflow-checks are on
    // in release, so a plain add is a panic (== board reset). Check it.
    let end = sector.checked_add(count as u64).ok_or(())?;
    if end > dev.capacity {
        kprintln!("[VIRTIO-BLK] Error: sector out of range");
        return Err(());
    }
    if write && dev.readonly {
        kprintln!("[VIRTIO-BLK] Error: disk is read-only");
        return Err(());
    }

    // The driver is idle here (strictly one request at a time, BLK_LOCK held
    // by our caller), so the used ring must be empty. An entry now can only
    // be a duplicate or forged completion — a device not following the
    // protocol. Left in the ring, it would make the NEXT request appear
    // complete the instant it was submitted, before any DMA happened, and
    // the caller would consume stale staging-buffer contents as disk data.
    // Same remedy as a timeout: reset, latch dead. (No descriptors are
    // allocated yet, so there is nothing to quarantine.)
    if virtq_poll(vq).is_some() {
        mmio_write(dev.vdev.base, VIRTIO_MMIO_STATUS, 0);
        dev.failed = true;
        dev.ready  = false;
        kprintln!("[VIRTIO-BLK] Error: spurious completion while idle - device reset, block I/O disabled");
        return Err(());
    }

    // Prepare request header
    BLK_REQ_HDR.req_type = if write { BLK_T_OUT } else { BLK_T_IN };
    BLK_REQ_HDR.reserved = 0;
    BLK_REQ_HDR.sector   = sector;
    BLK_STATUS           = 0xFF;

    // Allocate 3 descriptors: [header] → [data] → [status]
    let d_hdr    = virtq_alloc_desc(vq).ok_or(())?;
    let d_data   = virtq_alloc_desc(vq).ok_or_else(|| { virtq_free_desc(vq, d_hdr); })?;
    let d_status = virtq_alloc_desc(vq).ok_or_else(|| {
        virtq_free_desc(vq, d_hdr);
        virtq_free_desc(vq, d_data);
    })?;

    // count <= BLK_DMA_SECTORS was checked above, so this cannot overflow and
    // cannot exceed BLK_DMA_BYTES — the staging buffer is always large enough.
    let data_len = (count as usize).checked_mul(SECTOR_SIZE).ok_or(())?;

    // Descriptor 0: request header (device reads)
    let p = vq.desc.add(d_hdr);
    (*p).addr  = &raw const BLK_REQ_HDR as u64;
    (*p).len   = core::mem::size_of::<BlkReqHdr>() as u32;
    (*p).flags = VIRTQ_DESC_F_NEXT;
    (*p).next  = d_data as u16;

    // Descriptor 1: data buffer — always the driver-owned staging area, so a
    // late DMA after a timeout can only land in memory this driver owns.
    let p = vq.desc.add(d_data);
    (*p).addr  = (&raw const BLK_DMA_BUF) as u64;
    (*p).len   = data_len as u32;
    (*p).flags = VIRTQ_DESC_F_NEXT | if !write { VIRTQ_DESC_F_WRITE } else { 0 };
    (*p).next  = d_status as u16;

    // Descriptor 2: status byte (device writes)
    let p = vq.desc.add(d_status);
    (*p).addr  = &raw const BLK_STATUS as u64;
    (*p).len   = 1;
    (*p).flags = VIRTQ_DESC_F_WRITE;
    (*p).next  = 0;

    // Submit and busy-wait for completion (same as C kernel)
    virtq_submit(&dev.vdev, 0, d_hdr, vq);

    let mut timeout = 1_000_000i32;
    let fail_reason: Option<&str> = loop {
        match virtq_poll(vq) {
            // Only the completion for OUR chain counts. The device reports
            // which chain completed via the used-ring `id`; a value other
            // than `d_hdr` is a completion we never submitted. Accepting it
            // would report success on a request whose DMA has not happened
            // (and whose buffers the device may still write later), so it is
            // treated exactly like a timeout below.
            Some(id) if id == d_hdr => break None,
            Some(_) => break Some("foreign completion id"),
            None => {
                timeout -= 1;
                if timeout <= 0 { break Some("request timeout"); }
            }
        }
    };
    if let Some(reason) = fail_reason {
            // The request is still owned by the device. VirtIO has no cancel:
            // the chain stays in the ring and the device may post it — and DMA
            // into the header, the staging buffer and the status byte — at any
            // point in the future. Three properties make that late write inert,
            // and all three are enforced rather than assumed:
            //
            // 1. Every buffer in the chain is driver-owned `static` storage
            //    (BLK_REQ_HDR / BLK_DMA_BUF / BLK_STATUS), never caller memory.
            //    A late DMA cannot reach a popped stack frame.
            // 2. The descriptors are deliberately NOT returned to the free
            //    list. Recycling them would let the stale completion land on a
            //    live chain, and would leave `last_used_idx` desynchronised so
            //    the next request would mistake the stale used-ring entry for
            //    its own and report success on an untouched buffer. They stay
            //    quarantined for the lifetime of the kernel.
            // 3. The device is reset and the driver latched dead, so nothing
            //    ever submits again or reads those statics again.
            //
            // Reset first (stops the device touching guest memory per spec),
            // then latch. The cost is that one timeout disables block I/O
            // until reboot: recovering the queue would mean re-running init()
            // and re-allocating the ring pages, which the PMM cannot reclaim,
            // and re-arming a device that has already misbehaved. On a
            // safety-critical target, no disk beats silently wrong disk.
            mmio_write(dev.vdev.base, VIRTIO_MMIO_STATUS, 0);
            dev.failed = true;
            dev.ready  = false;
            kprintln!("[VIRTIO-BLK] Error: {} - device reset, block I/O disabled", reason);
            return Err(());
    }

    virtq_free_desc(vq, d_hdr);
    virtq_free_desc(vq, d_data);
    virtq_free_desc(vq, d_status);

    // The status byte was written by the device via DMA; read it volatile so
    // the 0xFF sentinel store above cannot be constant-propagated into this
    // comparison. (Ordering is provided by the acquire fence in `virtq_poll`,
    // which sits between observing the used index and this load.)
    let status = core::ptr::read_volatile(&raw const BLK_STATUS);
    if status != BLK_S_OK {
        kprintln!("[VIRTIO-BLK] Error: status {}", status);
        return Err(());
    }

    Ok(())
}

// ---- Public API ----

/// True while the driver may touch the staging buffer and submit requests.
/// False before init() and forever after a timeout, when the device may still
/// own `BLK_DMA_BUF`. Checked before staging, not just before submitting.
///
/// Must be called with `BLK_LOCK` held.
fn usable() -> bool {
    let dev = unsafe { &*(&raw const BLK_DEV) };
    dev.ready && !dev.failed
}

/// Read `count` sectors starting at `sector` into `buf`.
///
/// Transfers via the driver's staging buffer in chunks of at most
/// `BLK_DMA_SECTORS`; `buf` is never exposed to the device. Returns `Err` on a
/// short buffer or a bad count instead of panicking (release builds abort on
/// panic, which on this target is a board reset).
pub fn read(sector: u64, count: u32, buf: &mut [u8]) -> Result<(), ()> {
    // Held for the whole multi-chunk transfer: the staging buffer, the
    // request statics and the virtqueue are all shared mutable state.
    let _guard = BLK_LOCK.lock();
    if !usable() { return Err(()); }
    let total = (count as usize).checked_mul(SECTOR_SIZE).ok_or(())?;
    if total == 0 || buf.len() < total { return Err(()); }

    let mut done: u32 = 0;
    while done < count {
        let chunk = core::cmp::min((count - done) as usize, BLK_DMA_SECTORS);
        let bytes = chunk.checked_mul(SECTOR_SIZE).ok_or(())?;
        let off   = (done as usize).checked_mul(SECTOR_SIZE).ok_or(())?;
        let end   = off.checked_add(bytes).ok_or(())?;
        let lba   = sector.checked_add(done as u64).ok_or(())?;

        unsafe { blk_rw(lba, chunk as u32, false)? };

        // Staging -> caller. `get_mut` rather than an index so a later edit
        // cannot reintroduce a panicking slice.
        let dst = buf.get_mut(off..end).ok_or(())?;
        // SAFETY: the device has posted the used-ring entry (virtq_poll
        // succeeded, which fences Acquire), so it is done with the staging
        // buffer. `bytes <= BLK_DMA_BYTES` and `dst.len() == bytes`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                (&raw const BLK_DMA_BUF) as *const u8, dst.as_mut_ptr(), bytes);
        }

        done += chunk as u32;
    }
    Ok(())
}

/// Write `count` sectors starting at `sector` from `buf`.
///
/// Chunked like `read`. A failure part-way through a multi-chunk transfer
/// leaves the earlier chunks committed to the disk — same as any multi-sector
/// request that fails mid-flight; callers must not assume atomicity.
pub fn write(sector: u64, count: u32, buf: &[u8]) -> Result<(), ()> {
    // Held for the whole multi-chunk transfer — see `read`.
    let _guard = BLK_LOCK.lock();
    if !usable() { return Err(()); }
    let total = (count as usize).checked_mul(SECTOR_SIZE).ok_or(())?;
    if total == 0 || buf.len() < total { return Err(()); }

    let mut done: u32 = 0;
    while done < count {
        let chunk = core::cmp::min((count - done) as usize, BLK_DMA_SECTORS);
        let bytes = chunk.checked_mul(SECTOR_SIZE).ok_or(())?;
        let off   = (done as usize).checked_mul(SECTOR_SIZE).ok_or(())?;
        let end   = off.checked_add(bytes).ok_or(())?;
        let lba   = sector.checked_add(done as u64).ok_or(())?;

        let src = buf.get(off..end).ok_or(())?;
        // SAFETY: no request is in flight (BLK_LOCK serializes callers, the
        // driver is strictly one request at a time and latches dead on
        // timeout), so the device does not own the staging buffer here.
        // `bytes <= BLK_DMA_BYTES`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                src.as_ptr(), (&raw mut BLK_DMA_BUF) as *mut u8, bytes);
        }

        unsafe { blk_rw(lba, chunk as u32, true)? };

        done += chunk as u32;
    }
    Ok(())
}

/// Disk capacity in bytes. Saturates: `capacity` is device-reported.
///
/// Deliberately lock-free: an aligned u64 load is single-copy atomic on
/// RV64, and taking `BLK_LOCK` here would block behind a full transfer's
/// busy-wait just to read a number. Worst case during init is reading 0.
pub fn capacity_bytes() -> u64 {
    unsafe { BLK_DEV.capacity.saturating_mul(SECTOR_SIZE as u64) }
}

/// Disk capacity in sectors. Lock-free — see `capacity_bytes`.
pub fn capacity_sectors() -> u64 {
    unsafe { BLK_DEV.capacity }
}
