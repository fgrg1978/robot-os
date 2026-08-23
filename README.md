# KernOS

> **Verifiable. AI-native. Multi-platform. Real-time. Open.**
>
> A robotics-class operating system targeting ISO 26262 ASIL-B
> certification, ROS 2 / AUTOSAR Adaptive interop, and on-device
> AI inference — written in Rust, formally verified where it
> matters, multi-architecture by design (RV64 today; ARM +
> x86_64 planned), and Apache 2.0.

KernOS is the core of an open robotics platform: a bare-metal
kernel with capability-typed IPC, a multi-policy hierarchical
scheduler, hardware-rooted secure boot, and a typed AI runtime —
plus a Python companion (`robot-brain`) for fleet, perception,
and dev tooling.

---

## Why KernOS

The robotics-OS market has gaps that KernOS fills:

- **ROS 2** — powerful ecosystem; not real-time deterministic, not
  safety-cert path, ~256 MB+ baseline.
- **QNX / VxWorks / Integrity** — cert-grade, real-time; closed,
  expensive, AI-unfriendly.
- **Zephyr / FreeRTOS** — small + RT; no network ML, no
  capability isolation.
- **seL4** — formally proven; minimal ecosystem, no AI, no
  robotics-shape.
- **Hubris** — capability-typed Rust; tiny scope, no AI, no
  multi-platform.
- **Apex.OS** — ROS-API + ASIL-D; closed, single platform.

**KernOS is the first project pursuing all of: open + verifiable +
real-time + AI-native + multi-platform + cert-eligible.** That's
the gap.

| Capability | **KernOS** | ROS 2 | QNX | Zephyr | seL4 | Hubris |
|---|---|---|---|---|---|---|
| Open source | ✅ Apache 2.0 | ✅ | ❌ | ✅ | ✅ | ✅ |
| Real-time guarantees | ✅ APS + EDF + CBS | ❌ | ✅ | ✅ | ✅ | ✅ |
| Capability-typed IPC | ✅ `Cap<T>` | ❌ | partial | ❌ | ✅ | ✅ |
| Memory safe (Rust) | ✅ | ❌ (C++) | ❌ (C) | ❌ (C) | ❌ (C) | ✅ |
| Multi-architecture | ◐ RV64 today; ARM + x86 planned | ✅ | ✅ | ✅ | ✅ | ❌ ARM only |
| ISO 26262 cert path | ✅ ASIL-B Phase 3 | ❌ | ✅ ASIL-D | ✅ partial | ❌ | ✅ in progress |
| AI runtime built-in | ✅ Model Bundle + NPU | ❌ ext nodes | ❌ | ❌ | ❌ | ❌ |
| Formal verification | ◐ TLA+ + Kani + Loom (scaffold) | ❌ | partial | ❌ | ✅ proof | ✅ partial |
| Hardware root of trust | ◐ TF-A + OTP + SE (planned) | ❌ | ✅ | partial | ❌ | ✅ |
| Reproducible builds | ◐ SLSA L3 (planned) | ❌ | ❌ | partial | partial | ✅ |

---

## Status

**Phase 1 — Core architecture (current).** Cap-typed IPC mechanism
(28 syscalls) and multi-policy scheduler (APS, EDF, CBS, RR, CFS)
are implemented. Topology parser with signed verification is wired.
Verification skeleton in CI (Kani proofs, 23 host test suites).

This repo currently contains:

- Working RV64 kernel (~109k lines Rust, 6-config clean build).
- Working Python brain (1376 tests passing).
- 34 RFCs: constitutional (RFC-0001..0018), engineering
  (RFC-0019..0026), experiments with verdict (RFC-0027..0036).
- Strategic plan, roadmap, architecture, security model, test
  strategy in `docs/plan/`.

KernOS inherits the legacy "Robot OS" codebase and re-cuts it to the
standards in the RFCs. See git history for pre-Phase-1 evolution.

---

## The five-phase plan

| Phase | Duration | Outcome | Status |
|-------|----------|---------|--------|
| **0. Foundation** | ~3 months | Constitution: vision, RFCs, governance, branding | ✅ Done |
| **1. Core architecture** | ~6 months | Cap-IPC + multi-policy sched + topology + verification skeleton | ◐ In progress |
| **2. Multi-platform + AI runtime** | ~12 months | RV64 today; ARM + x86 planned; Model Bundle .MBL; reference robots | ⏹ Pending |
| **3. Cert + LTS** | ~12 months | ISO 26262 ASIL-B on i.MX 8M; LTS v1; bug bounty live | ⏹ Pending |
| **4. Ecosystem** | ~12 months | i18n docs; second cert SoC; ASIL-D pre-validation | ⏹ Pending |

Cumulative budget over 5 years: ~$15M. Pause-by-phase is
supported; gates 0/1, 1/2, 2/3, 3/4 are clean exit points.

Detail: [`docs/plan/ROADMAP.md`](docs/plan/ROADMAP.md).

---

## Three-tier separation

KernOS is split across three repository tiers (RFC-0018):

```
kernos              (repo: robot-os) — generic kernel, RFCs, verification
kernos-brain        (repo: robot-brain) — generic Python brain framework
<your-project>      — your private project-specific code
```

