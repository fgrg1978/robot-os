//! Zero-copy Pipeline (F15).
//!
//! A statically-allocated pool of large, page-aligned buffers used to hand
//! bulk data (camera frames, LiDAR scans, raw audio) between kernel
//! subsystems and user-space consumers without copying the payload.
//!
//! # Design
//!
//! * `BUF_POOL` is a const-sized array of `ZEROCOPY_BUF_COUNT` buffers, each
//!   `ZEROCOPY_BUF_SIZE` bytes, aligned to one page (`ZEROCOPY_PAGE_ALIGN`).
//!   All storage lives in `.bss` — there is no heap allocation, matching the
//!   `#![no_std]` contract of the rest of the IPC crate.
//!
//! * A `BufferHandle` is a `Copy` struct consisting of a buffer id and a
//!   per-slot `generation` counter.  The generation counter is bumped every
//!   time a buffer is returned to the pool, so any stale handle referring to
//!   an earlier generation is detected and rejected — this is our protection
//!   against use-after-free.
//!
//! * Producers call `pipeline_acquire()` to grab a free buffer, fill it, and
//!   then `pipeline_submit(handle, len, queue_id)` to hand it to a consumer
//!   queue.  `pipeline_submit_multi()` lets a producer fan-out to several
//!   consumers with a shared refcount; the buffer is only returned to the
//!   pool once *every* consumer calls `pipeline_release()`.
//!
//! * Consumers call `pipeline_receive(queue_id)` to pop a ready buffer.
//!   After processing they call `pipeline_release(handle)`.
//!
//! * Each consumer queue is a small ring.  The write cursor is protected by
//!   a global spinlock (matching the style of `lease.rs` and `fast_ipc.rs`),
//!   so multiple producers or consumers can safely share the same queue.
//!   A SeqLock-style sequence counter also guards the write position so
//!   debug readers can detect torn writes without taking the main lock.
//!
//! # Relationship to other primitives
//!
//! This module is **complementary** to `lease.rs` / `fast_ipc.rs` / `io_ring.rs`:
//!
//! * `lease`   — time-bounded hand-off of a single SHM region between two tasks.
//! * `fast_ipc`— 32-byte register-passing messages, no bulk data.
//! * `io_ring` — per-ring shared pages for queue-driven device I/O.
//! * `zerocopy`— pipeline of persistent large buffers shared across the system;
//!                 intended for multi-stage data flow (`camera -> perception ->
//!                 planner -> log`).

#![allow(clippy::needless_return)]

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use robot_os_sync::SpinLock;

// ---------------------------------------------------------------------------
// Constants — NO MAGIC NUMBERS
// ---------------------------------------------------------------------------

/// Page alignment for the buffer pool (RISC-V Sv39 page size).
pub const ZEROCOPY_PAGE_ALIGN: usize = 4096;

/// Number of buffers in the pool.
pub const ZEROCOPY_BUF_COUNT: usize = 8;

/// Size of each buffer in bytes (256 KiB — enough for a QVGA RGB frame or a
/// 64k-point LiDAR scan).
pub const ZEROCOPY_BUF_SIZE: usize = 256 * 1024;

/// Maximum number of consumer queues (distinct subscriber ports).
pub const ZEROCOPY_MAX_CONSUMERS: usize = 4;

/// Depth of each consumer ring (must be a power of two for the mask below).
pub const ZEROCOPY_RING_DEPTH: usize = 16;

/// Mask used to convert a monotonically-increasing cursor into a ring index.
pub const ZEROCOPY_RING_MASK: usize = ZEROCOPY_RING_DEPTH - 1;

/// Sentinel meaning "no buffer" — used in empty ring slots.
pub const ZEROCOPY_INVALID_ID: u16 = u16::MAX;

/// Initial generation value stored in a fresh pool slot.
pub const ZEROCOPY_INITIAL_GENERATION: u32 = 1;

/// Status codes returned from the public submit/release API.
pub const ZEROCOPY_OK: i32 = 0;
pub const ZEROCOPY_ERR_INVALID_HANDLE: i32 = -1;
pub const ZEROCOPY_ERR_STALE_GENERATION: i32 = -2;
pub const ZEROCOPY_ERR_QUEUE_FULL: i32 = -3;
pub const ZEROCOPY_ERR_INVALID_QUEUE: i32 = -4;
pub const ZEROCOPY_ERR_INVALID_LEN: i32 = -5;

