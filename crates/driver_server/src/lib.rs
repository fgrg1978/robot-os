#![no_std]

//! E11.AQ3 — Userspace driver framework.
//!
//! This crate provides the in-kernel registry that lets userspace processes
//! register themselves as drivers for specific IRQ / MMIO resources.
//! Clients then issue `sys_driver_request()` to operate on hardware without
//! needing kernel-level privileges.
//!
//! # Architecture
//! ```text
//!   userspace driver             kernel                   userspace client
//!   ───────────────────          ──────                   ─────────────────
//!   sys_driver_register(kind,                               ──┐
//!      irq, mmio_base, mmio_sz) ──► DRIVER_REGISTRY[kind]     │
//!                                                             │
//!                                                             ▼
//!                                                   sys_driver_request(
//!                                                     kind, op, &in, &out)
//!                                                             │
//!   sys_driver_wait_request() ◄── dispatch to matching drv ◄──┘
//!                               │
//!                               ▼
//!   [ userspace handles req ]
//!                               │
//!                               ▼
//!   sys_driver_reply(token, &out) ──► wake client
//! ```
//!
//! # IRQ routing
//! When a hardware IRQ fires, the kernel locates the driver registered for
//! that IRQ number and delivers a `DriverEvent::Irq(num)` via the same
//! channel. The driver's `sys_driver_wait()` returns with the event.
//!
//! # Current status (2026-04)
//! This crate provides the registry + dispatch scaffolding. Actual
//! migration of drivers (I²C, UART, GPIO, etc.) from in-kernel modules
//! to userspace processes happens post-hardware so we can validate
//! migration against real hardware behaviour. The `robot_os_drivers`
//! crate retains the in-kernel drivers for now.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use robot_os_sync::SpinLock;

// ───────────────────────────────────────────────────────────────────────────
// Driver kind IDs — every subsystem we expect to see in userspace gets one.
// ───────────────────────────────────────────────────────────────────────────

/// Upper bound on driver kinds. One `DriverSlot` per kind.
pub const DRIVER_MAX_KINDS: usize = 32;

pub const DRV_KIND_GPIO:      u32 = 0x0001;
pub const DRV_KIND_I2C:       u32 = 0x0002;
pub const DRV_KIND_SPI:       u32 = 0x0003;
pub const DRV_KIND_UART:      u32 = 0x0004;
pub const DRV_KIND_PWM:       u32 = 0x0005;
pub const DRV_KIND_DMA:       u32 = 0x0006;
pub const DRV_KIND_CSI_CAM:   u32 = 0x0007;
pub const DRV_KIND_LIDAR:     u32 = 0x0008;
pub const DRV_KIND_MOTOR_PID: u32 = 0x0009;
pub const DRV_KIND_IMU:       u32 = 0x000A;
pub const DRV_KIND_GPS:       u32 = 0x000B;
pub const DRV_KIND_ADC:       u32 = 0x000C;
pub const DRV_KIND_NPU:       u32 = 0x000D;
pub const DRV_KIND_CAN:       u32 = 0x000E;
pub const DRV_KIND_USB_XHCI:  u32 = 0x000F;

// ───────────────────────────────────────────────────────────────────────────
// Request queue size per driver (per-kind pending ring).
// ───────────────────────────────────────────────────────────────────────────

pub const DRIVER_REQUEST_QUEUE_DEPTH: usize = 8;

// ───────────────────────────────────────────────────────────────────────────
// Payload + reply buffer sizes (inline in request, small — larger transfers
// should use the F15 zero-copy pipeline).
// ───────────────────────────────────────────────────────────────────────────

pub const DRIVER_REQUEST_PAYLOAD_BYTES: usize = 64;
pub const DRIVER_REPLY_PAYLOAD_BYTES:   usize = 64;

// ───────────────────────────────────────────────────────────────────────────
// Event taxonomy received by driver's `sys_driver_wait()`.
// ───────────────────────────────────────────────────────────────────────────

/// No event pending.
pub const DRV_EVENT_NONE:    u32 = 0;
/// A new client request is ready to be processed.
pub const DRV_EVENT_REQUEST: u32 = 1;
/// Hardware IRQ fired.
pub const DRV_EVENT_IRQ:     u32 = 2;
/// Driver should shut down (kernel teardown).
pub const DRV_EVENT_SHUTDOWN: u32 = 3;

// ───────────────────────────────────────────────────────────────────────────
// Types.
// ───────────────────────────────────────────────────────────────────────────

/// One pending client request awaiting driver dispatch.
///
/// `#[repr(C)]` is mandatory: this struct is copied verbatim across the
/// user/kernel boundary (`sys_driver_fetch_request` does a `write_volatile`
/// into a userspace pointer), so a userspace driver process must see the exact
/// same field layout. A userspace mirror lives in `userspace/gpio_drv`.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct DriverRequest {
    /// Monotonic token used to match reply → waiter.
    pub token:      u64,
    /// Client task id — for wake after reply.
    pub client_tid: u32,
    /// Driver-defined op code (e.g. GPIO_WRITE, I2C_READ).
    pub op:         u32,
    /// Inline payload byte count.
    pub in_len:     u16,
    /// Reply buffer size (upper bound).
    pub out_cap:    u16,
    pub input:      [u8; DRIVER_REQUEST_PAYLOAD_BYTES],
}

