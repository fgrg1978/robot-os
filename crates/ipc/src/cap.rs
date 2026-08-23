//! Typed capability wrapper — RFC-0003.
//!
//! `Cap<T>` is a `#[repr(transparent)]` newtype over the wire-format
//! `CapHandle` from [`robot_os_abi::cap`]. The type parameter `T` is a
//! marker carrying the kind at compile time — so `Cap<Channel>` and
//! `Cap<Sensor>` are distinct types that the type system refuses to
//! interchange.
//!
//! ## Why typed?
//!
//! Today (W1) the kernel still accepts integer handles end-to-end — see
//! [`super::handle`]. From W3 onwards new syscalls take `Cap<T>` directly:
//!
//! ```ignore
//! // The old, kind-erased shape:
//! pub fn sys_chan_send(handle: u32, data_ptr: *const u8, len: usize) -> i64;
//!
//! // The new, kind-typed shape:
//! pub fn sys_chan_send(cap: Cap<Channel>, data_ptr: *const u8, len: usize) -> Result<usize, Errno>;
//! ```
//!
//! A `Cap<Channel>` cannot be passed where `Cap<Sensor>` is expected;
//! that's a compile error, not a runtime check.
//!
//! ## Forgery resistance
//!
//! On **dereference** (`cap_table::get(&cap)`) the kernel verifies:
//!
//! 1. The slot index is in range.
//! 2. The slot is occupied (`generation > 0`).
//! 3. The handle's generation matches the slot's generation.
//! 4. The handle's kind tag matches `T::KIND`.
//! 5. The handle's permission bits ⊆ slot's permission bits.
//!
//! Any failure returns [`Errno::ECAPSTALE`], [`Errno::ECAPKIND`], or
//! [`Errno::ECAPPERMS`].
//!
//! ## Generation rollover
//!
//! The generation is 8 bits. After 256 reuse cycles per slot, generations
//! wrap. We mitigate by:
//!
//! - Skipping generation `0` (treated as "empty slot").
//! - The slot index is included in the handle, so even after a wrap a
//!   *different* slot's generation is independent.
//! - For an attacker to forge, they would need to predict both the slot
//!   *and* the generation post-rollover. With 16 384 slots × 255
//!   generations = ~4 million unique handles per task, blind guessing has
//!   ~2⁻²² success per try; combined with the kind tag ⇒ ~2⁻²⁶.
//!
//! Phase 4: extend to 16-bit generation if blind-forgery analysis warrants.

use core::marker::PhantomData;
use core::sync::atomic::{AtomicU8, Ordering};

pub use robot_os_abi::cap::{CapHandle, CapKind, CapPerms, CAP_NULL};
use wcet_macro::wcet;

// ──────────────────────────────────────────────────────────────────────────
// Graded degraded mode — capability containment + speed ceiling (RFC-0037)
// ──────────────────────────────────────────────────────────────────────────
//
// Generalises the binary RFC-0036 armed/cleared flag into an ordered level
// so the brain can select a graded restriction. Higher = more restrictive.
//
// The level taxonomy and speed-ceiling constants live in the dep-free leaf
// crate `robot_os_degrade_policy` so that motor-actuation policy does not
// pollute the TCB. Re-exported here so callers that already reference
// `robot_os_ipc::cap::DEGRADE_LEVEL_*` continue to resolve without change.
//
// The level is *sticky*: it stays until the brain sends a new `PKT_SEMANTIC_LEVEL`
// or a `PKT_DEGRADE`/`MODE_CMD`. Fail-closed-on-link-loss is provided by the
// existing motor watchdog (500 ms), not by a TTL here — same pattern as
// `CMD_LOW_CONF` in `safety.rs`.
//
// Constrain-only: this can only deny / slow, never grant — a hallucinating brain
// can at worst over-contain (fail-safe). It is a global (set at packet ingest,
// read at each chokepoint); coarse but correct and conservative.

// Level taxonomy — single source of truth in the leaf; re-exported here.
pub use robot_os_degrade_policy::{
    DEGRADE_LEVEL_FULL,
    DEGRADE_LEVEL_CAUTIOUS,
    DEGRADE_LEVEL_SLOW,
    DEGRADE_LEVEL_CONTAINED,
    DEGRADE_LEVEL_MAX,
    // Speed-ceiling constants — same leaf, same motivation.
    DEGRADE_SPEED_CAP_FULL_PCT,
    DEGRADE_SPEED_CAP_CAUTIOUS_PCT,
    DEGRADE_SPEED_CAP_SLOW_PCT,
    DEGRADE_SPEED_CAP_CONTAINED_PCT,
    // Mapping function re-exported under the old name for backward compat;
    // behavior/safety.rs now calls robot_os_degrade_policy::level_cap_pct
    // directly, but any other callers of the old name still resolve.
    level_cap_pct as degrade_level_cap_pct,
};

