# PHANES — System invariants

This is the canonical ledger of system-wide invariants. Each invariant
is paired with the artefact that enforces it and the test or proof
that verifies it.

When an RFC introduces a new safety property, **add it here** with the
verification commitment. Removing an invariant requires an RFC.

| ID     | Invariant                                                                                  | RFC      | Enforced by                               | Verified by                                              |
|--------|--------------------------------------------------------------------------------------------|----------|-------------------------------------------|----------------------------------------------------------|
| INV-1  | The capability table is monotonic in generation per slot (skip 0 on wrap).                 | RFC-0003 | `crates/ipc/src/cap.rs::CapTable::bump_generation` | `formal/tla/cap_table.tla::AtMostOneValidPerSlot`        |
| INV-2  | A revoked cap is never validated by `CapTable::get`.                                       | RFC-0003 | `crates/ipc/src/cap.rs::CapTable::revoke` | `formal/kani/cap_revoked_stale` + unit tests             |
| INV-3  | A handle's kind tag must equal `T::KIND` on dereference.                                   | RFC-0003 | `crates/ipc/src/cap.rs::CapTable::get` + `crates/ipc/src/channel.rs::channel_send_cap` + `crates/syscall/src/handlers.rs::sys_chan_write_typed` | `formal/kani/cap_forge_impossible_empty_slot` + cap unit tests + integration via `cap_store + sys_chan_write_typed` (W3) |
| INV-4  | Granting a cap requires `CapPerms` ⊆ slot's stored perms.                                  | RFC-0003 | `crates/ipc/src/cap.rs::CapTable::get`    | `formal/kani/cap_perms_required` + unit tests            |
| INV-5  | All scheduler class budgets sum to ≤ 100 % per partition window.                           | RFC-0004 | `crates/sched/src/class.rs::DEFAULT_BUDGETS_PCT` + `crates/sched/src/partitions.rs::Aps` | `formal/tla/sched_aps.tla::ConsumptionBounded` + `crates/sched-policy-tests::default_budgets_sum_to_100` (43 host tests) |
| INV-6  | The OTA active slot has a valid signature **and** rollback counter ≥ stored.               | RFC-0011 | `crates/ota/src/secure_boot.rs`           | `crates/ota-tests` regression suite                       |
| INV-7  | The recovery slot is read-only after boot.                                                 | RFC-0011 | Hardware (flash partition WP)             | Hardware verification on platform bring-up                |
| INV-8  | ESTOP, when triggered, takes effect within 50 ms.                                          | RFC-0004 | `crates/behavior/src/safety.rs`           | (W4 — soak test asserting latency < 50 ms)                |
| INV-9  | No allocation in safety-class scheduler path.                                              | RFC-0013 | (W4 — SC01 lint pass)                     | (W4 — clippy custom lint, CI gate)                        |
| INV-10 | All loops in safety crates have static bounds.                                             | RFC-0013 | (W4 — SC01 lint pass)                     | (W4 — clippy custom lint, CI gate)                        |
| INV-11 | All `unsafe` blocks have `// SAFETY:` justification.                                       | RFC-0013 | (W4 — SC01 lint pass)                     | (W4 — clippy lint `clippy::undocumented_unsafe_blocks`)   |
| INV-12 | Brain link nonces never repeat within a session.                                           | RFC-0006 | `crates/behavior/src/auth_envelope.rs`    | `crates/regression-tests/auth_envelope_tests.rs` (Loom)   |
| INV-13 | The kernel's `.text` is read-only post-boot (W^X).                                         | RFC-0011 | `crates/mm` + `crates/arch::pmp`          | (W4 — runtime assertion + test)                           |
| INV-14 | Anti-rollback counter is monotonic at OTP write.                                           | RFC-0011 | `crates/ota/src/secure_boot.rs`           | `crates/ota-tests`                                        |
| INV-15 | Topology load failure prevents user-space spawn.                                           | RFC-0005 | `crates/topology/src/lib.rs` (admission + signature) | `formal/tla/topology_load.tla::SpawnImpliesLoaded` + `crates/topology-tests/` (boot integration W3) |

## Status legend

- **Done** — invariant has both code enforcement and test/proof
  shipped.
- **(W*N*)** — invariant is scheduled for that wave of Phase 1.
- Empty cell — not yet scheduled.

## Adding an invariant

1. Define the invariant as a single sentence; assign the next ID.
2. Reference the RFC that establishes it.
3. Identify the code file/function that maintains it.
4. Identify the test or proof that verifies it. If none yet, schedule
   it for a wave and put `(W*N*)` in the cell.
5. Open a PR adding the row + the test/proof together.

## Removing or weakening

Removing or weakening an invariant must go through an RFC supersede.
Stating *why* the property is no longer needed (architectural change,
threat-model change) is mandatory.