/// Zero value used to initialise counters and cursors.
const ZEROCOPY_ZERO_U32: u32 = 0;
const ZEROCOPY_ZERO_USIZE: usize = 0;

// Compile-time sanity.
const _: () = {
    assert!(ZEROCOPY_RING_DEPTH.is_power_of_two());
    assert!(ZEROCOPY_MAX_CONSUMERS >= 1);
    assert!(ZEROCOPY_BUF_COUNT >= 1);
    assert!(ZEROCOPY_BUF_COUNT < ZEROCOPY_INVALID_ID as usize);
    assert!(ZEROCOPY_BUF_SIZE % ZEROCOPY_PAGE_ALIGN == 0);
};

// ---------------------------------------------------------------------------
// Backing storage — page-aligned buffers in .bss
// ---------------------------------------------------------------------------

/// One page-aligned buffer.  `#[repr(align(4096))]` guarantees the struct
/// (and therefore its sole field) starts on a page boundary.
#[repr(align(4096))]
struct PageBuf {
    bytes: [u8; ZEROCOPY_BUF_SIZE],
}

impl PageBuf {
    const EMPTY: Self = PageBuf { bytes: [0u8; ZEROCOPY_BUF_SIZE] };
}

/// The actual pool.  Static mut, accessed via raw pointers only — contents
/// are externally synchronised by the `PoolMeta` spinlock plus atomic refcounts.
static mut BUF_POOL: [PageBuf; ZEROCOPY_BUF_COUNT] =
    [PageBuf::EMPTY; ZEROCOPY_BUF_COUNT];

// ---------------------------------------------------------------------------
// BufferHandle — Copy struct passed by value through the pipeline
// ---------------------------------------------------------------------------

/// Opaque handle to a pipeline buffer.
///
/// * `id`         — index into `BUF_POOL` (0..`ZEROCOPY_BUF_COUNT`)
/// * `generation` — per-slot counter incremented on each release-to-pool;
///                  detects use-after-free and double-release bugs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct BufferHandle {
    pub id:         u16,
    pub generation: u32,
}

impl BufferHandle {
    /// The always-invalid sentinel handle.
    pub const INVALID: Self = BufferHandle {
        id:         ZEROCOPY_INVALID_ID,
        generation: ZEROCOPY_ZERO_U32,
    };

    #[inline]
    pub const fn is_invalid(&self) -> bool {
        self.id == ZEROCOPY_INVALID_ID
    }
}

// ---------------------------------------------------------------------------
// Per-slot metadata (in pool)
// ---------------------------------------------------------------------------

/// Lifecycle of a pool slot.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotState {
    /// Slot is free — the buffer can be acquired.
    Free,
    /// Slot has been acquired by a producer (filling / submitting).
    InUse,
}

/// Per-slot metadata kept alongside the buffer bytes.
#[derive(Clone, Copy)]
struct SlotMeta {
    state:      SlotState,
    /// Generation counter — bumped when returning to `Free`.
    generation: u32,
    /// Valid bytes currently in the buffer (set by `pipeline_submit`).
    len:        usize,
    /// How many consumers still need to call `pipeline_release`.
    refcount:   u32,
}

impl SlotMeta {
    const EMPTY: Self = SlotMeta {
        state:      SlotState::Free,
        generation: ZEROCOPY_INITIAL_GENERATION,
        len:        ZEROCOPY_ZERO_USIZE,
        refcount:   ZEROCOPY_ZERO_U32,
    };
}

// ---------------------------------------------------------------------------
// Consumer ring — one per subscriber
// ---------------------------------------------------------------------------

/// A single entry in a consumer ring.
#[derive(Clone, Copy)]
struct RingSlot {
    handle: BufferHandle,
    len:    usize,
}

impl RingSlot {
    const EMPTY: Self = RingSlot {
        handle: BufferHandle::INVALID,
        len:    ZEROCOPY_ZERO_USIZE,
    };
}

