# Safety Coding Standard (SC01)

This document defines the coding rules that apply to **safety-critical
crates** in `robot-os`. The goal is to keep the code small, deterministic
and auditable — the same disciplines that DO-178C, IEC 61508 (SIL) and
ISO 26262 (ASIL) ask for, without pretending to be a certified product.

> *"You can't certify what you can't audit."* — adapted from IEC 61508-3 §7

---

## 1 · Scope

### 1.1 Crates **inside** the standard

These crates execute on the safety/control path. The rules in §2 are
mandatory for them. Violations should be either fixed or accompanied by
an `// SAFETY-EXEMPT(SC-N): reason` comment that the reviewer accepts.

| Crate | Why it's in scope |
|-------|-------------------|
| `crates/behavior` | Subsumption layers (L0-L3), ESTOP, safety profiles, geofence |
| `crates/ota`      | Boot validation, OTA receive, slot management |
| `crates/nav`      | A* path planning, pure-pursuit, speculative cache |
| `crates/sched`    | RT priorities, preemption, IRQ entry/exit |
| `crates/ahrs`     | Sensor fusion driving control loop |
| `crates/imu`      | IMU driver feeding AHRS |
| `crates/crypto`   | OTA signature, secure_channel |

### 1.2 Crates **outside** the standard

These crates can be more liberal. They are allowed to allocate, panic on
internal invariants, and use richer Rust patterns.

| Crate | Why it's out of scope |
|-------|----------------------|
| `crates/ml`        | Inference; failure tolerated by the safety task above it |
| `crates/shell`     | UX and diagnostics; not on the control path |
| `crates/trace`     | Observability; never in the critical loop |
| `crates/fs`        | Used by safety crates but its panic is recoverable (safe-mode) |
| `crates/telemetry` | Reporting; not control |
| `crates/camera`, `crates/baro`, `crates/gps` | Sensor drivers; failures handled upstream |
| `crates/dtb`, `crates/config` | Boot-time only |
| `crates/efi`, `crates/driver_server` | Future-facing scaffolding |

---

## 2 · Rules

### SC-1 · No dynamic allocation in the safety/control path

Safety crates must not call into `alloc::Vec`, `alloc::Box`, `alloc::String`
or any other heap-backed type during steady-state operation. Use `heapless`
(static-capacity collections) or fixed-size arrays.

**Why**: heap allocation has unbounded latency on macOS too — but on a
RTOS with a custom allocator, fragmentation is the real killer. A safety
loop that runs 1 kHz cannot tolerate a `Vec::push` blocking on a coalesce.

**Allowed**:
- Boot-time allocation, before tasks start.
- Allocation in setup paths that run once per OTA.

**Banned**:
- Allocation inside `behavior_task` loop, IRQ handlers, ESTOP path.

### SC-2 · Bounded loops

Every loop in a safety crate must have a statically-derivable upper bound.

```rust
// ✓ OK: bound is the array length
for sensor in &self.sensors { ... }

// ✓ OK: bound is a const
for _ in 0..MAX_RETRIES { ... }

// ✗ NOT OK: bound depends on runtime peer state
while !self.queue.is_empty() { ... }   // ← allowed only with a `take(N)`
```

**Why**: WCET (F16) requires bounded loops. An unbounded `while` defeats
the entire timing analysis.

**Pattern when input is dynamic**:
```rust
for _ in 0..MAX_DRAIN_ITER {  // explicit bound
    let Some(item) = queue.pop() else { break };
    process(item);
}
```

### SC-3 · No recursion in safety paths

Recursion makes stack usage unbounded. Convert to iteration with explicit
state. The kernel runs all task stacks at fixed sizes (`KERNEL_STACK_SIZE`,
`USER_STACK_SIZE`) so an unbounded recursion is a guaranteed stack overflow.

**Pattern**: A* path-finding uses an explicit open/closed list with
`heapless::BinaryHeap`, not recursive descent.

### SC-4 · No `unwrap()` / `expect()` in release code

A safety crate must propagate errors via `Result` and let the caller
decide. The caller of last resort is a safe-mode entry, not a panic.

```rust
// ✗ NOT OK
let n = sensor.read().unwrap();

// ✓ OK
let Ok(n) = sensor.read() else {
    return SafetyAction::EnterSafeMode { reason: "sensor read failed" };
};
```

**Exemptions**:
- `unwrap()` after compile-time-checked invariants is acceptable with
  `// SAFETY-EXEMPT(SC-4): proof = ...` comment.
- Tests can `unwrap()` freely.

### SC-5 · No reachable `panic!` from a safety task

A panic on a safety task halts the kernel (or worse, leaves it in a
half-state). Convert into a controlled safe-mode entry that the watchdog
can observe and act on.

```rust
// ✗ NOT OK
if state.is_corrupt() {
    panic!("safety state corrupt");
}

// ✓ OK
if state.is_corrupt() {
    safety::enter_safe_mode(SafeModeReason::StateCorruption);
    return;
}
```

### SC-6 · Single-purpose tasks

Each kernel task does one thing. The subsumption architecture already
enforces this for L0-L3 (reflex / sensor / behavior / cognition). New
tasks must justify why they aren't a layer of an existing one.

### SC-7 · `unsafe` requires `// SAFETY:` justification

