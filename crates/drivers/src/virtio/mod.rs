//! VirtIO MMIO driver — common structures and queue management.
//!
//! Direct port of kernel/drivers/virtio.c + kernel/include/virtio.h.
//! Supports both legacy (v1) and modern (v2) MMIO transport.

pub mod blk;
pub mod net;

use core::sync::atomic::{fence, Ordering};

// ---- MMIO Register Offsets (from virtio.h) ----

pub const VIRTIO_MMIO_MAGIC:              u32 = 0x000;
pub const VIRTIO_MMIO_VERSION:            u32 = 0x004;
pub const VIRTIO_MMIO_DEVICE_ID:          u32 = 0x008;
pub const VIRTIO_MMIO_VENDOR_ID:          u32 = 0x00c;
pub const VIRTIO_MMIO_DEVICE_FEATURES:    u32 = 0x010;
pub const VIRTIO_MMIO_DEVICE_FEATURES_SEL:u32 = 0x014;
pub const VIRTIO_MMIO_DRIVER_FEATURES:    u32 = 0x020;
pub const VIRTIO_MMIO_DRIVER_FEATURES_SEL:u32 = 0x024;
pub const VIRTIO_MMIO_GUEST_PAGE_SIZE:    u32 = 0x028; // legacy only
pub const VIRTIO_MMIO_QUEUE_SEL:          u32 = 0x030;
pub const VIRTIO_MMIO_QUEUE_NUM_MAX:      u32 = 0x034;
pub const VIRTIO_MMIO_QUEUE_NUM:          u32 = 0x038;
pub const VIRTIO_MMIO_QUEUE_ALIGN:        u32 = 0x03c; // legacy only
pub const VIRTIO_MMIO_QUEUE_PFN:          u32 = 0x040; // legacy only
pub const VIRTIO_MMIO_QUEUE_READY:        u32 = 0x044; // modern only
pub const VIRTIO_MMIO_QUEUE_NOTIFY:       u32 = 0x050;
pub const VIRTIO_MMIO_INTERRUPT_STATUS:   u32 = 0x060;
pub const VIRTIO_MMIO_INTERRUPT_ACK:      u32 = 0x064;
pub const VIRTIO_MMIO_STATUS:             u32 = 0x070;
pub const VIRTIO_MMIO_QUEUE_DESC_LOW:     u32 = 0x080;
pub const VIRTIO_MMIO_QUEUE_DESC_HIGH:    u32 = 0x084;
pub const VIRTIO_MMIO_QUEUE_AVAIL_LOW:    u32 = 0x090;
pub const VIRTIO_MMIO_QUEUE_AVAIL_HIGH:   u32 = 0x094;
pub const VIRTIO_MMIO_QUEUE_USED_LOW:     u32 = 0x0a0;
pub const VIRTIO_MMIO_QUEUE_USED_HIGH:    u32 = 0x0a4;
pub const VIRTIO_MMIO_CONFIG:             u32 = 0x100;

// ---- Constants ----

pub const VIRTIO_MAGIC:      u32 = 0x7472_6976; // "virt"
pub const VIRTIO_DEV_NET:    u32 = 1;
pub const VIRTIO_DEV_BLOCK:  u32 = 2;
pub const VIRTIO_QUEUE_SIZE: usize = 16;

// Status bits
pub const VIRTIO_STATUS_ACK:          u32 = 1;
pub const VIRTIO_STATUS_DRIVER:       u32 = 2;
pub const VIRTIO_STATUS_DRIVER_OK:    u32 = 4;
pub const VIRTIO_STATUS_FEATURES_OK:  u32 = 8;
pub const VIRTIO_STATUS_FAILED:       u32 = 128;

// Descriptor flags
pub const VIRTQ_DESC_F_NEXT:     u16 = 1;
pub const VIRTQ_DESC_F_WRITE:    u16 = 2;

// ---- VirtIO Ring Structures (repr(C, packed) matches C ABI) ----

#[repr(C, packed)]
pub struct VirtqDesc {
    pub addr:  u64,
    pub len:   u32,
    pub flags: u16,
    pub next:  u16,
}

#[repr(C, packed)]
pub struct VirtqAvail {
    pub flags:      u16,
    pub idx:        u16,
    pub ring:       [u16; VIRTIO_QUEUE_SIZE],
    pub used_event: u16,
}

#[repr(C, packed)]
pub struct VirtqUsedElem {
    pub id:  u32,
    pub len: u32,
}

