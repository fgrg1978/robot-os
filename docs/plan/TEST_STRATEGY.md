# PHANES — Test Strategy

> **Audience:** engineers, QA, auditors  
> **Pre-requisites:** RFC-0006 (verification), RFC-0013 (quality), RFC-0015 (cert)  
> **Last updated:** 2026-05-10

This document specifies the test strategy across the eight tiers
PHANES uses, the per-phase coverage targets, the HIL CI farm
plan, and the per-tier responsibilities of kernel vs. brain code.

---

## 1. Eight-tier test pyramid

```
          ┌──────────────────────────────────────────┐
          │  T8 — External cert assessment (Phase 3+)│
          ├──────────────────────────────────────────┤
          │  T7 — Continuous fuzzing (OSS-Fuzz)      │
          ├──────────────────────────────────────────┤
          │  T6 — HIL CI (real silicon, Phase 2+)    │
          ├──────────────────────────────────────────┤
          │  T5 — Soak / chaos (24h QEMU + chaos eng)│
          ├──────────────────────────────────────────┤
          │  T4 — End-to-end integration (sim + KOS) │
          ├──────────────────────────────────────────┤
          │  T3 — Concurrency (Loom) + property-based│
          ├──────────────────────────────────────────┤
          │  T2 — Formal (TLA+ + Kani)               │
          ├──────────────────────────────────────────┤
          │  T1 — Unit + mutation                    │
          └──────────────────────────────────────────┘
                       (broadest base ↑)
```

Each tier catches a different class of bug. None substitutes for
another.

---

## 2. Per-tier responsibility

| Tier | What it catches | Tools | Cadence |
|------|------------------|-------|---------|
| T1 Unit + mutation | Logic bugs in single function / module | `cargo test`, `pytest`, `cargo-mutants`, `mutmut` | Per-PR (unit), weekly (mutation) |
| T2 Formal | Algorithmic invariants, race-free designs | TLA+, Kani, Alloy | Per-PR for safety crates; quarterly review for invariant additions |
| T3 Concurrency + property | Race conditions, edge-case input | Loom, proptest, hypothesis | Per-PR |
| T4 End-to-end integration | Subsystem composition errors | QEMU integration harness, sim adapters (Isaac/MuJoCo/Genesis) | Per-PR |
| T5 Soak + chaos | Latent leaks, non-deterministic bugs | `scripts/soak_qemu.sh`, chaos toolkit | Daily (30 min); weekly (24 h) |
| T6 HIL CI | Hardware-specific bugs (driver, timing, voltage) | Self-hosted runner + USB DFU | Per-PR (Phase 2+) |
| T7 Continuous fuzzing | Untrusted-input bugs (parsers, network) | OSS-Fuzz, atheris | Continuous |
| T8 External cert | Process + design defects | TÜV / DEKRA assessor | Phase 3 entry, then yearly |

---

## 3. Coverage targets — kernel

Per RFC-0013, with phase-by-phase ratchet:

| Crate | Phase 0 floor | Phase 1 target | Phase 2 target | Phase 3 (cert-ready) |
|-------|---------------|-----------------|------------------|---------------------|
| `crates/ipc` | current ≥ 75% | ≥ 85% | ≥ 90% | ≥ 95% |
| `crates/sched` | current ≥ 70% | ≥ 85% | ≥ 90% | ≥ 95% |
| `crates/mm` | current ≥ 70% | ≥ 80% | ≥ 85% | ≥ 90% |
| `crates/ota` | current ≥ 80% | ≥ 90% | ≥ 90% | ≥ 95% |
| `crates/crypto` | current ≥ 90% | ≥ 95% | ≥ 95% | ≥ 98% |
| `crates/net` | current ≥ 70% | ≥ 80% | ≥ 85% | ≥ 90% |
| `crates/fs` | current ≥ 65% | ≥ 75% | ≥ 80% | ≥ 85% |
| `crates/drivers/*` | current varies | ≥ 70% | ≥ 75% | ≥ 80% |
| `crates/behavior` | current ≥ 75% | ≥ 85% | ≥ 90% | ≥ 95% |
| `crates/abi` | n/a (types only) | n/a | n/a | n/a |

**Branch coverage:** ~10 percentage points below the line target,
per crate.

**Mutation kill rate:** ≥ 70% Phase 1, ≥ 80% Phase 3 on safety
crates.

---

## 4. Coverage targets — brain (Python)