impl DriverRequest {
    pub const fn zeroed() -> Self {
        Self {
            token: 0, client_tid: 0, op: 0,
            in_len: 0, out_cap: 0,
            input: [0; DRIVER_REQUEST_PAYLOAD_BYTES],
        }
    }
}

/// Reply produced by a userspace driver for a given token.
///
/// `#[repr(C)]` is mandatory — same cross-boundary reason as [`DriverRequest`]
/// (`sys_driver_reply` does a `read_volatile` from a userspace pointer).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct DriverReply {
    pub token:      u64,
    pub status:     i32,
    pub out_len:    u16,
    pub _pad:       u16,
    pub output:     [u8; DRIVER_REPLY_PAYLOAD_BYTES],
}

impl DriverReply {
    pub const fn zeroed() -> Self {
        Self {
            token: 0, status: 0, out_len: 0, _pad: 0,
            output: [0; DRIVER_REPLY_PAYLOAD_BYTES],
        }
    }
}

/// Per-kind driver slot with its pending-request ring and IRQ latch.
pub struct DriverSlot {
    pub kind:         u32,
    pub driver_tid:   u32,
    pub mmio_base:    u64,
    pub mmio_size:    u64,
    pub irq:          u32,
    pub active:       AtomicBool,

    /// Pending requests (client → driver).
    pub queue:        SpinLock<DriverQueue>,

    /// Last reply published, indexed by token for clients waiting.
    pub last_reply:   SpinLock<DriverReply>,

    /// Latched IRQ flag — set by IRQ handler, cleared on `sys_driver_wait`.
    pub irq_pending:  AtomicBool,

    /// Monotonic per-driver token counter.
    pub next_token:   AtomicU64,
}

impl DriverSlot {
    pub const fn empty() -> Self {
        Self {
            kind:        0,
            driver_tid:  0,
            mmio_base:   0,
            mmio_size:   0,
            irq:         0,
            active:      AtomicBool::new(false),
            queue:       SpinLock::new(DriverQueue::new()),
            last_reply:  SpinLock::new(DriverReply::zeroed()),
            irq_pending: AtomicBool::new(false),
            next_token:  AtomicU64::new(1),
        }
    }
}

pub struct DriverQueue {
    pub entries: [DriverRequest; DRIVER_REQUEST_QUEUE_DEPTH],
    pub head:    usize,
    pub tail:    usize,
    pub count:   usize,
}

impl DriverQueue {
    pub const fn new() -> Self {
        Self {
            entries: [DriverRequest::zeroed(); DRIVER_REQUEST_QUEUE_DEPTH],
            head: 0, tail: 0, count: 0,
        }
    }

    pub fn push(&mut self, req: DriverRequest) -> bool {
        if self.count == DRIVER_REQUEST_QUEUE_DEPTH { return false; }
        self.entries[self.head] = req;
        self.head = (self.head + 1) % DRIVER_REQUEST_QUEUE_DEPTH;
        self.count += 1;
        true
    }

