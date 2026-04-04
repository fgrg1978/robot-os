//! Event Ports — multiplex wait on multiple sources (AQ5).
//!
//! Inspired by Zircon `zx_port` and macOS `kqueue`.
//! A port aggregates events from channels, rings, timers, and IRQs.
//! `port_wait()` blocks until ANY bound source has an event.

use core::sync::atomic::{AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum sources bound to one port.
pub const PORT_MAX_SOURCES: usize = 16;
/// Maximum ports system-wide.
pub const MAX_PORTS: usize = 32;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// What kind of source is bound to the port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PortSourceKind {
    None,
    Channel(u32),    // channel handle
    Ring(u32),       // ring buffer ID
    Irq(u32),        // PLIC IRQ number
    Timer(u64),      // absolute deadline (CLINT ticks)
}

/// A source bound to a port.
#[derive(Clone, Copy)]
pub struct PortSource {
    pub kind: PortSourceKind,
    pub user_key: u64,    // opaque key returned with events
}

/// An event delivered from a port.
#[derive(Clone, Copy, Default)]
pub struct PortEvent {
    pub key: u64,         // user_key from the source
    pub source_type: u8,  // 1=channel, 2=ring, 3=irq, 4=timer
    pub source_id: u32,
}

/// Kernel state for one port.
pub struct Port {
    pub sources: [PortSource; PORT_MAX_SOURCES],
    pub source_count: usize,
    pub owner_task: usize,
    pub active: bool,
    /// Pending events (written by wake, read by port_wait).
    pub pending: [PortEvent; PORT_MAX_SOURCES],
    pub pending_count: AtomicU32,
}

impl Port {
    pub const fn empty() -> Self {
        const EMPTY_SRC: PortSource = PortSource {
            kind: PortSourceKind::None,
            user_key: 0,
        };
        const EMPTY_EVT: PortEvent = PortEvent { key: 0, source_type: 0, source_id: 0 };
        Self {
            sources: [EMPTY_SRC; PORT_MAX_SOURCES],
            source_count: 0,
            owner_task: usize::MAX,
            active: false,
            pending: [EMPTY_EVT; PORT_MAX_SOURCES],
            pending_count: AtomicU32::new(0),
        }
    }
}

/// Global port array.
static mut PORTS: [Port; MAX_PORTS] = {
    const EMPTY: Port = Port::empty();
    [EMPTY; MAX_PORTS]
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a new port. Returns port_id or None.
pub fn port_create(owner_task: usize) -> Option<u32> {
    unsafe {
        for i in 0..MAX_PORTS {
            if !PORTS[i].active {
                PORTS[i] = Port::empty();
                PORTS[i].owner_task = owner_task;
                PORTS[i].active = true;
                return Some(i as u32);
            }
        }
    }
    None
}

/// Destroy a port.
pub fn port_destroy(port_id: u32) {
    if port_id as usize >= MAX_PORTS { return; }
    unsafe { PORTS[port_id as usize] = Port::empty(); }
}

/// Bind a source to a port.
pub fn port_bind(port_id: u32, kind: PortSourceKind, user_key: u64) -> bool {
    if port_id as usize >= MAX_PORTS { return false; }
    unsafe {
        let port = &mut PORTS[port_id as usize];
        if !port.active || port.source_count >= PORT_MAX_SOURCES { return false; }
        port.sources[port.source_count] = PortSource { kind, user_key };
        port.source_count += 1;
        true
    }
}

/// Queue an event on a port (called by wake functions).
pub fn port_queue_event(port_id: u32, event: PortEvent) {
    if port_id as usize >= MAX_PORTS { return; }
    unsafe {
        let port = &mut PORTS[port_id as usize];
        if !port.active { return; }
        let idx = port.pending_count.load(Ordering::Relaxed) as usize;
        if idx < PORT_MAX_SOURCES {
            port.pending[idx] = event;
            port.pending_count.store((idx + 1) as u32, Ordering::Release);
        }
    }
}

/// Dequeue one event from a port. Returns None if no events pending.
pub fn port_poll(port_id: u32) -> Option<PortEvent> {
    if port_id as usize >= MAX_PORTS { return None; }
    unsafe {
        let port = &mut PORTS[port_id as usize];
        if !port.active { return None; }
        let count = port.pending_count.load(Ordering::Acquire);
        if count == 0 { return None; }
        let event = port.pending[0];
        // Shift remaining events down
        for i in 1..count as usize {
            port.pending[i - 1] = port.pending[i];
        }
        port.pending_count.store(count - 1, Ordering::Release);
        Some(event)
    }
}

/// Check if a port has pending events.
pub fn port_has_events(port_id: u32) -> bool {
    if port_id as usize >= MAX_PORTS { return false; }
    unsafe {
        let port = &PORTS[port_id as usize];
        port.active && port.pending_count.load(Ordering::Relaxed) > 0
    }
}

/// Get the owner task of a port.
pub fn port_owner(port_id: u32) -> usize {
    if port_id as usize >= MAX_PORTS { return usize::MAX; }
    unsafe { PORTS[port_id as usize].owner_task }
}
