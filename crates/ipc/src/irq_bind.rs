//! IRQ Binding — route PLIC interrupts to userspace tasks/ports/rings (F00.3).
//!
//! When a userspace driver binds an IRQ, the kernel's PLIC handler will
//! additionally queue an event to the bound port or ring, enabling
//! event-driven userspace driver architectures.

use crate::port::{port_queue_event, PortEvent};

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

static mut IRQ_BINDINGS: [IrqBinding; MAX_IRQ_BINDINGS] = {
    const EMPTY: IrqBinding = IrqBinding::empty();
    [EMPTY; MAX_IRQ_BINDINGS]
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Bind an IRQ to a target. Returns 0 on success, -1 on failure.
pub fn irq_bind(irq: u32, owner_task: u32, target: IrqTarget) -> i32 {
    unsafe {
        // Check if this IRQ is already bound by this task
        for i in 0..MAX_IRQ_BINDINGS {
            if IRQ_BINDINGS[i].active && IRQ_BINDINGS[i].irq == irq
                && IRQ_BINDINGS[i].owner_task == owner_task
            {
                // Update existing binding
                IRQ_BINDINGS[i].target = target;
                return 0;
            }
        }
        // Find free slot
        for i in 0..MAX_IRQ_BINDINGS {
            if !IRQ_BINDINGS[i].active {
                IRQ_BINDINGS[i] = IrqBinding {
                    irq,
                    owner_task,
                    target,
                    active: true,
                };
                return 0;
            }
        }
    }
    -1 // No free slots
}

/// Unbind all IRQ bindings for a task (called on task exit).
pub fn irq_unbind_all(owner_task: u32) {
    unsafe {
        for i in 0..MAX_IRQ_BINDINGS {
            if IRQ_BINDINGS[i].active && IRQ_BINDINGS[i].owner_task == owner_task {
                IRQ_BINDINGS[i] = IrqBinding::empty();
            }
        }
    }
}

/// Called from the PLIC IRQ handler (kernel/src/main.rs) after an IRQ fires.
/// Dispatches to all bindings matching this IRQ number.
/// This is IN ADDITION to the existing wake_by_irq() call.
pub fn irq_dispatch(irq: u32) {
    unsafe {
        for i in 0..MAX_IRQ_BINDINGS {
            let binding = &IRQ_BINDINGS[i];
            if !binding.active || binding.irq != irq {
                continue;
            }
            match binding.target {
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
}
