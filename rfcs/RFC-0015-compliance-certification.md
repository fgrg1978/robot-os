# RFC-0015: Compliance & Certification — ISO 26262, ISO 21434, AUTOSAR Adaptive Subset

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-08-20


> **Status audit 2026-08-20.** The RFC itself is `accepted` and honest — but
> its illustrative traceability row is wrong on its own terms and should be
> corrected before anyone copies it verbatim. `SR-OTA-001` cites
> `crates/ota/src/secure_boot.rs:120`, which is a `SLOT_B` match arm rather
> than the enforcement logic, and a test
> `tests/ota_signed.rs::test_unsigned_rejected` that **does not exist anywhere
> in the repo**. By this RFC's own proposed CI gate — "every `SR-*` requirement
> must have `code_path` and `test_path` resolving to existing files" — the
> example would fail the moment `safety/traceability.csv` is checked in. The
> real negative test for that requirement is the `secure boot rejects unsigned`
> scenario in `tools/ci_check.sh`.

## Summary

PHANES is engineered from day one with the artefacts needed to
pursue **ISO 26262** functional-safety certification (target: ASIL-B
in Phase 3, ASIL-D pre-validation in Phase 4) and **ISO/SAE 21434**
cybersecurity certification, plus a documented subset of
**AUTOSAR Adaptive** API compatibility for automotive Tier-1
adoption. This RFC defines the engineering process, traceability,
and review gates that make certification achievable rather than
aspirational.

## Motivation

The market gap that PHANES fills (verifiable + AI-native +
multi-platform + open) only matters if PHANES is **actually
adoptable** in regulated industries. Automotive Tier-1 / Tier-2,
medical robotics, industrial robotics, and aerospace all have hard
certification requirements:

- **ISO 26262** — automotive functional safety. ASIL-A through
  ASIL-D. Without this, automotive ECU adoption is impossible.
- **ISO/SAE 21434** — automotive cybersecurity (counterpart to
  26262 for security). Mandatory in EU type approval since 2024.
- **IEC 61508** — generic functional safety (industrial). SIL-1
  through SIL-4.
- **DO-178C** — avionics. Catalogues PHANES functional-safety case;
  not a Phase-3 target but an option.
- **EU CRA** — broad cybersecurity + supply chain. In force 2027.
- **EU AI Act** — AI safety + transparency. In force 2026–2027.

Certification is not magic — it's an audit of engineering process.
We can pass it if we maintain the right artefacts continuously, not
retrofit at the end.

## Detailed design

### Certification roadmap

| Phase | Goal | Specific |
|-------|------|----------|
| 0 (now) | Process foundation | RFC corpus, ADR pattern, traceability scaffolding |
| 1 | TÜV-aligned process audit | OpenSSF Best Practices Badge gold, supply-chain compliant per RFC-0012 |
| 2 | ISO 26262 ASIL-B feasibility study | External assessor (TÜV / SGS / DEKRA) reviews safety case |
| 3 | **ASIL-B certification on i.MX 8M** | Official certificate; ISO 21434 audit pass |
| 4 | ASIL-D pre-validation | Technical evidence sufficient; full cert on customer-funded basis |
| 5 | DO-178C DAL-C feasibility for avionics | If aerospace customer engages |

We do **not** promise ASIL-D in Phase 3 — it requires customer-
funded silicon-specific work that's beyond foundation scope.

### The Safety Case — what auditors actually want

A "safety case" is the document set proving the system meets the
safety target. For ASIL-B:

| Artefact | Where it lives in PHANES |
|----------|--------------------------|
| Hazard analysis & risk assessment (HARA) | `safety/HARA.md` per platform |
| Safety goals + safety requirements | `safety/SAFETY_REQS.md` |
| Functional safety concept | RFC-0001, RFC-0011, RFC-0006 |
| Technical safety requirements (TSR) | Per-RFC requirements, traced to code |
| Hardware-software interface (HSI) | RFC-0008 + topology (RFC-0005) |
| Architectural design | RFC-0002, RFC-0003, RFC-0004, RFC-0008 |
| Software unit design | rustdoc + per-crate ADRs |
| Verification & validation reports | `safety/V&V/<release>/` |
| Coverage reports | RFC-0013 — auto-generated per release |
| Tool qualification reports | `safety/tools/<tool>.md` for compilers, fuzzers |
| Configuration management | RFC-0012 (SBOM + reproducible builds) |
| Change management | Git history + PR review + RFC process |

### Traceability — REQ → CODE → TEST