Every `unsafe` block must have a comment immediately above it explaining
*why* the operation is sound. Lints will fail review without this. Pattern:

```rust
// SAFETY: the MMIO region 0x1000_0000..0x1000_1000 is mapped as device
// memory by the linker script; concurrent access is gated by the
// `UART_LOCK` mutex obtained on entry to this function.
unsafe { core::ptr::write_volatile(UART_DR, byte); }
```

### SC-8 · IRQ handlers must be bounded

Any `#[interrupt]` or PLIC handler must execute in `O(1)` cycles measured
from entry to `mret`. Add a `// WCET: <cycles>` annotation and a unit test
that asserts the bound.

```rust
// WCET: 240 cycles on RV64IMAC @ 1 GHz = 240 ns
#[interrupt]
fn timer_irq() { ... }
```

### SC-9 · No magic numbers

Every numeric literal in a safety crate must be a named constant, a
const generic parameter, or a config value. Move all `4096`, `0x10`,
`100` into `const NAME: ... = ...;`.

```rust
// ✗ NOT OK
if buf.len() > 1024 { return Err(...); }

// ✓ OK
const MAX_PAYLOAD_BYTES: usize = 1024;
if buf.len() > MAX_PAYLOAD_BYTES { return Err(...); }
```

This is a project-wide rule but is **enforced strictly** in safety
crates.

### SC-10 · CRC/checksum on every boundary crossing

Any data leaving or entering a safety boundary must carry a checksum
that the receiver verifies before acting on it. Boundaries we care
about:

| Boundary | Checksum |
|----------|----------|
| FAT32 ↔ kernel (BOOTMETA, KERN_*.BIN) | CRC-32 (in BOOTMETA) |
| Brain ↔ kernel (TCP) | CRC-8 in `protocol.py` packets |
| OTA wire format | CRC-32 in OTA header |
| Inter-task SHM (R03 World state) | optional, sequence number is enough |
| Sensor → AHRS | none (per-sample, fault tolerated by EKF) |

---

## 3 · Enforcement

### 3.1 Lints

Each in-scope crate declares the following at the top of `lib.rs`:

```rust
#![warn(
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    clippy::missing_safety_doc,
    clippy::undocumented_unsafe_blocks,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
)]
#![deny(
    clippy::float_cmp,
    clippy::missing_errors_doc,
)]
```

Allow-list with rationale lives below the warn block:

```rust
#![allow(
    clippy::module_name_repetitions, // intentional naming convention
    clippy::missing_panics_doc,      // panics audited via SC-5
)]
```

### 3.2 `miri` for UB detection

CI runs `cargo +nightly miri test -p <crate>` for the host-testable
crates within scope (`ota` via `crates/ota-tests`, `nav` if extracted).
Detects:
- Use-after-free
- Data races
- Misaligned pointer access
- Out-of-bounds writes hidden in `unsafe` blocks

### 3.3 Trazabilidad: requirement → code → test

Every safety requirement (R1, R2, ...) in `docs/SAFETY_REQUIREMENTS.md`
points to:
1. A function/struct in the codebase.
2. At least one test that validates the requirement.

The matrix is maintained by hand. Whenever a requirement changes,
both the code link and the test must be updated.

---

## 4 · Process

### 4.1 New code

Every PR touching a safety crate runs `clippy --all-targets -- -D warnings`
in CI. Lints fail the build. Allow-list edits require a one-line
rationale in the PR description.

### 4.2 Existing code (audit)

Initial audit (SC01.C) classifies each existing violation as:

- **Fix now** — clear bug or clear win, fix in audit PR.
- **Allow with rationale** — false positive or accepted trade-off, add
  `#[allow(...)]` with comment.
- **Defer** — needs a refactor too large for this PR; create an issue.

### 4.3 Adding a new safety crate

To bring a crate into scope:
1. Add it to §1.1 above.
2. Add the lint block from §3.1 to its `lib.rs`.
3. Run audit, file follow-ups for deferred items.
4. Update `docs/SAFETY_REQUIREMENTS.md` with any new requirements.

---

## 5 · What this is NOT

- **Not** a path to commercial certification. To certify (DO-178C DAL-B,
  ISO 26262 ASIL-D, IEC 61508 SIL-3) you also need: tool qualification,
  test independence, documented hazard analysis, formal review records,
  and an accredited audit. None of that is in scope.
- **Not** a guarantee of correctness. These rules reduce a class of bugs
  (UB, panics, allocator-induced jitter, stack overflows). They do not
  prevent algorithmic mistakes.
- **Not** static. The rule set evolves as we learn what bugs actually
  hit our hardware in the field.

---

## 6 · Future work (SC02)

When migrating drivers to userspace (AQ3, post-Julio), evaluate seL4 IPC
patterns from [seL4 manual ch. 5–7](https://sel4.systems/Info/Docs/seL4-manual-13.0.0.pdf):

- **Capabilities unforgeable** — extend lease IPC with cryptographic
  tokens so a userspace driver can't forge a sender identity.
- **Synchronous endpoints (one-shot)** — alternative to channels that
  eliminates a class of data races by construction.
- **MCS time-partitioning** — guarantee that `ml_task` cannot starve
  `behavior_task` of CPU time.

This is research-level; do it before committing to an AQ3 design.
