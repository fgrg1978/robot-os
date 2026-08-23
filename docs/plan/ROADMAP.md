# Roadmap — 5 years to reference

Five phases, each with concrete deliverables, exit triggers, and
investment estimates. Phase N+1 only starts when N's exit triggers fire.

## Phase 0 — Foundations (months 0–3)

> **Goal: freeze the v0.1 base, design the differentiators, line up
> governance, write the first paper outline.**

### Deliverables

| ID | Item | Owner |
|----|------|-------|
| P0-A | Code freeze v0.1; semver-strict ABI doc | core team |
| P0-B | RFC-0002 modular pattern (trait + impl-per-file + Cargo feature + runtime/registry) | analysis |
| P0-C | RFC-0003 capability-typed IPC mechanism | analysis |
| P0-D | RFC-0004 multi-policy hierarchical scheduler | analysis |
| P0-E | RFC-0005 static topology format (CAPS.TOML + SCHED.TOML) | analysis |
| P0-F | RFC-0006 verification strategy (TLA+ + Kani + Loom) | analysis |
| P0-G | RFC-0007 AI runtime (Model Bundle, capability-isolated inference) | analysis |
| P0-H | RFC-0008 multi-platform support (RV64 + ARM + x86_64) | analysis |
| P0-I | RFC-0009 governance + foundation hosting outreach | analysis |
| P0-J | RFC-0010 brand & naming proposal (5 candidates) | analysis |
| P0-K | TLA+ models scheduler + IPC + OTA | analysis |
| P0-L | Foundation outreach (Linux Foundation, Eclipse OpenADx, Arm SOAFEE) | governance |
| P0-M | Apache 2.0 license adoption + CLA + governance docs | governance |
| P0-N | Paper-1 outline ("Capability-typed IPC + hierarchical scheduling for verified Rust microkernel") | research |

### Exit triggers (all must hold to advance)

- [ ] All RFCs at "stable" stage (reviewed, no major objections)
- [ ] Foundation incubation application submitted
- [ ] Brand decided + dominios/repo registered
- [ ] Paper-1 outline reviewed by 1 external systems researcher
- [ ] At least 2 contributors onboarded beyond the core team

### Investment

- 1–2 senior engineers, 3 months
- ~$150K labour + $20K legal/governance/foundation fees

---

## Phase 1 — Differentiator (months 3–12)

> **Goal: caps + multi-policy scheduler MERGED, verification scaffold
> in CI, paper-1 submitted, multi-platform working.**

### Deliverables

| ID | Item | Owner | LoC est. |
|----|------|-------|----------|
| P1-A | `Cap<T>` mechanism in `crates/ipc/` (preserves M02/M04/F15 fast-paths) | impl | ~2500 |
| P1-B | Scheduler multi-policy `crates/sched/policies/{priority,edf,rr,cfs,sporadic,partition}.rs` | impl | ~1700 |
| P1-C | Static topology loader (boot reads `/fat/CAPS.TOML` + `/fat/SCHED.TOML`) | impl | ~400 |
| P1-D | Modular pattern applied to: drivers (uart, i2c, blk, net), allocator | impl | ~1500 |
| P1-E | Kani + Loom CI on cap unforgeability, scheduler safety, IPC ordering | impl | ~600 |
| P1-F | Multi-platform: ARM Cortex-A53 + x86_64 build configs working | impl | ~1500 |
| P1-G | Paper-1 submitted (RTAS or EuroSys 2027) | research | ~30 pages |
| P1-H | docs.X.org with reference manual + tutorials + book | docs | ~5000L MD |
| P1-I | RFC process formalised, GitHub Discussions active, monthly releases | process | proceso |

### Exit triggers

- [ ] All Phase 0 RFCs implemented + tests + CI green on all platforms
- [ ] Paper-1 in submission
- [ ] At least 5 external contributors with merged PRs
- [ ] Foundation incubation accepted

### Investment

- 3–5 senior engineers, 9 months
- ~$700K–$1M labour + $50K conferences + $30K infra/CI

---

## Phase 2 — Ecosystem entry (year 1–2)

> **Goal: stop being an island. ROS2 bridge, NPU framework, AUTOSAR
> Adaptive subset, sim integration. First commercial pilot.**

### Deliverables

| ID | Item | Owner | LoC est. |
|----|------|-------|----------|
| P2-A | ROS2 bridge (rclrs-style, lifecycle nodes, parameter server, DDS interop) | impl | ~3000 |
| P2-B | NPU driver framework + reference impls (Hailo, RK3588, Coral USB) | impl | ~2000 |
| P2-C | AUTOSAR Adaptive subset (`ara::com`, `ara::exec`, `ara::log`) | impl | ~4000 |
| P2-D | Sim bridges (Isaac Sim + Genesis + MuJoCo MJX) | impl | ~1500 |
| P2-E | Reference robot platforms (BOM + firmware + brain) for wheeled, quadruped, manipulator arm | impl + content | ~hardware + 2000L |
| P2-F | Paper-2 submitted ("Hierarchical scheduling for mixed-criticality robots") | research | |
| P2-G | Paper-3 submitted ("Capability-isolated AI runtime") | research | |
| P2-H | Conference circuit: RTAS, ROSCon, EmbeddedWorld, ROS2 World | community | |
| P2-I | First industry partner pilot (commercial deployment internal) | partnerships | |

