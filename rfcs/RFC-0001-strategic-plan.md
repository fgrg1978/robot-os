# RFC-0001: Strategic Plan & 5-Phase Roadmap

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

PHANES is positioned to become the open-source reference for verifiable
Rust microkernels with native AI runtime and a complete safety +
cybersecurity certification path, targeted at robots and autonomous
vehicles. This RFC sets the 5-phase plan, the exit triggers, and the
investment frame.

## Motivation

The market gap is concrete. No existing system marks all of these
boxes simultaneously: open-source, formally-verifiable, hard-real-time,
AI-native, safety-certifiable, capability-based, multi-platform.

| | Open | Verified | RT | AI-native | Cert | Multi-arch |
|---|------|----------|----|-----------|------|------------|
| seL4 | ✅ | ✅ | ✅ | ❌ | partial | ✅ |
| QNX | ❌ | ❌ | ✅ | ❌ | ✅ | ✅ |
| Linux+RT | ✅ | ❌ | soft | ✅ | hard | ✅ |
| Apex.OS | ❌ | ❌ | ✅ | partial | ✅ | partial |
| Hubris | ✅ | ❌ | ✅ | ❌ | ❌ | partial |
| **PHANES** | **✅** | **✅** | **✅** | **✅** | **✅** | **✅** |

That gap is the strategic foothold. The plan below is how we exploit it.

## The five phases

### Phase 0 — Foundations (months 0–3, ~$200K)

Analysis and governance, no shipping code beyond what already exists.

- All 16 RFCs written and reviewed (this RFC included).
- Brand decided (PHANES, see RFC-0010), TM cleared, domains held.
- Foundation outreach (Linux Foundation incubation; see RFC-0009).
- License selected (Apache 2.0, see RFC-0009).
- TLA+ models drafted for scheduler, IPC, OTA, secure_boot.
- Paper-1 outline drafted.
- ABI v0.1.0 freeze on the existing 47 KLoC codebase as the baseline
  to evolve from.

**Exit triggers:** all RFCs at `accepted` status; foundation
incubation application submitted; brand registration filed; ≥ 2
external contributors onboarded.

### Phase 1 — Differentiator (months 3–12, ~$1.3M)

The architectural shift that defines PHANES vs every other open RTOS.

- Capability-typed IPC (RFC-0003) merged.
- Multi-policy scheduler (RFC-0004) merged with EDF + CBS + adaptive
  partitioning.
- Modular pattern (RFC-0002) applied to every replaceable subsystem.
- Static topology loader (RFC-0005) reads `CAPS.TOML` + `SCHED.TOML`
  at boot.
- Multi-platform: ARM Cortex-A53 + x86_64 build configurations clean
  in addition to RV64.
- Secure boot HW-ROT (RFC-0011) on at least one reference SoC (NXP
  i.MX as primary).
- Supply chain hardening (RFC-0012): SBOM + SLSA L3 + Sigstore on
  every release.
- Verification scaffold (RFC-0006): Kani + Loom in CI on cap
  unforgeability + scheduler safety + IPC ordering.
- Paper-1 submitted to RTAS or EuroSys.
- Reference manual + book scaffold at `docs.phanes.org`.

**Exit triggers:** caps + scheduler merged and tested across all
platforms; paper sometido; ≥ 5 external contributors with merged
PRs; foundation incubation accepted.

### Phase 2 — Ecosystem entry (year 1–2, ~$2.3M)

Stop being an island. Make PHANES interoperable with the dominant
toolchains.

- ROS2 high-fidelity bridge (rclrs-style; lifecycle nodes; parameter
  server; DDS interop).
- NPU driver framework (RFC-0007) plus reference impls for Hailo,
  RK3588 NPU, Coral USB.
- AUTOSAR Adaptive subset (`ara::com`, `ara::exec`, `ara::log`).
- Sim bridges: Isaac Sim + Genesis + MuJoCo MJX.
- Reference robot platforms published (BOM + firmware + brain) for
  wheeled, quadruped, manipulator arm.
- Hardware-in-the-Loop (HIL) CI farm: VF2 + K1 + i.MX + ARM Cortex-R
  in a server rack running CI on real silicon.
- OTP provisioning tooling and Secure Element drivers (ATECC608,
  OPTIGA Trust M).
- OSS-Fuzz integration; bug bounty (HackerOne).
- Conference circuit: RTAS, ROSCon, EmbeddedWorld, ROS2 World.
- Papers 2 and 3 submitted.
- First commercial pilot (internal or partner).

**Exit triggers:** one product / pilot in production using PHANES;
two papers accepted; ROS2 bridge has ≥ 100 external users; cert
audit prep complete (RFC-0015).

### Phase 3 — Certification + adoption (year 2–3, ~$3.8M)

Move from "interesting open source" to "in production with cert".