    pub fn pop(&mut self) -> Option<DriverRequest> {
        if self.count == 0 { return None; }
        let r = self.entries[self.tail];
        self.tail = (self.tail + 1) % DRIVER_REQUEST_QUEUE_DEPTH;
        self.count -= 1;
        Some(r)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Registry (indexed by kind).
// ───────────────────────────────────────────────────────────────────────────

pub static REGISTRY: SpinLock<DriverRegistry> = SpinLock::new(DriverRegistry::new());

pub struct DriverRegistry {
    pub slots: [DriverSlot; DRIVER_MAX_KINDS],
}

impl DriverRegistry {
    pub const fn new() -> Self {
        const S: DriverSlot = DriverSlot::empty();
        Self { slots: [S; DRIVER_MAX_KINDS] }
    }

    fn find_kind(&mut self, kind: u32) -> Option<&mut DriverSlot> {
        self.slots.iter_mut().find(|s| {
            s.active.load(Ordering::Relaxed) && s.kind == kind
        })
    }

    fn find_free(&mut self) -> Option<&mut DriverSlot> {
        self.slots.iter_mut().find(|s| !s.active.load(Ordering::Relaxed))
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Syscall-layer entry points.
// ───────────────────────────────────────────────────────────────────────────

/// Register `tid` as the driver for `kind`.  Returns `true` on success.
pub fn driver_register(
    kind: u32,
    tid: u32,
    mmio_base: u64,
    mmio_size: u64,
    irq: u32,
) -> bool {
    let mut reg = REGISTRY.lock();
    if reg.find_kind(kind).is_some() { return false; } // already registered
    if let Some(slot) = reg.find_free() {
        slot.kind       = kind;
        slot.driver_tid = tid;
        slot.mmio_base  = mmio_base;
        slot.mmio_size  = mmio_size;
        slot.irq        = irq;
        slot.active.store(true, Ordering::Release);
        true
    } else {
        false
    }
}

/// Unregister a driver. Called on process exit / explicit unregister.
pub fn driver_unregister(kind: u32) -> bool {
    let mut reg = REGISTRY.lock();
    if let Some(slot) = reg.find_kind(kind) {
        slot.active.store(false, Ordering::Release);
        slot.driver_tid = 0;
        slot.irq_pending.store(false, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// Enqueue a client request. Returns the issued token, or 0 on failure
/// (no such driver / queue full).
pub fn driver_submit_request(
    kind: u32,
    client_tid: u32,
    op: u32,
    input: &[u8],
    out_cap: u16,
) -> u64 {
    let mut reg = REGISTRY.lock();
    let slot = match reg.find_kind(kind) {
        Some(s) => s,
        None    => return 0,
    };

    let token = slot.next_token.fetch_add(1, Ordering::Relaxed);
    let mut req = DriverRequest::zeroed();
    req.token      = token;
    req.client_tid = client_tid;
    req.op         = op;
    req.out_cap    = out_cap;
    let n = core::cmp::min(input.len(), DRIVER_REQUEST_PAYLOAD_BYTES);
    req.input[..n].copy_from_slice(&input[..n]);
    req.in_len = n as u16;

    let mut q = slot.queue.lock();
    if q.push(req) { token } else { 0 }
}

/// Signal an IRQ to the registered driver for `irq`, if any.
pub fn driver_signal_irq(irq: u32) -> bool {
    let mut reg = REGISTRY.lock();
    for s in reg.slots.iter_mut() {
        if s.active.load(Ordering::Relaxed) && s.irq == irq {
            s.irq_pending.store(true, Ordering::Release);
            return true;
        }
    }
    false
}

/// Driver polls for the next event. Returns (event_kind, payload_word).
/// The payload word meaning depends on event kind:
///   - `DRV_EVENT_REQUEST` → first u64 of the payload (token)
///   - `DRV_EVENT_IRQ`     → irq number
///   - `DRV_EVENT_NONE`    → 0 (caller should block via sched)
pub fn driver_poll_event(kind: u32) -> (u32, u64) {
    let mut reg = REGISTRY.lock();
    if let Some(slot) = reg.find_kind(kind) {
        if slot.irq_pending.swap(false, Ordering::AcqRel) {
            return (DRV_EVENT_IRQ, slot.irq as u64);
        }
        if slot.queue.lock().count > 0 {
            // Return just the token of the oldest pending request.
            let q = slot.queue.lock();
            let tail_req = q.entries[q.tail];
            return (DRV_EVENT_REQUEST, tail_req.token);
        }
    }
    (DRV_EVENT_NONE, 0)
}

/// Consume the next pending request (driver picks it up from the queue).
pub fn driver_fetch_request(kind: u32) -> Option<DriverRequest> {
    let mut reg = REGISTRY.lock();
    let slot = reg.find_kind(kind)?;
    let mut q = slot.queue.lock();
    q.pop()
}

/// Publish a reply for a previously-submitted token. Clients matching
/// this token get woken; they are expected to poll by token.
pub fn driver_reply(kind: u32, reply: DriverReply) -> bool {
    let mut reg = REGISTRY.lock();
    let slot = match reg.find_kind(kind) {
        Some(s) => s,
        None    => return false,
    };
    *slot.last_reply.lock() = reply;
    true
}

/// Check if a reply for `token` has been published; if so, copy out.
pub fn driver_try_take_reply(kind: u32, token: u64, out: &mut DriverReply) -> bool {
    let mut reg = REGISTRY.lock();
    if let Some(slot) = reg.find_kind(kind) {
        let r = slot.last_reply.lock();
        if r.token == token {
            *out = *r;
            return true;
        }
    }
    false
}

// ───────────────────────────────────────────────────────────────────────────
// Statistics / diagnostic.
// ───────────────────────────────────────────────────────────────────────────

pub static TOTAL_REQUESTS: AtomicU32 = AtomicU32::new(0);
pub static TOTAL_IRQS:     AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
pub struct DriverServerStats {
    pub active_drivers:   u32,
    pub total_requests:   u32,
    pub total_irqs:       u32,
    pub queue_high_water: u32,
}

pub fn stats() -> DriverServerStats {
    let reg = REGISTRY.lock();
    let mut active = 0u32;
    let mut high = 0u32;
    for s in reg.slots.iter() {
        if s.active.load(Ordering::Relaxed) {
            active += 1;
            let c = s.queue.lock().count as u32;
            if c > high { high = c; }
        }
    }
    DriverServerStats {
        active_drivers:   active,
        total_requests:   TOTAL_REQUESTS.load(Ordering::Relaxed),
        total_irqs:       TOTAL_IRQS.load(Ordering::Relaxed),
        queue_high_water: high,
    }
}
