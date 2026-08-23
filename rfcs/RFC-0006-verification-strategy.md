# RFC-0006: Verification Strategy (TLA+ + Kani + Loom)

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-08-20


> **Status audit 2026-08-20.** The mechanism is real — `formal/tla/` holds
> `cap_table`, `driver_registry`, `sched_aps` and `topology_load` specs with
> their `.cfg` files, and `cfg(kani)`/`cfg(loom)` are wired into
> `crates/ipc/Cargo.toml` and `crates/topology/Cargo.toml`. But this RFC
> promises TLA+ models for "scheduler, IPC, OTA, secure_boot", and **no OTA or
> secure_boot spec exists**. The gap is worth naming precisely because it falls
> on the two security components: the formal coverage is narrower than a reader
> of this document would assume, and narrower exactly where it would matter
> most.

## Summary

PHANES adopts a **layered verification strategy** rather than aiming
at full seL4-style mechanised proof on day one. Three layers, each
with distinct cost and benefit:

1. **TLA+ models** of safety-critical state machines (scheduler, IPC,
   OTA, secure boot). Specified, model-checked, treated as the
   authoritative spec.
2. **Kani** (bounded model checking on Rust) on safety crates
   (`ipc`, `sched`, `mm`, `ota`, `crypto`). Proves bounded-input
   invariants automatically in CI.
3. **Loom** (concurrent property testing) on every IPC and scheduler
   primitive that touches atomics or locks.

Plus: **proptest** for parsers; **Miri** for unsafe-code soundness;
external **annual security audits** (Trail of Bits / NCC) starting
Phase 2.

This is the path to ASIL-D and academic citations without the 10-year
bill of full theorem-proving.

## Motivation

"Trust me, I tested it" is not a cert-acceptable safety argument. The
IEC 61508 / ISO 26262 V&V process expects:

- Formal specification of safety requirements
- Model-checked or proved invariants
- Bounded testing with documented coverage
- Independent audit

Our budget, timeline, and team size make full seL4-style proof
infeasible. But the layered approach above is **proven** to satisfy
ASIL-B and most ASIL-D arguments when combined with rigorous testing.

Equally important: this stack runs in CI continuously. Every PR
re-checks the invariants. Verification is not a one-time milestone;
it is a continuous gate.

## Detailed design

### Layer 1 — TLA+ specifications

**What:** Plus-Cal (PCAL) descriptions of the small set of
safety-critical state machines that the rest of the system rests on.

**Files:** `formal/tla/{scheduler,ipc,ota,secure_boot,topology}.tla`.

**Initial scope (Phase 0–1):**

| Model | Properties checked |
|-------|--------------------|
| `scheduler.tla` | No deadline missed in HardRT class given admission. SafetyCritical never starves below `min_budget`. Time-slice expiry preempts non-RT correctly. No two CPUs run the same task. |
| `ipc.tla` | A cap can only originate from the kernel. A revoked cap fails on use. No path delivers a message to a task without a matching `Cap<Channel<T>>`. FIFO ordering preserved within a channel. |
| `ota.tla` | A/B slot atomicity: at any instant, at most one slot is in mid-write. Boot loop detection eventually rolls back. Anti-rollback floor monotonic. Recovery slot bootable when both A/B fail. |
| `secure_boot.tla` | Chain of trust: BROM → TF-A → U-Boot → kernel. Every link verified before execute. No path executes unverified code in S-mode. |
| `topology.tla` | Admission control: total HardRT utilisation ≤ 1. Class budget guarantees hold under arbitrary task interleaving. |

**Tooling:** TLC model checker (free). Invariants and temporal
properties (`☐(P)`, `☐♦Q`).

**Cadence:** Models are run nightly (cheap) and on PRs that touch
the corresponding subsystem (gated).

**Effort:** ~1 senior engineer for 6 months to write all five
initial models. Maintenance cost ~10% of subsystem work going
forward.

### Layer 2 — Kani (bounded model checking, Rust-native)

**What:** Symbolic execution of Rust functions with bounded inputs.
Proves invariants automatically without explicit spec.