The dependency arrow points one way: your project imports KernOS;
KernOS never sees your project. Yocto-style layering. You keep
your secret sauce; you get upstream security patches.

---

## Architecture at a glance

```
┌─────────────────────────────────────────────────────┐
│ robot-brain (HOST, Python — perception, planner,    │
│ fleet, dashboard, REST, Telegram)                   │
└─────────────────────┬───────────────────────────────┘
                      │ TCP / UART / LoRa  (HMAC-auth)
                      ▼
┌─────────────────────────────────────────────────────┐
│ KernOS kernel (DEVICE, Rust no_std)                 │
│  ┌──────────────────────────────────────────────┐  │
│  │ Capability-typed IPC  (RFC-0003)             │  │
│  ├──────────────────────────────────────────────┤  │
│  │ Multi-policy scheduler (RFC-0004)            │  │
│  │   APS + EDF + CBS + RR + CFS + Sporadic      │  │
│  ├──────────────────────────────────────────────┤  │
│  │ MM • FS • Net • Crypto • OTA • AI • Drivers  │  │
│  ├──────────────────────────────────────────────┤  │
│  │ arch / HAL  (RV64 today; ARM + x86 planned)  │  │
│  └──────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
                      │ HW Root of Trust (RFC-0011)
                      ▼
   Boot ROM • OTP • Secure Element • encrypted flash
```

Detail: [`docs/plan/ARCHITECTURE.md`](docs/plan/ARCHITECTURE.md).

---

## Quick start (developer)

### Build the kernel (host: macOS / Linux)

```bash
# RV64 default (QEMU)
cargo build --release

# Other configs
cargo build --release --features vf2     # StarFive VisionFive 2
cargo build --release --features k1      # SpacemiT K1 (Banana Pi BPI-F3)
cargo build --release --features no-ml   # without ML stack
cargo build --release --features no-mmu  # MMU-less variant
cargo build --release --features rvv     # RISC-V Vector extension (QEMU)
```

### Run in QEMU

```bash
make qemu              # 1 CPU, minimal
make qemu-smp          # 4 CPUs
make qemu-full-smp     # 4 CPUs + disk + network
make qemu-systest      # syscall test from ring-3
make qemu-tftp-smoke   # TFTP boot smoke
make qemu-dhcp-smoke   # DHCP client smoke
make qemu-net-pair     # two-node TCP round-trip
make qemu-pi-smoke     # Priority Inheritance mutex test
```

### Run the brain (host)

```bash
cd ../robot-brain      # (separate repo from Phase 1+)
python -m server
```

### Run tests

Full verification suite (13 builds + 23 host test suites + 12 QEMU scenarios = 49 checks):
```bash
make ci                # or bash tools/ci_check.sh
```

---

## Documents you should read

| Doc | Purpose |
|-----|---------|
| [`docs/plan/VISION.md`](docs/plan/VISION.md) | The 5-year north star |
| [`docs/plan/ROADMAP.md`](docs/plan/ROADMAP.md) | 5-phase plan, gates, budgets |
| [`docs/plan/ARCHITECTURE.md`](docs/plan/ARCHITECTURE.md) | System diagram, layers, components |
| [`docs/plan/SECURITY_MODEL.md`](docs/plan/SECURITY_MODEL.md) | Threats, properties, invariants |
| [`docs/plan/TEST_STRATEGY.md`](docs/plan/TEST_STRATEGY.md) | 8-tier test pyramid |
| [`rfcs/README.md`](rfcs/README.md) | RFC index + process |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | DCO + RFC + ADR + style |

Constitutional RFCs (read first):

- [`RFC-0001` Strategic plan](rfcs/RFC-0001-strategic-plan.md)
- [`RFC-0002` Modular module pattern](rfcs/RFC-0002-modular-pattern.md)
- [`RFC-0003` Capability-typed IPC](rfcs/RFC-0003-capability-ipc.md)
- [`RFC-0004` Multi-policy scheduler](rfcs/RFC-0004-multi-policy-scheduler.md)

---

## License & governance

- **License:** Apache 2.0 (RFC-0009).
- **Governance:** Pre-incubation; targeting Linux Foundation
  incubation Phase 1 (RFC-0009).
- **Code of Conduct:** Contributor Covenant v2.1.
- **DCO sign-off** required on all commits.
- **Trademark:** "KernOS" is a project name; "KernOS Inside"
  badge program coming Phase 2+ (RFC-0010 + RFC-0018).

---

## Contributing

Read [`CONTRIBUTING.md`](CONTRIBUTING.md). Substantial changes go
through the RFC process. Bug fixes and small improvements just
need a good PR.

Security disclosures: see [`SECURITY.md`](SECURITY.md).

---

## Status taxonomy

This README is a Phase-1 snapshot. Items marked planned (multi-arch
beyond RV64, TF-A + OTP, SLSA L3) reflect the roadmap. The core
architecture (Cap-typed IPC, multi-policy scheduler, topology
verification) is implemented. RFC-0027..0036 are experiments with
published verdicts (some rejected/deferred by design).

---

**KernOS** — the capability-typed kernel for autonomous
systems, formally verified where it matters.
