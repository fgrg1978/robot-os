//! Kernel-global topology storage.
//!
//! `Topology<'static>` lives in a single static slot, set once during
//! boot via [`init`] and accessed read-only thereafter via [`get`].
//!
//! # Concurrency
//!
//! `Topology` is `Sync` (all fields are `Copy` and free of interior
//! mutability). Once `init` returns successfully, the slot is **immutable**;
//! repeated `init` calls return `Err(InitError::AlreadyInit)` so a buggy
//! caller cannot replace a loaded topology mid-run.
//!
//! On hardware with multiple CPUs, the BootCpu writes the slot before
//! AP CPUs come online; AP CPUs only read.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::types::Topology;

/// State of the topology slot.
const STATE_EMPTY: u8 = 0;
const STATE_INITIALISING: u8 = 1;
const STATE_READY: u8 = 2;

/// Errors that can occur during [`init`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InitError {
    /// `init` was called more than once.
    AlreadyInit,
    /// The provided topology failed admission_check.
    Admission(crate::AdmissionError),
}

impl From<crate::AdmissionError> for InitError {
    fn from(e: crate::AdmissionError) -> Self {
        InitError::Admission(e)
    }
}

// SAFETY: see `Sync` impl below. The slot uses an atomic state byte to
// publish writes; readers are gated through `get()` which observes the
// state with `Acquire` ordering.
struct TopologySlot {
    state: AtomicU8,
    cell: UnsafeCell<Option<Topology<'static>>>,
}

// SAFETY: All access is synchronised through `state` (acquire/release).
// The `UnsafeCell` is written exactly once, by the BootCpu, before any
// AP CPU is allowed to read; readers see `STATE_READY` only with an
// `Acquire` load, which establishes the happens-before edge.
unsafe impl Sync for TopologySlot {}

static SLOT: TopologySlot = TopologySlot {
    state: AtomicU8::new(STATE_EMPTY),
    cell: UnsafeCell::new(None),
};

/// Install the topology. Must be called exactly once, before any
/// task is spawned. Returns `Err` on a second call.
pub fn init(topology: Topology<'static>) -> Result<(), InitError> {
    // CAS from EMPTY → INITIALISING.
    SLOT.state
        .compare_exchange(
            STATE_EMPTY,
            STATE_INITIALISING,
            Ordering::Acquire,
            Ordering::Acquire,
        )
        .map_err(|_| InitError::AlreadyInit)?;
    // Validate before publishing.
    if let Err(e) = topology.admission_check() {
        // Roll the state back so the caller can panic / halt cleanly.
        SLOT.state.store(STATE_EMPTY, Ordering::Release);
        return Err(InitError::Admission(e));
    }
    // SAFETY: we hold the INITIALISING state exclusively (CAS above).
    unsafe {
        *SLOT.cell.get() = Some(topology);
    }
    SLOT.state.store(STATE_READY, Ordering::Release);
    Ok(())
}

/// Borrow the loaded topology. Returns `None` until [`init`] succeeds.
pub fn get() -> Option<&'static Topology<'static>> {
    if SLOT.state.load(Ordering::Acquire) != STATE_READY {
        return None;
    }
    // SAFETY: state == READY ⇒ the cell holds `Some(...)` written before
    // the Release in `init`. We hand out a shared reference; the cell
    // is never written again.
    unsafe {
        let opt: &'static Option<Topology<'static>> = &*SLOT.cell.get();
        opt.as_ref()
    }
}

/// Returns `true` once the topology has been initialised.
pub fn is_ready() -> bool {
    SLOT.state.load(Ordering::Acquire) == STATE_READY
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────
//
// Unit tests for the slot behaviour live in the host-side
// `topology-tests` crate because the `static SLOT` is global and we
// cannot exercise its CAS path twice in a single test binary without
// process restart. The host-side suite includes a single
// `init_then_get_then_double_init_fails` test.