- AI runtime feature-complete (Model Bundle, capability-isolated
  inference, OTA atomic + rollback, VLA path with OpenVLA / π0).
- ISO 26262 ASIL-B certification audit and report.
- ISO/SAE 21434 cybersecurity certification.
- Remote attestation + fleet protocol.
- Safety case + cybersecurity case documents (RFC-0015).
- University adoption pack deployed at ≥ 3 universities.
- First Tier-2 automotive ECU shipping with PHANES.
- Foundation Working Group with ≥ 3 funded partners.
- External annual security audit (Trail of Bits / Cure53 / NCC).
- Pre-built dev kits for evaluation.
- Telemetry / observability nativos (OpenTelemetry, Prometheus,
  structured logs).

**Exit triggers:** ASIL-B cert obtained; first commercial product
shipped publicly; foundation working group has ≥ 3 funded members.

### Phase 4 — Reference status (year 3–5, ~$7.5M)

Become *the* answer for verifiable Rust RT in robots and cars.

- AUTOSAR Adaptive complete, open-source reference implementation.
- ISO 26262 ASIL-D track.
- WASM userspace runtime (research bet for hot-reload + sandboxing).
- Multi-OS / AMP support (Linux app cores + PHANES RT cores via
  OpenAMP).
- Citations in EU AI Act, EU Cyber Resilience Act, NIST publications.
- Multilingual docs: EN + ZH + DE + JP.
- ≥ 5 commercial deployments documented as case studies.
- Curriculum at ≥ 10 universities.

**Exit triggers (when "PHANES is the reference"):** cited in ≥ 50
academic papers per year; appears in EU / NIST / ISO compliance
references; ≥ 10 commercial deployments; foundation working group
sustained ≥ 5 funded members.

## Investment summary

| Phase | Months | Cost | + Cert | + Enterprise | **Total** |
|-------|--------|------|--------|--------------|-----------|
| 0 | 3 | $170K | — | $30K | **$200K** |
| 1 | 9 | $1M | — | $300K | **$1.3M** |
| 2 | 12 | $1.7M | $100K prep | $500K | **$2.3M** |
| 3 | 12 | $3M | $400K ASIL-B | $400K | **$3.8M** |
| 4 | 24 | $6M | $1M ASIL-D | $500K | **$7.5M** |
| **5y** | 60 | **$11.9M** | **$1.5M** | **$1.7M** | **~$15M** |

Realistic external funding offset (Horizon Europe / NLnet / NSF /
DARPA / partner co-funding): **~$5M** of the $15M.

Effective sustained internal spend: **~$10M over 5 years**.

## Pause structure

The plan has explicit decision gates between phases (see ROADMAP.md).
At each gate, the project may continue, pause as LTS, slow down, or
pivot. Pausing is cheapest at gate 0/1 (only docs invested) and
becomes structurally harder once cert and commercial customers depend
on us (post-gate 3/4).

## Drawbacks

- Multi-year, multi-million commitment. Fails if funding dries up.
- Architectural decisions made now constrain choices later.
- Open-source governance is slower than autocratic decisions.
- Cert path is expensive and irreversible once entered.

## Rationale and alternatives

**Alternative A — narrower scope.** Drop cert, drop multi-platform,
drop AUTOSAR. Result: a more focused open-source RTOS, but no
defensible position vs Hubris / Tock / Drone. Discarded.

**Alternative B — closed-source commercial.** Capture more value,
faster decisions. But the strategic moat (foundation hosting,
academic citations, regulatory references) requires open-source.
Discarded.

**Alternative C — fork an existing system** (seL4, Hubris). Faster
start, but you inherit constraints + community politics + IP
questions. The robot-os codebase already exists and is closer to the
target than any fork would be. Discarded.

## Prior art

Reference points used to calibrate this plan:

- **seL4** verification programme: 10+ years from start to widely-cited.
- **Linux Foundation projects** (Zephyr, Open vSwitch): 5–8 years to
  reference status.
- **AUTOSAR Adaptive** standard: 8 years from first spec to volume
  production.
- **Apex.OS** commercial trajectory: 3 years from founding to first
  production deployment in a vehicle.

## Unresolved questions

- Foundation choice: Linux Foundation vs Eclipse OpenADx vs Apache.
  Working assumption: Linux Foundation incubation. Confirmed in RFC-0009.
- Final license: Apache 2.0 confirmed.
- Brand: PHANES confirmed pending professional TM search.

## Future possibilities

- Phase 5: PHANES as foundation for distributed-fleet AI in 2030+.
- Phase 5: WASM-based dynamic component model as the dominant
  userspace shape.
- Phase 5: Formal-verified ML inference (the next decade's research
  frontier).

These are not commitments. They are signposts that the architecture
designed in Phases 0–1 should not foreclose.