| Module | Phase 0 floor | Phase 1 target | Phase 3 |
|--------|---------------|-----------------|---------|
| `protocol.py` | ≥ 80% | ≥ 95% | ≥ 95% |
| `secure_channel.py` | ≥ 80% | ≥ 95% | ≥ 95% |
| `api.py` | ≥ 70% | ≥ 85% | ≥ 90% |
| `planner/*` | ≥ 65% | ≥ 80% | ≥ 85% |
| `executor/*` | ≥ 65% | ≥ 80% | ≥ 85% |
| `notifications.py` | ≥ 50% | ≥ 70% | ≥ 75% |
| `dashboard/*` | ≥ 50% | ≥ 70% | ≥ 75% |
| `fleet/*` | ≥ 60% | ≥ 80% | ≥ 85% |

Brain isn't cert-scope but it's the operator-facing surface; bugs
here cause user-visible incidents.

---

## 5. T2 — Formal verification

### TLA+ models

Minimum set Phase 1:

| Spec | What's modelled |
|------|-----------------|
| `formal/tla/cap_table.tla` | Capability table, generation monotonicity |
| `formal/tla/sched_aps.tla` | APS partition fairness |
| `formal/tla/edf_cbs.tla` | EDF deadline + CBS budget |
| `formal/tla/ota_state.tla` | OTA A/B atomic + rollback |
| `formal/tla/auth_envelope.tla` | Replay-attack resistance |
| `formal/tla/topology_load.tla` | Topology load atomicity |

**Cadence:** TLC model-check on every PR that touches the
modelled subsystem.

### Kani harnesses

Minimum set Phase 1:

| Harness | Property |
|---------|----------|
| `formal/kani/cap_forge.rs` | Cap forgery returns Err |
| `formal/kani/cap_dealloc_safe.rs` | Use-after-free impossible |
| `formal/kani/edf_deadline.rs` | EDF picks earliest deadline |
| `formal/kani/dtb_parse_safe.rs` | DTB parser no panic on adversarial input |
| `formal/kani/ota_anti_rollback.rs` | Counter monotonic |

**Cadence:** Kani run on every PR that touches the harness or its
target.

### Loom

Already used in `auth_envelope_tests.rs`. Extend to:

| Loom test | Concurrency property |
|-----------|----------------------|
| `loom/cap_table.rs` | Cap table is data-race free under SMP |
| `loom/channel.rs` | Channel send/receive ordering |
| `loom/scheduler_runqueue.rs` | Runqueue insert/extract ordering |
| `loom/secure_channel.rs` | Session key rotation atomicity |
| `loom/ota_state.rs` | A/B switch atomic across power loss simulation |

---

## 6. T4 — End-to-end integration

### QEMU integration tests

`scripts/qemu.sh full` runs the full E2E suite:

1. Boot kernel in QEMU (default + smp + RVV variants).
2. Brain (Python) connects via TCP.
3. Brain issues mission via `task_planner`.
4. Kernel executes skills; sensors stream back; safety active.
5. Mid-mission inject failures (network drop, sensor glitch).
6. Verify recovery + correct mission completion.

### Sim adapters (Phase 1+)

| Sim | Purpose |
|-----|---------|
| Isaac Sim | Photorealistic; complex scenarios; PHANES-brain ↔ kernel via TCP-loopback |
| MuJoCo | Lightweight; physics; fast iteration |
| Genesis | Open, GPU-accelerated; large-scale fleet sims |
| Gazebo | ROS interop, broad community |

Brain-side `sim/` adapters speak each sim's API; kernel runs in
QEMU. Same kernel binary tested against all four sims.

---

## 7. T5 — Soak + chaos

### Daily soak (30 min)

Per platform target (QEMU default, smp, RVV, no-mmu, no-ml):

```
boot → brain connect → mission running on infinite loop:
  1. spawn skill (drive forward)
  2. inject sensor glitch every 5 s
  3. swap mode every 30 s
  4. trigger ESTOP every 60 s
  5. recover and resume
assertions:
  - no kernel panic
  - no fatal log
  - no canary violation
  - heap usage stable (< 10% drift)
  - rss stable
```

### Weekly soak (24 h)

Same but with:
- Random network blackouts (10–30 s every 10 min)
- Random brain restarts (every hour)
- OTA mid-flight (slot switch + reboot)
- Power glitch simulation (kill QEMU mid-OTA, restart, verify
  recovery)

### Chaos toolkit

`scripts/chaos.sh`:

- `--kill-brain-mid-mission` — kill brain at random; kernel must
  enter offline mode + complete mission within 60s.
- `--corrupt-flash` — flip random bits in non-active OTA slot;
  verify boot still works from active slot.
- `--clock-skew` — set system clock to past / future; verify
  monotonic time math holds.
- `--memory-pressure` — flood with allocations; verify safety
  paths unaffected.

---

## 8. T6 — HIL CI farm (Phase 2+)

### Phase 2 hardware fleet