static DEGRADE_LEVEL: AtomicU8 = AtomicU8::new(DEGRADE_LEVEL_FULL);

/// Set the graded degrade level (RFC-0037). Any value greater than
/// `DEGRADE_LEVEL_MAX` is clamped to `DEGRADE_LEVEL_CONTAINED` (fail-closed on
/// out-of-range wire input — never panic).
pub fn degrade_level_set(level: u8) {
    let clamped = level.min(DEGRADE_LEVEL_MAX);
    DEGRADE_LEVEL.store(clamped, Ordering::Release);
}

/// Current graded degrade level. `DEGRADE_LEVEL_FULL` (0) means normal
/// operation; higher values impose progressively tighter constraints.
#[inline]
pub fn degrade_level() -> u8 {
    DEGRADE_LEVEL.load(Ordering::Acquire)
}

// ── RFC-0036 back-compat shim ─────────────────────────────────────────────
//
// All existing callers (`PKT_DEGRADE` handler, `MODE_CMD` handler, Kani proofs,
// unit tests) continue to use `degraded_set` / `degraded_active` unchanged.
// Internally they now delegate to the graded level so the two APIs stay
// consistent — `degraded_active() == true` iff `degrade_level() == CONTAINED`.

/// Arm or clear degraded mode (RFC-0036 back-compat). `true` → CONTAINED;
/// `false` → FULL. Use `degrade_level_set` for graded control.
pub fn degraded_set(on: bool) {
    if on {
        degrade_level_set(DEGRADE_LEVEL_CONTAINED);
    } else {
        degrade_level_set(DEGRADE_LEVEL_FULL);
    }
}

/// Whether full containment (RFC-0036) is currently active.
/// Returns `true` only at `DEGRADE_LEVEL_CONTAINED`; CAUTIOUS and SLOW do NOT
/// trip cap-denial — they only clamp speed via `motor_envelope`.
#[inline]
pub fn degraded_active() -> bool {
    degrade_level() == DEGRADE_LEVEL_CONTAINED
}

// ──────────────────────────────────────────────────────────────────────────
// Cap<T> — typed handle
// ──────────────────────────────────────────────────────────────────────────

/// Marker trait implemented by every capability target type. Carries the
/// `CapKind` discriminant at compile time.
///
/// New targets are added as zero-sized marker types in [`mod targets`].
pub trait CapTarget: 'static {
    /// The wire-format `CapKind` tag for this target type.
    const KIND: CapKind;
}

/// A typed capability handle.
///
/// `Cap<T>` is `#[repr(transparent)]` over [`CapHandle`] so that it has
/// the same ABI as the wire format. The `PhantomData<T>` is zero-sized.
#[repr(transparent)]
pub struct Cap<T: CapTarget> {
    raw: CapHandle,
    _phantom: PhantomData<fn() -> T>,
}

// Manual `Clone` / `Copy` so we can stay generic over `T` without
// requiring `T: Clone`.
impl<T: CapTarget> Clone for Cap<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: CapTarget> Copy for Cap<T> {}

impl<T: CapTarget> PartialEq for Cap<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl<T: CapTarget> Eq for Cap<T> {}

impl<T: CapTarget> Cap<T> {
    /// The null typed cap.
    pub const NULL: Self = Self {
        raw: CAP_NULL,
        _phantom: PhantomData,
    };

    /// Construct from a wire-format `CapHandle`. Does **not** verify the
    /// kind matches `T`; that check happens on dereference via
    /// `cap_table::get`.
    #[inline]
    pub const fn from_raw(raw: CapHandle) -> Self {
        Self { raw, _phantom: PhantomData }
    }

    /// Get the underlying wire-format handle.
    #[inline]
    pub const fn raw(self) -> CapHandle {
        self.raw
    }

    /// Returns `true` iff this is the null cap.
    #[inline]
    pub const fn is_null(self) -> bool {
        self.raw.is_null()
    }
}

impl<T: CapTarget> core::fmt::Debug for Cap<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Cap<{}>({:?})", core::any::type_name::<T>(), self.raw)
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Marker types for each capability target
// ──────────────────────────────────────────────────────────────────────────

/// Zero-sized marker types used as the `T` in `Cap<T>`.
pub mod targets {
    use super::{CapKind, CapTarget};

    macro_rules! target {
        ($name:ident, $kind:ident, $doc:literal) => {
            #[doc = $doc]
            pub struct $name;
            impl CapTarget for $name {
                const KIND: CapKind = CapKind::$kind;
            }
        };
    }

