# RFC-0014: Documentation Strategy — Book, Reference Manual, Internationalisation

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-08-20


> **Status audit 2026-08-20.** Understated rather than overstated: `book/src/`
> already holds 15 chapters (getting-started, architecture, brain, appendix)
> plus a rendered mdBook site under `book/book/`, none of which this document
> mentions as existing. Left at `accepted` since the i18n and reference-manual
> halves are genuinely not built, but a reader should not conclude from this
> status that the book itself is unwritten.

## Summary

PHANES ships three tiers of documentation: a **Book** (narrative,
mdBook) that teaches PHANES from zero, a **Reference Manual** (per-
crate API doc, generated from rustdoc) that is exhaustive, and a
**Specification** (the RFC corpus + formal models) that is
authoritative. All three are versioned per release. English is
canonical; Chinese, German, and Japanese translations are first-tier
once Phase 1 is shipped.

## Motivation

A serious OS lives or dies by its docs:

- Linux kernel has a Book (`kernel.org/doc/html/latest/`) plus
  exhaustive in-tree comments + Documentation/ tree.
- seL4 has a paper, a book, formal proofs, and a manual.
- Tock has the Tockbook, rustdoc, and tutorials.

Without high-quality docs:

- Adoption stalls — engineers can't onboard.
- Cert audit fails — auditors ask "show me the design"; "read the
  code" is not an acceptable answer.
- Foundation review fails — incubator graduation requires user
  documentation.
- Research adoption never happens — academics need spec + paper.

## Detailed design

### Three documentation tiers

| Tier | Audience | Format | Where | Generated from |
|------|----------|--------|-------|----------------|
| **Book** | Newcomers + integrators | Narrative prose, mdBook | `phanes-project.org/book` | `book/` mdBook source |
| **Reference Manual** | Active developers | API reference | `phanes-project.org/api/<version>` | `cargo doc` (rustdoc) |
| **Specification** | Auditors, researchers | RFCs + TLA+ + Kani harnesses + ABI freeze | `phanes-project.org/spec` | `rfcs/` + `formal/` |

### The Book

**Tooling:** mdBook (Rust ecosystem standard).

**Structure:**

```
book/
├── src/
│   ├── SUMMARY.md                  ← TOC
│   ├── introduction.md
│   ├── 01-getting-started/
│   │   ├── installation.md
│   │   ├── hello-robot-qemu.md
│   │   └── first-skill.md
│   ├── 02-architecture/
│   │   ├── overview.md
│   │   ├── caps-and-ipc.md
│   │   ├── scheduler.md
│   │   ├── topology.md
│   │   ├── ai-runtime.md
│   │   └── secure-boot.md
│   ├── 03-developing/
│   │   ├── modular-pattern.md
│   │   ├── adding-a-driver.md
│   │   ├── adding-a-syscall.md
│   │   ├── adding-a-skill.md
│   │   └── testing-strategy.md
│   ├── 04-operating/
│   │   ├── deployment.md
│   │   ├── ota.md
│   │   ├── monitoring.md
│   │   └── troubleshooting.md
│   ├── 05-platforms/
│   │   ├── qemu.md
│   │   ├── visionfive2.md
│   │   ├── bananapi-f3.md
│   │   ├── imx8mp.md
│   │   ├── rk3588.md
│   │   └── porting.md
│   ├── 06-brain/
│   │   ├── overview.md
│   │   ├── protocol.md
│   │   ├── plugins.md
│   │   ├── fleet.md
│   │   └── dashboard.md
│   ├── 07-security/
│   │   ├── threat-model.md
│   │   ├── secure-boot.md
│   │   ├── supply-chain.md
│   │   └── disclosure.md
│   ├── 08-verification/
│   │   ├── tla-models.md
│   │   ├── kani-proofs.md
│   │   └── reading-formal-specs.md
│   ├── appendix/
│   │   ├── glossary.md
│   │   ├── packet-types.md
│   │   ├── syscalls.md
│   │   └── error-codes.md
│   └── refs.md
├── theme/                          ← Sigstore-signed PHANES theme
└── book.toml
```

**Quality bar:**

- Every chapter starts with a one-paragraph summary (TL;DR).
- Every code example is tested (mdbook-test plugin).
- Every concept has at least one diagram (Mermaid).
- Every chapter ends with "next steps" + "see also" links.
- Reading age: target ~12th grade (Hemingway score ≤ 10).

**Phase deliverables:**

- Phase 0: skeleton TOC + introduction + getting-started + capability
  IPC + scheduler + topology chapters.
- Phase 1: full coverage of architecture + adding-a-driver +
  testing.
- Phase 2: full coverage of platforms + brain + operating.
- Phase 3: security + verification + appendices polished.
- Phase 4: i18n EN/ZH/DE/JP.

### Reference Manual

**Tooling:** `cargo doc` for kernel; `pdoc` (or `mkdocs` +
`mkdocstrings`) for brain. Pubished automatically per release to
`phanes-project.org/api/<version>/{kernel,brain}/`.

**Quality requirements:**

- Every public item has a doc comment (lint-enforced).
- Every doc comment has at least one example (lint-enforced where
  possible).
- Every safety-relevant item has explicit `# Safety` / `# Errors`
  sections.
- Doc tests run as part of test suite.

**rustdoc lints (CI gate):**