Every safety-relevant requirement traces to:

1. The RFC that establishes it.
2. The code that implements it (file:line).
3. The test that verifies it.
4. The CI run that ran the test.

Implementation: a `safety/traceability.csv` checked in:

```csv
req_id,rfc,description,code_path,test_path,severity
SR-IPC-001,RFC-0003,Cap forgery shall be impossible,crates/ipc/src/cap.rs:42,crates/ipc/tests/cap_forgery.rs:test_cap_forgery,ASIL-B
SR-SCHED-001,RFC-0004,Safety-class deadline shall be met,crates/sched/src/policies/edf.rs:88,crates/regression-tests/sched_edf.rs:test_deadline,ASIL-B
SR-OTA-001,RFC-0011,Unsigned firmware shall be rejected,crates/ota/src/secure_boot.rs:120,tests/ota_signed.rs:test_unsigned_rejected,ASIL-B
...
```

CI gate: every `SR-*` requirement must have `code_path` and
`test_path` resolving to existing files; the test must have run
and passed in the latest CI run.

### Coding standards

We adopt three layered coding standards, pulling forward existing
SC01 work in the kernel:

1. **MISRA-C-equivalent for Rust** — adapted from
   "Rust Embedded Working Group safety guidelines" + Ferrocene's
   safety subset.
2. **IEC 61508 Annex C** — for general systematic safety integrity.
3. **ISO 26262 Part 6** — software unit design + implementation.

Specific PHANES rules (codified in `safety/CODING_STANDARD.md`):

- **No dynamic allocation in safety paths.** Allocator banned in
  `crates/{ipc,sched,ota,crypto,arch}` at compile time
  (`#![cfg_attr(not(test), no_alloc)]` enforced via custom proc
  macro).
- **All loops bounded** — every loop must have a static-analysable
  upper bound. Lint enforced.
- **No panics in safety paths** — `cargo-call-stack` proves
  panic-freedom; checks `core::panicking::panic_fmt` is unreachable.
- **No recursion** — banned via call-graph analysis.
- **No `unsafe` without `// SAFETY:` comment** — RFC-0013 lint.
- **All public APIs have type-state where possible** — e.g.,
  capability checks at compile time (RFC-0003).
- **Numeric overflow either explicit or panic-free** — `unwrap()`
  banned in safety paths; `checked_*` / `saturating_*` mandatory.
- **No floating point in safety scheduler / deadline math** —
  fixed-point integer only; FP allowed in non-safety AI paths.
- **Bounded recursion in formal proofs** — Kani harnesses bounded
  loops to enable model-checker termination.

CI enforces all of the above via clippy + custom lints + static
analysis.

### Tool qualification (TCL)

ISO 26262 requires that any tool whose error could miss a defect
or insert one must be **qualified**. Our tool inventory:

| Tool | Use | TCL classification (target) | Qualification approach |
|------|-----|------------------------------|------------------------|
| `rustc` | Compiler | TCL3 (highest) | Use **Ferrocene** (qualified rustc). Phase 2+. |
| `cargo` | Build orchestrator | TCL2 | Reproducible builds (RFC-0012) + tests. |
| `clippy` | Lint | TCL1 | Verify no false negatives via mutation tests (RFC-0013). |
| `cargo-llvm-cov` | Coverage | TCL1 | Manual sampling vs. tarpaulin cross-check. |
| `cargo-mutants` | Mutation testing | TCL1 | Tool error → false-positive only; safety neutral. |
| `kani` | Model checker | TCL2 | Trust AWS-maintained, version-pinned, manually inspected proofs. |
| `proptest` | Property testing | TCL1 | Random gen → false-negative neutral. |
| `loom` | Concurrency tester | TCL1 | False-negative neutral. |
| Sigstore cosign | Signing | Out of scope | Not in build pipeline that produces ASIL artefact. |

**Ferrocene** is the qualified Rust compiler from Ferrous Systems
(certified for ISO 26262 TCL3 + IEC 61508 + IEC 62304 + EN 50128).
We adopt it for ASIL-B builds in Phase 3.

### ISO/SAE 21434 — cybersecurity

This standard is the security counterpart to 26262. Required
artefacts:

- **TARA** (Threat Analysis & Risk Assessment) — `security/TARA.md`.
- **Security concept** — RFC-0011 (boot), RFC-0012 (supply chain),
  RFC-0009 (PSIRT).
- **Security requirements** — `security/SEC_REQS.md`, traced like
  safety requirements.