    target!(Channel,    Channel,    "IPC channel endpoint.");
    target!(Shm,        Shm,        "Shared memory region.");
    target!(Port,       Port,       "Event port.");
    target!(Irq,        Irq,        "Hardware IRQ binding.");
    target!(MmioRegion, MmioRegion, "MMIO region.");
    target!(IoRing,     IoRing,     "IO ring.");
    target!(Sensor,     Sensor,     "Sensor descriptor.");
    target!(Gpio,       Gpio,       "GPIO pin.");
    target!(I2c,        I2c,        "I2C bus + address.");
    target!(Pwm,        Pwm,        "PWM channel.");
    target!(Motor,      Motor,      "Motor channel.");
    target!(File,       File,       "File descriptor.");
    target!(Socket,     Socket,     "Socket descriptor.");
    target!(Task,       Task,       "Process / task handle.");
    target!(AiSession,  AiSession,  "AI inference session.");
}

// ──────────────────────────────────────────────────────────────────────────
// Per-task cap table — kernel-internal
// ──────────────────────────────────────────────────────────────────────────

/// Maximum cap-table slots per task. RFC-0003 sets this in `SCHED.TOML`
/// per partition; the build-time constant is the *upper bound*.
pub const MAX_CAPS_PER_TASK: usize = 256;

/// One slot in a per-task cap table.
///
/// **Occupancy** is tracked by `kind`: `CapKind::Null` means the slot
/// is free for reuse. **Generation** is a monotonic per-slot counter
/// that survives revoke; it is *only* reset when the slot has never
/// been granted. This separation is what makes a stale cap impossible
/// to confuse with a freshly granted cap on the same slot.
#[derive(Clone, Copy)]
pub struct CapSlot {
    /// Kind of the resource this slot points to. `CapKind::Null` ⇒ slot
    /// is empty.
    pub kind: CapKind,
    /// Permissions granted on this slot.
    pub perms: CapPerms,
    /// Monotonic generation counter, always non-zero on a granted slot.
    /// Survives revoke so that a re-grant gets a fresh generation and
    /// the previous holder's `Cap` is detectably stale. Wraps from
    /// `255` → `1` (skipping `0` to keep the wire format unambiguous).
    pub generation: u8,
    /// Resource-specific opaque pointer (channel ID, shm ID, etc.). The
    /// resource subsystem owns interpreting this value.
    pub resource: u32,
}

impl CapSlot {
    /// Construct a fresh, never-granted slot.
    pub const EMPTY: Self = Self {
        kind: CapKind::Null,
        perms: CapPerms::NONE,
        generation: 0,
        resource: 0,
    };

    /// Returns `true` iff the slot currently holds a granted cap.
    #[inline]
    pub const fn is_occupied(&self) -> bool {
        !matches!(self.kind, CapKind::Null)
    }
}

/// Per-task cap table.
pub struct CapTable {
    slots: [CapSlot; MAX_CAPS_PER_TASK],
}

impl CapTable {
    /// Build a fresh empty cap table.
    pub const fn empty() -> Self {
        Self {
            slots: [CapSlot::EMPTY; MAX_CAPS_PER_TASK],
        }
    }

    /// Grant a fresh cap, returning a typed handle.
    ///
    /// Returns `None` if every slot is occupied (`EMFILE`).
    pub fn grant<T: CapTarget>(&mut self, perms: CapPerms, resource: u32) -> Option<Cap<T>> {
        // Single allocation path shared with `grant_raw` on purpose: the
        // generation invariant (never 0, always bumped on reuse) is what makes
        // a stale handle detectable, and two copies of that arithmetic is how
        // it silently drifts. `T::KIND` is never `Null`, so `grant_raw`'s
        // Null-kind refusal below can never fire on this path.
        self.grant_raw(T::KIND, perms, resource).map(Cap::from_raw)
    }

    /// Kind-erased `grant`, for the delegation path.
    ///
    /// **WHY kind-erased (W3-F10).** `SYS_CAP_GRANT` receives a `u32` wire
    /// handle from ring 3 and has to mint a copy of *whatever kind that slot
    /// holds* into another task's table. There is no `T` at that point: the
    /// kind is runtime data read out of the grantor's own slot, so a generic
    /// `grant<T>` cannot express it without a 15-arm match that would have to
    /// be re-edited every time a `CapKind` is added.
    ///
    /// Refuses `CapKind::Null`: occupancy is encoded *as* `kind != Null`
    /// ([`CapSlot::is_occupied`]), so writing a Null-kind slot would produce
    /// an entry that reads as free while `bump_generation` has already moved
    /// on — a torn slot, and the one input a caller passing runtime kind bits
    /// could actually supply.
    pub fn grant_raw(
        &mut self,
        kind: CapKind,
        perms: CapPerms,
        resource: u32,
    ) -> Option<CapHandle> {
        if matches!(kind, CapKind::Null) {
            return None;
        }
        let slot = self.allocate_slot()?;
        let slot_idx = slot as usize;
        let next_gen = self.bump_generation(slot_idx);
        self.slots[slot_idx] = CapSlot {
            kind,
            perms,
            generation: next_gen,
            resource,
        };
        Some(CapHandle::pack(kind, perms, next_gen, slot))
    }

