# PHANES — Linux Foundation Incubation Application

> **Status:** draft (submitted concurrent with PHANES v1.0.0)  
> **Target foundation:** Linux Foundation, robotics / safety-critical
> SIG track  
> **Authority:** RFC-0009 (governance) — Apache 2.0 + LF incubation
> decided.

This document is the application package PHANES submits to the
Linux Foundation when seeking incubation status. It follows the
standard
[LF Project Lifecycle](https://www.linuxfoundation.org/lifecycle)
template plus the additional artefacts LF requests for
safety-critical projects.

---

## 1. Project Name

> **RENAMED (2026-08-23, user decision): the project is now called
> `KernOS`.** The text below is preserved because the RFC-0010 trademark
> analysis was done for PHANES and must be **redone for KernOS** before
> submitting this application. The mechanical rename of the tree (commit
> prefixes, `PHANES Phase ...` comments, the `aps_state` module) is
> scheduled for the post-hardware batch.

**PHANES** *(previous name)* — Greek primordial deity of light and
creation, "the first form". The project's tagline:

> *Verifiable. AI-native. Multi-platform. Real-time. Open.*

The name was selected after extensive trademark and collision
analysis (see RFC-0010). Closest existing uses are pharmaceutical
(Phanes Therapeutics) and networking (Phanes Networks/Cloud) —
neither blocks robotics-OS usage.

## 2. One-paragraph description

PHANES is an open-source operating system for autonomous robots
and safety-critical embedded systems. Written in Rust (`no_std`),
it provides capability-typed IPC (`Cap<T>`), a multi-policy
hierarchical scheduler (RFC-0004) with five class-budget partitions,
hardware-rooted secure boot, an AI runtime (Model Bundle), and
multi-architecture support (RV64 + ARM Cortex-A/R + x86_64). The
project is engineered from day one for ISO 26262 ASIL-B
certification (target: Phase 3) and EU Cyber Resilience Act
compliance.

## 3. Project goals

| Horizon | Goal |
|---------|------|
| 6 months (Phase 1) | Stable ABI v1.0, OpenSSF Best Practices Badge, single-platform RV64 production. **Status: in progress, v1.0.0-rc1 tagged.** |
| 12 months (Phase 2) | Multi-platform (RV64 + ARM-A + ARM-R + x86_64), AI runtime (.MBL), HIL CI farm. |
| 24 months (Phase 3) | ISO 26262 ASIL-B certification on NXP i.MX 8M Plus, LTS v1 maintenance, bug bounty program live. |
| 36 months (Phase 4) | i18n docs (EN/ZH-CN/DE/JA), second SoC cert, ASIL-D pre-validation. |
| 60 months (Phase 5) | Customer-funded extensions: medical (IEC 62304), avionics (DO-178C), rail (EN 50128). |

Full plan: `rfcs/RFC-0001-strategic-plan.md`.

## 4. Why incubate with the Linux Foundation?

PHANES targets industries — automotive, medical, aerospace — where
LF projects (Zephyr, ELISA, Automotive Grade Linux) have established
credibility, governance, and cert paths. Specific value:

1. **Cross-industry credibility.** PHANES will sit alongside Zephyr,
   AGL, ELISA — natural neighbours for a safety-critical robotics OS.
2. **Cert support infrastructure.** LF's ELISA project (Enabling
   Linux In Safety Applications) has paved the way for Linux-based
   safety certification; their lessons inform PHANES's parallel
   path.
3. **Foundation legal / IP / governance scaffolding.** PHANES is
   small enough that DIY foundation infra would burn most of the
   energy. LF handles this professionally.
4. **Trademark holding.** Foundation-owned brand is a stronger
   trust signal for cert customers.
5. **Diversity of contributors.** LF projects attract contributors
   that wouldn't engage with single-vendor-led projects.

## 5. Current state

**Status of code as of v1.0.0-rc1 (2026-05-14):**

- 13 git commits on `main`, ~50,000 LoC Rust across 30+ workspace
  crates.
- 5 build configurations clean (QEMU + VisionFive 2 + SpacemiT K1
  + no-ml + no-mmu).