**Scope:** safety crates only — `ipc`, `sched`, `mm`, `ota`,
`crypto`, `behavior::auth_envelope`.

**Patterns:**

```rust
#[cfg(kani)]
#[kani::proof]
fn proof_cap_unforgeability() {
    let handle: u32 = kani::any();
    let kind:   u8  = kani::any();
    // Kani enumerates all possible (handle, kind) and checks the
    // invariant: a cap with kind A cannot be used to invoke an
    // operation that requires kind B.
    let cap = unsafe { Cap::__from_kernel(handle) };
    let result = sys_motor_cmd(cap.cast::<MotorCh>(), 100);
    if cap_table[handle].kind() != Kind::Motor {
        kani::cover!(matches!(result, Err(CapErr::WrongKind)));
    }
}

#[cfg(kani)]
#[kani::proof]
fn proof_tcp_ack_correctness() {
    // ... same idea: bounded range of seq/ack values, prove the
    // ring-buffer ACK never advances past stored bytes.
}
```

**Invariants in scope (Phase 1):**

- Cap unforgeability (kind, generation, perm checks)
- Scheduler: no double-pick of a task across CPUs
- TCP recv: ACK ≤ bytes-actually-stored (we already fixed this; Kani
  locks it down)
- OTA: write-to-tmp + rename atomicity
- HMAC: `auth_envelope` rejects on any single-byte tamper

**Tooling:** Kani (open source, Amazon-developed). Proofs run on a
GitHub Actions runner; budget ~30 minutes/PR.

**Effort:** ~3 months 1 engineer to instrument the invariants.
Ongoing: ~1 day per new safety-crate function.

### Layer 3 — Loom (concurrency property testing)

**What:** Loom permutes thread schedules to find data races and
ordering bugs that "happened to work" in normal testing.

**Scope:** anything with atomics or locks crossing tasks: IPC
primitives, scheduler queues, allocator, secure_channel nonce
counter.

```rust
#[test]
fn loom_fast_ipc_no_lost_message() {
    loom::model(|| {
        let server = thread::spawn(|| { fast_ipc_recv(...) });
        let client = thread::spawn(|| { fast_ipc_send(...) });
        server.join(); client.join();
        assert!(received_count == 1);
    });
}
```

**Invariants in scope:**

- No lost messages on send/recv
- No double-deliver
- Atomic ordering correctness on all `Acquire`/`Release` pairs we
  introduced in the audit
- Cap-table updates are observed atomically

**Effort:** ~1 month to instrument key primitives. Ongoing: write a
loom test alongside every new concurrent primitive.

### Layer 4 — Property-based testing (proptest)

Already in use (regression-tests crate). Expand to cover every
parser (DTB, OTA header, FAT32 entries, brain protocol packets,
TOML topology) and every state machine (TCP, OTA boot validate).

### Layer 5 — Miri (unsafe-code soundness)

Run `cargo miri test` on host-testable crates in CI. Catches:

- Out-of-bounds pointer arithmetic
- Use-after-free
- Strict provenance violations
- Data races (where `loom` doesn't apply)

Already free; we just enable it.

### Layer 6 — External security audit

Annual engagement with Trail of Bits / NCC Group / Cure53 starting
Phase 2. Scope rotates: year 1 IPC + scheduler, year 2 OTA + secure
boot, year 3 AI runtime, etc.

Public report on `docs.phanes.org/audits/`.

### Layer 7 — Internal continuous fuzzing

OSS-Fuzz integration (Phase 2). Continuous fuzzing of every parser.
Bugs auto-filed via OSS-Fuzz.

## CI integration

```yaml
# .github/workflows/verify.yml
jobs:
  tla:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: nix-shell -p tlaplus --run 'tlc -workers auto formal/tla/scheduler.tla'
      - run: tlc -workers auto formal/tla/ipc.tla
      - run: tlc -workers auto formal/tla/ota.tla
      - run: tlc -workers auto formal/tla/secure_boot.tla
      - run: tlc -workers auto formal/tla/topology.tla

  kani:
    runs-on: ubuntu-latest
    steps:
      - uses: model-checking/kani-github-action@v1
      - run: cargo kani --workspace --only-codegen-tests=false

  loom:
    runs-on: ubuntu-latest
    steps:
      - run: RUSTFLAGS="--cfg loom" cargo +nightly test --workspace --features loom

  miri:
    runs-on: ubuntu-latest
    steps:
      - run: cargo +nightly miri test -p ota-tests -p regression-tests

  proptest:
    runs-on: ubuntu-latest
    steps:
      - run: bash scripts/test_regression.sh
```

All jobs are required for merge to `main`.

## Ramping up

| Phase | What's in CI | What's deferred |
|-------|--------------|-----------------|
| 0 | Proptest, Miri | Everything else |
| 1 | + TLA+ scheduler/IPC, Kani caps + ACK + HMAC, Loom on fast_ipc | Full TLA+ ota/secure_boot |
| 2 | + Full TLA+, OSS-Fuzz, first external audit | None |
| 3 | + Mutation testing (`cargo-mutants`) | None |
| 4 | Selective seL4-style theorem proving on the IPC fastpath only | Full kernel proof |

## Drawbacks

- **TLA+ has a learning curve.** Allocate explicit training time for
  the team (3–4 weeks per engineer).
- **Kani has limits** — recursion depth, loop unrolling. We design
  invariants to fit Kani's capabilities, not the other way around.
- **Loom is single-process; can't catch SMP-only bugs.** We
  complement with HIL CI (RFC-0011) on real silicon.
- **External audits are expensive** ($50–150K each). Justified for
  cert + customer-trust signal.
- **No path to ASIL-D from this stack alone.** ASIL-D usually
  requires diverse implementations or formal proof. We accept that
  Phase 4 may need a focused theorem-proving sprint on the fastpath.

## Rationale and alternatives

**Alternative A — full seL4 verification.** Cost: 10+ years, $20M+.
Out of scope.

**Alternative B — testing only.** Insufficient for cert. Auditor
expects formal artifacts.

**Alternative C — only Kani, skip TLA+.** Misses high-level safety
properties expressed naturally in temporal logic (e.g. "no deadline
missed eventually"). TLA+ is cheap and pays back.

**Alternative D (chosen) — layered.** Each layer catches a different
class of bug. None alone is sufficient; together they cover the
ASIL-B and most ASIL-D requirements at affordable cost.

## Prior art

- **seL4** verification: gold standard but unaffordable. Inspires
  what to verify, not how.
- **CompCert** (compiler verification): another point on the cost
  curve.
- **AWS s2n / NitroSecureModule**: production cryptography verified
  with Kani-style tools. Same approach we copy.
- **Tock**: Loom + proptest in production embedded Rust kernel.
- **Hubris**: minimal verification today; CI gates compilation +
  unit tests. We go further than Hubris in this dimension.
- **TLC + TLA+** (Lamport, 2002+). Industry-tested for verifying
  distributed and concurrent systems.

## Unresolved questions

- **What level of FFI invariants do we need to express in TLA+ for
  ISO 26262 ASIL-D?** Working assumption: Phase 1 covers ASIL-B
  level. ASIL-D layer added in Phase 3 with cert auditor input.
- **Can Kani handle the full scheduler `pick_next` symbolically?**
  Probably not — we'll prove bounded portions (single-CPU, single-
  class). Multi-CPU emergent properties go to TLA+.
- **Naming conventions for proofs in code.** Working assumption:
  `proof_*` for Kani, `loom_*` for Loom, `prop_*` for proptest, all
  in `#[cfg(test)]` or `#[cfg(kani)]`.

## Future possibilities

- **Phase 4:** sealed proof scripts for IPC fastpath (Coq or
  Isabelle). 1–2 engineers for ~12 months. Brings us into seL4
  territory for the most-trusted code paths.
- **Phase 4:** model-driven testing — TLA+ models generate test
  vectors automatically, replayed in CI on the implementation.
- **Phase 5:** verified compiler invariants (CompCert-style) for the
  Rust toolchain we use to build PHANES. Out of scope, but a
  research-grade goal.