    /// Kind-erased dereference: validate a wire handle against this table and
    /// report what the slot actually holds.
    ///
    /// **WHY this is not `get` with the kind check dropped.** `get` is the
    /// authorization path and its check order is load-bearing — the four
    /// rejections are ordered Stale → WrongKind → MissingPerms → Contained so
    /// that a caller learns as little as possible about slots it does not
    /// hold, and `degraded_mode_contains_writes` pins that order. This one
    /// answers a different question ("what is in the slot the grantor named,
    /// so the delegation logic can attenuate it?") and deliberately does
    /// **not** take a `need` mask: the permission comparison for a delegation
    /// is `requested ⊆ held`, which the caller does, not `held ⊇ need`.
    ///
    /// It keeps every forgery check `get` makes, including the kind tag:
    /// the handle's embedded 4-bit kind must match the slot's, so a caller
    /// cannot relabel one of its own caps on the way through.
    ///
    /// Returns `(kind, perms, resource)` of the named slot.
    pub fn inspect_raw(
        &self,
        raw: CapHandle,
    ) -> Result<(CapKind, CapPerms, u32), CapError> {
        if raw.is_null() {
            return Err(CapError::Stale);
        }
        let slot_idx = raw.slot() as usize;
        // The wire format carries a 16-bit slot field but a table only has
        // `MAX_CAPS_PER_TASK` (256) slots, so 65 280 of the 65 536 encodable
        // values are out of range. Bounds-check before indexing: with
        // `panic = "abort"` an out-of-range index is a board reset, and this
        // integer comes straight from ring 3.
        if slot_idx >= MAX_CAPS_PER_TASK {
            return Err(CapError::Stale);
        }
        let slot = &self.slots[slot_idx];
        if !slot.is_occupied() {
            return Err(CapError::Stale);
        }
        if slot.generation != raw.generation() {
            return Err(CapError::Stale);
        }
        if slot.kind as u8 != raw.kind() {
            return Err(CapError::WrongKind);
        }
        Ok((slot.kind, slot.perms, slot.resource))
    }

    /// Look up a typed cap and verify kind + generation + permissions.
    ///
    /// Returns the resource ID if the cap is valid; otherwise an error.
    #[wcet(20_us)]
    pub fn get<T: CapTarget>(
        &self,
        cap: Cap<T>,
        need: CapPerms,
    ) -> Result<u32, CapError> {
        if cap.is_null() {
            return Err(CapError::Stale);
        }
        let raw = cap.raw();
        let slot_idx = raw.slot() as usize;
        if slot_idx >= MAX_CAPS_PER_TASK {
            return Err(CapError::Stale);
        }
        let slot = &self.slots[slot_idx];
        if !slot.is_occupied() {
            return Err(CapError::Stale);
        }
        if slot.generation != raw.generation() {
            return Err(CapError::Stale);
        }
        if slot.kind != T::KIND {
            return Err(CapError::WrongKind);
        }
        if !slot.perms.contains(need) {
            return Err(CapError::MissingPerms);
        }
        // RFC-0036: degraded-mode containment. Applied AFTER the forgery checks
        // (a stale/wrong-kind/under-permissioned cap still fails first, so the
        // Kani forgery proofs are unchanged). Denies write/actuation through any
        // user-task cap; READ stays live. Skipped entirely when not degraded.
        if need.contains(CapPerms::WRITE) && degraded_active() {
            return Err(CapError::Contained);
        }
        Ok(slot.resource)
    }

    /// Revoke a cap. Subsequent dereferences return `Stale`.
    ///
    /// **Preserves** the slot's generation counter so that a future
    /// `grant` on the same slot yields a *different* generation —
    /// without that, a freshly minted cap could collide with a stale
    /// one on the same slot. Only `kind` / `perms` / `resource` are
    /// cleared; the next `grant` bumps `generation` further.
    ///
    /// Idempotent: revoking an already-empty slot is a no-op.
    pub fn revoke<T: CapTarget>(&mut self, cap: Cap<T>) {
        if cap.is_null() {
            return;
        }
        let raw = cap.raw();
        let slot_idx = raw.slot() as usize;
        if slot_idx >= MAX_CAPS_PER_TASK {
            return;
        }
        let slot = &mut self.slots[slot_idx];
        if slot.is_occupied() && slot.generation == raw.generation() {
            slot.kind = CapKind::Null;
            slot.perms = CapPerms::NONE;
            slot.resource = 0;
            // generation deliberately preserved; bumped on next grant.
        }
    }

    /// Count occupied slots — for diagnostics and quota enforcement.
    pub fn occupied(&self) -> usize {
        self.slots.iter().filter(|s| s.is_occupied()).count()
    }