/// One consumer queue.
///
/// The `head`/`tail` cursors are monotonically increasing `AtomicUsize` values;
/// the modulus is applied with `ZEROCOPY_RING_MASK`.  A write-position
/// `SeqLock`-style sequence (`wseq`) lets readers detect torn writes in debug
/// inspection without needing to take the main lock.
struct ConsumerQueue {
    active: bool,
    head:   AtomicUsize, // consumer cursor (bumped on receive)
    tail:   AtomicUsize, // producer cursor (bumped on submit)
    wseq:   AtomicU32,   // SeqLock sequence — odd = write in flight
    slots:  [RingSlot; ZEROCOPY_RING_DEPTH],
    depth_high_water: usize,
    drops: u64,
}

impl ConsumerQueue {
    const fn new() -> Self {
        ConsumerQueue {
            active: false,
            head:   AtomicUsize::new(ZEROCOPY_ZERO_USIZE),
            tail:   AtomicUsize::new(ZEROCOPY_ZERO_USIZE),
            wseq:   AtomicU32::new(ZEROCOPY_ZERO_U32),
            slots:  [RingSlot::EMPTY; ZEROCOPY_RING_DEPTH],
            depth_high_water: ZEROCOPY_ZERO_USIZE,
            drops:  0,
        }
    }

    #[inline]
    fn depth(&self) -> usize {
        let h = self.head.load(Ordering::Acquire);
        let t = self.tail.load(Ordering::Acquire);
        t.wrapping_sub(h)
    }

    #[inline]
    fn is_full(&self) -> bool {
        self.depth() >= ZEROCOPY_RING_DEPTH
    }
}

// ---------------------------------------------------------------------------
// Global pool state (slots + queues + counters)
// ---------------------------------------------------------------------------

struct PoolMeta {
    slots:  [SlotMeta; ZEROCOPY_BUF_COUNT],
    queues: [ConsumerQueue; ZEROCOPY_MAX_CONSUMERS],
    /// Diagnostic: current number of slots with `state == InUse`.
    pool_in_use: usize,
    /// Diagnostic: all-time high water mark for `pool_in_use`.
    pool_high_water: usize,
    /// Diagnostic: total acquire failures (pool empty).
    acquire_fails: u64,
}

impl PoolMeta {
    const fn new() -> Self {
        const E: SlotMeta = SlotMeta::EMPTY;
        const Q: ConsumerQueue = ConsumerQueue::new();
        PoolMeta {
            slots:           [E; ZEROCOPY_BUF_COUNT],
            queues:          [Q; ZEROCOPY_MAX_CONSUMERS],
            pool_in_use:     ZEROCOPY_ZERO_USIZE,
            pool_high_water: ZEROCOPY_ZERO_USIZE,
            acquire_fails:   0,
        }
    }
}

static POOL: SpinLock<PoolMeta> = SpinLock::new(PoolMeta::new());

// ---------------------------------------------------------------------------
// Buffer access helpers
// ---------------------------------------------------------------------------

/// Return the mutable byte slice backing a buffer.
///
/// # Safety
/// The caller must hold a valid, non-stale `BufferHandle` obtained from
/// `pipeline_acquire()` and must not have called `pipeline_submit()` on it
/// yet (once submitted, the producer must not touch the buffer).
#[inline]
pub unsafe fn buffer_bytes_mut(handle: BufferHandle) -> Option<&'static mut [u8]> {
    let idx = handle.id as usize;
    if idx >= ZEROCOPY_BUF_COUNT { return None; }
    // Generation check via the pool meta.
    if !generation_valid(handle) { return None; }
    let buf_ptr = core::ptr::addr_of_mut!(BUF_POOL[idx].bytes);
    Some(&mut *buf_ptr)
}

/// Return the immutable byte slice backing a buffer (consumer side).
///
/// # Safety
/// The caller must hold a valid, non-stale `BufferHandle` obtained from
/// `pipeline_receive()` and must not keep the reference beyond
/// `pipeline_release()`.
#[inline]
pub unsafe fn buffer_bytes(handle: BufferHandle) -> Option<&'static [u8]> {
    let idx = handle.id as usize;
    if idx >= ZEROCOPY_BUF_COUNT { return None; }
    if !generation_valid(handle) { return None; }
    let buf_ptr = core::ptr::addr_of!(BUF_POOL[idx].bytes);
    Some(&*buf_ptr)
}

