# Introduction

> **PHANES** — Verifiable. AI-native. Multi-platform. Real-time. Open.

PHANES is a robotics-class operating system designed from day one to be:

- **Verifiable** — capability-typed IPC, formally specified scheduler,
  Kani harnesses on safety paths.
- **AI-native** — typed Model Bundle (`.MBL`) loader, NPU drivers, on-
  device inference as a first-class kernel service.
- **Multi-platform** — RV64 (StarFive, SpacemiT), ARM Cortex-A/R (NXP,
  STM32, Rockchip), x86_64 from a single Rust source tree.
- **Real-time** — hierarchical scheduler with five policy classes
  (Safety / HardRT / SoftRT / BestEffort / Idle) under Adaptive
  Partitioning.
- **Open** — Apache 2.0; targeting Linux Foundation incubation; SBOM
  + reproducible builds + SLSA Level 3 from release one.
- **Cert-eligible** — engineered for ISO 26262 ASIL-B (Phase 3) and
  ASIL-D pre-validation (Phase 4).

This book is the **narrative** introduction. For exhaustive APIs, read
the generated rustdoc at `phanes-project.org/api/`. For authoritative
design rationale, read the RFCs in `rfcs/` (linked from the [RFC
index](./appendix/rfcs.md)).

## Audience

Three readers will find different things here:

- **The hobbyist** building their first autonomous robot — start with
  [*Hello robot in QEMU*](./01-getting-started/hello-robot-qemu.md).
- **The integrator** porting PHANES to an existing platform — read
  [*Architecture*](./02-architecture/overview.md) end to end, then
  consult the platform chapter (Phase 2+).
- **The auditor** evaluating PHANES for cert or procurement — pair this
  book with the **Specification** (RFCs + invariants ledger) at
  `phanes-project.org/spec`.

## What's in this book vs the RFCs

| The book                              | The RFCs                                     |
|---------------------------------------|----------------------------------------------|
| How to use PHANES                     | Why PHANES is shaped this way                |
| Examples + diagrams + tutorials       | Authoritative spec + invariants              |
| Versioned per release; can rewrite    | Append-only; supersede by newer RFC          |
| Translated EN/ZH/DE/JP (Phase 4)      | English only                                 |

If a discrepancy ever exists, **the RFC wins**. The book is updated to
match.

## Status of this book

PHANES is in **Phase 0** (foundation). The book exists today as a
skeleton; chapters fill in across phases:

| Phase | Book milestone                                          |
|-------|---------------------------------------------------------|
| 0     | Skeleton + introduction (this section)                  |
| 1     | Architecture chapters (caps, scheduler, topology)       |
| 2     | Platform chapters; brain expansion                      |
| 3     | Security chapter; verification chapter                  |
| 4     | i18n EN / ZH / DE / JP                                  |

If a chapter says "Coming in Phase *N*", that's not a placeholder
mistake — it's an honest commitment to ship in that phase.

## Next

Read [Installation](./01-getting-started/installation.md).