    /// Does this table hold **any** occupied cap of `kind` whose permissions
    /// are a superset of `need`?
    ///
    /// **WHY this exists (W3-F9):** `SYS_DRV_INVOKE` needs to answer "may
    /// this client call this driver?" from `DriverManifest::required_perms`,
    /// which is a permission mask with no slot index attached — the client
    /// does not pass a cap handle, so there is nothing to dereference. This
    /// is a *presence* test over the caller's own table, not a dereference,
    /// and it is deliberately weaker than `get`: it proves the caller was
    /// granted authority over the subsystem, not over one specific resource
    /// within it. Where a syscall can take a `Cap<T>` it should, and the
    /// typed `sys_*_typed` family does.
    ///
    /// O(`MAX_CAPS_PER_TASK`) with no locking of its own (the caller holds
    /// the table lock) — one linear pass, no interrupt toggling.
    pub fn holds_kind_with(&self, kind: CapKind, need: CapPerms) -> bool {
        // A null "kind" would match empty slots; refuse it explicitly rather
        // than let a caller accidentally assert that every task is authorized.
        if matches!(kind, CapKind::Null) {
            return false;
        }
        self.slots
            .iter()
            .any(|s| s.is_occupied() && s.kind == kind && s.perms.contains(need))
    }

    /// Does this table hold an occupied cap of `kind` **for this specific
    /// `resource`** whose permissions are a superset of `need`?
    ///
    /// **WHY this exists, and how it differs from [`holds_kind_with`]
    /// (2026-08-24, `Cap<Motor>` per-motor granularity — RFC-0003 P1).**
    /// `holds_kind_with` answers "does the caller hold *any* cap of this
    /// kind", which is right for `SYS_DRV_INVOKE` (one manifest, one
    /// permission mask, no per-resource distinction). Pair-wide actuation —
    /// `motor_cap.rs`'s `require_pair_write` — needs the stronger question
    /// "does the caller hold write on resource 0 *and* on resource 1
    /// specifically", because a task holding only `Cap<Motor>(0)` must not
    /// be able to drive wheel 1 by having the presence check degrade into
    /// "some Motor cap exists". Filtering on `resource` is what makes that
    /// distinction possible.
    ///
    /// **Containment is checked here too, unlike `holds_kind_with`.** This
    /// deliberately diverges from its sibling: every caller of this method
    /// today is checking WRITE for an actuation path (the motor pair rule),
    /// so the RFC-0036 degraded-mode denial has to apply here exactly as it
    /// does inside `get()` — otherwise a task could hold two valid Motor
    /// caps and drive through containment via the "other leg" check while
    /// `get()`'s own containment correctly denies the leg it dereferences
    /// directly. `holds_kind_with`'s callers (`SYS_DRV_INVOKE`) are a
    /// pre-existing, differently-scoped presence test that this change does
    /// not touch — see the migration survey for the scope boundary.
    ///
    /// O(`MAX_CAPS_PER_TASK`), same shape as `holds_kind_with`.
    pub fn holds_kind_resource_with(&self, kind: CapKind, resource: u32, need: CapPerms) -> bool {
        if matches!(kind, CapKind::Null) {
            return false;
        }
        if need.contains(CapPerms::WRITE) && degraded_active() {
            return false;
        }
        self.slots.iter().any(|s| {
            s.is_occupied() && s.kind == kind && s.resource == resource && s.perms.contains(need)
        })
    }

    // Pick the next free slot index, or `None` if full.
    fn allocate_slot(&self) -> Option<u16> {
        for (i, slot) in self.slots.iter().enumerate() {
            if !slot.is_occupied() {
                return Some(i as u16);
            }
        }
        None
    }

    // Bump the slot's generation, skipping `0`. Wraps after 255.
    fn bump_generation(&self, slot_idx: usize) -> u8 {
        let prev = self.slots[slot_idx].generation;
        match prev.checked_add(1) {
            Some(n) if n != 0 => n,
            _ => 1, // wrapped (255 → 0); skip 0
        }
    }
}

/// Error returned by [`CapTable::get`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CapError {
    /// The handle's generation is stale (slot empty or reused).
    Stale,
    /// The handle's kind does not match the expected `T`.
    WrongKind,
    /// The slot does not have the required permission bits.
    MissingPerms,
    /// Degraded mode (RFC-0036) is armed and this is a write/actuation: the
    /// capability is valid but its use is contained until degraded mode clears.
    Contained,
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

// ──────────────────────────────────────────────────────────────────────────
// Kani harnesses (RFC-0003 / RFC-0006)
// ──────────────────────────────────────────────────────────────────────────
//
// These compile only under `cargo kani --features kani` and prove the
// forgery-resistance properties of `CapTable::get`.
#[cfg(kani)]
mod kani_proofs {
    use super::targets::Channel;
    use super::*;