/// Return the physical address (== virtual in identity-mapped kernel space)
/// of a buffer — useful for DMA.
pub fn buffer_addr(handle: BufferHandle) -> Option<usize> {
    let idx = handle.id as usize;
    if idx >= ZEROCOPY_BUF_COUNT { return None; }
    if !generation_valid(handle) { return None; }
    unsafe { Some(core::ptr::addr_of!(BUF_POOL[idx].bytes) as usize) }
}

fn generation_valid(handle: BufferHandle) -> bool {
    let idx = handle.id as usize;
    if idx >= ZEROCOPY_BUF_COUNT { return false; }
    let pool = POOL.lock();
    pool.slots[idx].generation == handle.generation
}

// ---------------------------------------------------------------------------
// Producer API
// ---------------------------------------------------------------------------

/// Acquire a free buffer from the pool.
///
/// Returns `None` if every slot is currently in use.  The returned handle is
/// exclusively owned by the caller until `pipeline_submit()` is called.
pub fn pipeline_acquire() -> Option<BufferHandle> {
    let mut pool = POOL.lock();
    for i in 0..ZEROCOPY_BUF_COUNT {
        if pool.slots[i].state == SlotState::Free {
            pool.slots[i].state    = SlotState::InUse;
            pool.slots[i].len      = ZEROCOPY_ZERO_USIZE;
            pool.slots[i].refcount = ZEROCOPY_ZERO_U32;
            let handle = BufferHandle {
                id:         i as u16,
                generation: pool.slots[i].generation,
            };
            pool.pool_in_use = pool.pool_in_use.saturating_add(1);
            if pool.pool_in_use > pool.pool_high_water {
                pool.pool_high_water = pool.pool_in_use;
            }
            return Some(handle);
        }
    }
    pool.acquire_fails = pool.acquire_fails.saturating_add(1);
    None
}

/// Submit a buffer to a single consumer queue.
///
/// Sets the valid length to `len`, increments the buffer's refcount, and
/// enqueues it on `queue_id`.  Returns `ZEROCOPY_OK` on success.
pub fn pipeline_submit(handle: BufferHandle, len: usize, queue_id: usize) -> i32 {
    pipeline_submit_multi(handle, len, &[queue_id])
}

/// Submit a buffer to multiple consumer queues (fan-out / shared refcount).
///
/// The buffer is only returned to the free pool after every subscriber has
/// called `pipeline_release()` on it.  Invalid / inactive queues are skipped
/// but do not cause the whole call to fail — the returned value is
/// `ZEROCOPY_OK` if *at least one* delivery succeeded, otherwise a negative
/// error code.
pub fn pipeline_submit_multi(
    handle:    BufferHandle,
    len:       usize,
    queue_ids: &[usize],
) -> i32 {
    let idx = handle.id as usize;
    if idx >= ZEROCOPY_BUF_COUNT { return ZEROCOPY_ERR_INVALID_HANDLE; }
    if len > ZEROCOPY_BUF_SIZE   { return ZEROCOPY_ERR_INVALID_LEN; }

    let mut pool = POOL.lock();

    // Validate slot ownership and generation.
    {
        let slot = &pool.slots[idx];
        if slot.state != SlotState::InUse        { return ZEROCOPY_ERR_INVALID_HANDLE; }
        if slot.generation != handle.generation  { return ZEROCOPY_ERR_STALE_GENERATION; }
    }

    // Record the payload length.
    pool.slots[idx].len = len;

    // Enqueue on every valid, active queue.
    let mut delivered: u32 = ZEROCOPY_ZERO_U32;
    let mut last_error: i32 = ZEROCOPY_ERR_INVALID_QUEUE;

    for &qid in queue_ids {
        if qid >= ZEROCOPY_MAX_CONSUMERS {
            last_error = ZEROCOPY_ERR_INVALID_QUEUE;
            continue;
        }
        let q = &mut pool.queues[qid];
        if !q.active {
            last_error = ZEROCOPY_ERR_INVALID_QUEUE;
            continue;
        }
        if q.is_full() {
            q.drops = q.drops.saturating_add(1);
            last_error = ZEROCOPY_ERR_QUEUE_FULL;
            continue;
        }

        // SeqLock-style write-position update: bump wseq to odd before
        // write, even after.  One-producer-at-a-time is enforced by the
        // SpinLock on POOL — wseq lets lockless observers detect torn writes.
        let seq = q.wseq.load(Ordering::Relaxed);
        q.wseq.store(seq.wrapping_add(1), Ordering::Release);

        let tail = q.tail.load(Ordering::Relaxed);
        let slot_idx = tail & ZEROCOPY_RING_MASK;
        q.slots[slot_idx] = RingSlot { handle, len };
        q.tail.store(tail.wrapping_add(1), Ordering::Release);

        q.wseq.store(seq.wrapping_add(2), Ordering::Release);

        let depth = tail
            .wrapping_add(1)
            .wrapping_sub(q.head.load(Ordering::Relaxed));
        if depth > q.depth_high_water {
            q.depth_high_water = depth;
        }

        delivered = delivered.saturating_add(1);
    }

    if delivered == ZEROCOPY_ZERO_U32 {
        // Nothing delivered — buffer stays owned by producer so they can
        // retry or release.  Do NOT bump refcount.
        return last_error;
    }

    // Bump refcount by number of successful deliveries.
    pool.slots[idx].refcount = pool.slots[idx].refcount.saturating_add(delivered);
    ZEROCOPY_OK
}

