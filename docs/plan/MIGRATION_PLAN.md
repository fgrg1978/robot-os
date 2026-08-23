# PHANES — Phase 1 Migration Plan

> **Audience:** PHANES contributors  
> **Pre-requisites:** RFC-0002 (modular), RFC-0003 (caps), RFC-0004
> (sched), RFC-0005 (topology), RFC-0008 (ABI)  
> **Last updated:** 2026-05-10

This document maps the **current** kernel code (legacy "Robot OS",
inherited at the start of Phase 1) to the **RFC-compliant target state**
of Phase 1 exit. It is the working plan for Waves W2 → W7.

W1 (this wave) shipped scaffolding only — `crates/abi/`, `Cap<T>`,
`formal/` skeleton, `book/` skeleton, this plan. Nothing existing has
been refactored yet.

---

## Big picture

```
                          ┌─────────────────────────────┐
                          │   Cap-typed surface         │
                          │   (target: end of W5)       │
                          │                             │
   Today: integer-handle  │   Cap<T> typed handles      │
   surface (handle.rs +   │   on every syscall +        │
   port.rs + channel.rs)  │   topology-driven cap_table │
                          └─────────────────────────────┘
                                      ▲
                                      │ migration
                                      │
   ┌──────────────────────────────────┴───────────────────────────────┐
   │ W2: topology parser ⇒ cap_table populate at task spawn           │
   │ W3: pick 1 syscall, migrate end-to-end as reference              │
   │ W4: scheduler v2 (new policies) + APS combinator                 │
   │ W5: bulk migrate remaining syscalls + drivers                    │
   │ W6: verification CI gates + mutation + OpenSSF badge             │
   │ W7: book + ABI freeze v1.0 + Phase-1 release tag                 │
   └──────────────────────────────────────────────────────────────────┘
```

---

## Inventory — what exists today

### `crates/ipc/src/`

| File          | LoC (~) | Status                        | Migration plan              |
|---------------|--------:|--------------------------------|------------------------------|
| `cap.rs`      | 350     | **W1: new — done**             | Canonical Cap<T> API         |
| `handle.rs`   | 220     | Legacy global table (AQ6); dead-code surface pruned 2026-08-24 | W3/W5 shipped the typed `Cap<T>` syscall surface, but W2 — seeding `cap_store` from topology at task spawn — was never written, so `handle.rs` still backs every live authorization check. File deletion is now phase P4 of the HANDLES→Cap<T> migration (topology-seeding must land first), not W5. |
| `channel.rs`  | ?       | Legacy channel API             | Wrap in `Cap<Channel>` in W3 |
| `port.rs`     | ?       | Legacy port API                | Wrap in `Cap<Port>` in W3   |
| `fast_ipc.rs` | ?       | M02 fast-path IPC              | Lift `Cap<Channel>` form     |
| `lease.rs`    | ?       | M04 lease IPC                  | Lift `Cap<Shm>` form         |
| `zerocopy.rs` | ?       | F15 zero-copy                  | Lift `Cap<Shm>` form         |
| `shm.rs`      | ?       | Shared memory                  | Lift `Cap<Shm>` form         |
| `io_ring.rs`  | ?       | AQ1 io_ring                    | Lift `Cap<IoRing>` form      |
| `irq_bind.rs` | ?       | AQ1 IRQ binding                | Lift `Cap<Irq>` form         |
| `pipe.rs`     | ?       | POSIX pipe                     | Wrap in `Cap<File>` (POSIX  surface kept) |
| `signal.rs`   | ?       | POSIX signals                  | Stays — no cap surface       |
| `rpc.rs`      | ?       | RPC                            | Lift `Cap<Channel>` form     |
| `trace.rs`    | ?       | Tracing                        | No change                    |

### `crates/sched/src/`

| File          | LoC (~) | Status                       | Migration plan                |
|---------------|--------:|------------------------------|-------------------------------|
| `scheduler.rs`| ?       | Priority queue + RR + RT     | W4: replace dispatch core with APS combinator |
| `task.rs`     | ?       | Task struct                  | W4: add `class: SchedClass`, `cap_table: CapTable` fields |
| `process.rs`  | ?       | ELF loader, process spawn    | W2: cap_table populate from CAPS.TOML during spawn |
| `smp.rs`      | ?       | SMP support                  | No structural change          |
| `wait.rs`     | ?       | Wait queues                  | No structural change          |
| `seccomp.rs`  | ?       | AQ11 syscall filter          | No structural change          |
| `driver.rs`   | ?       | Driver tasks                 | No structural change          |
| _new_         | —       | `policies/{fifo,edf_cbs,rr,cfs,sporadic}.rs` | W4: new files |
| _new_         | —       | `partitions.rs` (APS)        | W4: new file                  |

