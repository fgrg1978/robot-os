# Contributing to PHANES

Thank you for your interest in PHANES. This document is the
authoritative source for how to contribute.

## TL;DR

1. Discuss substantial changes in an RFC first (`rfcs/`).
2. Sign all commits with a DCO sign-off (`Signed-off-by:`).
3. Open a PR; reviewers respond within 5 business days.
4. CI must pass; coverage must not regress.
5. Be patient and kind in review.

---

## 1. Code of Conduct

PHANES adopts the
[Contributor Covenant v2.1](https://www.contributor-covenant.org/version/2/1/code_of_conduct/).
Read it. Follow it. Reports → `conduct@phanes-project.org`.

---

## 2. License & DCO

PHANES is licensed under **Apache 2.0**. By contributing you
agree your contribution is licensed the same.

We use the **Developer Certificate of Origin** (DCO) — every
commit must end with:

```
Signed-off-by: Your Name <your-email@example.com>
```

This is automated by `git commit -s`. If you forget,
`git commit --amend -s` fixes the last commit.

The DCO certifies you have the right to submit the patch under
the project's license. See `https://developercertificate.org`.

---

## 3. Where to start

| You want to | Read |
|-------------|------|
| Understand the design | `docs/plan/VISION.md` + `docs/plan/ARCHITECTURE.md` |
| Pick something to work on | GitHub issues labelled `good-first-issue` |
| Propose a new feature / API | Open an RFC: copy `rfcs/_template.md` → `rfcs/RFC-NNNN-*.md` |
| Fix a bug | Open a PR; small fixes don't need an RFC |
| Add a driver | RFC-0002 (modular pattern); add `crates/drivers/<class>/impls/<vendor>.rs` |
| Add a syscall | RFC-0003 + RFC-0008 (ABI) |
| Add a skill | Brain side: `phanes-brain/planner/skills.py` + corresponding policy |
| Improve docs | `book/src/` (mdBook) — small docs PRs are very welcome |
| Translate docs | Coordinate via Weblate (Phase 3+) — see `book/i18n/README.md` |

---

## 4. RFC process

Substantial changes (anything cross-cutting, anything affecting
ABI, scheduler, IPC, security, or governance) **must** go through
an RFC.

Trivial does not require RFC: typos, single-driver impls, bug
fixes, performance improvements without API change.

The RFC flow (RFC-0014):

1. Copy `rfcs/_template.md` → `rfcs/RFC-NNNN-<slug>.md`.
2. Allocate the next sequential number (check the index in
   `rfcs/README.md`).
3. Open a PR with status `draft`.
4. Discussion happens in PR comments.
5. After ≥ 2 reviewers approve and ≥ 1 week has passed, status
   moves to `accepted`. If approval is contentious, the TSC
   (RFC-0009) decides.
6. Implementation can start. Implementation PRs reference the
   RFC.
7. Once implementation is merged, RFC status → `implemented`.

---

## 5. ADRs (per-module decisions)

For decisions local to a single crate / module that don't warrant
an RFC, use an ADR. See RFC-0016 for template + lifecycle.

```
crates/<crate>/ADRs/NNN-title.md
```

ADRs are reviewed by the module owner; approval = merge.

---

## 6. Code style

### Rust

- **Edition:** 2021.
- **MSRV:** documented per crate; we keep MSRV ≥ 12 months old.
- **`rustfmt`** with default config; run before commit.
- **`cargo clippy --all-targets -- -D warnings`** must pass.
- **`#![deny(unsafe_op_in_unsafe_fn)]`** in safety crates.
- Every `unsafe { }` block must have `// SAFETY:` comment.
- Doc comments `///` mandatory for all public items; with at
  least one example where applicable.
- For safety crates (`crates/{ipc,sched,mm,ota,crypto,arch}`):
  no allocations, bounded loops, no panics, `clippy::pedantic`
  enforced. See `safety/CODING_STANDARD.md`.

### Python (`phanes-brain`)

- **Python:** 3.11+.
- **Style:** `black --line-length 100` + `ruff check`.
- **Types:** `mypy --strict` mandatory on safety modules
  (`protocol`, `secure_channel`, `api`); recommended elsewhere.
- **Logging:** structured logs via `logging` module; no `print()`
  in production paths.
- **Tests:** pytest + hypothesis for property tests.

---

## 7. Commit messages

Format (Conventional Commits-like, but loose):

```
<scope>: <imperative summary>

<optional body explaining why, not what>

Closes: #NNN          (if applicable)
Refs: RFC-NNNN        (if applicable)

Signed-off-by: Your Name <your-email@example.com>
```

Examples:

```
sched: implement EDF + CBS budget enforcement

Adds the per-class CBS budget tracker described in RFC-0004.
Validated by Kani harness `formal/kani/edf_cbs_budget.rs`.

Refs: RFC-0004
Signed-off-by: ...
```

```
ipc: fix capability generation wrap on free-then-realloc

Found by mutation test #42 — the generation field could wrap
back to a value that aliased a freed cap.

Closes: #137
Signed-off-by: ...
```

---

## 8. PR expectations

### Before opening:

- ✅ `cargo build --release` clean on all 5 configs (kernel
  changes).
- ✅ `cargo test --all` passes.
- ✅ `cargo clippy --all-targets -- -D warnings` passes.
- ✅ `cargo fmt --all -- --check` passes.
- ✅ `python -m pytest tests/` passes (brain changes).
- ✅ `mypy --strict` passes on safety modules (brain changes).
- ✅ Coverage did not regress (CI checks; you can run
  `cargo llvm-cov --workspace` locally).
- ✅ DCO sign-off on every commit.
- ✅ If touching scheduler / IPC / OTA / secure-boot / crypto:
  RFC must exist + be ≥ accepted.

### What good PRs look like:

- One logical change per PR. Refactors separate from features
  separate from fixes.
- Description explains **why** not just **what**.
- Links to RFC / issue / discussion.
- Tests added or modified to cover the change.
- For safety crates: justification for any new `unsafe` block.

### Review SLA:

- First-pass review: 5 business days.
- Subsequent rounds: 3 business days.
- If a reviewer goes silent > 7 days, ping in PR or contact
  maintainers list.

---

## 9. Testing expectations

Per RFC-0013:

- New code: ≥ 80% line coverage on safety crates, ≥ 70%
  elsewhere.
- New parser / state machine / wire format → property test
  required.
- Touching `crates/{ipc,sched,ota,crypto}`: Kani harness updated
  or new harness added.
- Touching `auth_envelope`, secure_channel: Loom test updated.
- Bug fixes: regression test in `crates/regression-tests/`.

---

## 10. Branch policy

- `main` — current development; expected stable enough to demo.
- `release/vX.Y` — release branches; only critical fixes back-
  ported.
- `lts/vX.Y` — LTS branches (Phase 2+); only security back-
  ports.

Force-push to `main` and protected branches: forbidden.
Force-push to your fork's feature branch: fine.

---

## 11. Security

**Do not file public issues for security vulnerabilities.**

Email `security@phanes-project.org` (PGP key in `SECURITY.md`).
RFC-0009 + RFC-0016 describe the PSIRT process, embargo policy,
and bounty rewards (Phase 2+).

---

## 12. Translations

(Phase 3+) PHANES docs are translated to zh-CN, de-DE, ja-JP.

To contribute a translation:

1. Pick the doc in `book/src/<lang>/<chapter>.md`.
2. Translate; preserve all code blocks, URLs, mermaid diagrams.
3. Update the `last-translated-rev` marker at top.
4. PR; reviewed by a native-speaker reviewer.

We use Weblate for tracking; Crowdin for community.

---

## 13. Project-specific code (your robot)

If you're building **your own robots** on top of PHANES, your
project-specific code lives in **your own repo**, not in PHANES
upstream. See RFC-0018 for the three-tier model.

You contribute to PHANES upstream when you have something
generic that benefits other adopters.

---

## 14. Becoming a maintainer

Sustained, high-quality contribution → invitation by TSC vote.
Criteria:

- ≥ 6 months of substantial contributions.
- Reviewed others' PRs constructively.
- Demonstrates judgement on architecture / security / testing.
- Endorsed by ≥ 2 existing maintainers.

Maintainer responsibilities: PR review, RFC review, PSIRT
rotation (Phase 2+), LTS back-porting share.

---

## 15. Communication

| Channel | Purpose |
|---------|---------|
| GitHub Issues | Bugs, feature requests |
| GitHub Discussions | Design talk, questions, RFC-pre-discussion |
| `dev@phanes-project.org` | Mailing list — non-blocking discussion |
| Matrix `#phanes:matrix.org` (Phase 1+) | Real-time chat |
| `security@phanes-project.org` | Vulnerability reports (PGP) |
| `conduct@phanes-project.org` | Code-of-Conduct issues |

---

## 16. Recognition

All contributors are listed in `CONTRIBUTORS.md` (auto-updated
from `git log`). Significant contributions get called out in
release notes. The "Hall of Fame" recognises security finders
post-Phase-2.

---

## 17. Questions?

Open a Discussion. We answer.

Welcome.
