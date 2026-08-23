//! Event Ports — multiplex wait on multiple sources (AQ5).
//!
//! Inspired by Zircon `zx_port` and macOS `kqueue`.
//! A port aggregates events from channels, rings, timers, and IRQs.
//! `port_wait()` blocks until ANY bound source has an event.

use core::sync::atomic::{AtomicU32, Ordering};
use robot_os_sync::SpinLock;
pub use robot_os_limits::MAX_PORTS;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum sources bound to one port.
pub const PORT_MAX_SOURCES: usize = 16;

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
///
/// Protected by a single `SpinLock` covering the whole table (coarse-
/// grained, same shape as `channel.rs`'s `POOL`). Unlike `POOL` though,
/// this lock is shared with IRQ context: `port_queue_event()` is called
/// from `irq_bind::irq_dispatch()`, itself invoked from the PLIC IRQ
/// handler, while `port_bind()` / `port_poll()` / `port_create()` /
/// `port_destroy()` / `port_owner()` / `port_has_events()` all run in
/// syscall context. Every accessor below therefore uses `lock_irqsave()`,
/// never plain `lock()`: if a syscall on hart N took a plain lock and an
/// IRQ then fired on that same hart N before release, the IRQ handler
/// would spin forever on a lock whose holder can't run again until the
/// IRQ handler returns — a same-hart deadlock. `lock_irqsave()` avoids
/// this by disabling local interrupts for the duration of the critical
/// section (see `robot_os_sync::spinlock`).
const EMPTY_PORT: Port = Port::empty();
static PORTS: SpinLock<[Port; MAX_PORTS]> = SpinLock::new([EMPTY_PORT; MAX_PORTS]);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a new port. Returns port_id or None.
pub fn port_create(owner_task: usize) -> Option<u32> {
    let mut ports = PORTS.lock_irqsave();
    for i in 0..MAX_PORTS {
        if !ports[i].active {
            ports[i] = Port::empty();
            ports[i].owner_task = owner_task;
            ports[i].active = true;
            return Some(i as u32);
        }
    }
    None
}

/// Destroy a port.
pub fn port_destroy(port_id: u32) {
    if port_id as usize >= MAX_PORTS { return; }
    PORTS.lock_irqsave()[port_id as usize] = Port::empty();
}

/// Bind a source to a port.
pub fn port_bind(port_id: u32, kind: PortSourceKind, user_key: u64) -> bool {
    if port_id as usize >= MAX_PORTS { return false; }
    // IRQ-safe: PORTS is shared with port_queue_event(), which runs from
    // PLIC IRQ dispatch on the same hart. See the comment on `PORTS`.
    let mut ports = PORTS.lock_irqsave();
    let port = &mut ports[port_id as usize];
    if !port.active || port.source_count >= PORT_MAX_SOURCES { return false; }
    port.sources[port.source_count] = PortSource { kind, user_key };
    port.source_count += 1;
    true
}

/// Queue an event on a port (called by wake functions).
///
/// Runs in IRQ context (`irq_bind::irq_dispatch`, called from the PLIC
/// handler). Must use the same `lock_irqsave()` discipline as the
/// syscall-side accessors below — see the comment on `PORTS`.
pub fn port_queue_event(port_id: u32, event: PortEvent) {
    if port_id as usize >= MAX_PORTS { return; }
    let mut ports = PORTS.lock_irqsave();
    let port = &mut ports[port_id as usize];
    if !port.active { return; }
    let idx = port.pending_count.load(Ordering::Relaxed) as usize;
    if idx < PORT_MAX_SOURCES {
        port.pending[idx] = event;
        port.pending_count.store((idx + 1) as u32, Ordering::Release);
    }
}

