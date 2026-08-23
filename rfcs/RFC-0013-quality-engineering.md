# RFC-0013: Quality Engineering — Coverage, Fuzzing, Mutation Testing

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

PHANES enforces quantitative quality gates in CI: line + branch
coverage thresholds per crate, mutation testing on safety paths,
continuous fuzzing on every parser via OSS-Fuzz, type checking
strict on all safety-critical modules. Failures block PR merge.

## Motivation

"It builds and tests pass" is not a sufficient quality bar for an
OS targeting cert. We need:

- **Quantitative coverage** — auditor expects ≥ 80% line and
  ≥ 70% branch coverage on safety crates.
- **Mutation testing** — verifies that the test suite *actually*
  catches bugs (not just that lines were executed).
- **Fuzzing** — finds bugs that hand-crafted tests miss.
- **Type discipline** — `unsafe` audited; Python `mypy --strict`.
- **Static analysis** — clippy pedantic + cargo-deny.

These gates run in CI; PRs failing them cannot merge.

## Detailed design

### Coverage thresholds

Per-crate gates (enforced via `cargo-llvm-cov` for kernel,
`coverage.py` for brain):

| Crate | Line coverage | Branch coverage |
|-------|---------------|-----------------|
| `crates/ipc` (caps, IPC) | ≥ 90% | ≥ 85% |
| `crates/sched` (scheduler) | ≥ 90% | ≥ 80% |
| `crates/mm` (memory) | ≥ 85% | ≥ 75% |
| `crates/ota` (firmware update) | ≥ 90% | ≥ 80% |
| `crates/crypto` (security primitives) | ≥ 95% | ≥ 90% |
| `crates/net` (TCP, IP) | ≥ 80% | ≥ 70% |
| `crates/fs` (FAT32, vfs) | ≥ 80% | ≥ 70% |
| `crates/drivers` | ≥ 70% | ≥ 60% |
| `crates/behavior` (auth_envelope, layers) | ≥ 85% | ≥ 75% |
| `phanes-brain/protocol.py` | ≥ 95% | ≥ 90% |
| `phanes-brain/secure_channel.py` | ≥ 95% | ≥ 90% |
| `phanes-brain/api.py` | ≥ 85% | ≥ 75% |
| `phanes-brain/planner/*` | ≥ 80% | ≥ 70% |

**Drift policy:** PR may not decrease coverage on any crate. If a
PR decreases coverage, it must include rationale and reviewer
approval.

### Mutation testing

**Tooling:** `cargo-mutants` for Rust, `mutmut` for Python.

**Cadence:** weekly on `main`; gated on release branches.

**Scope:** safety paths only (mutation testing is slow):

- Kernel: `crates/{ipc,sched,ota,crypto}`, `auth_envelope`.
- Brain: `protocol.py`, `secure_channel.py`, planner parsers.

**Threshold:** ≥ 70% mutants killed (i.e., the test suite catches
70% of artificial bugs introduced).

If a mutation survives, the surviving change is reviewed: either
add a test that catches it, or document why the mutation is
inconsequential.

### Continuous fuzzing — OSS-Fuzz

**Targets** (Phase 2 onwards):

| Target | Why |
|--------|-----|
| `dtb_parse` | Already had a depth bug we fixed |
| `ota_parse_header` | Untrusted external input |
| `parse_packet` (brain protocol) | Untrusted network input |
| `tcp::handle` | Untrusted network input |
| `arp::handle` | Same |
| `ip::handle` | Same |
| FAT32 BPB / cluster parsers | Untrusted disk input |

OSS-Fuzz integration:

- `fuzz/` directory with `cargo-fuzz`-style harnesses.
- `infra/oss-fuzz/project.yaml` for OSS-Fuzz upstream.
- Crashes auto-filed via OSS-Fuzz against `security@phanes-project.org`.
- Public dashboard at oss-fuzz.com.