### `crates/syscall/src/`

| File          | LoC (~) | Status                  | Migration plan                |
|---------------|--------:|--------------------------|--------------------------------|
| `numbers.rs`  | 250     | Inline syscall numbers   | W2: `pub use robot_os_abi::syscall_nr::*;` |
| `dispatch.rs` | ?       | Trap dispatch            | W3: Cap-aware arg parsing      |
| `handlers.rs` | ?       | Per-syscall handlers     | W3+W5: per-handler migration   |
| `lib.rs`      | ?       | Crate root               | Re-exports                     |

### Other crates

- `crates/behavior/`: keep as-is in W2-W3; `auth_envelope` already
  loom-tested.
- `crates/drivers/`: W5 starts modular-pattern refactor (pick UART
  first; trait + impls/<vendor>.rs).
- `crates/ota/`: no structural change in Phase 1; OT01-OT05 already
  scoped.
- `crates/crypto/`: no structural change.
- `crates/net/`: no structural change in Phase 1.

---

## Wave-by-wave plan

### W2 — Topology parser (~1 session)

**Goal:** parse signed CAPS.TOML + SCHED.TOML at boot and stash the
parsed data in a per-task structure ready to be consumed by spawn.

**Files to add:**
- `crates/topology/Cargo.toml` (new crate; depends on `robot_os_abi`,
  `robot_os_crypto`, no_std-friendly TOML parser like `toml_edit` is
  too heavy → write minimal parser or pin a small one).
- `crates/topology/src/lib.rs` — `Topology`, `TaskSpec`, `CapSpec`,
  parser, signature verification.
- `crates/topology/src/builder.rs` — host-side helper to build
  topology programmatically (test).
- `formal/tla/topology_load.tla` — TLA+ spec of "load fails ⇒ no task
  spawn".

**Files to modify:**
- `kernel/src/main.rs` — load topology immediately after mm init,
  before user-space spawn.
- `crates/sched/src/process.rs` — accept `&TaskSpec` on spawn,
  populate `task.cap_table`.

**Tests:**
- Unit tests in `crates/topology/`: parse golden TOML, malformed
  rejection, invalid kind name rejection, unknown task ref.
- Integration test: malformed signature ⇒ `boot_fail()`.
- INV-15 entry in `formal/proofs/INVARIANTS.md` upgrades from `(W2)`
  to "Done".

**Open questions for W2:**
- Pick a TOML parser. Candidates:
  - `toml-rs`: needs alloc; OK if we restrict to host-build path.
  - Hand-rolled minimal subset parser: ~300 LOC, no alloc.
  - Decision: hand-rolled. Topology format is small; we control the
    grammar; alloc-free is a hard constraint.
- Signature key — for W2 use the existing dev key; production key
  rotation is OT05 work.

### W3 — One-syscall reference migration (~1-2 sesiones)

**Goal:** demonstrate the end-to-end `Cap<T>` flow on a single,
non-controversial syscall as a template for the bulk migration.

**Choice:** `SYS_CHAN_WRITE` (channel write). Rationale: small surface,
already-loom-tested adjacent code, easy to add a parallel typed entry
without breaking existing callers.

**Files to add / modify:**
- `crates/syscall/src/handlers.rs` — add `sys_chan_write_typed` that
  takes `Cap<Channel>`, leaving the legacy entry as adapter.
- `crates/ipc/src/channel.rs` — add `channel_write_cap(cap, ...)`.
- `crates/syscall/src/numbers.rs` — re-export from `robot_os_abi`.
- `crates/abi/src/syscall_nr.rs` — already canonical from W1.

**Tests:**
- `crates/regression-tests/sys_chan_write_typed.rs` — exercise
  EBADF, ECAPSTALE, ECAPKIND.
- Integration test in QEMU: brain sends WAYPOINT; kernel uses Cap<Channel>
  to dispatch.

