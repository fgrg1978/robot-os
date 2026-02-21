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
}

static mut BLK_DEV: BlkDev = BlkDev {
    vdev:     VirtioDev::zeroed(),
    vq:       Virtq::zeroed(),
    capacity: 0,
    readonly: false,
};

// Static request buffers (aligned, single-request protocol like the C kernel)
static mut BLK_REQ_HDR: BlkReqHdr = BlkReqHdr { req_type: 0, reserved: 0, sector: 0 };
static mut BLK_STATUS:  u8         = 0xFF;

// ---- virtio_blk_init (port of virtio_blk_init in virtio_blk.c) ----

/// Initialize the VirtIO block device.
/// Scans MMIO slots 0x10001000–0x10008000 for a block device.
pub fn init() -> Result<(), ()> {
    kprintln!("[VIRTIO-BLK] Scanning for block devices...");

    let dev = unsafe { &mut *(&raw mut BLK_DEV) };
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

        kprintln!("[VIRTIO-BLK] Disk: {} sectors ({} MB)",
            dev.capacity,
            dev.capacity * SECTOR_SIZE as u64 / (1024 * 1024));

        // Initialize the request queue (queue 0)
        virtq_init(&mut dev.vdev, 0, &mut dev.vq).map_err(|_| {
            kprintln!("[VIRTIO-BLK] Failed to initialize queue");
        })?;

        // Mark device as DRIVER_OK
        let s = mmio_read(dev.vdev.base, VIRTIO_MMIO_STATUS);
        mmio_write(dev.vdev.base, VIRTIO_MMIO_STATUS, s | VIRTIO_STATUS_DRIVER_OK);
    }

    kprintln!("[VIRTIO-BLK] Block device ready");
    Ok(())
}

// ---- virtio_blk_rw (port of virtio_blk_rw in virtio_blk.c) ----

/// Internal read/write — submits a 3-descriptor chain (header | data | status).
unsafe fn blk_rw(sector: u64, count: u32, buf: *mut u8, write: bool) -> Result<(), ()> {
    let dev = unsafe { &mut *(&raw mut BLK_DEV) };
    let vq  = &mut dev.vq;

    if count == 0 || buf.is_null() { return Err(()); }
    if sector + count as u64 > dev.capacity {
        kprintln!("[VIRTIO-BLK] Error: sector out of range");
        return Err(());
    }
    if write && dev.readonly {
        kprintln!("[VIRTIO-BLK] Error: disk is read-only");
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

    let data_len = count as usize * SECTOR_SIZE;

    // Descriptor 0: request header (device reads)
    let p = vq.desc.add(d_hdr);
    (*p).addr  = &raw const BLK_REQ_HDR as u64;
    (*p).len   = core::mem::size_of::<BlkReqHdr>() as u32;
    (*p).flags = VIRTQ_DESC_F_NEXT;
    (*p).next  = d_data as u16;

    // Descriptor 1: data buffer
    let p = vq.desc.add(d_data);
    (*p).addr  = buf as u64;
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
    loop {
        if virtq_poll(vq).is_some() { break; }
        timeout -= 1;
        if timeout <= 0 {
            kprintln!("[VIRTIO-BLK] Error: request timeout");
            virtq_free_desc(vq, d_hdr);
            virtq_free_desc(vq, d_data);
            virtq_free_desc(vq, d_status);
            return Err(());
        }
    }

    virtq_free_desc(vq, d_hdr);
    virtq_free_desc(vq, d_data);
    virtq_free_desc(vq, d_status);

    if BLK_STATUS != BLK_S_OK {
        let status = BLK_STATUS;
        kprintln!("[VIRTIO-BLK] Error: status {}", status);
        return Err(());
    }

    Ok(())
}

// ---- Public API ----

/// Read `count` sectors starting at `sector` into `buf`.
pub fn read(sector: u64, count: u32, buf: &mut [u8]) -> Result<(), ()> {
    assert!(buf.len() >= count as usize * SECTOR_SIZE);
    unsafe { blk_rw(sector, count, buf.as_mut_ptr(), false) }
}

/// Write `count` sectors starting at `sector` from `buf`.
pub fn write(sector: u64, count: u32, buf: &[u8]) -> Result<(), ()> {
    assert!(buf.len() >= count as usize * SECTOR_SIZE);
    unsafe { blk_rw(sector, count, buf.as_ptr() as *mut u8, true) }
}

/// Disk capacity in bytes.
pub fn capacity_bytes() -> u64 {
    unsafe { BLK_DEV.capacity * SECTOR_SIZE as u64 }
}

/// Disk capacity in sectors.
pub fn capacity_sectors() -> u64 {
    unsafe { BLK_DEV.capacity }
}
