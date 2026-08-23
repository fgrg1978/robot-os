# PHANES — Formal verification

This directory holds the project's formal-verification artefacts:

| Path             | Tool           | Purpose                                       |
|------------------|----------------|-----------------------------------------------|
| `tla/`           | TLA+ / TLC     | High-level specs of state-machine invariants  |
| `kani/`          | Kani (Rust BMC)| Bounded-model proofs over Rust source         |
| `proofs/`        | (markdown)     | The system invariant ledger                   |

See **RFC-0006** for the verification strategy and **RFC-0013** for
the gates that integrate these artefacts into CI.

## Running

```bash
# TLA+ — TLC ships in tla2tools.jar.
# Download once:
#   curl -sL https://github.com/tlaplus/tlaplus/releases/download/v1.8.0/tla2tools.jar \
#        -o tools/tla2tools.jar
# Run any spec (uses the .cfg next to it):
#   cd formal/tla
#   java -jar ../../tools/tla2tools.jar -workers auto -config cap_table.cfg cap_table.tla

# Kani (requires `cargo install --locked kani-verifier && cargo kani setup`)
cargo kani --harness cap_forge_impossible -p robot_os_ipc

# Loom is run via `cargo test` in the relevant crate with
# `RUSTFLAGS="--cfg loom"` (already wired in regression-tests).
```

## Verified specs (TLC, 2026-05-14)

| Spec | States | Distinct | Invariants |
|------|--------|----------|------------|
| `cap_table.tla` | 289 | 93 | TypeOK · AtMostOneValidPerSlot · RevokedNeverValid · GenInRange |
| `topology_load.tla` | 55 | 39 | TypeOK · SpawnImpliesLoaded |
| `sched_aps.tla` | 13,589 | 4,135 | TypeOK · ConsumptionBounded · ChosenIsRunnable |

## Maintenance policy

- Every safety crate change touching cap-table, scheduler, OTA, IPC,
  or auth_envelope **must** update the corresponding spec or harness.
- The invariant ledger (`proofs/INVARIANTS.md`) is the canonical list
  of system-wide guarantees; new RFCs introducing safety properties
  must add their invariant here.
- Specs use the same naming as the code: `crates/ipc/src/cap.rs` ↔
  `formal/tla/cap_table.tla` ↔ `formal/kani/cap_forge.rs`.