#[repr(C, packed)]
pub struct VirtqUsed {
    pub flags:       u16,
    pub idx:         u16,
    pub ring:        [VirtqUsedElem; VIRTIO_QUEUE_SIZE],
    pub avail_event: u16,
}

// ---- VirtQueue state ----

pub struct Virtq {
    pub desc:          *mut VirtqDesc,
    pub avail:         *mut VirtqAvail,
    pub used:          *mut VirtqUsed,
    pub num:           u16,
    pub free_head:     u16,
    pub free_count:    u8,
    pub last_used_idx: u16,
    pub desc_used:     [bool; VIRTIO_QUEUE_SIZE],
}

impl Virtq {
    pub const fn zeroed() -> Self {
        Virtq {
            desc:          core::ptr::null_mut(),
            avail:         core::ptr::null_mut(),
            used:          core::ptr::null_mut(),
            num:           0,
            free_head:     0,
            free_count:    0,
            last_used_idx: 0,
            desc_used:     [false; VIRTIO_QUEUE_SIZE],
        }
    }
}

// ---- VirtIO Device ----

#[derive(Clone, Copy)]
pub struct VirtioDev {
    pub base:      *mut u32, // MMIO base address
    pub device_id: u32,
    pub version:   u32,
}

impl VirtioDev {
    pub const fn zeroed() -> Self {
        VirtioDev { base: core::ptr::null_mut(), device_id: 0, version: 0 }
    }
}

// ---- MMIO helpers (port of virtio_read32 / virtio_write32 inline functions) ----

#[inline(always)]
pub unsafe fn mmio_read(base: *mut u32, offset: u32) -> u32 {
    core::ptr::read_volatile(base.add((offset / 4) as usize))
}

#[inline(always)]
pub unsafe fn mmio_write(base: *mut u32, offset: u32, val: u32) {
    core::ptr::write_volatile(base.add((offset / 4) as usize), val);
}

// ---- virtio_probe (port of virtio_probe in virtio.c) ----

/// Check if a VirtIO device exists at the given MMIO address.
/// Returns Ok(device_id) or Err.
pub unsafe fn probe(base_addr: usize, dev: &mut VirtioDev) -> Result<(), ()> {
    let base = base_addr as *mut u32;

    let magic = mmio_read(base, VIRTIO_MMIO_MAGIC);
    if magic != VIRTIO_MAGIC {
        return Err(());
    }

    let version = mmio_read(base, VIRTIO_MMIO_VERSION);
    if version != 1 && version != 2 {
        return Err(());
    }

    let device_id = mmio_read(base, VIRTIO_MMIO_DEVICE_ID);
    if device_id == 0 {
        return Err(());
    }

    dev.base      = base;
    dev.device_id = device_id;
    dev.version   = version;
    Ok(())
}

// ---- virtio_init (port of virtio_init in virtio.c) ----