// ---------------------------------------------------------------------------
// Consumer API
// ---------------------------------------------------------------------------

/// Register a consumer queue.  Must be called once before `pipeline_receive`.
///
/// `queue_id` must be `< ZEROCOPY_MAX_CONSUMERS`.  Returns true on success,
/// false if already active or out of range.
pub fn pipeline_register_queue(queue_id: usize) -> bool {
    if queue_id >= ZEROCOPY_MAX_CONSUMERS { return false; }
    let mut pool = POOL.lock();
    let q = &mut pool.queues[queue_id];
    if q.active { return false; }
    // Reset cursors and stats in case of re-use after a previous unregister.
    q.head.store(ZEROCOPY_ZERO_USIZE, Ordering::Relaxed);
    q.tail.store(ZEROCOPY_ZERO_USIZE, Ordering::Relaxed);
    q.wseq.store(ZEROCOPY_ZERO_U32,   Ordering::Relaxed);
    q.depth_high_water = ZEROCOPY_ZERO_USIZE;
    q.drops = 0;
    q.active = true;
    true
}

/// Unregister a consumer queue.  Any still-queued handles are silently
/// released back to the pool.
pub fn pipeline_unregister_queue(queue_id: usize) -> bool {
    if queue_id >= ZEROCOPY_MAX_CONSUMERS { return false; }
    // Drain by repeatedly receiving and releasing.  We drop the lock between
    // iterations because pipeline_release takes the lock too.
    loop {
        let handle = {
            let mut pool = POOL.lock();
            let q = &mut pool.queues[queue_id];
            if !q.active { return false; }
            let head = q.head.load(Ordering::Relaxed);
            let tail = q.tail.load(Ordering::Relaxed);
            if head == tail { break; }
            let slot_idx = head & ZEROCOPY_RING_MASK;
            let entry = q.slots[slot_idx];
            q.slots[slot_idx] = RingSlot::EMPTY;
            q.head.store(head.wrapping_add(1), Ordering::Release);
            entry.handle
        };
        let _ = pipeline_release(handle);
    }
    let mut pool = POOL.lock();
    pool.queues[queue_id].active = false;
    true
}

/// Receive the next buffer from `queue_id`.
///
/// Returns `(handle, len)` on success, `None` if the queue is empty or inactive.
pub fn pipeline_receive(queue_id: usize) -> Option<(BufferHandle, usize)> {
    if queue_id >= ZEROCOPY_MAX_CONSUMERS { return None; }
    let mut pool = POOL.lock();
    let q = &mut pool.queues[queue_id];
    if !q.active { return None; }
    let head = q.head.load(Ordering::Relaxed);
    let tail = q.tail.load(Ordering::Relaxed);
    if head == tail { return None; }
    let slot_idx = head & ZEROCOPY_RING_MASK;
    let entry = q.slots[slot_idx];
    q.slots[slot_idx] = RingSlot::EMPTY;
    q.head.store(head.wrapping_add(1), Ordering::Release);
    Some((entry.handle, entry.len))
}