For brain side: `atheris` (Google's Python coverage-guided fuzzer).
Same pattern: `phanes-brain/fuzz/` directory.

### Type checking

**Rust:** `rustc` is the type checker; we add:

- `clippy::pedantic` on safety crates (RFC-0013 SC01.B in current
  codebase already partially does this).
- `clippy::nursery` warnings reviewed (don't auto-deny).
- `unsafe` audited via custom lint: every `unsafe` block must have
  `// SAFETY:` comment.

**Python:** `mypy --strict` on safety modules:

```toml
# pyproject.toml
[tool.mypy]
strict = true

[[tool.mypy.overrides]]
module = ["protocol", "secure_channel", "api"]
strict = true
disallow_untyped_calls = true
warn_return_any = true
```

### Static analysis pipeline

| Tool | Scope | Severity |
|------|-------|----------|
| `cargo clippy --all-targets -- -D warnings` | Rust everywhere | Error |
| `cargo deny check` | License + advisory | Error |
| `cargo audit` | CVE database | Error on high+ |
| `clippy::pedantic` | Safety crates (RFC-0013 SC01) | Error |
| `clippy::nursery` | All | Warning (review) |
| `ruff check` | Python | Error |
| `mypy --strict` | Brain safety modules | Error |
| `bandit` | Python security lint | Error on high+ |
| `pip-audit` | Python CVE | Error on high+ |
| `Semgrep` | Custom rules (Phase 2) | Error |

### Property-based testing

Already in use (proptest in `crates/regression-tests`,
`hypothesis` to be added in brain). Required for:

- Every parser
- Every state machine
- Every wire-format encoder/decoder
- Every numeric / arithmetic helper that could overflow

### Concurrency testing — Loom

`crates/regression-tests/src/auth_envelope_tests.rs` already loom-
tested. Extend to: every IPC primitive, scheduler queues,
allocator, every `Acquire` / `Release` pairing introduced in the
audit.

### Soak tests — TS02

Daily in CI: 30-minute QEMU soak per platform target. Weekly:
24-hour soak. Asserts: no panic, no fatal, no page fault, no
canary violation, no OOM.

### Hardware-in-the-Loop CI (Phase 2)

Real silicon (VF2, K1, i.MX, ARM Cortex-R) connected to a CI
server. Every PR runs unit tests + integration tests on real
hardware.

Architecture (Phase 2 deliverable):

```
GitHub Actions runner
       │ schedules
       ▼
Self-hosted runner (Mac mini or Linux box)
       │ controls power + UART + flash via USB
       ▼
[VF2] [K1] [i.MX 8M] [Cortex-R52]   ← physical SBCs
```

Tests run end-to-end: flash via USB DFU, boot, drive UART,
collect logs, assert.

### Performance regression tracking

`benchmarks/` directory with criterion-based benches. Per-PR
output uploaded to a benchmark dashboard; PRs that regress > 5%
without justification are flagged.

Targets:

| Bench | Target | Regression threshold |
|-------|--------|----------------------|
| Timer ISR wall time | < 50 µs QEMU | +5% |
| Context switch latency | < 5 µs hardware | +5% |
| TCP packet RTT (loopback) | < 200 µs | +5% |
| HMAC envelope wrap+unwrap | < 30 µs | +5% |
| FAT32 small-file read | < 5 ms warm cache | +10% |

### Test infrastructure summary

| Tier | Tool | Cadence |
|------|------|---------|
| Unit | `cargo test`, `pytest` | Per-PR |
| Property | `proptest`, `hypothesis` | Per-PR |
| Mutation | `cargo-mutants`, `mutmut` | Weekly + release |
| Fuzz | OSS-Fuzz, `atheris` | Continuous |
| Concurrency | Loom | Per-PR |
| Soak | `scripts/soak_qemu.sh` | Daily |
| HIL | Self-hosted runner (Phase 2) | Per-PR |
| Bench | criterion + dashboard | Per-PR |

## Drawbacks

- **CI time** — full pipeline ~30 minutes per PR. Mitigated by
  parallel matrix and incremental builds.
- **Mutation testing slow** — hours for full run. Run weekly, not
  per-PR.
- **HIL requires hardware investment** (~$30–50K). Justified
  Phase 2.
- **Strict mypy on brain has migration cost** — ~2–3 weeks
  retrofitting types. Already started in protocol.py.

## Rationale and alternatives

**Alternative A — manual test discipline.** Rejected: doesn't scale
to 5+ engineers; loses cert audit.

**Alternative B — coverage only (no mutation, no fuzz).**
Insufficient: high coverage with bad tests is the classic failure
mode.

**Alternative C (chosen) — multi-layer quantitative gates.**
Industry standard for safety-critical software.

## Prior art

- **Linux kernel** — has its own elaborate testing (LTP, syzkaller,
  KASAN). Inspires.
- **Rust `rustc` itself** — uses `crater` for ecosystem-wide
  regression checks.
- **Tock** — Loom + proptest from day one; we follow.
- **Hubris** — has its own test infrastructure; we copy patterns.
- **AWS s2n** — Kani + LibFuzzer + cargo-mutants on a production
  TLS stack. Reference quality.
- **OSS-Fuzz** — Google's continuous fuzzing infrastructure;
  PHANES will integrate.

## Unresolved questions

- **Initial coverage gap.** Some safety crates today are below the
  thresholds proposed here. Phase 1 includes a coverage-ratchet
  plan: each PR must not decrease; coverage rises monthly until
  threshold met.
- **HIL CI hardware cost** — exact spec depends on platform mix.
  Working assumption: ~$50K for Phase 2 buy-in.
- **Mutation budget** — full sweep takes hours. Phase 1: weekly
  per-crate. Phase 2: parallelised, daily.

## Future possibilities

- **Phase 4:** Differential testing — feed same inputs to PHANES
  TCP and Linux TCP, compare. Catches subtle protocol bugs.
- **Phase 4:** Symbolic execution at scale (Klee on host-portable
  parts).
- **Phase 5:** Quantum-safe cryptography re-validation under same
  gates.
