//! IRQ Binding — route PLIC interrupts to userspace tasks/ports/rings (F00.3).
//!
//! When a userspace driver binds an IRQ, the kernel's PLIC handler will
//! additionally queue an event to the bound port or ring, enabling
//! event-driven userspace driver architectures.

use crate::port::{port_queue_event, PortEvent};
use robot_os_sync::SpinLock;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of IRQ bindings system-wide.
pub const MAX_IRQ_BINDINGS: usize = 32;

/// Source type constants for PortEvent.
const PORT_SOURCE_TYPE_IRQ: u8 = 3;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Target for an IRQ binding.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IrqTarget {
    /// No binding (slot is free).
    None,
    /// Wake a specific task (same as existing wake_by_irq behavior).
    WakeTask(u32),
    /// Queue event to a port.
    QueueToPort(u32, u64), // (port_id, user_key)
}

/// An IRQ binding entry.
#[derive(Clone, Copy)]
pub struct IrqBinding {
    /// The PLIC IRQ number.
    pub irq: u32,
    /// Owner task that created this binding.
    pub owner_task: u32,
    /// Where to dispatch.
    pub target: IrqTarget,
    /// Whether this slot is active.
    pub active: bool,
}

impl IrqBinding {
    pub const fn empty() -> Self {
        Self {
            irq: 0,
            owner_task: 0,
            target: IrqTarget::None,
            active: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

/// Global IRQ binding table.
///
/// Protected by a single `SpinLock` (same shape as `port.rs`'s `PORTS`).
/// `irq_dispatch()` runs from the PLIC IRQ handler while `irq_bind()` /
/// `irq_unbind_all()` run from syscall context on any hart — was a bare
/// `static mut` with zero synchronization, so a bind/unbind racing the
/// PLIC handler mid-mutation could dispatch to a torn/half-written entry,
/// and two harts binding concurrently could both claim the same free slot.
/// Uses `lock_irqsave()` throughout for the same same-hart-deadlock reason
/// `PORTS` does — see its doc comment.
const EMPTY_IRQ_BINDING: IrqBinding = IrqBinding::empty();
static IRQ_BINDINGS: SpinLock<[IrqBinding; MAX_IRQ_BINDINGS]> =
    SpinLock::new([EMPTY_IRQ_BINDING; MAX_IRQ_BINDINGS]);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Bind an IRQ to a target. Returns 0 on success, -1 on failure.
pub fn irq_bind(irq: u32, owner_task: u32, target: IrqTarget) -> i32 {
    let mut bindings = IRQ_BINDINGS.lock_irqsave();
    // Check if this IRQ is already bound by this task
    for i in 0..MAX_IRQ_BINDINGS {
        if bindings[i].active && bindings[i].irq == irq && bindings[i].owner_task == owner_task {
            // Update existing binding
            bindings[i].target = target;
            return 0;
        }
    }
    // Find free slot
    for i in 0..MAX_IRQ_BINDINGS {
        if !bindings[i].active {
            bindings[i] = IrqBinding {
                irq,
                owner_task,
                target,
                active: true,
            };
            return 0;
        }
    }
    -1 // No free slots
}

/// Unbind all IRQ bindings for a task (called on task exit).
pub fn irq_unbind_all(owner_task: u32) {
    let mut bindings = IRQ_BINDINGS.lock_irqsave();
    for i in 0..MAX_IRQ_BINDINGS {
        if bindings[i].active && bindings[i].owner_task == owner_task {
            bindings[i] = IrqBinding::empty();
        }
    }
}

/// Called from the PLIC IRQ handler (kernel/src/main.rs) after an IRQ fires.
/// Dispatches to all bindings matching this IRQ number.
/// This is IN ADDITION to the existing wake_by_irq() call.
pub fn irq_dispatch(irq: u32) {
    // Collect matching targets under the lock, then dispatch after releasing
    // it — port_queue_event() takes PORTS' own lock, and there's no reverse
    // path (nothing under PORTS ever locks IRQ_BINDINGS), but there's no
    // reason to hold this lock any longer than needed to read the table.
    let mut targets = [IrqTarget::None; MAX_IRQ_BINDINGS];
    let mut n = 0;
    {
        let bindings = IRQ_BINDINGS.lock_irqsave();
        for i in 0..MAX_IRQ_BINDINGS {
            let binding = &bindings[i];
            if binding.active && binding.irq == irq {
                targets[n] = binding.target;
                n += 1;
            }
        }
    }
    for target in &targets[..n] {
        match *target {
            IrqTarget::WakeTask(_tid) => {
                // Already handled by wake_by_irq() in the scheduler.
                // No additional action needed here.
            }
            IrqTarget::QueueToPort(port_id, user_key) => {
                port_queue_event(port_id, PortEvent {
                    key: user_key,
                    source_type: PORT_SOURCE_TYPE_IRQ,
                    source_id: irq,
                });
            }
            IrqTarget::None => {}
        }
    }
}
