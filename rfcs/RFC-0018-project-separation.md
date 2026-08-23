# RFC-0018: Generic PHANES vs Project-Specific Development

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

PHANES is divided into **three repository tiers** to keep the
generic, foundation-hosted, certifiable artefact distinct from
project-specific, application-level robot development:

1. **Generic PHANES** (`phanes`) — the kernel, drivers, RFCs,
   verification artefacts. Apache 2.0, foundation-hosted,
   cert-eligible. Used by anyone.
2. **Generic brain framework** (`phanes-brain`) — the Python
   orchestrator framework: protocol, `auth_envelope`, plugin API,
   skill catalog scaffolding, dashboard, fleet, sim adapters.
   Apache 2.0, generic, reusable.
3. **Project-specific** (e.g. `myrobots-stack`, owned by the user
   or company) — custom skills, robot-specific integrations,
   private hardware wiring, custom missions, deployment configs,
   private models. **NOT part of PHANES upstream.** Lives in a
   separate repo with whatever license / privacy the owner
   prefers.

This RFC defines the boundary between "generic PHANES" (owned by
the foundation, used by everyone) and "your specific robot
development" (owned by you, used by you).

## Motivation

Without this separation, two failure modes:

- **Upstream pollution.** Project-specific custom skills, hardware
  wiring, or mission code lands in `phanes` / `phanes-brain` and
  bloats the generic OS with stuff irrelevant to other adopters.
  Foundation governance rejects.
- **Downstream lock-in.** A robotics startup that builds on PHANES
  has to fork the generic repos to add their proprietary skills,
  losing upstream updates. Bad for adoption.

The clean answer: a third repo tier for project-specific code that
**imports PHANES** as a dependency, the same way an app imports a
library.

## Detailed design

### Three-tier repository structure

```
┌────────────────────────────────────────────────────────────────────┐
│                     GENERIC TIER (foundation, public)              │
│                                                                    │
│   github.com/phanes-project/phanes                                 │
│     ─ kernel (Rust no_std)                                         │
│     ─ verification (TLA+, Kani, Loom)                              │
│     ─ secure boot, OTA, IPC, scheduler, drivers                    │
│     ─ RFCs                                                         │
│     ─ Apache 2.0, LF-hosted, cert-eligible                         │
│                                                                    │
│   github.com/phanes-project/phanes-brain                           │
│     ─ Python framework (asyncio, plugin API)                       │
│     ─ generic protocol (`protocol.py`)                             │
│     ─ generic auth (`secure_channel.py`)                           │
│     ─ generic dashboard / fleet / mode mgr                         │
│     ─ skill catalog SCAFFOLD (no robot-specific skills)            │
│     ─ Apache 2.0, generic, reusable                                │
└────────────────────────────────────────────────────────────────────┘
                              │
                              │ depends on (cargo / pip)
                              ▼
┌────────────────────────────────────────────────────────────────────┐
│              PROJECT-SPECIFIC TIER (your private repo)             │
│                                                                    │
│   github.com/<your-org>/myrobots-stack    (or private)             │
│     ─ custom skills for your specific robots                       │
│     ─ hardware-specific BSP files (wiring, GPIO, sensors)          │
│     ─ proprietary models / weights                                 │
│     ─ deployment-specific CAPS.TOML / SCHED.TOML                   │
│     ─ mission code, custom modes                                   │
│     ─ private telemetry endpoints, fleet IDs                       │
│     ─ company-internal docs                                        │
│     ─ Whatever license / privacy you choose                        │
└────────────────────────────────────────────────────────────────────┘
```

### What goes in each tier

#### `phanes` (generic kernel) — public, Apache 2.0, foundation

- Kernel: `kernel/`, `crates/{ipc,sched,mm,fs,net,ota,...}`
- Drivers: `crates/drivers/{uart,i2c,blk,net,...}` for **canonical**
  SoCs (QEMU, VF2, K1, i.MX, RK3588). Vendor-specific exotic SoCs
  go to project-specific tier.