/// Dequeue one event from a port. Returns None if no events pending.
pub fn port_poll(port_id: u32) -> Option<PortEvent> {
    if port_id as usize >= MAX_PORTS { return None; }
    // IRQ-safe: shares PORTS with port_queue_event(). See comment on `PORTS`.
    let mut ports = PORTS.lock_irqsave();
    let port = &mut ports[port_id as usize];
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

/// Check if a port has pending events.
pub fn port_has_events(port_id: u32) -> bool {
    if port_id as usize >= MAX_PORTS { return false; }
    let ports = PORTS.lock_irqsave();
    let port = &ports[port_id as usize];
    port.active && port.pending_count.load(Ordering::Relaxed) > 0
}

/// Release every port owned by `tid` — task-exit hook (IPC-3).
///
/// **WHY this exists (IPC-3):** `PORTS` is a fixed `MAX_PORTS`-entry BSS
/// table and nothing ever reclaimed it. `task_release_all` called only
/// `handle_revoke_all`, `cap_store::reset` and `shm_release_all`, so a task
/// that died holding a port burned that slot for the life of the board:
/// `port_create` scans for `!active` and simply starts returning `None`.
/// There is no diagnostic for that — a robot that has restarted a crashing
/// driver `MAX_PORTS` times silently loses the ability to create event ports
/// at all.
///
/// It is also a confidentiality fix, not just a leak fix. `port_access_ok`
/// in `crates/syscall/src/dispatch.rs` authorizes on
/// `port_owner(id) == current_tid`, and `owner_task` holds a **TID**, which
/// this kernel recycles. Leave a dead task's port active and the next task to
/// draw that TID inherits it wholesale — including its bound sources and the
/// opaque `user_key`s the original owner used to correlate events. Same
/// inheritance hazard `task_exit`'s own comment describes for the handle
/// table.
///
/// No wakes are needed here: a task blocked in `SYS_PORT_WAIT` waits on its
/// **own** port (that is what `port_access_ok` enforces), so the only task
/// that could be sleeping on these ports is the one that is dying.
///
/// Cost: exit path only, one pass over `MAX_PORTS` under the lock the table
/// already uses. Nothing added to `port_poll` / `port_queue_event`.
pub fn port_release_all(tid: u32) {
    let mut ports = PORTS.lock_irqsave();
    for i in 0..MAX_PORTS {
        // `owner_task` is a TID widened to `usize` (`port_create(tid as usize)`
        // at every call site: `SYS_PORT_CREATE` in dispatch.rs and
        // `port_create_cap` here). Compare in `usize` so the `usize::MAX`
        // "unowned" sentinel can never alias a real u32 TID.
        if ports[i].active && ports[i].owner_task == tid as usize {
            ports[i] = Port::empty();
        }
    }
}

/// Get the owner task of a port.
pub fn port_owner(port_id: u32) -> usize {
    if port_id as usize >= MAX_PORTS { return usize::MAX; }
    PORTS.lock_irqsave()[port_id as usize].owner_task
}

// ──────────────────────────────────────────────────────────────────────────
// Cap<Port> typed wrappers (RFC-0003 W5)
// ──────────────────────────────────────────────────────────────────────────
//
// Mirrors the pattern established by `channel_send_cap` in W3:
// the typed entry validates the cap against the calling task's
// `CapTable`, then delegates to the existing untyped logic.

/// Errors returned by the typed `port_*_cap` functions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PortCapError {
    /// Capability dereference failed (stale, wrong kind, missing perms).
    Cap(crate::cap::CapError),
    /// Underlying port slot is full / port slot table exhausted.
    Full,
    /// No events pending for receive.
    Empty,
    /// Port doesn't exist or isn't active.
    Closed,
}

impl From<crate::cap::CapError> for PortCapError {
    fn from(e: crate::cap::CapError) -> Self {
        Self::Cap(e)
    }
}

/// Typed `port_create`: allocates a port and mints a `Cap<Port>`
/// into the calling task's cap-table with `RW` permissions.
///
/// Returns the cap handle on success, or `None` if either the
/// port table or the cap-table is full.
pub fn port_create_cap(tid: u32) -> Option<crate::cap::Cap<crate::cap::targets::Port>> {
    let port_id = port_create(tid as usize)?;
    crate::cap_store::grant::<crate::cap::targets::Port>(
        tid,
        crate::cap::CapPerms::RW,
        port_id,
    )
}

/// Typed `port_poll`: validates the cap (requires `READ`) and
/// dequeues one event.
pub fn port_poll_cap(
    table: &crate::cap::CapTable,
    cap: crate::cap::Cap<crate::cap::targets::Port>,
) -> Result<PortEvent, PortCapError> {
    let port_id = table.get(cap, crate::cap::CapPerms::READ)?;
    port_poll(port_id).ok_or(PortCapError::Empty)
}

/// Typed `port_destroy`: validates the cap (requires `WRITE`), frees the
/// port slot, **and revokes the cap**.
///
/// **WHY the revoke is here (W3-F5):** `port_create` allocates the
/// first free index, and destroying a port does not touch the *cap table*
/// slot's generation — which is the only thing `CapTable::get` validates.
/// So a cap left live after its port is destroyed keeps dereferencing to the
/// same integer id, and the next `port_create` by any task hands that id
/// straight back out. Task A destroys port 0, task B creates port 0, A's
/// stale cap now polls and destroys B's port. Revoking inside the destroy is
/// what makes cap lifetime track resource lifetime; the previous doc pushed
/// that duty onto callers and no caller did it.
pub fn port_destroy_cap(
    table: &mut crate::cap::CapTable,
    cap: crate::cap::Cap<crate::cap::targets::Port>,
) -> Result<(), PortCapError> {
    let port_id = table.get(cap, crate::cap::CapPerms::WRITE)?;
    port_destroy(port_id);
    table.revoke(cap);
    Ok(())
}

