# PHANES — Changelog

All notable changes to the PHANES kernel and tooling.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning is SemVer. Per-crate changelogs live next to each crate
(currently only `crates/abi/CHANGELOG.md` is published; per-crate
changelogs roll out with Phase 2).

## [Unreleased]

Phase 1.x point releases will bring:

- W5 (target v1.1.0): bulk migration of remaining IPC syscalls
  (`SYS_PORT_*`, `SYS_IPC_SHARE`, `SYS_IO_*`, etc.) + hardware caps
  (`SYS_GPIO_*`, `SYS_PWM_*`, `SYS_I2C_*`) to `Cap<T>` typed entries.
- W5 (target v1.1.0): 3 drivers migrated to the RFC-0002 modular
  pattern (UART, then blk + nic).
- W6 remaining (target v1.2.0): `cargo-kani` runs on the 3 cap.rs
  harnesses, OpenSSF Best Practices Badge silver tier, mutation
  testing (cargo-mutants) weekly.
- 2 additional TLA+ specs (`auth_envelope`, `ota_state`) — target
  v1.1.0.

Phase 2 (multi-platform + AI runtime + HIL CI farm) begins after
v1.0 announcement + LF incubation TAC review.

## [1.0.0] — Phase 1 release (2026-05-14)

**First stable PHANES release.** `crates/abi` is locked for the
v1.x series. Same content as v1.0.0-rc1 below, promoted after the
release-candidate verification passed across all CI gates +
end-to-end QEMU smoke (single CPU + 4-CPU SMP + virtio disk +
network forwarding).

Linux Foundation incubation application
(`docs/plan/LF_INCUBATION_APPLICATION.md`) submitted concurrent
with this release. The PHANES word mark + future logo transfer
to the Linux Foundation upon incubation acceptance.

## [1.0.0-rc1] — Phase 1 freeze candidate (2026-05-14)

The first release-candidate of PHANES (formerly `robot-os`). All
constitutional work is in place; the W4 scheduler is feature-complete
modulo a few migration items. **`crates/abi` is frozen for the v1.x
series** — see `crates/abi/CHANGELOG.md` for the public-surface diff.

### Added — kernel

- `crates/abi` — frozen ABI crate (RFC-0008). Single source of truth
  for syscall numbers, errno, `#[repr(C)]` types, `CapHandle` wire
  format. **v1.0 stability declaration.**
- `crates/ipc/src/cap.rs` — `Cap<T>` typed capability wrapper +
  per-task `CapTable` (RFC-0003). 7 unit tests, 3 Kani harnesses
  (under `#[cfg(kani)]`).
- `crates/ipc/src/cap_store.rs` — per-tid `[SpinLock<CapTable>; 64]`
  for kernel-side cap-table lookups.
- `crates/topology/` — alloc-free RFC-0005 TOML subset parser +
  signed CAPS.TOML / SCHED.TOML loader. `default_minimal()` builder
  for QEMU / dev boots.
- `crates/sched/src/class.rs` — `SchedClass` enum (5 RFC-0004
  classes) + `ClassBudget` with `AtomicU32` bookkeeping.
- `crates/sched/src/policies/` — 5 scheduling policies under common
  `Policy` trait: FIFO, EDF + CBS, RoundRobin, CFS, Sporadic.
- `crates/sched/src/partitions.rs` — Adaptive Partitioning Scheduler
  combinator. Three-phase pick (under-min → non-exhausted →
  degraded). Multi-window catch-up.
- `crates/sched/src/aps_state.rs` — per-CPU APS state + enqueue /
  pick / account helpers.
- `crates/sched/src/scheduler.rs` — extended `Task` struct with
  `sched_class_raw`, `sched_deadline_us`, `sched_time_slice_us`.
  New entry points `task_create_with_class`, `task_set_class`.
  Dispatch core branches on `SCHED_USE_APS` atomic flag (default
  false; legacy path drives boot, APS bookkeeping stays warm).
- `kernel/src/main.rs` — `topology::init(default_minimal())` wired
  before `sched::init()`. Boot-time `[APS] smoke OK` print
  confirms the APS path end-to-end.
- `SYS_CHAN_WRITE_TYPED` (528), `SYS_CHAN_READ_TYPED` (529) — first
  cap-typed syscalls. Errno discipline preserves cap-deref failure
  modes (`ECAPSTALE` / `ECAPKIND` / `ECAPPERMS`) end-to-end.

### Added — verification

- `formal/tla/cap_table.tla` — capability-table forgery resistance
  spec (4 invariants, 289 states TLC-verified).
- `formal/tla/topology_load.tla` — boot state-machine spec
  (`SpawnImpliesLoaded`, 55 states verified).
- `formal/tla/sched_aps.tla` — Adaptive Partitioning Scheduler
  invariants (3 invariants, 13,589 states verified).
- `formal/proofs/INVARIANTS.md` — system invariant ledger.

### Added — docs & process

- 16 RFCs in `rfcs/` (RFC-0001 strategic plan through RFC-0018
  three-tier project separation).
- 5 strategic docs in `docs/plan/` (VISION, ROADMAP, ARCHITECTURE,
  SECURITY_MODEL, TEST_STRATEGY).
- `docs/plan/MIGRATION_PLAN.md` — wave-by-wave migration plan for
  Phase 1.
- `book/` mdBook skeleton with full chapters for capability IPC,
  scheduler, topology, brain overview, brain protocol.
- `CONTRIBUTING.md` — DCO + RFC + ADR + style.
- `README.md` rewritten with PHANES branding.
- `.github/workflows/ci.yml` — full CI matrix: 5 kernel build
  configs, regression + ota suites, 4 host test crates, brain
  pytest, TLA+ TLC on the 3 specs, mdBook build.

### Added — testing

- 4 host test crates excluded from the workspace
  (`crates/{abi-tests, cap-tests, topology-tests,
  sched-policy-tests}/`).
- Total real tests passing: 1341 (103 regression + 58 ota + 24
  topology + 18 abi + 7 cap + 44 sched-policy + 1087 brain pytest).

### Fixed

- **`crates/ipc/src/cap.rs::CapTable::revoke`** — generation was
  reset on revoke, allowing forgery collision after slot reuse.
  Now `revoke` preserves generation and only clears kind / perms /
  resource. Discovered by `cap-tests::generation_bump_after_reuse`
  on first real run; fixed before Phase 1 close.
- **`crates/sched/src/partitions.rs::Aps::tick`** —
  multi-window catch-up bug. A delayed first tick at boot would
  reset budgets on every subsequent tick until the start caught up.
  Now advances `(elapsed / window)` windows in one step.

### Branding

- Project rebrand: `robot-os` → `phanes` (RFC-0010). The GitHub
  repo rename will land at Phase 1 final release.

## Phase 0 — Strategic foundation (2026-05-10)

Initial RFC corpus and planning docs. No code changes.