/// Release a buffer back to the pool (decrement refcount).
///
/// When the refcount reaches zero the slot is marked `Free` and its generation
/// is bumped, invalidating any surviving handles.
pub fn pipeline_release(handle: BufferHandle) -> i32 {
    let idx = handle.id as usize;
    if idx >= ZEROCOPY_BUF_COUNT { return ZEROCOPY_ERR_INVALID_HANDLE; }

    let mut pool = POOL.lock();
    let slot = &mut pool.slots[idx];

    if slot.state != SlotState::InUse {
        return ZEROCOPY_ERR_INVALID_HANDLE;
    }
    if slot.generation != handle.generation {
        return ZEROCOPY_ERR_STALE_GENERATION;
    }

    if slot.refcount > 0 {
        slot.refcount -= 1;
    }

    if slot.refcount == ZEROCOPY_ZERO_U32 {
        // Last reference — return to pool, bump generation.
        slot.state      = SlotState::Free;
        slot.len        = ZEROCOPY_ZERO_USIZE;
        slot.generation = slot.generation.wrapping_add(1);
        // Don't let generation wrap to 0 — keep it non-zero so a fresh
        // handle never collides with our INVALID sentinel's generation.
        if slot.generation == ZEROCOPY_ZERO_U32 {
            slot.generation = ZEROCOPY_INITIAL_GENERATION;
        }
        pool.pool_in_use = pool.pool_in_use.saturating_sub(1);
    }

    ZEROCOPY_OK
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Snapshot of pipeline counters — read-only view for `/proc` or trace dumps.
#[derive(Clone, Copy, Debug, Default)]
pub struct ZerocopyStats {
    pub pool_in_use:      usize,
    pub pool_high_water:  usize,
    pub acquire_fails:    u64,
    pub queue_depths:     [usize; ZEROCOPY_MAX_CONSUMERS],
    pub queue_high_water: [usize; ZEROCOPY_MAX_CONSUMERS],
    pub queue_drops:      [u64;   ZEROCOPY_MAX_CONSUMERS],
    pub queue_active:     [bool;  ZEROCOPY_MAX_CONSUMERS],
}

/// Return a consistent snapshot of all pipeline diagnostic counters.
pub fn pipeline_stats() -> ZerocopyStats {
    let pool = POOL.lock();
    let mut s = ZerocopyStats::default();
    s.pool_in_use     = pool.pool_in_use;
    s.pool_high_water = pool.pool_high_water;
    s.acquire_fails   = pool.acquire_fails;
    for i in 0..ZEROCOPY_MAX_CONSUMERS {
        let q = &pool.queues[i];
        s.queue_depths[i]     = q.depth();
        s.queue_high_water[i] = q.depth_high_water;
        s.queue_drops[i]      = q.drops;
        s.queue_active[i]     = q.active;
    }
    s
}

/// Shortcut: number of buffers currently in use.
pub fn pipeline_in_use() -> usize {
    POOL.lock().pool_in_use
}

/// Shortcut: total drops across all consumer queues.
pub fn pipeline_total_drops() -> u64 {
    let pool = POOL.lock();
    let mut total: u64 = 0;
    for q in pool.queues.iter() {
        total = total.saturating_add(q.drops);
    }
    total
}

/// Shortcut: largest queue depth ever observed.
pub fn pipeline_max_depth() -> usize {
    let pool = POOL.lock();
    let mut m: usize = ZEROCOPY_ZERO_USIZE;
    for q in pool.queues.iter() {
        if q.depth_high_water > m { m = q.depth_high_water; }
    }
    m
}

// ---------------------------------------------------------------------------
// Test hook — resets state.  Unit-test helper; not used at runtime.
// ---------------------------------------------------------------------------

/// Reset all pipeline state to its post-boot configuration.  Primarily for
/// tests — not to be called while any handles are in flight.
#[doc(hidden)]
pub fn __pipeline_reset_for_tests() {
    let mut pool = POOL.lock();
    for i in 0..ZEROCOPY_BUF_COUNT {
        pool.slots[i] = SlotMeta::EMPTY;
    }
    for i in 0..ZEROCOPY_MAX_CONSUMERS {
        pool.queues[i] = ConsumerQueue::new();
    }
    pool.pool_in_use     = ZEROCOPY_ZERO_USIZE;
    pool.pool_high_water = ZEROCOPY_ZERO_USIZE;
    pool.acquire_fails   = 0;
}