| Board | Quantity | Role |
|-------|----------|------|
| StarFive VisionFive 2 | 4 | RV64 reference |
| Banana Pi BPI-F3 (K1) | 4 | RV64 alt platform |
| NXP i.MX 8M Plus EVK | 4 | ARM cert reference |
| Rockchip RK3588 | 2 | ARM AI-class |
| Cortex-R52 (NXP S32) | 2 | RT-class ARM |
| ESP32-C3 (companion MCU) | 4 | Vigilance / wake gatekeeper |

### Architecture

```
GitHub Actions (cloud)
        │ schedules
        ▼
Self-hosted runner (Linux box, on-premises)
        │ controls power + UART + JTAG + USB-DFU
        ▼
USB hub controlled by PCI relay → power-cycle each device on cmd
        │ flash via DFU
        ▼
Test orchestrator: connects to UART, runs scenarios, asserts
```

### What HIL catches that QEMU doesn't

- Real timer drift, real interrupt latency (vs. QEMU's
  optimistic).
- DMA / cache coherency on real silicon.
- Real driver edge cases (e.g. NIC PHY hand-shake).
- Real OTA flash erase/write timing.
- Power-glitch induced corruption.

### Cadence

- **Per-PR**: smoke test (boot + 5-min mission) on each platform.
- **Daily**: full unit + integration suite on each platform.
- **Weekly**: 24-h soak on each platform.

### Phase 2 budget

~$50K hardware + ~$5K infra (network, power, racking) + ongoing
electricity / replacement.

---

## 9. T7 — Continuous fuzzing

OSS-Fuzz integration; targets in RFC-0013 §"Continuous fuzzing".

**Phase 1 corpus seed:** every protocol parser + every state-
machine entry point gets ≥ 1000 corpus seeds drawn from existing
test cases.

**Crash policy:** OSS-Fuzz auto-files; PSIRT triages within 48 h
(critical) / 5 days (other).

---

## 10. Regression test catalogue

`crates/regression-tests/` is the kitchen-sink of "things that
have ever broken." Every fixed bug gets a test added here. Never
delete a regression test. Phase 1 inventory includes:

- TCP retransmit edge cases
- ARP request/reply state machine
- DTB parse depth (we already fixed)
- OTA slot switch with power loss
- Sched preemption when budget exhausts mid-task
- Cap table generation wrap-around (16M cycles)
- secure_channel rekey race
- Geofence crossing at exact boundary
- ESTOP latency under load
- COW fork stress
- HMAC envelope replay window
- ...

Total Phase 0: ~70 tests. Target Phase 3: ≥ 500 regression tests.

---

## 11. CI matrix

| Stage | Targets | Time budget per PR |
|-------|---------|---------------------|
| `cargo build` | 5 configs (qemu/vf2/k1/no-ml/no-mmu) | 6 min |
| `cargo test --all` | host-portable | 4 min |
| `cargo clippy --all-targets -- -D warnings` | all | 2 min |
| `cargo deny + cargo audit` | all | 1 min |
| QEMU integration | default + smp | 5 min |
| Loom + proptest | safety crates | 4 min |
| Kani | safety crates | 6 min |
| Brain pytest + hypothesis | full | 3 min |
| `mypy --strict` | brain safety | 1 min |
| `ruff + bandit + pip-audit` | brain | 1 min |
| HIL smoke (Phase 2+) | all platforms parallel | 5 min |

**Total per-PR (Phase 1):** ~35 min wall-clock with parallel
matrix.

**Per-night additional:** mutation testing (4 h), full QEMU soak
(30 min × 5 platforms = 2.5 h).

---

## 12. Test data discipline

- All test inputs deterministic by default.
- Random tests use seeded RNG with seed printed on failure.
- Fuzzers store crashing inputs in `fuzz/corpora/`.
- No live secrets in fixtures (use placeholder keys).
- Network tests use loopback or fake-net adapters; no external
  network in CI without explicit opt-in.

---

## 13. Tool qualification

Per RFC-0015 §"Tool qualification":

- Compilers (`rustc`/Ferrocene): TCL3, qualified evidence
  required Phase 3.
- Coverage tools (`cargo-llvm-cov`, `coverage.py`): TCL1; verified
  by cross-tool comparison.
- Mutation tools: TCL1; false-positive only impact, safety-
  neutral.
- Fuzzers: TCL1; sample evidence checked manually quarterly.

---

## 14. Phase exit criteria

| Phase | Test exit gate |
|-------|----------------|
| 0 | Process + RFCs done; no test gate change |
| 1 | T1+T2+T3+T4 fully wired; coverage Phase-1 targets met |
| 2 | T5+T6+T7 wired; HIL farm operational; OpenSSF Best Practices Badge gold |
| 3 | T8 external assessor pass on i.MX 8M Plus; ASIL-B cert |
| 4 | All targets above plus customer-funded ASIL-D pre-validation |