```toml
# Cargo.toml workspace lints
[workspace.lints.rust]
missing_docs = "warn"

[workspace.lints.rustdoc]
broken_intra_doc_links = "deny"
private_intra_doc_links = "warn"
missing_crate_level_docs = "warn"
```

For safety crates, `missing_docs = "deny"` (every public item must
have a doc).

### Specification

The "spec" is the union of:

- `rfcs/` — design RFCs.
- `formal/` — TLA+ models + Kani harnesses + Loom tests.
- `crates/abi/` — frozen ABI types (RFC-0008).
- The Reference Manual at frozen versions.

The spec is what auditors read. It is what cert auditors check
against. It is what researchers cite.

We bundle a "Spec PDF" per release: `phanes-spec-<version>.pdf` —
all RFCs + ABI freeze + invariants list + formal model summaries,
typeset with pandoc → LaTeX, signed cosign.

### Internationalisation

**Phase 0–2:** English only. We optimise for clarity in one
language before fragmenting.

**Phase 3:** add Chinese (zh-CN). Why first: largest robotics
market; many Chinese SoC vendors (StarFive, Allwinner, RK,
SpacemiT) — directly aligned.

**Phase 4:** add German (de-DE) — automotive Tier-1 base. And
Japanese (ja-JP) — robotics + automotive Tier-2.

**Tooling:** mdBook supports i18n via separate `book/zh-CN/src/`
trees. Translations tracked via per-page `last-translated-rev`
markers; out-of-date warnings rendered automatically.

**Translation policy:**

- Native speakers, not machine translation. Crowdin or Weblate for
  community. Paid translators for cert-relevant sections.
- Translations track English; if English changes, translation
  flagged stale until re-reviewed.
- The English version is canonical. In a discrepancy, English wins.

### Diagrams as code

All diagrams in `book/` are source-controlled, generated from text:

- **Mermaid** for sequence / flow / state diagrams.
- **PlantUML** for component / deployment diagrams.
- **D2** for advanced architecture diagrams.

Banned: hand-drawn images, screenshots of whiteboards, copy-pasted
draw.io exports without source.

### Architectural Decision Records (ADRs)

Lightweight per-module decisions live alongside the code:

```
crates/sched/
├── ADRs/
│   ├── 001-multi-policy.md
│   ├── 002-cbs-budget-replenishment.md
│   └── 003-priority-inheritance.md
└── ...
```

ADR template:

```markdown
# ADR-NNN: Title

Date: YYYY-MM-DD
Status: proposed | accepted | superseded by ADR-NNN
Context: …
Decision: …
Consequences: …
```

ADRs differ from RFCs: RFCs are repo-wide and constitutional;
ADRs are per-module and tactical.

### Continuous publication

Every push to `main`:
- `cargo doc` regenerates → published to staging API site.
- `mdbook build` → staging book site.
- Spec PDF rebuilt → staging.

Every release tag:
- Frozen book version added to `phanes-project.org/book/<version>/`.
- API docs frozen at `phanes-project.org/api/<version>/`.
- Spec PDF published + signed.

### Contributor docs

`CONTRIBUTING.md` at repo root explains:

- DCO sign-off.
- RFC process (link to `rfcs/README.md`).
- Code style + clippy expectations.
- Testing expectations.
- ADR process for module-local design.
- Translation contribution flow.

## Drawbacks

- **Documentation is a sustained engineering cost** — perhaps
  20–25% of total effort across phases.
- **i18n adds compounding cost** — every translation drift creates
  re-review work.
- **Tooling spread** (mdBook, rustdoc, pdoc, pandoc, Mermaid,
  PlantUML, D2) — each tool has its own quirks.

We accept these costs because the alternative — under-documented
OS — fails on adoption, cert, and incubation.

## Rationale and alternatives

**Alternative A — only API docs.** Insufficient for newcomers,
cert.

**Alternative B — wiki / informal docs.** Not version-able with
releases; rots. Rejected.

**Alternative C — outsource docs.** Tempting at scale but
documentation that lags engineering by months kills credibility.
Rejected as primary mode.

**Alternative D (chosen) — three integrated tiers, source-
controlled, version-aligned.** Industry standard for serious
projects.

## Prior art

- **Linux kernel** — Documentation/ tree + kernel.org book + man
  pages.
- **Rust** — The Book, the Nomicon, the Reference, rustdoc.
- **seL4** — Manual + paper + reference proof.
- **Tock** — Tockbook, rustdoc, tutorials.
- **Zephyr** — Sphinx-based docs, multi-language.
- **NixOS** — manual + reference + community wiki.

## Unresolved questions

- **Hosting venue.** GitHub Pages or self-hosted under
  `phanes-project.org`? Working assumption: self-hosted (more
  control), via static site deploy from CI.
- **Search indexing.** Algolia DocSearch (free for open source) vs
  on-site search? Working assumption: Algolia + GitHub-issue search
  fallback.
- **Translation labour budget.** Phase 3+ — community-driven via
  Weblate; cert-specific sections paid pro. Exact split TBD.

## Future possibilities

- **Phase 4:** Interactive playground (PHANES kernel running in
  browser via WASM emulation; readers tweak topology and see
  effects).
- **Phase 5:** Video courses + university curriculum partnerships.
- **Phase 5:** "PHANES Certified Engineer" program (training +
  exam).
