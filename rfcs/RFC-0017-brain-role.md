# RFC-0017: Brain (Python) — Role, Scope, Evolution

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

The PHANES brain (currently `robot-brain`, to be renamed
`phanes-brain`) is and **always will be** a Python asyncio host-side
server for **robot development, operations, and high-level
orchestration**. It is **explicitly NOT part of the certifiable OS
artefact**. The OS (kernel) is the certifiable, safety-critical
component; the brain is the developer / operator tooling layer that
sits next to it. No migration to Rust is planned.

This RFC defines the boundary, the scope, and how each foundational
RFC (0001–0016) lands or doesn't land in the brain repo.

## Motivation

A clean separation between "certifiable OS" and "host tooling"
prevents two categories of mistake:

- **Brain dragged into cert scope.** If brain were treated as part
  of the OS artefact, it would need ISO 26262 ASIL-N qualification,
  which is unrealistic for a Python codebase. Cert costs would
  balloon for no functional benefit.
- **OS dragged into brain scope.** If the kernel adopted brain-level
  patterns (Python interop, dynamic dispatch, ML-library bindings),
  it would lose its determinism and verifiability story.

The boundary is: **the kernel runs on the robot in production. The
brain runs on a developer / operator workstation or fleet edge
server.** They communicate over an authenticated TCP link
(RFC-0011's `auth_envelope`).

## What the brain IS

The brain is the **host-side toolchain + operator layer** for PHANES
robots. Its purposes:

1. **Robot development.** Engineers iterate on robot behaviour by
   editing Python code in the brain, not by re-flashing kernels.
2. **Operator console.** HTTP REST API + Telegram bot + dashboard
   web UI for humans to instruct, monitor, and recover the robot.
3. **Mission planning.** LLM-based decomposition of free-text tasks
   into skill sequences. Iteration cycle is fast (minutes) because
   it's Python prompt engineering, not kernel rebuild.
4. **VLM perception orchestration.** Vision-language model on the
   host (LM Studio, vLLM, ollama). Latency: 100–500 ms. Acceptable
   for cognition; not for control.
5. **Skill catalog.** A library of skills per robot type, dispatched
   to the kernel via brain protocol.
6. **Fleet coordination.** Multi-robot messaging, broadcast, fleet
   dashboard.
7. **Experience / learning store.** Persistent JSONL of
   plan-execution outcomes; informs future planning.
8. **External integrations.** ROS2 bridge (Phase 2), MAVLink (PX4),
   Telegram, custom HTTP endpoints.
9. **Simulator front-end.** When sim is the target (Isaac, Genesis,
   MuJoCo), brain talks to sim instead of robot — same code path.

## What the brain IS NOT

- **Not part of the certifiable OS.** Brain is excluded from ASIL
  cert, ISO 21434 cert, and other formal artefacts. The OS audit
  ends at the brain ↔ kernel TCP boundary.
- **Not a control loop.** No hard-real-time. No motor command
  emitted directly without going through the kernel's safety arbiter.
- **Not running on the robot in production.** Brain runs on host /
  edge / cloud. The robot can run **brain-less** (kernel uses
  on-board AI models — RFC-0007 — for full autonomy).
- **Not migrating to Rust.** Python is the right tool for ML
  prompt-engineering, LLM orchestration, web UI, and developer
  iteration. We commit to Python permanently.
- **Not the safety arbiter.** Even if the brain emits "FORWARD 100",
  the kernel's safety crate (geofence, ESTOP, watchdog) decides
  whether to honour it.

## Architecture as it stands

```
HOST ─ developer / operator workstation OR edge server
┌──────────────────────────────────────────┐
│   Operator (HTTP / Telegram / Dashboard) │
└────────────┬────────────────────────────┘
             │
             ▼
┌──────────────────────────────────────────┐
│   PHANES Brain (Python asyncio)         │
│   ─ perception / vision.py (VLM)        │
│   ─ planner / decide.py (LLM)           │
│   ─ planner / task_planner.py           │
│   ─ planner / experience.py              │
│   ─ executor / skill_runner.py          │
│   ─ policy / wheeled.py / drone.py / .. │
│   ─ fleet.py                             │
│   ─ secure_channel.py (HMAC envelope)   │
│   ─ api.py (HTTP REST)                   │
│   ─ telegram_bot.py                      │
└────────────┬────────────────────────────┘
             │  TCP brain protocol
             │  (auth_envelope HMAC; RFC-0011)
             ▼
ROBOT (production target)
┌──────────────────────────────────────────┐
│   PHANES Kernel (Rust no_std)           │
│   ─ behavior task                       │
│   ─ rt-motor / flight-ctrl              │
│   ─ AI runtime (Phase 3+)               │
│   ─ filesystem / OTA / network / IPC    │
│   ─ secure boot / verification          │
└──────────────────────────────────────────┘
```

The kernel is the certifiable side. Everything above the TCP line
is "host tooling" with its own engineering rigour but a different
risk profile.

## How RFCs 0001–0016 land in `phanes-brain`

| RFC | Brain action |
|-----|--------------|
| 0001 Strategic | Brain has its own roadmap (below); milestones decoupled from kernel cert. |
| 0002 Modular | Apply Python ABC + plugin pattern: each policy / skill / model is an ABC implementor. |
| 0003 Cap IPC | Brain receives caps from kernel via `auth_envelope` extended for cap-transfer; can only invoke what it has. |
| 0004 Scheduler | n/a (host process; OS-level scheduling). |
| 0005 Topology | Brain reads its own `BRAIN.TOML` declaring which kernel caps it expects to receive. |
| 0006 Verification | Brain adds `hypothesis` (Python proptest), `mutmut` (mutation), `coverage.py` thresholds. |
| 0007 AI runtime | **Migration trigger.** As kernel AI runtime matures, models migrate **from** brain (LM Studio integration) **to** kernel. Brain becomes thinner orchestrator. |
| 0008 Multi-platform | Brain runs on Linux + macOS + Windows. CI matrix expanded. |
| 0009 Governance | `phanes-brain` under same Apache 2.0 + DCO + PSIRT + LF. |
| 0010 Brand | `robot-brain` → `phanes-brain` rename. |
| 0011 Secure boot | Brain not in chain. But **brain ↔ kernel link uses signed HMAC handshake** (already in `auth_envelope` + extends to cap-transfer). |
| 0012 Supply chain | SBOM via `pip-audit` + `cyclonedx-bom`; signed wheels via `sigstore-python`; locked deps in `requirements.txt` + `requirements.lock`. |
| 0013 Quality | Coverage gate ≥ 85% on safety paths; `hypothesis` + `mutmut`; `mypy --strict` on protocol modules. |
| 0014 Documentation | Brain section in docs.phanes.org book; per-module API docs via `pdoc` or `sphinx`. |
| 0015 Compliance | **Brain explicitly out of cert scope.** Customer integrations may need their own cert path (e.g. for production operator stations); we don't provide. |
| 0016 Operational | Same LTS / PSIRT / ADRs apply. |

## Evolution — brain over the next 5 years

### Phase 0–1 (now → m12) — brain as today, hardened

- Python asyncio (no change).
- VLM via LM Studio (no change).
- LLM planner emits skill sequences (no change).
- **Add:** caps integration (extension of `auth_envelope` to carry
  caps), hypothesis property tests, SBOM, brand rename, `mypy
  --strict` on protocol modules, `mutmut` mutation testing.
- **Add:** `BRAIN.TOML` config file (mirrors kernel's `CAPS.TOML`).
- **No architectural shift.**

### Phase 2 (y1–2) — interop layer

- ROS2 bridge: brain becomes a ROS2 node, slottable into existing
  ROS2 stacks. Critical for adoption.
- Sim integration: brain talks to Isaac / Genesis / MuJoCo as if to
  a robot.
- Plugin API: third-party skills, policies, modes load cleanly.
- **Brain is fully developer-friendly tooling.**

### Phase 3 (y2–3) — model migration KERNEL-WARD

When the kernel AI runtime (RFC-0007) is feature-complete:

- VLA / Foundation models that today run host-side via brain's LM
  Studio integration **migrate to the kernel** as Model Bundles.
- Robot can run **autonomously without the brain**: kernel hosts
  the models, perception, control. Brain is optional UX layer.
- Brain remains for: development iteration, operator UI, fleet
  coordination, mission planning that benefits from host LLM, sim
  integration.
- **The robot in production may not need the brain at runtime.**
  Brain is dev / ops layer, not a runtime dependency for autonomy.

### Phase 4–5 — brain stays Python, scope deepens

- More integrations (more LLMs, more sim engines, more robot types).
- Brain becomes the **standard development environment** for PHANES
  robots: clone brain repo + connect to robot = ready to develop.
- Plugin ecosystem matures: third-party skills / policies / models
  / dashboards.
- **Python forever.** No Rust port. The robot in production
  doesn't run Python; the developer's host does.

## Brain has its own development discipline

Even though brain is not cert-scope, it gets the same engineering
rigour minus the cert overhead:

| Discipline | Brain | Kernel |
|------------|-------|--------|
| Apache 2.0 | yes | yes |
| DCO | yes | yes |
| PSIRT (security@) | yes | yes |
| Coverage thresholds | ≥ 85% safety paths | ≥ 80% safety crates |
| Mutation testing | yes | yes |
| Property-based tests | hypothesis | proptest |
| Type discipline | mypy --strict on safety | rustc |
| Fuzzing | atheris | OSS-Fuzz / Kani |
| LTS releases | yes (5y) | yes (5y) |
| External security audit | yearly | yearly |
| SBOM | pip-audit + cyclonedx | cargo-cyclonedx |
| Signed releases | sigstore-python | cosign + Rekor |
| ISO 26262 cert | **explicitly NO** | yes (Phase 3+) |
| ISO 21434 cert | **explicitly NO** | yes (Phase 3+) |
| Auditor-readable spec | n/a | RFC-0006 |

## Brain-specific topology — `BRAIN.TOML`

Mirrors `CAPS.TOML` for brain side. Declares which kernel caps the
brain expects to receive on connect:

```toml
[brain]
identity = "operator-station-001"
fleet_id = "fleet-alpha"

[required_caps]
# Granted by the kernel to brain after auth handshake (RFC-0011).
caps = [
    { kind = "channel-pub",  target = "/cmd/motor",       perm = "w" },
    { kind = "channel-sub",  target = "/sensors/imu",     perm = "r" },
    { kind = "channel-sub",  target = "/perception/cam",  perm = "r" },
    { kind = "service-call", target = "robot.estop",      perm = "rw" },
]

[ml]
vlm_endpoint = "http://localhost:1234/v1"
llm_endpoint = "http://localhost:1234/v1"
vlm_model    = "internvl2-2b"
llm_model    = "qwen2.5-1.5b-instruct"

[fleet]
broadcast_addr = "10.0.0.255:9000"
heartbeat_period_s = 5

[security]
link_key_env = "ROBOT_BRAIN_LINK_KEY"
api_key_env  = "ROBOT_BRAIN_API_KEY"
```

## Quality gates for brain

| Gate | Tool | Threshold |
|------|------|-----------|
| Coverage | `coverage.py` | ≥ 85% on safety paths (protocol, secure_channel) |
| Mutation | `mutmut` | ≥ 70% killed on parsers |
| Property tests | `hypothesis` | every parser + state machine |
| Type checking | `mypy --strict` | required on `protocol.py`, `secure_channel.py`, `api.py` |
| Linting | `ruff check` | 0 warnings on safety modules |
| Fuzzing | `atheris` (Phase 2) | continuous on parser modules |
| SBOM | `cyclonedx-bom` | per release |

## Drawbacks

- **Two-language project.** Cognitive load on contributors. Mitigated
  by shared protocol RFCs and clear architecture diagram.
- **Brain Python deps drift.** Mitigated by SBOM + `pip-audit` in CI
  and `requirements.lock`.
- **Some duplication of protocol code.** Brain Python `protocol.py`
  and kernel Rust `brain_protocol.rs` define the same wire format
  in two languages. Acceptable: small, unit-tested, cross-checked
  via `protocol-sync` skill, plus a host-side test that the same
  byte stream parses to equivalent structures in both.
- **Brain never gets ASIL.** Customers wanting "every bit of code
  cert" are out of luck — but those customers should run the robot
  brain-less in production.

## Rationale and alternatives

**Alternative A — Rust brain from day one.** Rejected: Python's ML
ecosystem dominance + LLM iteration speed + dashboard libraries
make Python the right tool. Rust would slow development.

**Alternative B — Brain merged into kernel.** Rejected: brain runs
on host / edge / cloud, not on the robot. Different deployment.
Different language idioms.

**Alternative C — Brain as part of cert artefact.** Rejected: would
require Python qualification (impractical for ASIL-D). Confines
the project to a Python subset that wouldn't be useful.

**Alternative D (chosen) — Brain explicitly out of cert scope, host
tooling, Python forever.** Clean boundary. Right tool for each side.

## Prior art

- **ROS2** — separation between user-space nodes (Python / Rust /
  C++) and DDS middleware. Different shape, similar philosophy.
- **NVIDIA Isaac** — Isaac (host orchestrator, Python) +
  Jetson runtime (deterministic). Same pattern.
- **PX4 Autopilot + QGroundControl** — flight controller (C++) +
  ground station (Qt/QML). Mature precedent for cert-side / dev-side
  separation.
- **Autoware** — automotive autonomy stack with similar split.

## Unresolved questions

- **Brain ↔ kernel cap-transfer protocol.** Extends `auth_envelope`
  to carry caps. Detailed in a follow-up RFC during Phase 1.
- **How much of the existing `robot-brain` Python code stays vs.
  refactors during Phase 1?** Working assumption: minimal refactor
  (rename + tests + SBOM); architectural cleanup is opportunistic.

## Future possibilities

- **Phase 4:** federated learning across brain instances (brains
  share experience across a fleet).
- **Phase 4:** multi-brain redundancy (active / standby for fleet
  high-availability deployments).
- **Phase 5:** Brain-less production deployments standard. Brain
  becomes purely the developer / operator UX layer; no runtime
  dependency on it.