/// Wipe the port table. Host-test hygiene only — the suite shares one static
/// `PORTS`. Never built into the kernel: a reachable "destroy every port on
/// the board" entry point is the DoS `port_access_ok` exists to prevent.
#[cfg(test)]
pub fn __port_reset_for_tests() {
    let mut ports = PORTS.lock_irqsave();
    for i in 0..MAX_PORTS {
        ports[i] = Port::empty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static SERIAL: Mutex<()> = Mutex::new(());

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        __port_reset_for_tests();
        g
    }

    #[test]
    fn release_all_frees_only_the_dying_tasks_ports() {
        let _g = setup();
        let a = port_create(1).unwrap();
        let b = port_create(2).unwrap();
        let c = port_create(1).unwrap();
        assert!(port_bind(a, PortSourceKind::Irq(7), 0xF00D));

        port_release_all(1);

        // Freed: slot back in the pool, owner reset to the unowned sentinel.
        assert_eq!(port_owner(a), usize::MAX);
        assert_eq!(port_owner(c), usize::MAX);
        // A stale bind must not survive the owner — `port_queue_event` from
        // IRQ context would otherwise deliver a dead task's events.
        assert!(!port_bind(a, PortSourceKind::Irq(7), 0xF00D));
        // The other task's port is untouched.
        assert_eq!(port_owner(b), 2);
        assert!(port_bind(b, PortSourceKind::Irq(8), 1));
    }

    #[test]
    fn released_slots_are_reusable_and_carry_no_stale_events() {
        let _g = setup();
        let p = port_create(1).unwrap();
        port_queue_event(p, PortEvent { key: 0xDEAD, source_type: 3, source_id: 7 });
        assert!(port_has_events(p));

        port_release_all(1);

        let reused = port_create(9).unwrap();
        assert_eq!(reused, p);
        assert_eq!(port_owner(reused), 9);
        // The new owner must not inherit the dead task's queued events — the
        // `user_key` is how the old owner correlated them.
        assert!(!port_has_events(reused));
        assert!(port_poll(reused).is_none());
    }

    #[test]
    fn exhausting_the_table_then_killing_the_owner_makes_ports_creatable_again() {
        let _g = setup();
        for i in 0..MAX_PORTS {
            assert!(port_create(1).is_some(), "slot {i}");
        }
        // The permanent-failure state before IPC-3: nothing reclaimed ports,
        // so MAX_PORTS driver restarts killed port creation for good.
        assert!(port_create(2).is_none());

        port_release_all(1);

        assert!(port_create(2).is_some());
    }

    #[test]
    fn release_all_ignores_uninvolved_tasks() {
        let _g = setup();
        let a = port_create(1).unwrap();
        port_release_all(42);
        assert_eq!(port_owner(a), 1);
        // `usize::MAX` is the unowned sentinel; a u32 TID can never equal it,
        // so passing an absurd TID must not sweep inactive slots into a state
        // that looks owned.
        port_release_all(u32::MAX);
        assert_eq!(port_owner(a), 1);
        assert_eq!(port_owner(MAX_PORTS as u32 - 1), usize::MAX);
    }

    #[test]
    fn out_of_range_and_boundary_port_ids_never_panic() {
        let _g = setup();
        for id in [MAX_PORTS as u32, MAX_PORTS as u32 + 1, u32::MAX, u32::MAX - 1] {
            assert_eq!(port_owner(id), usize::MAX);
            assert!(!port_bind(id, PortSourceKind::Irq(1), 0));
            assert!(port_poll(id).is_none());
            assert!(!port_has_events(id));
            port_queue_event(id, PortEvent::default());
            port_destroy(id);
        }
        // Last valid index must still work.
        let last = MAX_PORTS as u32 - 1;
        assert!(!port_has_events(last));
        port_destroy(last);
    }

    #[test]
    fn binding_is_bounded_by_port_max_sources() {
        let _g = setup();
        let p = port_create(1).unwrap();
        for i in 0..PORT_MAX_SOURCES {
            assert!(port_bind(p, PortSourceKind::Irq(i as u32), i as u64));
        }
        assert!(!port_bind(p, PortSourceKind::Irq(99), 99));
    }

    #[test]
    fn queued_events_are_bounded_and_polled_in_order() {
        let _g = setup();
        let p = port_create(1).unwrap();
        for i in 0..PORT_MAX_SOURCES + 4 {
            port_queue_event(p, PortEvent { key: i as u64, source_type: 1, source_id: 0 });
        }
        for i in 0..PORT_MAX_SOURCES {
            assert_eq!(port_poll(p).unwrap().key, i as u64);
        }
        assert!(port_poll(p).is_none());
    }
}