- AI runtime framework (`crates/ml/`) — backend abstraction, not
  bundled models.
- Verification scaffolding (`formal/`, regression-tests).
- All RFCs (this directory).
- Reference topology files (`/templates/wheeled.caps.toml` etc.) —
  starting points, not your real deployment.
- Reference robot platforms (BOM published, code public).

#### `phanes-brain` (generic brain framework) — public, Apache 2.0

- Python framework: `asyncio` server scaffold, plugin loading.
- Generic protocol: `protocol.py`, `secure_channel.py`,
  `auth_envelope` extensions.
- Generic UI: dashboard scaffold, REST API, Telegram bot framework.
- Skill catalog **API** (ABCs), with **stub implementations** for
  reference (e.g., `WheeledForwardSkill`, `DroneTakeoffSkill`).
- Fleet coordinator framework.
- LLM / VLM client library (interfaces; not bundled models).
- Sim adapters (Isaac, MuJoCo, Genesis).
- ROS2 bridge framework.
- Tests: hypothesis property tests, mutation tests, fuzzing.

What's **NOT** here:

- Your specific robot's wheel circumference, motor PWM mapping,
  IMU calibration constants.
- Your custom skills (e.g. "deliver coffee in office X").
- Your fleet's deployment topology.
- Your private LLM prompts.
- Your customer-specific dashboards.

#### `myrobots-stack` (your project) — private, your license

This is **your repo**, owned by you, hosted wherever you prefer
(private GitHub, GitLab, self-hosted). Imports `phanes` and
`phanes-brain` as upstream dependencies.

Suggested layout:

```
myrobots-stack/
├── Cargo.toml          ← depends on phanes via git or crates.io
├── pyproject.toml      ← depends on phanes-brain via pip / git
├── kernel-bsp/
│   ├── src/
│   │   ├── my_motor_driver.rs   ← my custom motor control
│   │   ├── my_imu_layout.rs     ← my IMU wiring
│   │   └── lib.rs                ← BSP exports
│   └── Cargo.toml
├── brain-skills/
│   ├── deliver_coffee.py        ← my custom skill
│   ├── patrol_office.py
│   └── ...
├── topology/
│   ├── my_wheeled_robot.caps.toml
│   ├── my_wheeled_robot.sched.toml
│   ├── my_drone.caps.toml
│   └── my_drone.sched.toml
├── deployments/
│   ├── home_lab/
│   │   └── config.yaml
│   ├── customer_a/                ← gated to ops, optional
│   └── customer_b/
├── models/                        ← private trained models
│   ├── my_perception_v3.MBL
│   └── my_policy_v2.MBL
├── docs/
│   ├── INTERNAL_NOTES.md
│   ├── HARDWARE_BOM.md
│   └── ...
└── README.md (private)
```

### Pattern: how generic and specific compose

A typical PHANES-based robot deployment:

1. Pull `phanes` (kernel) + `phanes-brain` from upstream public
   releases.
2. Build the kernel with project-specific BSP crate from
   `myrobots-stack/kernel-bsp/` linked in.
3. Spawn the brain with project-specific skill plugins from
   `myrobots-stack/brain-skills/`.
4. Deploy with project-specific `topology/my_wheeled_robot.*.toml`
   loaded from your repo's deployment config.

Generic PHANES never sees `myrobots-stack`. The dependency arrow
points one way.

### Why this matters for the user

**This split lets you:**

- Develop your robots privately without sharing your secret sauce.
- Get upstream PHANES updates (kernel security patches, scheduler
  improvements, ROS2 bridge fixes) without merge conflicts.
- Apply your own license / privacy / governance to your robot
  project independently of PHANES Foundation policies.
- Sell / open-source / sunset your project on your own timeline.

**And lets PHANES be:**

- Cert-eligible without your private code in scope.
- Foundation-governed without your business decisions affecting it.
- Adopted by other robot projects independently.

### Contribution flow

When something useful in `myrobots-stack` becomes generic enough to
be PHANES-worthy:

1. You file an RFC at `phanes-project/phanes` or `phanes-brain`
   proposing the upstream addition.
2. You submit a PR with the generic version (no project-specific
   wiring).
3. After review, it merges; you remove the now-redundant code from
   `myrobots-stack`.

Conversely, if upstream PHANES introduces a breaking change:

1. You read the deprecation notice in the release notes.
2. Your `myrobots-stack` is pinned to the previous version until
   you migrate.
3. Migrate at your pace; LTS branches give 5 years of safety.

### Naming

| Name | Owner | Purpose |
|------|-------|---------|
| `phanes` | PHANES Foundation | Generic kernel + RFCs + verification |
| `phanes-brain` | PHANES Foundation | Generic brain framework |
| `myrobots-stack` (placeholder) | You | Your robot-specific code |

Your project doesn't need to be called `myrobots-stack` — call it
whatever fits your business (e.g. `acme-robotics-fleet`, `my-robot`,
etc.). It is **not** branded PHANES; it **uses** PHANES.

### Trademark angle

PHANES Foundation will (Phase 1+) publish a "PHANES Inside"
program: products built on PHANES can use a "Built on PHANES" badge
and tagline, contingent on:

- Using an unmodified release version of `phanes` kernel (or LTS
  with security patches only).
- Following the topology + cap discipline of RFC-0005.
- Not modifying secure boot, OTA, or scheduler core.

This badges your `myrobots-stack` as PHANES-compliant **without**
making it part of PHANES.

## Drawbacks

- **Three-repo cognitive load** for users. Mitigated by a clear
  starter template (`phanes-project/template-stack`) that
  bootstraps a `myrobots-stack` skeleton with examples.
- **Discipline required** to not slip project-specific code into
  upstream. CI checks for this in upstream PRs (no
  customer-identifiers, no deployment-specific values).
- **Sample / reference robots tension.** We do publish reference
  robot platforms in `phanes` (e.g., reference wheeled). These are
  *generic* templates. Your real wheeled robot lives in
  `myrobots-stack`.

## Rationale and alternatives

**Alternative A — single mega-repo with everything.** Rejected:
foundation can't govern private code; you can't have private code
under their governance.

**Alternative B — fork PHANES privately.** Rejected: loses upstream
updates; classical fork bitrot.

**Alternative C — submodule the project repo into PHANES.**
Rejected: same governance problem.

**Alternative D (chosen) — three tiers, dependency arrow points
one way.** Industry-standard pattern (Linux + your custom kernel
modules; ROS2 + your custom packages; Yocto + your custom
recipes).

## Prior art

- **Yocto / OpenEmbedded.** Three-tier: poky (generic), `meta-*`
  layers (semi-generic), `meta-yourcompany` (private).
- **Linux kernel + your-out-of-tree-driver.** Same pattern.
- **ROS2 + custom workspace.** Same pattern.
- **Buildroot + your config + your packages.** Same pattern.

We borrow heavily from the Yocto layer model: PHANES = `poky`,
`phanes-brain` = `meta-frameworks`, `myrobots-stack` =
`meta-yourcompany`.

## Unresolved questions

- **Where does the user's `myrobots-stack` skeleton template
  live?** Working assumption: `phanes-project/template-stack` —
  public Apache 2.0 starter, you fork it private.
- **CI for `myrobots-stack`** — should we provide a reusable
  GitHub Actions workflow that runs PHANES tests + your tests?
  Working assumption: yes; lives in `template-stack`.
- **Versioning compatibility matrix** — which `phanes` version is
  compatible with which `phanes-brain` version? Working assumption:
  semver-locked; published in release notes.

## Future possibilities

- **Phase 3:** "PHANES Inside" certification program for downstream
  projects.
- **Phase 4:** PHANES Marketplace — public skill / model bundles
  shared across projects, signed and reviewed.
- **Phase 5:** Multi-tenant brain — a single brain instance hosting
  multiple `myrobots-stack` projects safely (cap-isolated).