    /// A handle whose slot is empty must always fail with `Stale`.
    #[kani::proof]
    fn cap_forge_impossible_empty_slot() {
        let t = CapTable::empty();
        let raw_bits: u32 = kani::any();
        let forged: Cap<Channel> = Cap::from_raw(CapHandle::from_raw(raw_bits));
        // No slot is occupied ⇒ every dereference must fail Stale.
        match t.get(forged, CapPerms::READ) {
            Err(CapError::Stale) => (),
            Err(CapError::WrongKind) => (),  // also acceptable
            _ => panic!("forged cap into empty table must not succeed"),
        }
    }

    /// After grant + revoke, the cap is never re-validated.
    #[kani::proof]
    fn cap_revoked_stale() {
        let mut t = CapTable::empty();
        let resource: u32 = kani::any();
        let perms_bits: u8 = kani::any();
        kani::assume(perms_bits <= 0b1111);
        let perms = CapPerms::from_bits_truncate(perms_bits);
        let cap: Cap<Channel> = match t.grant(perms, resource) {
            Some(c) => c,
            None => return, // table was full (impossible from empty, but Kani must accept)
        };
        t.revoke(cap);
        let need = CapPerms::READ;
        let got = t.get(cap, need);
        // Either Stale (slot now empty) or some other rejection — never Ok.
        match got {
            Err(_) => (),
            Ok(_) => panic!("revoked cap returned Ok"),
        }
    }

    /// RFC-0036: in degraded mode, a write through any (otherwise valid) cap is
    /// contained — never `Ok`. Read access is unaffected.
    #[kani::proof]
    fn cap_contained_when_degraded() {
        let mut t = CapTable::empty();
        let resource: u32 = kani::any();
        // A cap that DOES carry WRITE, so the perms check passes and the
        // containment check is what rejects it.
        let cap: Cap<Channel> = match t.grant(CapPerms::RW, resource) {
            Some(c) => c,
            None => return,
        };
        degraded_set(true);
        let w = t.get(cap, CapPerms::WRITE);
        let r = t.get(cap, CapPerms::READ);
        degraded_set(false);
        // Write is contained; read still resolves.
        assert!(matches!(w, Err(CapError::Contained)));
        assert!(matches!(r, Ok(_)));
    }