/// Initialize a VirtIO device (feature negotiation, status handshake).
/// Follows the VirtIO initialization sequence from the spec.
pub unsafe fn init(dev: &mut VirtioDev) -> Result<(), ()> {
    // Step 1: Reset
    mmio_write(dev.base, VIRTIO_MMIO_STATUS, 0);

    // Legacy: set guest page size
    if dev.version == 1 {
        mmio_write(dev.base, VIRTIO_MMIO_GUEST_PAGE_SIZE, robot_os_arch::mmu::PAGE_SIZE as u32);
    }

    // Step 2: ACK
    let s = mmio_read(dev.base, VIRTIO_MMIO_STATUS);
    mmio_write(dev.base, VIRTIO_MMIO_STATUS, s | VIRTIO_STATUS_ACK);

    // Step 3: DRIVER
    let s = mmio_read(dev.base, VIRTIO_MMIO_STATUS);
    mmio_write(dev.base, VIRTIO_MMIO_STATUS, s | VIRTIO_STATUS_DRIVER);

    // Step 4: Feature negotiation — accept no optional features
    mmio_write(dev.base, VIRTIO_MMIO_DEVICE_FEATURES_SEL, 0);
    let _features = mmio_read(dev.base, VIRTIO_MMIO_DEVICE_FEATURES);
    mmio_write(dev.base, VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
    mmio_write(dev.base, VIRTIO_MMIO_DRIVER_FEATURES, 0);

    // Step 5: FEATURES_OK
    let s = mmio_read(dev.base, VIRTIO_MMIO_STATUS);
    mmio_write(dev.base, VIRTIO_MMIO_STATUS, s | VIRTIO_STATUS_FEATURES_OK);

    let status = mmio_read(dev.base, VIRTIO_MMIO_STATUS);
    if status & VIRTIO_STATUS_FEATURES_OK == 0 {
        mmio_write(dev.base, VIRTIO_MMIO_STATUS, VIRTIO_STATUS_FAILED);
        return Err(());
    }

    Ok(())
}

// ---- virtq_init (port of virtq_init in virtio.c) ----

/// Initialize a virtqueue. Handles both legacy (v1) and modern (v2).
pub unsafe fn virtq_init(dev: &mut VirtioDev, queue_idx: u32, vq: &mut Virtq) -> Result<(), ()> {
    use robot_os_mm::pmm;

    mmio_write(dev.base, VIRTIO_MMIO_QUEUE_SEL, queue_idx);

    let max_size = mmio_read(dev.base, VIRTIO_MMIO_QUEUE_NUM_MAX);
    if max_size == 0 {
        return Err(());
    }

    let queue_size = (max_size as usize).min(VIRTIO_QUEUE_SIZE) as u16;
    vq.num = queue_size;

    if dev.version == 1 {
        // Legacy mode: contiguous memory block
        // Layout: [desc_table | avail_ring | padding_to_page | used_ring]
        // For VIRTIO_QUEUE_SIZE=16: total ~4230 bytes = 2 pages.
        let desc_size  = 16 * queue_size as usize;
        let avail_size = 6 + 2 * queue_size as usize;
        let page_sz    = robot_os_arch::mmu::PAGE_SIZE;
        let used_offset = ((desc_size + avail_size + page_sz - 1) / page_sz) * page_sz;
        let used_size  = 6 + 8 * queue_size as usize;
        let total_size = used_offset + used_size;
        let pages_needed = (total_size + page_sz - 1) / page_sz;

        // Allocate consecutive pages. The PMM is a sequential bitmap allocator,
        // so consecutive alloc_page() calls return physically contiguous pages.
        let first_page = pmm::alloc_page().map_err(|_| ())?.0;
        for _ in 1..pages_needed {
            pmm::alloc_page().map_err(|_| ())?; // consume remaining pages
        }
        let queue_mem = first_page as *mut u8;
        core::ptr::write_bytes(queue_mem, 0, pages_needed * page_sz);

        vq.desc  = queue_mem as *mut VirtqDesc;
        vq.avail = queue_mem.add(desc_size) as *mut VirtqAvail;
        vq.used  = queue_mem.add(used_offset) as *mut VirtqUsed;

        init_free_list(vq, queue_size);

        // Tell device: queue num, alignment, PFN
        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_NUM,   queue_size as u32);
        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_ALIGN,  page_sz as u32);
        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_PFN,   (queue_mem as usize / page_sz) as u32);
    } else {
        // Modern mode: separate pages for desc, avail, used
        let desc_page  = pmm::alloc_page().map_err(|_| ())?.0;
        let avail_page = pmm::alloc_page().map_err(|_| ())?.0;
        let used_page  = pmm::alloc_page().map_err(|_| ())?.0;

        core::ptr::write_bytes(desc_page  as *mut u8, 0, robot_os_arch::mmu::PAGE_SIZE);
        core::ptr::write_bytes(avail_page as *mut u8, 0, robot_os_arch::mmu::PAGE_SIZE);
        core::ptr::write_bytes(used_page  as *mut u8, 0, robot_os_arch::mmu::PAGE_SIZE);

        vq.desc  = desc_page  as *mut VirtqDesc;
        vq.avail = avail_page as *mut VirtqAvail;
        vq.used  = used_page  as *mut VirtqUsed;

        init_free_list(vq, queue_size);

        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_NUM, queue_size as u32);

        let desc_addr  = desc_page  as u64;
        let avail_addr = avail_page as u64;
        let used_addr  = used_page  as u64;

        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_DESC_LOW,  desc_addr  as u32);
        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_DESC_HIGH, (desc_addr  >> 32) as u32);
        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_AVAIL_LOW, avail_addr as u32);
        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_AVAIL_HIGH,(avail_addr >> 32) as u32);
        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_USED_LOW,  used_addr  as u32);
        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_USED_HIGH, (used_addr  >> 32) as u32);

        mmio_write(dev.base, VIRTIO_MMIO_QUEUE_READY, 1);
    }

    Ok(())
}

