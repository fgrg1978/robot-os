# RFC-0016: Operational Excellence — PSIRT, LTS, ADRs, Bug Bounty, Release Cadence

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

PHANES sustains itself through documented operational practice:
a Product Security Incident Response Team (PSIRT) process, Long-
Term Support (LTS) branches with 5-year support, lightweight
Architectural Decision Records (ADRs) for module-local design,
a bug bounty program, time-based release cadence, deprecation
policy, and incident post-mortems. Together these turn an open-
source project into one a Tier-1 enterprise can rely on.

## Motivation

Tools like the kernel itself can be impeccable, but if a critical
CVE has no triage process, or a deployed-customer can't get a
back-port, PHANES isn't viable for serious deployment. We define
the operational layer here:

- **PSIRT** — coordinated vulnerability disclosure with embargo.
- **LTS** — 5-year security back-ports for cert customers.
- **Bug bounty** — paid third-party scrutiny.
- **Release cadence** — predictable, time-based.
- **Deprecation policy** — protect customers from breaking
  changes.
- **Incident response** — when (not if) something breaks in the
  field.

## Detailed design

### Release cadence

**Time-based, not feature-based.**

- **Major releases** — every 6 months (April, October).
- **Minor / point releases** — as-needed; ~monthly cadence on
  active branches.
- **Security patches** — within 14 days of advisory for `critical`
  / `high`; coordinated with PSIRT.

Rationale: a predictable schedule lets enterprise customers plan
deployments. Cert customers schedule audits around it.

Naming: SemVer-ish — `major.minor.patch`. ABI changes only at
major boundaries. Within a major series, kernel ABI freeze is
binding (RFC-0008).

Example timeline:
```
v1.0 (Apr 2027)  — first stable, ASIL-B candidate
v1.1 (Oct 2027)  — minor features, no breaking changes
v1.2 (Apr 2028)
v2.0 (Oct 2028)  — first ABI break since v1.0; LTS for v1.x continues
v1.LTS (continues to Apr 2032; 5 years from v1.0)
```

### LTS (Long-Term Support) branches

**Cadence:** every other major release becomes LTS.

| Release | Type | Active maintenance | Security back-ports |
|---------|------|---------------------|---------------------|
| v1.0 | LTS | 2 years (until v3.0) | 5 years (until 2032) |
| v2.0 | normal | until v3.0 | until v3.0 |
| v3.0 | LTS | 2 years (until v5.0) | 5 years |
| v4.0 | normal | until v5.0 | until v5.0 |
| ... | ... | ... | ... |

**LTS scope:**

- Critical / high CVE security back-ports — yes.
- Bug fixes for safety-impacting issues — yes.
- Performance improvements — only if regression.
- New features — no.
- ABI changes — never.

**Funding:** LTS maintenance is the foundation's recurring labour.
Sustained via:
- Foundation membership tiers (Tier-1 customers fund this).
- Cert-build service revenue.
- Paid LTS extension contracts beyond 5 years.

### PSIRT — Product Security Incident Response Team

**Reporting endpoint:** `security@phanes-project.org`. PGP-encrypted
intake mailbox; key in `SECURITY.md`.

**Triage SLA** (RFC-0009 inherits this):

| Severity | First response | Patch landed | Public advisory |
|----------|----------------|--------------|------------------|
| Critical (RCE in safety path, secure-boot bypass) | 24 h | ≤ 14 days | Coordinated with patch |
| High (RCE non-safety, privilege escalation) | 48 h | ≤ 30 days | Coordinated |
| Medium (DoS, info-leak) | 5 days | ≤ 90 days | Public after patch + 30 d grace |
| Low (hardening) | 14 days | next release | Public on disclosure |

**Embargo:**

- Critical / high: embargo until patch ready + 14-day grace for
  customers to deploy.
- Distribution to upstream LTS distributors (Red Hat-style allies)
  via private list.

**Advisory format:** GHSA-style:

```yaml
id: PHANES-2027-0042
severity: critical
cwe: CWE-787
package: phanes/crates/net
affected_versions: ">= 1.0.0, < 1.0.5"
patched_versions: ">= 1.0.5"
description: …
mitigation: …
references: [URL …]
discovered_by: …
```

Published to `phanes-project.org/advisories` + GHSA + CVE.

### Bug bounty

**Phase 2 launch** (after Phase 1 stabilises):

| Severity | Reward |
|----------|--------|
| Critical (sandbox escape, secure boot bypass, ASIL-impacting) | $5,000 – $25,000 |
| High (RCE, privilege escalation) | $1,500 – $5,000 |
| Medium (DoS, info-leak) | $300 – $1,500 |
| Low (hardening) | $100 – $300 |

Hosted on **HackerOne** or **Bugcrowd** — established platforms
with experienced triagers.

**Eligibility:** Latest stable + LTS branches. Out-of-scope:
brain (`phanes-brain` is dev tooling, not in safety scope; covered
by separate program if needed).

**Hall of Fame** at `phanes-project.org/security/hall-of-fame`.

### ADRs (Architectural Decision Records)

Module-local decisions that don't warrant a full RFC. Already
sketched in RFC-0014; here we formalise:

**Location:** `<crate>/ADRs/NNN-title.md`.

**Numbering:** Per-crate, sequential (`crates/sched/ADRs/001-`,
`crates/ipc/ADRs/001-`).

**Lifecycle:**