**Decisions to lock in W3:**
- Whether legacy + typed entries co-exist permanently or only during
  migration. Recommendation: co-exist through W5; remove legacy entry
  when last caller migrates. `#[deprecated]` markers on the legacy
  entries in W4.
- Cap-table location in `Task` struct: pointer or inline. Inline (256
  slots × 8 B = 2 KB per task) is simpler; pointer reduces memory for
  tasks that hold few caps. Recommendation: inline for W3, evaluate
  perf in W7.

### W4 — Scheduler v2 (~2-3 sesiones)

**Goal:** ship the multi-policy hierarchical scheduler from RFC-0004.

**Files to add:**
- `crates/sched/src/policies/fifo.rs`
- `crates/sched/src/policies/edf_cbs.rs`
- `crates/sched/src/policies/rr.rs`
- `crates/sched/src/policies/cfs.rs`
- `crates/sched/src/policies/sporadic.rs`
- `crates/sched/src/partitions.rs` — APS combinator
- `crates/sched/src/class.rs` — `SchedClass` enum, budgets

**Files to modify:**
- `crates/sched/src/scheduler.rs` — replace dispatch core to call APS;
  keep priority queues as a fallback compat path.
- `crates/sched/src/task.rs` — add `class: SchedClass`, `deadline:
  Option<u64>`, `budget_remaining: u32`.
- `kernel/src/main.rs` — boot init reads `SCHED.TOML` (W2) and
  configures partitions.

**Tests:**
- `crates/regression-tests/sched_classes.rs`
- `formal/tla/sched_aps.tla` + `formal/tla/edf_cbs.tla`
- `formal/kani/edf_deadline.rs` — bounded harness asserting EDF picks
  earliest deadline.
- Soak test: best-effort task cannot starve safety class.
- INV-5 + INV-8 → "Done".

### W5 — Bulk migration (~3-4 sesiones)

**Goal:** all syscalls take `Cap<T>` (or POSIX-shaped wrapper); legacy
`handle.rs` deleted; first 3 drivers in modular pattern.

**Order of attack:**
1. IPC family (channel, port, shm, io_ring, lease, zerocopy, fast).
2. Hardware caps (gpio, pwm, i2c, motor, sensor).
3. Filesystem (file, socket — POSIX-shaped, wrapped in `Cap<File>`).
4. AI session.
5. Drivers: UART trait + impls/{ns16550,jh7110_uart,k1_uart}.

**Each migration follows the W3 template:** typed entry; adapter for
legacy callers; tests; remove legacy when last caller is migrated.

### W6 — Verification + CI gates (~2 sesiones)

**Goal:** OpenSSF Best Practices Badge passing + mutation testing
weekly + coverage thresholds enforced.

**Files to add:**
- `.github/workflows/cap-coverage-gate.yml`
- `.github/workflows/mutation-weekly.yml`
- `.github/workflows/oss-fuzz-stub.yml` (target ingest)
- `safety/CODING_STANDARD.md` — SC-1..SC-10 codified.
- `formal/kani/` — extend to ≥ 5 harnesses.
- `formal/tla/` — extend to ≥ 5 specs.

### W7 — Book completion + ABI freeze (~1-2 sesiones)

**Goal:** Phase-1 release ready.

- Fill in book chapters: scheduler, topology, ai-runtime stub.
- Tag `crates/abi` v1.0; lock structs.
- Release process: tag, SBOM, sign, GHA SLSA action.
- LF incubation application package.

---

## Cross-cutting rules during migration

- **No backwards-compat hacks.** When a legacy entry is fully migrated,
  delete it. Don't keep ghost wrappers "just in case."
- **DCO sign-off** every commit (`git commit -s`).
- **One change per PR** — easier to revert.
- **Keep `make qemu-full` green** at every commit — never let it bitrot.
- **Update `formal/proofs/INVARIANTS.md`** when you add a verified
  invariant.

## Exit criteria for Phase 1

- ✅ All five build configs clean.
- ✅ All current tests + new W3-W6 tests passing.
- ✅ ≥ 5 TLA+ specs + ≥ 5 Kani harnesses + Loom suite.
- ✅ Coverage ≥ Phase-1 targets (RFC-0013).
- ✅ OpenSSF Best Practices Badge **passing** (silver target Phase 2).
- ✅ Book ≥ 12 chapters complete (intro + getting-started + architecture
  + brain + appendix).
- ✅ ABI freeze v1.0 tagged.
- ✅ LF incubation application submitted.
