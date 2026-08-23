# RFC-0009: Governance, Foundation Hosting, License

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-08-20


> **Status audit 2026-08-20.** This RFC stated that "a `LICENSE` file (Apache
> 2.0 text) and a `NOTICE` file (attribution)" were added at repo root in Phase
> 0. They were not — `git ls-files` returned no licence file of any kind, while
> 15 of 85 `Cargo.toml` files already declared `license = "Apache-2.0"`. With
> no licence text the tree was, by default, all-rights-reserved: the opposite
> of what this RFC decides, and a debt that compounds, since relicensing later
> needs the consent of everyone who contributed in the meantime. Closed the
> same day: canonical Apache 2.0 `LICENSE` and a `NOTICE` (Copyright 2026
> Fernando Rodriguez) now exist at root, and all 84 package manifests declare
> `license = "Apache-2.0"` (the 85th is the workspace root, which has no
> `[package]` section). The rest of this RFC — foundation hosting, TSC, DCO —
> remains undelivered process work, correctly `accepted`.

## Summary

PHANES is licensed Apache 2.0, hosted under Linux Foundation
incubation, governed during Phase 0–1 by a Benevolent Dictator For
Life (BDFL) transitional model that converts to a Technical Steering
Committee (TSC) by end of Phase 2. Contribution gated by Developer
Certificate of Origin (DCO). PSIRT (security incident response)
established from Phase 0. Full LF Best Practices badge achieved by
end of Phase 1.

## Motivation

Governance and licensing decisions made now constrain everything
that follows: who can adopt the code, who can commit, who decides on
breaking changes, who handles security disclosures, and who owns the
trademark. Getting this right at Phase 0 saves months of relicensing
or restructuring later.

The choices below align PHANES with the operational norms of
foundation-hosted, enterprise-adoptable open-source projects.

## Detailed design

### License — Apache 2.0

**Decision:** Apache License, Version 2.0.

Reasoning:

- **Patent grant.** Contributors implicitly license their patents
  for use in PHANES. Critical for an OS exposed to legal scrutiny.
- **Foundation-friendly.** Linux Foundation, Eclipse, Apache, CNCF
  all accept or prefer Apache 2.0.
- **Enterprise-acceptable.** Corporate legal teams have established
  precedent for Apache 2.0; clearance is fast.
- **Compatible with most ecosystems.** Works with GPLv3 (one-way),
  MIT, BSD, and the Rust ecosystem (which is itself dual MIT/Apache).
- **Notice retention obligation manageable.** Single `NOTICE` file
  at repo root.

**Rejected alternatives:** MIT (no patent grant); GPLv2/GPLv3
(viral; wrong fit for an OS that vendors will want to integrate);
BSD-3 (no patent grant); dual MIT/Apache (cargo-friendly but
foundation legal teams find dual-licensing awkward).

A `LICENSE` file (Apache 2.0 text) and a `NOTICE` file (attribution)
are added at repo root in Phase 0.

### Foundation hosting — Linux Foundation incubation

**Decision (working assumption, pending acceptance):** Linux
Foundation, via the **LF Edge** or **OpenChain** project umbrellas
depending on which fits best at application time.

Reasoning:

- LF has the most experience with multi-vendor systems software.
- Existing LF projects (Zephyr, Edge X Foundry, Open Vehicle Stack,
  OpenChain) are natural neighbours.
- LF's intellectual property policy is well-understood by enterprise
  legal.
- LF provides infrastructure (CI, security audits subsidised, legal
  counsel) that we'd otherwise pay for separately.

**Application timeline:**

- Phase 0 month 1–3: pre-application discussions with LF staff.
- Phase 0 month 3 (RFCs accepted, brand cleared): formal application
  filed.
- Phase 1 month 3–6: incubation status under review.
- Phase 1 month 6–9: incubation accepted (or rejected → fallback to
  Eclipse OpenADx).

**Fallback:** Eclipse Foundation, specifically Eclipse OpenADx
(open Automotive Development) or Eclipse SDV (Software Defined
Vehicle). Eclipse has stronger automotive credentials but smaller
overall footprint.

**Last resort:** Apache Incubator. Slower, more bureaucratic, but
guaranteed to accept any project meeting the criteria.

### Contribution model

**Phase 0–1 (BDFL transitional):**
- Project lead has final say on technical direction.
- All RFCs go through public PR review with at minimum 2 +1s.
- Technical decisions documented as RFCs (this directory) or ADRs.
- Project lead is publicly named and accountable.

**Phase 2 onwards (Technical Steering Committee):**
- TSC of 5–7 members elected by contributors with > 5 merged PRs.
- One-year terms, staggered.
- TSC has technical veto on RFCs.
- Project lead role retired or moves to Foundation Board liaison.

**Always:**
- All contributions via pull requests on GitHub.
- DCO sign-off required (`Signed-off-by:`) — see below.
- Breaking changes require an RFC.
- No direct pushes to `main`.

### DCO (Developer Certificate of Origin)

We use **DCO** (not CLA) for individual contributions. DCO is a per-
commit assertion that the contributor has the right to submit the
work. It is:

- Lower friction than CLA (no separate signing process).
- Sufficient for Apache 2.0 + Linux Foundation governance.
- Enforced by the `dco` GitHub Action: a PR cannot be merged unless
  every commit has `Signed-off-by:` matching the GitHub account.

The DCO text is added as `DCO.txt` at repo root, and the relevant
sentence appears in `CONTRIBUTING.md`.

### Roles

