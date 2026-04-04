//! IO Ring — shared-memory submission/completion queues (AQ1).
//!
//! Inspired by Linux io_uring. Zero-copy, zero-syscall data path for
//! high-frequency sensor data between kernel/drivers and userspace.
//!
//! Layout (fits in one 4 KiB page):
//!   SQ (Submission Queue): userspace writes requests, kernel reads
//!   CQ (Completion Queue): kernel writes results, userspace reads
//!   Data buffer: large results (LiDAR scans, camera frames)

use core::sync::atomic::{AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of entries in each queue.
pub const RING_SQ_SIZE: usize = 32;
pub const RING_CQ_SIZE: usize = 32;

/// Maximum data per slot in the data buffer.
pub const RING_DATA_SLOT_SIZE: usize = 64;

/// Total data buffer size.
pub const RING_DATA_BUF_SIZE: usize = 2048;

/// Maximum number of io_rings system-wide.
pub const MAX_IO_RINGS: usize = 16;

// ---------------------------------------------------------------------------
// Ring opcodes
// ---------------------------------------------------------------------------

pub const OP_NOP: u16 = 0;
pub const OP_READ_SENSOR: u16 = 1;
pub const OP_WRITE_GPIO: u16 = 2;
pub const OP_READ_GPIO: u16 = 3;
pub const OP_I2C_READ: u16 = 4;
pub const OP_I2C_WRITE: u16 = 5;
pub const OP_PWM_SET: u16 = 6;
pub const OP_MOTOR_SPEED: u16 = 7;
pub const OP_NET_SEND: u16 = 8;
pub const OP_NET_RECV: u16 = 9;
pub const OP_CAMERA_CAPTURE: u16 = 10;
pub const OP_IRQ_WAIT: u16 = 11;

// ---------------------------------------------------------------------------
// Shared structures (mapped in both kernel and userspace)
// ---------------------------------------------------------------------------

/// Submission queue entry — written by userspace, read by kernel.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SqEntry {
    pub opcode: u16,
    pub flags: u16,
    pub param0: u32,       // sensor_type, pin, i2c_addr, etc.
    pub param1: u32,       // buf offset, value, reg, etc.
    pub param2: u32,       // buf len, etc.
    pub user_data: u64,    // opaque tag for correlation
}

/// Completion queue entry — written by kernel, read by userspace.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CqEntry {
    pub user_data: u64,    // copied from SqEntry
    pub result: i32,       // bytes read, or negative error
    pub flags: u32,
}

/// The shared ring structure (fits in ~4 KiB page).
#[repr(C)]
pub struct IoRing {
    // Submission queue
    pub sq_head: AtomicU32,          // kernel consumes from here
    pub sq_tail: AtomicU32,          // userspace produces to here
    pub sq_entries: [SqEntry; RING_SQ_SIZE],

    // Completion queue
    pub cq_head: AtomicU32,          // userspace consumes from here
    pub cq_tail: AtomicU32,          // kernel produces to here
    pub cq_entries: [CqEntry; RING_CQ_SIZE],

    // Shared data buffer for large results
    pub data_buf: [u8; RING_DATA_BUF_SIZE],
}

// ---------------------------------------------------------------------------
// Kernel-side ring management
// ---------------------------------------------------------------------------

/// Kernel state for one io_ring instance.
pub struct IoRingState {
    /// Physical address of the shared page (0 = unused slot).
    pub phys_addr: usize,
    /// Owning task index.
    pub owner_task: usize,
    /// Ring ID (index in global array).
    pub ring_id: u32,
    /// Whether this ring is active.
    pub active: bool,
}

impl IoRingState {
    pub const fn empty() -> Self {
        Self { phys_addr: 0, owner_task: usize::MAX, ring_id: 0, active: false }
    }
}

/// Global array of io_ring instances.
static mut IO_RINGS: [IoRingState; MAX_IO_RINGS] = {
    const EMPTY: IoRingState = IoRingState::empty();
    [EMPTY; MAX_IO_RINGS]
};

/// Allocate a new io_ring. Returns (ring_id, phys_addr) or None.
pub fn io_ring_create(owner_task: usize) -> Option<(u32, usize)> {
    // Allocate a physical page for the shared ring
    let page = robot_os_mm::pmm::alloc_page().ok()?;
    let phys = page.as_usize();

    unsafe {
        for i in 0..MAX_IO_RINGS {
            if !IO_RINGS[i].active {
                IO_RINGS[i] = IoRingState {
                    phys_addr: phys,
                    owner_task,
                    ring_id: i as u32,
                    active: true,
                };
                // Zero-init the ring (alloc_page already zeroes, but be explicit)
                let ring = phys as *mut IoRing;
                (*ring).sq_head.store(0, Ordering::Relaxed);
                (*ring).sq_tail.store(0, Ordering::Relaxed);
                (*ring).cq_head.store(0, Ordering::Relaxed);
                (*ring).cq_tail.store(0, Ordering::Relaxed);
                return Some((i as u32, phys));
            }
        }
    }
    // No free slots — free the page
    let _ = robot_os_mm::pmm::free_page(page);
    None
}

/// Destroy an io_ring and free its page.
pub fn io_ring_destroy(ring_id: u32) {
    if ring_id as usize >= MAX_IO_RINGS { return; }
    unsafe {
        let state = &mut IO_RINGS[ring_id as usize];
        if state.active {
            let _ = robot_os_mm::pmm::free_page(
                robot_os_mm::addr::PhysAddr::new(state.phys_addr)
            );
            *state = IoRingState::empty();
        }
    }
}

/// Get the number of pending submissions in an io_ring.
pub fn io_ring_pending(ring_id: u32) -> u32 {
    if ring_id as usize >= MAX_IO_RINGS { return 0; }
    unsafe {
        let state = &IO_RINGS[ring_id as usize];
        if !state.active { return 0; }
        let ring = state.phys_addr as *const IoRing;
        let head = (*ring).sq_head.load(Ordering::Acquire);
        let tail = (*ring).sq_tail.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }
}