- 1341 tests passing in CI: 103 regression + 58 OTA + 24 topology
  + 18 ABI + 7 capability + 44 scheduler-policy + 1087 brain (Python).
- 13,933 TLA+ states model-checked across 3 specs (`cap_table`,
  `topology_load`, `sched_aps`); all invariants satisfied.
- ABI frozen at v1.0 (`crates/abi/CHANGELOG.md`).
- 16 RFCs + 8 strategic documents in `rfcs/` and `docs/plan/`.

**Maturity:** Pre-incubation early-stage. The technical foundation
is in place; the contributor community is single-author at
submission time but the design is engineered for multi-vendor
collaboration (modular pattern RFC-0002, capability discipline
RFC-0003, three-tier project separation RFC-0018).

## 6. Initial committers

- **Fernando Rodriguez** (project author, RFCs 0001-0018,
  initial implementation) — committer + maintainer.

PHANES is **explicitly seeking** additional initial committers
during incubation. Target roles:

- **Scheduler / RT specialist** (own RFC-0004 implementation
  evolution)
- **Cryptography / secure-boot lead** (own RFC-0011)
- **AI runtime architect** (own RFC-0007 Phase 2 work)
- **Cert engineer** (own RFC-0015 Phase 3 work)
- **Documentation / DevRel lead**

LF incubation status helps us recruit these roles; pre-incubation
they are gating commits.

## 7. Source code repository

- **Primary repository:** `github.com/phanes-project/phanes`
  *(currently at `github.com/<personal>/robot-os`; will be
  transferred upon incubation acceptance)*
- **Brain framework:** `github.com/phanes-project/phanes-brain`
  *(currently at `github.com/<personal>/robot-brain`)*
- **Issue tracker:** GitHub Issues (incubated under the
  `phanes-project` GitHub organization owned by the LF).

## 8. License

**Apache License 2.0** — confirmed in RFC-0009 (governance) and
applied uniformly across all source files. Dependencies are
audited via `cargo-deny` to require permissive licenses (Apache
2.0 / MIT / BSD / 0BSD); GPL is rejected.

CLA / DCO: **Developer Certificate of Origin** (DCO) — every
commit must end with `Signed-off-by:`. Already enforced in the
repository.

## 9. Source control

- **Git** (primary). Default branch `main`. Conventional commit
  message style.
- Force-push to protected branches forbidden.
- All merges via signed PRs; review required from at least one
  maintainer.

## 10. Issue tracker

GitHub Issues, with labels matching the RFC corpus
(`area:scheduler`, `area:ipc`, `phase:1`, `severity:critical`,
etc.). Private security issues use the dedicated PSIRT mailbox
(RFC-0016) — `security@phanes-project.org` PGP-encrypted.

## 11. External dependencies

PHANES is `no_std` by design. The kernel-side dependency graph is
intentionally minimal:

| Crate          | License    | Purpose                                  |
|----------------|------------|------------------------------------------|
| `linked_list_allocator` | Apache-2.0 / MIT | Kernel allocator |
| `log`          | Apache-2.0 / MIT | Logging facade                         |
| `bitflags`     | Apache-2.0 / MIT | Bitfield types                         |
| `static_assertions` | Apache-2.0 / MIT | Compile-time invariants           |

Brain-side (`phanes-brain`) is Python with optional NumPy /
hypothesis / asyncio. Full dependency list maintained in SBOM
(CycloneDX format, generated per release per RFC-0012).

**No GPL dependencies anywhere.** Audited via `cargo-deny check`
in CI.

## 12. Cryptography

PHANES uses cryptography for:

- **Code signing** (Ed25519) — secure boot, OTA, topology
- **Session keys** (X25519 ECDH + HKDF-SHA256) — brain ↔ kernel
  authentication
- **Integrity** (HMAC-SHA256) — brain link auth_envelope
- **Encryption at rest** (AES-128-XTS where SoC supports) — flash
  encryption per RFC-0011

All algorithms are NIST-recommended or RFC-standardised. Test
vectors (Wycheproof, RFC 8032, RFC 2104) run in CI.