### Exit triggers

- [ ] First product / pilot in production using robot-os (any vertical)
- [ ] Two papers accepted at top-tier venues
- [ ] ROS2 bridge has 100+ external users (measurable via crates.io / GitHub stars)

### Investment

- 5 senior engineers + community manager + tech writer, 12 months
- ~$1.5M labour + $150K hardware + $80K conferences

---

## Phase 3 — Cert + adoption (year 2–3)

> **Goal: ISO 26262 ASIL-B in hand, AI runtime feature-complete,
> first Tier-2 ECU shipping, foundation working group active.**

### Deliverables

| ID | Item |
|----|------|
| P3-A | AI runtime feature-complete: model bundles, capability-isolated inference, OTA atomic + rollback, VLA path (OpenVLA/π0) |
| P3-B | ISO 26262 ASIL-B certification (formal audit) |
| P3-C | ISO 21434 cybersecurity certification (co-design with ASIL) |
| P3-D | Reference robot platforms publicly published, replicable kits |
| P3-E | University adoption pack (curriculum + lab kits) deployed at 3+ universities |
| P3-F | First Tier-2 automotive ECU shipping with robot-os |
| P3-G | Foundation Working Group with 3–5 partners (Linux Foundation or Eclipse hosted) |
| P3-H | Industry consortium output: spec extensions, reference test suites |

### Exit triggers

- [ ] Cert ASIL-B achieved (audit report public)
- [ ] First commercial product shipped publicly
- [ ] Working group has 3+ funded members

### Investment

- 8–10 engineers + cert team contractors + foundation cuotas, 12 months
- ~$3M labour + $300K cert audit + $100K foundation fees

---

## Phase 4 — Reference status (year 3–5)

> **Goal: be cited in regulatory / standards bodies, AUTOSAR Adaptive
> complete, ASIL-D path, multiple commercial deployments.**

### Deliverables

| ID | Item |
|----|------|
| P4-A | AUTOSAR Adaptive complete open-source reference impl |
| P4-B | ISO 26262 ASIL-D track |
| P4-C | EU AI Act / regulatory citations: robot-os in technical specifications |
| P4-D | WASM userspace runtime (research bet for hot-reload + sandbox) |
| P4-E | Fleet brain protocol + multi-robot coordination |
| P4-F | Multi-OS / AMP support (Linux app cores + robot-os RT cores via OpenAMP) |
| P4-G | 5+ commercial deployments documented (case studies) |
| P4-H | Curriculum at 10+ universities |

### Exit triggers (when robot-os is "the reference")

- [ ] Cited in 50+ academic papers per year
- [ ] EU / NIST / ISO bodies reference robot-os in compliance docs
- [ ] 10+ commercial deployments
- [ ] Foundation working group sustained funding ≥ 5 partners

### Investment

- 12–15 engineers, 24 months
- ~$6–8M labour + ~$1M ASIL-D cert + ongoing foundation/conferences

---

## Total investment

| Phase | Months | Cost |
|-------|--------|------|
| 0 | 3 | ~$170K |
| 1 | 9 | ~$1M |
| 2 | 12 | ~$1.7M |
| 3 | 12 | ~$3.4M |
| 4 | 24 | ~$7M |
| **Total** | **60 months** | **~$13M** |

Reduction levers (~30–40% offset):

- Horizon Europe / EU consortium grant: €3–5M typical
- NLnet, NSF, DARPA funding
- Linux Foundation member contributions
- University research partnerships (sub-contracted PhD students)
- Commercial customer co-funding (Tier-2 + robotics startups)

Realistic external funding target: ~$4–5M of the ~$13M, leaving
~$8M sustained spend over 5 years.

---

## Decisions still pending (tracked in RFC-0009)

- [ ] Brand name (RFC-0010 proposes 5; needs decision before Phase 1 starts)
- [ ] Foundation choice: Linux Foundation incubation vs Eclipse OpenADx vs Apache Incubator
- [ ] License: Apache 2.0 (recommended) vs MIT vs dual
- [ ] Cert target: ASIL-D (Phase 4 stretch) or stop at ASIL-B (Phase 3 floor)
- [ ] Governance model: BDFL transitional → meritocracy by Phase 2

These are explicit decision points. Each one is documented in its
respective RFC with options + recommendations + tradeoffs.