| Role | Phase 0–1 | Phase 2+ |
|------|-----------|----------|
| Project Lead / BDFL | named individual | retired |
| Technical Steering Committee | n/a | 5–7 members |
| Maintainer (per crate / subsystem) | core team | nominated by TSC |
| Reviewer | crate maintainers | crate maintainers |
| Contributor | anyone with merged PR | anyone with merged PR |
| PSIRT (security response) | 3-person rotating | 5-person rotating |
| Documentation lead | tech writer | tech writer |
| Community manager / DevRel | (Phase 1+) | community manager |
| Foundation liaison | Project Lead | TSC chair |

### PSIRT — Product Security Incident Response Team

**Established Phase 0.**

- Public email: `security@phanes.org` (held by the project, monitored
  by 3 rotating responders).
- Public policy: `/.well-known/security.txt` (RFC 9116 format) on
  all official domains.
- Disclosure process: 90-day embargo by default; coordinated with
  reporter; public CVE assigned via LF CNA after fix lands.
- Annual external audit (Trail of Bits, Cure53, NCC) starting
  Phase 2 — public report.
- Bug bounty program (HackerOne) starting Phase 2 — symbolic
  rewards initially, scaled when funding allows.

### Code of Conduct

Adopted from Contributor Covenant 2.1, customised lightly for
PHANES context. Lives at `CODE_OF_CONDUCT.md`. Enforcement via TSC.

### Trademark — PHANES

**Working plan:**

- Phase 0 month 1: knockout TM search (already done — see RFC-0010).
- Phase 0 month 2: professional TM search (~$300, IP attorney).
- Phase 0 month 3: file TM application in US (USPTO classes 9 + 42),
  EU (EUIPO), JP, CN. ~$10K total fees + attorney.
- Trademark held by the Foundation entity (or by the project's legal
  successor) — not by an individual.

The Foundation entity is **either** the Linux Foundation (if
incubation accepted, in which case LF holds the TM) **or** a project-
owned 501(c)(6) we incorporate (if going independent).

### Best Practices badges (Phase 1)

| Badge | What it certifies |
|-------|-------------------|
| **CII / OpenSSF Best Practices Badge** | Project follows OSS best practices (security, docs, testing). 3 levels: passing → silver → gold. |
| **OpenChain ISO/IEC 5230 conformance** | License compliance management. |
| **SLSA Level 3** | Supply-chain provenance. RFC-0012. |
| **SBOM availability** | Per-release CycloneDX or SPDX BOM. RFC-0012. |

Goal: passing on all four by end of Phase 1; silver / gold on CII by
end of Phase 2.

### Release cadence

- **Master branch (`main`)**: rolling, must be green at all times.
- **Stable releases**: every 3 months (e.g. v0.1, v0.2, v0.3, ...).
- **LTS releases**: annually, supported with security-only patches
  for 5 years (Phase 2 onwards). First LTS = v1.0 at end of Phase 1.
- **Pre-release**: rolling RC tags 2 weeks before each stable.

### Communication channels

| Channel | Purpose |
|---------|---------|
| GitHub Discussions | Long-form Q&A, RFC discussion |
| GitHub Issues | Bug tracking, feature requests |
| `users@phanes.org` mailing list | User questions |
| `dev@phanes.org` mailing list | Contributor discussion |
| `security@phanes.org` | PSIRT (email only) |
| Discord / Matrix | Real-time chat (Phase 1) |
| Quarterly community call | Open video call, recorded |
| Annual conference | "PhanesCon" — Phase 3+ |

## Drawbacks

- **Foundation acceptance is uncertain.** LF may reject incubation
  if they consider PHANES too overlapping with existing projects
  (Zephyr — but Zephyr is RTOS not microkernel; differentiation is
  defensible). Eclipse fallback addresses this.
- **DCO requires git history hygiene.** Contributors must sign-off
  every commit. We accept this; well-documented in CONTRIBUTING.md.
- **TSC transition (Phase 2) is risky** — power transfer can split
  community. Mitigated by phased transition and clear bylaws written
  before Phase 2 starts.

## Rationale and alternatives

**Alternative A — no foundation, independent.** Faster decisions but
inferior credibility for cert / enterprise / regulator. Rejected.

**Alternative B — host under existing project (e.g. Zephyr).**
Zephyr is RTOS and very different scope. PHANES would be a
constrained subproject; can't pursue full vision. Rejected.

**Alternative C — Apache Incubator instead of LF.** Apache has a
mature process but slower; LF is more enterprise-aligned for systems
software. We reserve Apache as fallback to LF.

**Alternative D — CLA instead of DCO.** CLA gives the project legal
right to relicense (e.g., dual-license commercial). LF projects
explicitly avoid this; we adopt LF norms.

## Prior art

- **Linux kernel** governance: BDFL (Linus) + maintainers per
  subsystem + DCO. PHANES copies the shape.
- **Zephyr** governance: TSC + working groups under LF. We will
  resemble this from Phase 2.
- **Rust** governance: teams with focused remits. Inspires our
  per-crate maintainer model.
- **Mozilla, OpenSSL, Apache HTTPD** — examples of foundation-hosted
  systems software.

## Unresolved questions

- **Project Lead identity.** Working assumption: the original author
  during Phase 0–1, transitioning to elected role by Phase 2.
- **TSC composition criteria.** Working assumption: PRs merged + RFC
  authored + visible community contribution. Refined before Phase 2
  in a follow-up RFC.
- **Foundation choice (LF vs Eclipse vs Apache).** Decided after
  outreach in Phase 0 month 1–3. Working assumption: LF.

## Future possibilities

- **Phase 3:** PHANES Foundation as an independent legal entity if
  LF / Eclipse hosting becomes a poor fit later.
- **Phase 4:** Working groups (Automotive WG, Robotics WG, AI
  Runtime WG) under TSC.
- **Phase 5:** Formal certification body (PHANES-certified products
  list).