**Export compliance:** PHANES contains open-source cryptography
implementations widely available in similar projects (Linux
kernel, BSDs, seL4). Export classification will be requested per
LF guidance during incubation.

## 13. Release methodology

**Time-based releases** (RFC-0016 §"Release cadence"):

- Major releases every 6 months (April, October)
- Minor releases as-needed (~monthly on active branches)
- Security patches within 14 days of advisory for critical

LTS branches: every other major. v1.x receives 5-year security
back-ports.

Per-release artefacts (signed via Sigstore cosign + SLSA Level 3
provenance):

- Kernel binaries for each supported SoC
- CycloneDX SBOM
- Provenance attestation
- Signed checksums

Full process documented in `docs/plan/RELEASE.md`.

## 14. Distribution

- **GitHub Releases** for binaries + SBOMs + signatures
- **crates.io** for `robot_os_abi` (the only crate published as
  Phase 1; kernel internals stay workspace-only until Phase 2's
  multi-platform refactor stabilizes the boundaries)
- **PyPI** for `phanes-brain` once branding migration completes
- Self-hosted documentation site at `phanes-project.org` (TBD
  during incubation onboarding)

## 15. Documentation

- **The Book** (mdBook) — narrative introduction + tutorial. 12+
  chapters. Built in CI via `mdbook build`.
- **Reference Manual** (rustdoc + pdoc) — exhaustive API
  reference, generated per release.
- **Specification** — the RFC corpus + TLA+ specs + Kani
  harnesses. Cert-grade audit material.

Phase 4 brings translations to Chinese (zh-CN), German (de-DE),
Japanese (ja-JP) per RFC-0014.

## 16. Code of Conduct

[Contributor Covenant v2.1](https://www.contributor-covenant.org/version/2/1/code_of_conduct/),
already adopted in `CODE_OF_CONDUCT.md` (to be added at incubation
onboarding) and referenced from `CONTRIBUTING.md`.

Reports: `conduct@phanes-project.org`. Initial enforcement by the
TSC (RFC-0009); transition to a Code of Conduct committee during
incubation.

## 17. Trademark / branding

The PHANES word mark and logo (TBD) will be transferred to the
Linux Foundation upon incubation acceptance, per the standard LF
trademark policy.

A "PHANES Inside" downstream-compliance badge program is planned
for Phase 1+ per RFC-0018 §"Trademark angle". This lets downstream
projects (commercial robots built on PHANES) display branded
compliance without becoming part of the upstream project.

## 18. Diversity and inclusion

Single-author at submission, so contributor-pool diversity isn't
yet meaningful to report. As incubation enables additional initial
committers (§6), PHANES commits to:

- Active outreach to underrepresented groups in safety-critical
  software.
- Mentorship program (Outreachy / Google Summer of Code partnerships
  during Phase 2).
- Public technical roadmap that reduces information-asymmetry
  barriers to contribution.
- All design discussion conducted asynchronously via RFCs +
  mailing list, accommodating contributors in any timezone.

The Code of Conduct (§16) is the floor; diversity is the target.

---

## Attachments / references

- `rfcs/RFC-0001-strategic-plan.md` — 5-phase plan
- `rfcs/RFC-0009-governance.md` — license, governance, TSC
- `rfcs/RFC-0010-branding.md` — PHANES brand selection
- `docs/plan/VISION.md`, `ROADMAP.md`, `ARCHITECTURE.md`
- `docs/plan/RELEASE.md` — release process
- `CONTRIBUTING.md` — DCO + RFC process
- `CHANGELOG.md` — v1.0.0-rc1 entry
- `safety/CODING_STANDARD.md` — SC-1..SC-10 codified
- `formal/proofs/INVARIANTS.md` — 15 invariants tracked

## Submission status

| Step | Status |
|------|--------|
| Application drafted | ✅ this document |
| TSC formed | ⏳ first additional committer needed |
| Code transferred to `phanes-project` GitHub org | ⏳ on acceptance |
| Trademark assignment to LF | ⏳ on acceptance |
| First TAC review | ⏳ scheduled post-submission |

---

*Submitted concurrent with PHANES v1.0.0 release.*