- **Verification of security goals** — penetration tests + fuzzing
  + formal verification of crypto primitives.
- **Vulnerability management** — RFC-0009 PSIRT + bug bounty.
- **Security incident response** — documented procedure.
- **Cybersecurity case** — analogous to safety case, focused on
  threats.

### AUTOSAR Adaptive subset

AUTOSAR Adaptive Platform is the modern (post-Classic) automotive
standard for high-end ECUs running infotainment, ADAS, and
autonomous-driving software. Tier-1s expect a "POSIX-like" API
surface compatible with AUTOSAR AP.

PHANES will provide an **AUTOSAR-compatibility crate**
(`crates/autosar-ap/`) that exposes a documented subset:

- `ara::com` — service-oriented communication (mappable to PHANES
  capability IPC).
- `ara::log` — logging (mappable to PHANES `crates/log/`).
- `ara::diag` — diagnostics (mappable to PHANES OTA + telemetry).
- `ara::exec` — execution management (mappable to PHANES scheduler
  + supervisor).
- `ara::per` — persistency (mappable to PHANES FAT32 + tmpfs).

This is **subset, not full AUTOSAR-AP**. Goal: enough surface for
demo apps to port; not a full AUTOSAR replacement. Customer-funded
expansion if needed.

### Audit cadence

- **Internal audit** — every release; safety + security cases
  re-checked.
- **External assessor** — Phase 3, then yearly.
- **Pen test** — yearly (Phase 2+); Phase 3+ before each major
  release.
- **Bug bounty** — continuous (RFC-0009).
- **OSS-Fuzz** — continuous (RFC-0013).

### Cost & timeline reality check

| Phase | Approximate cost | Notes |
|-------|------------------|-------|
| 0–1 | ~$50K | Internal process build |
| 2 | ~$300K | First external assessor + pen test |
| 3 | ~$1.5–2M | Full ASIL-B cert: assessor fees + Ferrocene license + dev time |
| 4 | ~$2.5–3.5M | ASIL-D pre-validation + customer-funded targeted certs |
| 5 | TBD | Customer-driven (avionics, medical) |

Foundation revenue model (training, support, certified-build
service) needs to fund this; otherwise customer-funded pilots
shoulder Phase 3+.

## Drawbacks

- **Massive documentation overhead** — accepted.
- **Process discipline costs ~25% of engineering time** —
  accepted; this is what cert really costs.
- **Tool qualification requires Ferrocene** — paid, but the only
  realistic path. Foundation budget Phase 3.
- **Cert is platform-specific** — i.MX 8M Plus first. Each
  additional platform = additional cert work.

## Rationale and alternatives

**Alternative A — skip cert, target hobbyist + research only.**
Eliminates entire automotive/medical/aerospace markets. Rejected.

**Alternative B — single hard target (ASIL-D Phase 1).** Too
ambitious; budget burns before product matures. Rejected.

**Alternative C (chosen) — staged: ASIL-B Phase 3, ASIL-D pre-
validation Phase 4, customer-driven beyond.** Realistic and
funding-aligned.

## Prior art

- **QNX Neutrino** — ISO 26262 ASIL-D certified. The reference for
  what we're trying to match (open).
- **VxWorks** — DO-178C certified.
- **Integrity** — ASIL-D + DO-178C + medical.
- **seL4** — formally proven; uniquely positioned but not certified
  per industry standard (formal proof ≠ ISO cert).
- **Hubris** — ISO 26262 ASIL-B in progress (Oxide Computer).
- **Apex.OS** — ASIL-D-certified ROS-API-compatible RTOS (closed).
- **Ferrocene** — qualified Rust toolchain.

## Unresolved questions

- **Which assessor?** TÜV vs. SGS vs. DEKRA. Working assumption:
  TÜV SÜD (largest automotive practice, EU-based, widely accepted).
- **Will Linux Foundation host certification work?** Unclear; LF
  Energy + Zephyr have set precedent. Working assumption: yes,
  via a "PHANES Safety WG."
- **Multi-platform cert strategy.** Each platform = new safety
  case but most artefacts shared. Work-out: Phase 3 cert is i.MX
  8M Plus; Phase 4 adds RV64 (VF2 + K1) cert path.

## Future possibilities

- **Phase 4:** Medical device cert (IEC 62304 / FDA Class II) on
  customer demand.
- **Phase 5:** Avionics DO-178C DAL-C / DAL-B.
- **Phase 5:** Rail (EN 50128).
- **Phase 5:** Defence (DO-356 / Common Criteria EAL).