| Status | Meaning |
|--------|---------|
| `proposed` | Author drafted; under review |
| `accepted` | Reviewed; in effect |
| `deprecated` | No longer recommended; for history |
| `superseded by ADR-NNN` | Replaced |

**Template:**

```markdown
# ADR-NNN: <title>

**Date:** YYYY-MM-DD
**Status:** proposed | accepted | deprecated | superseded by ADR-NNN
**Author:** name <email>

## Context
What's the problem we're solving?

## Decision
What did we decide?

## Consequences
- (+) what we gain
- (-) what we accept

## Alternatives considered
- A: …
- B: …
```

**ADRs vs RFCs:**

| ADR | RFC |
|-----|-----|
| Per-crate / per-module | Repo-wide / cross-cutting |
| Tactical | Strategic |
| Author + module owner approve | Two reviewers + 1-week wait |
| ~50–200 lines | ~200–1000 lines |

### Deprecation policy

When a feature must be removed:

1. **Announce** in release notes — "deprecated in v1.3, removal in
   v3.0".
2. **Compiler warning** — `#[deprecated(note = "use Foo")]` for at
   least one full minor cycle.
3. **Migration guide** — written, with examples.
4. **Removal** — at the next major boundary (≥ 1 year later), only
   if migration path is documented and shipped.

LTS branches **never** remove deprecated APIs.

### Incident response

When something breaks in production (customer reports a kernel
panic, OTA brick, etc.):

1. **Triage** — is it security-impacting? if yes → PSIRT.
2. **Acknowledge** — within 4 business hours during Phase 2+.
3. **Assess** — ASIL impact? customer scope? regression?
4. **Hotfix path** — within 7 days for safety-critical, 14 for
   high, 30 for medium.
5. **Post-mortem** — public retrospective within 30 days, no
   blame, root cause + fix + prevention.

Post-mortems published at `phanes-project.org/postmortems`. Format:

```markdown
# Incident PHANES-INCIDENT-2027-0007

**Date:** 2027-NN-NN
**Severity:** Major
**Affected:** v1.0.0 – v1.0.4 on i.MX 8M Plus
**Duration:** ~6 hours customer downtime
**Root cause:** …
**Fix:** PR #1234, shipped v1.0.5
**Prevention:** RFC-0099 adds …
**Timeline:** …
```

### Telemetry & observability

For supported customers, optional opt-in telemetry:

- Boot success / failure
- Panic / fatal counters
- OTA success / failure
- Anonymised crash reports (no PII; configurable)

Strict opt-in. Hosted by foundation. Deletes after 90 days. Source
data published only in aggregate.

### Membership / governance ties

| Role | Privileges | Responsibilities |
|------|-----------|------------------|
| Maintainer | Merge access, RFC review | LTS patching, PSIRT triage |
| Reviewer | RFC review, code review | Quality gate |
| Contributor | PR submission | DCO sign-off, RFC for substantial change |

Becoming a maintainer is by TSC vote, after sustained contribution.

### KPIs the foundation tracks

| KPI | Target Phase 1 | Target Phase 3 |
|-----|----------------|----------------|
| PSIRT median first-response (critical) | 48 h | 12 h |
| Median patch time (critical) | 30 d | 7 d |
| Bug-bounty submissions / quarter | 5 | 30 |
| External contributors | 5 | 50 |
| Active downstream projects | 2 | 25 |
| LTS branches supported | 1 (v1.x) | 2 (v1.x + v3.x) |

## Drawbacks

- **Operational discipline costs sustained labour** — perhaps
  20–25% of foundation engineering capacity.
- **Bug bounty has a budget** — Phase 2 needs ~$50–100K reserve.
- **LTS branches multiply maintenance** — each LTS is a fork to
  back-port to.

## Rationale and alternatives

**Alternative A — ad-hoc operations.** Fails enterprise adoption.
Rejected.

**Alternative B — only critical-severity LTS, no major LTS.**
Insufficient for cert customers (they need 5+ year support).
Rejected.

**Alternative C (chosen) — full mature operational layer from
Phase 1.** Industry standard for serious projects.

## Prior art

- **Linux kernel LTS** — Greg Kroah-Hartman's process; 6 LTS
  branches at any time, 2-year + extension model.
- **CNCF projects** — supply-chain ops + PSIRT shape we copy.
- **Mozilla MOSS / RIG** — bug bounty discipline.
- **OpenStack** — release cadence + branches we mirror.
- **Kubernetes** — KEPs ≈ our RFCs; release cadence we mirror.
- **Rust language** — release cadence (6-week train) we adapt to
  6 months for cert audience.

## Unresolved questions

- **PSIRT staffing model.** Full-time or rotation? Working
  assumption: rotation Phase 1, full-time post Phase 2.
- **Bug-bounty platform choice.** HackerOne vs. Bugcrowd. Working
  assumption: HackerOne (CNCF projects largely use it; established
  triage).
- **Telemetry hosting.** Foundation-self-hosted vs. partner.
  Working assumption: self-hosted under data-protection statute (EU
  hosting + GDPR / CCPA compliance).

## Future possibilities

- **Phase 3:** PHANES Foundation funded full-time PSIRT manager.
- **Phase 4:** Coordinated disclosure relationship with CERT/CC.
- **Phase 5:** Industry-wide robotics CSIRT — PHANES-sponsored
  cross-project security alliance.
