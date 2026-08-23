# PHANES RFCs

> **Request For Comments** — the design documents that define the
> technical and operational shape of PHANES.

## Purpose

Every architectural / governance decision lands here as an RFC before
becoming code. RFCs serve three purposes:

1. **Forcing function for thinking** — writing exposes ambiguity that
   coding hides.
2. **Onboarding artefact** — new contributors read RFCs to understand
   *why*, not just *what*.
3. **Audit trail** — for cert (ISO 26262 / 21434) and foundation
   review, RFCs are the design evidence.

## Status taxonomy

| Status | Meaning |
|--------|---------|
| `draft` | Proposed, under discussion. Not authoritative. |
| `accepted` | Reviewed, approved, implementation may start. Stable API. |
| `implemented` | Code merged, RFC reflects shipped reality. |
| `superseded by RFC-NNNN` | Replaced by a newer RFC; kept for history. |
| `rejected` | Considered, rejected. Kept as record of the decision. |

## Numbering

Sequential. Once allocated, an RFC number is permanent — even if
rejected. Numbering reflects *when written*, not topical grouping.

## Index

| ID   | Title                                                      | Status                  |
|------|------------------------------------------------------------|-------------------------|
| 0001 | Strategic plan & 5-phase roadmap                           | accepted                |
| 0002 | Modular module pattern (constitutional)                    | accepted                |
| 0003 | Capability-typed IPC (`Cap<T>`) — constitutional           | accepted                |
| 0004 | Multi-policy hierarchical scheduler — constitutional       | accepted                |
| 0005 | Static topology format (CAPS.TOML + SCHED.TOML)            | accepted                |
| 0006 | Verification strategy (TLA+ + Kani + Loom)                 | accepted                |
| 0007 | AI runtime (Model Bundle + capability isolation)           | accepted                |
| 0008 | Multi-platform support (RV64 + ARM + x86_64)               | accepted                |
| 0009 | Governance, foundation hosting, license                    | accepted                |
| 0010 | Branding — PHANES                                          | accepted                |
| 0011 | Secure Boot & Anti-Tamper (HW-ROT)                         | accepted                |
| 0012 | Supply chain hardening (SBOM + SLSA)                       | accepted                |
| 0013 | Quality engineering (coverage + fuzz + mutation)           | accepted                |
| 0014 | Documentation strategy (book + manual + i18n)              | accepted                |
| 0015 | Compliance & Certification (ISO 26262 + 21434)             | accepted                |
| 0016 | Operational excellence (PSIRT + LTS + ADRs)                | accepted                |
| 0017 | Brain (Python) — role, scope, evolution                    | accepted                |
| 0018 | Generic PHANES vs project-specific development             | accepted                |
| 0019 | Forward-secret link (X25519 + AES-128-CTR + HMAC)          | implemented (handshake + kernel TCP wire-up landed, fail-closed; rekey / session-id / camera-frame encryption open) |
| 0020 | Driver migration plan (user-mode drivers post-AQ3)         | accepted (planned for post-AQ3 work) |
| 0021 | Multi-stream brain link                                    | implemented (lib + 15 tests + kernel TCP wire-up landed; gated default-off) |
| 0022 | Hardware video encoder (JH7110)                            | partially implemented (cam-ring SPSC + 10 tests only; `VideoEncoder`/`NoOpEncoder`/Wave420L NOT built) |
| 0023 | Scalable resource limits (3 profiles)                      | superseded by RFC-0026  |
| 0024 | *(reserved)* PMP region scaling beyond 16 (RV64 M-mode)    | not yet written         |
| 0025 | *(reserved)* Capability ABI v2 — 8-bit CapKind             | not yet written         |
| 0026 | Phanes config — Kconfig-style                              | implemented (13 fragments, 157 symbols, 18 build-time invariants, `make menuconfig` operative, brain const bridge) |
| 0027 | `#[wcet(N_us)]` budget annotations (Phase B I1)            | **rejected** (KILL2 — variance floor too high on QEMU; I1.1–I1.4 infra landed and works, the CI *gate* is what failed) |
| 0028 | Multi-stream priority scheduling (control preempts bulk) — I2 | experiment-running (`MULTISTREAM_SCHED_PRIORITY` default n) |
| 0029 | Transactional control ticks (rollback-on-fault) — I-13     | accepted (`CONTROL_TXN_TICKS` default y) |
| 0030 | Stackless async control plane — I-12                       | experiment-running (executor not yet built; bench proxy only) |
| 0031 | Lease/capability priority inheritance — I3                 | accepted (measured benefit, productized, gated default-off) |
| 0032 | AI-tensor primitives — I6                                  | deferred (no gross win on the emulation substrate; revisit at HW) |
| 0033 | Bounded runtime safety monitor (motor output envelope)     | accepted (design RFC — enforcement wired at `rt_motor_task`; no measured ratio) |
| 0034 | Speculative actuation — predictive brain→kernel channel    | accepted (capability v1 — channel real, speculative APPLY HW-deferred) |
| 0035 | Confidence-aware real-time (confidence-scaled envelope)    | accepted (design RFC — wired at the `motor_envelope` chokepoint) |
| 0036 | Brain-triggered degraded mode (capability containment)     | accepted (design RFC — `CapTable::get` denies WRITE when degraded) |

## Process

1. Author opens PR with `rfcs/RFC-NNNN-*.md` in `draft` status.
2. Discussion in GitHub Discussions or PR review comments.
3. After ≥ 2 reviewers approve and ≥ 1 week passes (or longer if
   substantive concerns are open), status moves to `accepted`.
4. Implementation begins; RFC's "Open questions" section closes as
   answers come from impl experience.
5. When code is merged, status moves to `implemented`. RFC is
   updated to reflect any drift between spec and implementation.

## Template

See `rfcs/_template.md`.