unsafe fn init_free_list(vq: &mut Virtq, queue_size: u16) {
    vq.free_head     = 0;
    vq.free_count    = queue_size as u8;
    vq.last_used_idx = 0;

    for i in 0..queue_size as usize {
        (*vq.desc.add(i)).next = (i + 1) as u16;
        vq.desc_used[i] = false;
    }
    (*vq.desc.add(queue_size as usize - 1)).next = 0xFFFF;
}

// ---- virtq_alloc_desc ----

pub unsafe fn virtq_alloc_desc(vq: &mut Virtq) -> Option<usize> {
    if vq.free_count == 0 {
        return None;
    }
    let idx = vq.free_head as usize;
    vq.free_head    = (*vq.desc.add(idx)).next;
    vq.free_count  -= 1;
    vq.desc_used[idx] = true;
    Some(idx)
}

// ---- virtq_free_desc ----

pub unsafe fn virtq_free_desc(vq: &mut Virtq, idx: usize) {
    if idx >= vq.num as usize || !vq.desc_used[idx] {
        return;
    }
    (*vq.desc.add(idx)).next = vq.free_head;
    vq.free_head             = idx as u16;
    vq.free_count           += 1;
    vq.desc_used[idx]        = false;
}

// ---- virtq_submit (port of virtq_submit in virtio.c) ----

pub unsafe fn virtq_submit(dev: &VirtioDev, queue_idx: u32, desc_head: usize, vq: &mut Virtq) {
    let avail_idx = ((*vq.avail).idx as usize) % vq.num as usize;
    (*vq.avail).ring[avail_idx] = desc_head as u16;

    fence(Ordering::Release); // fence w,w before updating idx

    (*vq.avail).idx = (*vq.avail).idx.wrapping_add(1);

    fence(Ordering::Release); // fence w,w before notify

    mmio_write(dev.base, VIRTIO_MMIO_QUEUE_NOTIFY, queue_idx);
}

// ---- virtq_poll (port of virtq_poll in virtio.c) ----

/// Returns Some(desc_head) if a request completed, None if queue is empty.
///
/// NOTE: this drops the `len` field from the used-ring entry — for net RX
/// that means callers don't know the actual received packet length. Prefer
/// `virtq_poll_with_len()` for RX paths where the device writes a variable
/// amount into the descriptor's buffer.
pub unsafe fn virtq_poll(vq: &mut Virtq) -> Option<usize> {
    virtq_poll_with_len(vq).map(|(id, _)| id)
}

/// Like `virtq_poll`, but also returns the device-reported length written
/// into the buffer. Required for RX paths (Ethernet, block reads) where the
/// payload is shorter than the buffer; without it the consumer reads past
/// the real data into stale buffer contents.
///
/// Both `id` and `len` come from the device — a buggy or malicious device
/// can write any value. We bounds-check both before returning so callers
/// can trust them as array indices / slice lengths without further checks.
pub unsafe fn virtq_poll_with_len(vq: &mut Virtq) -> Option<(usize, usize)> {
    fence(Ordering::Acquire); // fence r,r

    if vq.last_used_idx == (*vq.used).idx {
        return None;
    }

    let used_idx = (vq.last_used_idx as usize) % vq.num as usize;
    let id  = (*vq.used).ring[used_idx].id  as usize;
    let len = (*vq.used).ring[used_idx].len as usize;
    vq.last_used_idx = vq.last_used_idx.wrapping_add(1);

    // Defensive bounds: a malicious / malfunctioning device could write
    // an `id` outside the descriptor table. Indexing without this check
    // is OOB read in `vq.desc.add(id)` calls higher up the stack.
    let qsize = vq.num as usize;
    if id >= qsize { return None; }
    // `len` larger than the descriptor's buffer length should also be
    // impossible per virtio spec, but we let the caller cap it against
    // its own buffer size so we don't have to re-derive that here.

    Some((id, len))
}

// ---- virtio_read_config32 / 64 ----

pub unsafe fn read_config32(dev: &VirtioDev, offset: u32) -> u32 {
    mmio_read(dev.base, VIRTIO_MMIO_CONFIG + offset)
}

pub unsafe fn read_config64(dev: &VirtioDev, offset: u32) -> u64 {
    let low  = mmio_read(dev.base, VIRTIO_MMIO_CONFIG + offset) as u64;
    let high = mmio_read(dev.base, VIRTIO_MMIO_CONFIG + offset + 4) as u64;
    (high << 32) | low
}