    /// Granted cap with insufficient perms is rejected.
    #[kani::proof]
    fn cap_perms_required() {
        let mut t = CapTable::empty();
        let cap: Cap<Channel> = match t.grant(CapPerms::READ, 0) {
            Some(c) => c,
            None => return,
        };
        match t.get(cap, CapPerms::WRITE) {
            Err(CapError::MissingPerms) => (),
            other => panic!("expected MissingPerms, got {:?}", other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::targets::{Channel, Sensor};
    use super::*;

    /// Serialises every test that touches the process-global degrade level.
    ///
    /// `DEGRADE_LEVEL` is one static and `cargo test` runs test functions in
    /// parallel, so the six tests below were racing each other: e.g.
    /// `degraded_set_false_maps_to_full` sets FULL and asserts FULL, while
    /// `degrade_level_roundtrip` is free to store CAUTIOUS in between. The
    /// old comment on `degraded_mode_contains_writes` claimed it was "the
    /// only test touching the global DEGRADED flag" — that stopped being
    /// true when the RFC-0037 graded-level tests were added, and the suite
    /// has been latently flaky since. Held for the whole body, poison
    /// recovered so one failure does not cascade into five.
    static DEGRADE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn degrade_guard() -> std::sync::MutexGuard<'static, ()> {
        DEGRADE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn grant_and_get_roundtrip() {
        let mut t = CapTable::empty();
        let c: Cap<Channel> = t.grant(CapPerms::RW, 42).unwrap();
        let resource = t.get(c, CapPerms::READ).unwrap();
        assert_eq!(resource, 42);
    }

    #[test]
    fn wrong_kind_fails() {
        let mut t = CapTable::empty();
        let c: Cap<Channel> = t.grant(CapPerms::RW, 7).unwrap();
        // Forge a Cap<Sensor> with the same raw handle bits — not a real
        // attack vector since Cap<T> is private to the kernel, but
        // verifies the runtime check.
        let forged = Cap::<Sensor>::from_raw(c.raw());
        let kind_ok = matches!(t.get(forged, CapPerms::READ), Err(CapError::WrongKind));
        assert!(kind_ok);
    }

    #[test]
    fn revoked_cap_is_stale() {
        let mut t = CapTable::empty();
        let c: Cap<Channel> = t.grant(CapPerms::RW, 1).unwrap();
        t.revoke(c);
        assert_eq!(t.get(c, CapPerms::READ), Err(CapError::Stale));
    }

    #[test]
    fn missing_perms_rejected() {
        let mut t = CapTable::empty();
        let c: Cap<Channel> = t.grant(CapPerms::READ, 9).unwrap();
        assert_eq!(t.get(c, CapPerms::WRITE), Err(CapError::MissingPerms));
    }

    #[test]
    fn generation_bump_after_reuse() {
        let mut t = CapTable::empty();
        let c1: Cap<Channel> = t.grant(CapPerms::RW, 1).unwrap();
        let g1 = c1.raw().generation();
        t.revoke(c1);
        let c2: Cap<Channel> = t.grant(CapPerms::RW, 2).unwrap();
        let g2 = c2.raw().generation();
        // Same slot reused with a new generation.
        assert_eq!(c1.raw().slot(), c2.raw().slot());
        assert_ne!(g1, g2);
        // The original cap is now stale.
        assert_eq!(t.get(c1, CapPerms::READ), Err(CapError::Stale));
    }

    #[test]
    fn degraded_mode_contains_writes() {
        let _serial = degrade_guard();
        // RFC-0036: degraded mode denies WRITE through a valid cap but leaves
        // READ live; clearing it restores writes; and a cap lacking WRITE still
        // reports MissingPerms (forgery/perms checks run first). Single test so
        // it is the only one asserting the get()-side containment behaviour.
        // The global flag itself is shared with the RFC-0037 level tests, so
        // every one of them takes `DEGRADE_LOCK` — see its doc.
        let mut t = CapTable::empty();
        let rw: Cap<Channel> = t.grant(CapPerms::RW, 77).unwrap();
        let ro: Cap<Channel> = t.grant(CapPerms::READ, 5).unwrap();

        // Normal: write resolves.
        assert_eq!(t.get(rw, CapPerms::WRITE), Ok(77));

        degraded_set(true);
        assert_eq!(t.get(rw, CapPerms::WRITE), Err(CapError::Contained));
        assert_eq!(t.get(rw, CapPerms::READ), Ok(77)); // reads stay live
        // perms check runs before containment → MissingPerms, not Contained.
        assert_eq!(t.get(ro, CapPerms::WRITE), Err(CapError::MissingPerms));
        degraded_set(false);

        // Cleared: write resolves again.
        assert_eq!(t.get(rw, CapPerms::WRITE), Ok(77));
    }

    // ── RFC-0037 graded degrade level tests ───────────────────────────────

    #[test]
    fn degrade_level_roundtrip() {
        let _serial = degrade_guard();
        // Reset to FULL after each variant so tests are independent of run order.
        degrade_level_set(DEGRADE_LEVEL_FULL);
        assert_eq!(degrade_level(), DEGRADE_LEVEL_FULL);

        degrade_level_set(DEGRADE_LEVEL_CAUTIOUS);
        assert_eq!(degrade_level(), DEGRADE_LEVEL_CAUTIOUS);

        degrade_level_set(DEGRADE_LEVEL_SLOW);
        assert_eq!(degrade_level(), DEGRADE_LEVEL_SLOW);

        degrade_level_set(DEGRADE_LEVEL_CONTAINED);
        assert_eq!(degrade_level(), DEGRADE_LEVEL_CONTAINED);

        // Restore global state for other tests.
        degrade_level_set(DEGRADE_LEVEL_FULL);
    }

    #[test]
    fn degrade_level_oob_clamps_to_contained() {
        let _serial = degrade_guard();
        // Out-of-range index (e.g. 99) must clamp to CONTAINED — fail-closed,
        // never panic.
        degrade_level_set(99);
        assert_eq!(degrade_level(), DEGRADE_LEVEL_CONTAINED);

        // Restore.
        degrade_level_set(DEGRADE_LEVEL_FULL);
    }

    #[test]
    fn degraded_set_true_maps_to_contained() {
        let _serial = degrade_guard();
        degraded_set(true);
        assert!(degraded_active());
        assert_eq!(degrade_level(), DEGRADE_LEVEL_CONTAINED);
        degraded_set(false);
    }

    #[test]
    fn degraded_set_false_maps_to_full() {
        let _serial = degrade_guard();
        // Arm first, then clear via bool shim.
        degrade_level_set(DEGRADE_LEVEL_CONTAINED);
        degraded_set(false);
        assert!(!degraded_active());
        assert_eq!(degrade_level(), DEGRADE_LEVEL_FULL);
    }

    #[test]
    fn cautious_and_slow_do_not_trip_cap_denial() {
        let _serial = degrade_guard();
        // CAUTIOUS and SLOW restrict speed only; cap-denial stays off.
        degrade_level_set(DEGRADE_LEVEL_CAUTIOUS);
        assert!(!degraded_active(), "CAUTIOUS must not arm cap-denial");

        degrade_level_set(DEGRADE_LEVEL_SLOW);
        assert!(!degraded_active(), "SLOW must not arm cap-denial");

        // Restore.
        degrade_level_set(DEGRADE_LEVEL_FULL);
    }

    #[test]
    fn null_cap_is_stale() {
        let t = CapTable::empty();
        assert_eq!(
            t.get::<Channel>(Cap::NULL, CapPerms::READ),
            Err(CapError::Stale)
        );
    }

    #[test]
    fn inspect_raw_reports_slot_contents_and_rejects_forgeries() {
        // The delegation path's only view into the grantor's table. It must
        // agree with `get` on what is valid, and must reject the three things
        // ring 3 can send: a null handle, an out-of-range slot index (the
        // wire format has 16 slot bits, the table has 256 slots), and a
        // handle whose kind tag was relabelled.
        let mut t = CapTable::empty();
        let c: Cap<Channel> = t.grant(CapPerms::RW_DUP, 4242).unwrap();
        assert_eq!(
            t.inspect_raw(c.raw()),
            Ok((CapKind::Channel, CapPerms::RW_DUP, 4242))
        );

        assert_eq!(t.inspect_raw(CAP_NULL), Err(CapError::Stale));

        let oob = CapHandle::pack(CapKind::Channel, CapPerms::RW, 1, 60_000);
        assert_eq!(t.inspect_raw(oob), Err(CapError::Stale));

        // Same slot, same generation, different kind tag: relabelling a cap
        // you DO hold must fail, or a Cap<Gpio> could be delegated as a
        // Cap<Motor> pointing at the same resource id.
        let relabelled = CapHandle::pack(
            CapKind::Motor,
            CapPerms::RW_DUP,
            c.raw().generation(),
            c.raw().slot(),
        );
        assert_eq!(t.inspect_raw(relabelled), Err(CapError::WrongKind));

        // Stale after revoke, exactly like `get`.
        t.revoke(c);
        assert_eq!(t.inspect_raw(c.raw()), Err(CapError::Stale));
    }

    #[test]
    fn grant_raw_refuses_null_kind() {
        // Occupancy is `kind != Null`, so a Null-kind grant would burn a
        // generation on a slot that still reads as free.
        let mut t = CapTable::empty();
        assert!(t.grant_raw(CapKind::Null, CapPerms::RW, 1).is_none());
        assert_eq!(t.occupied(), 0);

        // A real kind still round-trips through the same allocation path
        // `grant<T>` uses, generation included.
        let h = t.grant_raw(CapKind::Gpio, CapPerms::READ, 7).unwrap();
        assert_ne!(h.generation(), 0);
        assert_eq!(t.inspect_raw(h), Ok((CapKind::Gpio, CapPerms::READ, 7)));
    }

    #[test]
    fn holds_kind_resource_with_is_resource_specific() {
        let mut t = CapTable::empty();
        let _m0: Cap<crate::cap::targets::Motor> = t.grant(CapPerms::RW, 0).unwrap();
        // Only resource 0 is held — resource 1 must not be reported present,
        // even though the kind matches and READ/WRITE would pass on 0.
        assert!(t.holds_kind_resource_with(CapKind::Motor, 0, CapPerms::WRITE));
        assert!(!t.holds_kind_resource_with(CapKind::Motor, 1, CapPerms::WRITE));
        // Wrong kind at the same resource id must not match either.
        assert!(!t.holds_kind_resource_with(CapKind::Gpio, 0, CapPerms::WRITE));
    }

    #[test]
    fn holds_kind_resource_with_denies_write_when_degraded() {
        let _serial = degrade_guard();
        let mut t = CapTable::empty();
        let _m1: Cap<crate::cap::targets::Motor> = t.grant(CapPerms::RW, 1).unwrap();
        assert!(t.holds_kind_resource_with(CapKind::Motor, 1, CapPerms::WRITE));
        degraded_set(true);
        assert!(!t.holds_kind_resource_with(CapKind::Motor, 1, CapPerms::WRITE));
        // READ is unaffected by containment, same as `get`.
        assert!(t.holds_kind_resource_with(CapKind::Motor, 1, CapPerms::READ));
        degraded_set(false);
    }

    #[test]
    fn full_table_returns_none() {
        let mut t = CapTable::empty();
        for i in 0..MAX_CAPS_PER_TASK {
            let _: Cap<Channel> = t.grant(CapPerms::RW, i as u32).unwrap();
        }
        let extra: Option<Cap<Channel>> = t.grant(CapPerms::RW, 0);
        assert!(extra.is_none());
        assert_eq!(t.occupied(), MAX_CAPS_PER_TASK);
    }

    // ── RFC-0037: degrade_level_cap_pct mapping tests ─────────────────────
    //
    // These 6 tests have moved to crates/degrade-policy/src/lib.rs where
    // they live next to the mapping function (level_cap_pct) and run via
    // `cargo test` from that crate's directory. The cap-tests host runner
    // no longer needs to cover them; cap.rs re-exports the function from
    // the leaf so that callers resolve unchanged.
}
