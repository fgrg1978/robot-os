# PHANES — Architecture Overview

> **Audience:** engineers, integrators, auditors, contributors  
> **Pre-requisites:** RFC-0001 (strategy), RFC-0002 (modular pattern)  
> **Last updated:** 2026-05-10

This document gives the system-level picture of PHANES. For
detailed contracts, see the per-component RFCs.

---

## 1. System diagram

```
┌──────────────────────────────────────────────────────────────────────┐
│                       USER / OPERATOR / FLEET                        │
└──────────────────────────────────────────────────────────────────────┘
                                  │ HTTPS / Telegram / dashboard
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│                          phanes-brain (HOST, Python)                 │
│ ┌────────────┐ ┌────────────┐ ┌──────────┐ ┌───────────┐ ┌────────┐ │
│ │ Perception │ │   Planner  │ │ Skill    │ │  Fleet    │ │  REST  │ │
│ │ (VLM/LLM)  │ │ task→skill │ │ catalog  │ │ registry  │ │  API   │ │
│ └────────────┘ └────────────┘ └──────────┘ └───────────┘ └────────┘ │
│ ┌──────────────────────────────────────────────────────────────────┐ │
│ │ Server  • protocol.py  • secure_channel.py (auth_envelope HMAC) │ │
│ └──────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────┬────────────────────────────────────┘
                                  │ TCP / UART / LoRa  (PHANES protocol)
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│                            PHANES kernel (DEVICE, Rust)              │
│  USER SPACE  ┌────────────┐ ┌────────────┐ ┌──────────────────────┐ │
│              │   Skills   │ │  Mission   │ │  AI inference daemon │ │
│              │  runtime   │ │  manager   │ │  (Model Bundle .MBL) │ │
│              └─────┬──────┘ └─────┬──────┘ └──────────┬───────────┘ │
│                    │ syscall      │ syscall           │ syscall      │
│  ─────────────────┼─────────────┼──────────────────┼───────────────  │
│  KERNEL SPACE     ▼             ▼                  ▼                │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │                  Capability-typed IPC (RFC-0003)                ││
│  └─────────────────────────────────────────────────────────────────┘│
│  ┌──────────────┐ ┌──────────────┐ ┌────────────┐ ┌──────────────┐ │
│  │ Multi-policy │ │  Memory mgmt │ │  FS layer  │ │  AI runtime  │ │
│  │  scheduler   │ │   (Sv39 /    │ │  (FAT32 /  │ │  framework   │ │
│  │  (RFC-0004)  │ │    MMU /COW) │ │   tmpfs)   │ │  (RFC-0007)  │ │
│  └──────────────┘ └──────────────┘ └────────────┘ └──────────────┘ │
│  ┌──────────────┐ ┌──────────────┐ ┌────────────┐ ┌──────────────┐ │
│  │  Net stack   │ │  Crypto      │ │  OTA + sec │ │  Watchdog +  │ │
│  │ (TCP/IP/v6)  │ │ (Ed25519/AES)│ │  boot ROT  │ │  health      │ │
│  └──────────────┘ └──────────────┘ └────────────┘ └──────────────┘ │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │              Driver layer  (UART, NIC, blk, IMU, GPS, …)        ││
│  └─────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │       arch / HAL  (RV64 GC + ARM Cortex-A/R + x86_64)           ││
│  └─────────────────────────────────────────────────────────────────┘│
└──────────────────────────────────────────────────────────────────────┘
                                  │ HW root of trust (RFC-0011)
                                  ▼
┌──────────────────────────────────────────────────────────────────────┐
│  Boot ROM • OTP / eFuse • Secure Element • encrypted flash • tamper  │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 2. Layer map

### Layer 0 — Hardware Root of Trust

Boot ROM, OTP / eFuse keys, optional Secure Element (ATECC608A),
encrypted flash, tamper-detection lines. Defined in RFC-0011.

### Layer 1 — Architecture / HAL

`crates/arch-*/` (rv64, aarch64, x86_64). Bootstrap, MMU
configuration, interrupt handling, atomic primitives. Frozen ABI
in `crates/abi/` so user-space contracts are stable across
platforms.

### Layer 2 — Drivers

`crates/drivers/`. Per-board UART, NIC, blk, IMU, GPS, PWM, GPIO,
I2C, secure-element. Modular pattern (RFC-0002) — every driver
type is a `trait` + `impls/<vendor>.rs`.

### Layer 3 — Kernel core services

| Crate | Responsibility | RFC |
|-------|----------------|-----|
| `crates/ipc` | Capability-typed IPC, fast-path, shared memory | RFC-0003 |
| `crates/sched` | Multi-policy scheduler, partitioning, CBS | RFC-0004 |
| `crates/mm` | Sv39 paging, demand paging, COW, allocator | (constitutional) |
| `crates/fs` | VFS, FAT32, tmpfs, procfs | (impl) |
| `crates/net` | TCP/IP stack, IPv6, DNS, NTP, DHCP | (impl) |
| `crates/crypto` | Ed25519, AES, SHA256, X25519, secure_channel | (impl) |
| `crates/ota` | A/B + recovery slot + signature verification | RFC-0011 |
| `crates/ml` | NPU abstraction, Model Bundle loader, inference | RFC-0007 |

### Layer 4 — Behaviour & supervision

`crates/behavior/`. Safety profiles, geofence, ESTOP, mode mgr,
auth envelope. Topology (CAPS.TOML, SCHED.TOML) bound here.

### Layer 5 — Userspace

ELF loader, syscall surface (60+), userspace tasks: skills runtime,
mission manager, AI inference daemon, telemetry, fleet client.

### Layer 6 — Brain (HOST, Python)

Out of cert scope (RFC-0017). Talks to device via PHANES protocol
over TCP/UART/LoRa; provides VLM/LLM perception, planner,
dashboard, fleet, sim adapters. Generic framework in
`phanes-brain` repo; project-specific code in user's
`myrobots-stack` (RFC-0018).

---

## 3. Component inventory (Phase-1 target)

### Kernel crates (Rust no_std)

```
crates/
├── arch-rv64gc/          ← Phase 1 reference
├── arch-aarch64/         ← Phase 1 reference
├── arch-x86_64/          ← Phase 2
├── abi/                  ← Frozen user-space ABI types
├── ipc/                  ← Cap<T>, fast-path, SHM, channels (RFC-0003)
├── sched/                ← Multi-policy scheduler (RFC-0004)
│   ├── policies/
│   │   ├── fifo.rs
│   │   ├── edf_cbs.rs
│   │   ├── rr.rs
│   │   ├── cfs.rs
│   │   └── sporadic.rs
│   └── partitions/
├── mm/                   ← Paging, demand, COW, allocator
├── fs/
│   ├── vfs.rs
│   ├── fat32/
│   ├── tmpfs/
│   └── procfs/
├── net/
│   ├── eth.rs
│   ├── arp.rs
│   ├── ip.rs (v4 + v6)
│   ├── tcp/
│   ├── udp/
│   ├── dns.rs
│   ├── ntp.rs
│   └── dhcp.rs
├── crypto/
├── ota/
├── ml/
│   ├── runtime.rs
│   ├── bundle.rs        ← .MBL loader
│   └── backends/
│       ├── ggml_nano.rs
│       ├── tflm.rs
│       └── npu_*.rs
├── drivers/
│   ├── uart/{api,impls/*.rs}
│   ├── blk/{api,impls/*.rs}
│   ├── nic/{api,impls/*.rs}
│   ├── imu/{api,impls/*.rs}
│   ├── gps/{api,impls/*.rs}
│   ├── se/{api,impls/*.rs}     ← Secure Element (RFC-0011)
│   ├── i2c/, gpio/, pwm/, …
├── behavior/
│   ├── safety.rs               ← Safety profiles + ESTOP
│   ├── auth_envelope.rs
│   ├── modes.rs
│   ├── offline.rs
│   └── logger.rs
├── trace/                ← F27 tracing + profiling
├── watchdog/
├── config/               ← CONFIG.INI + AtomicU32 runtime
├── trace/
└── regression-tests/
```

### Verification artefacts

```
formal/
├── tla/                  ← TLA+ specs (sched, IPC, OTA)
├── kani/                 ← Bounded model checker harnesses
├── loom/                 ← Concurrency tests
└── proofs/
    └── INVARIANTS.md     ← System-wide invariants list
```

### Brain (Python, host)

```
phanes-brain/
├── server.py
├── protocol.py
├── secure_channel.py
├── perception/{vision.py,llm.py,...}
├── planner/{decide.py,modes.py,skills.py,task_planner.py}
├── policy/{wheeled.py,drone.py,humanoid.py,ackermann.py}
├── executor/skill_runner.py
├── fleet/                ← E07 fleet mgmt
├── sim/                  ← B01 SITL adapters
├── dashboard/            ← B02 fleet dashboard
├── notifications.py
├── api.py
├── telegram_bot.py
├── mavlink_bridge.py     ← E08
└── tests/
```

---

## 4. Cross-cutting concerns

### 4.1 Capability discipline

Every kernel object handed to a user task is a `Cap<T>` — typed,
unforgeable, populated at boot from CAPS.TOML. POSIX-style
integer fds **only at the syscall ABI boundary** for legacy
adapter purposes. Internally everything is Cap-typed.

### 4.2 Topology binding

Every deployable PHANES image embeds a signed CAPS.TOML +
SCHED.TOML pair (RFC-0005). Static topology only; no runtime
discovery in safety mode.

### 4.3 Modular pattern

All extensible subsystems follow RFC-0002:

```
crates/<sub>/src/
├── api.rs
├── lib.rs              ← cfg-selects active impl
├── impls/<x>.rs        ← per-implementation code
├── runtime/registry.rs ← Phase 4 dynamic table (skeleton from Phase 1)
└── common/
```

### 4.4 Supply-chain integrity

Every binary in a release has SBOM + cosign signature + SLSA L3
provenance + reproducible build (RFC-0012).

### 4.5 Time

System time is monotonic (`Instant`-style); wall-clock obtained
via NTP only. Safety paths never depend on wall-clock; only
monotonic deltas. Deadlines in scheduler and CBS budgets all use
monotonic time.

---

## 5. Boot flow (i.MX 8M Plus reference)

```
HW BROM (mask ROM)
  → verifies TF-A image via OTP-anchored Ed25519
  → jumps to TF-A
TF-A
  → initialises memory, brings up cores (BL31 secure monitor)
  → verifies + jumps to U-Boot SPL
U-Boot SPL → U-Boot proper
  → loads kernel.bin from flash
  → verifies kernel.bin signature against OTP-anchored key
  → if A-slot fails: try B-slot
  → if both fail: try KERN_R.BIN (immutable recovery slot)
PHANES kernel (`kernel/src/main.rs`)
  → arch init (paging, GIC/PLIC, timer)
  → mm init
  → ipc init (cap table)
  → sched init (load SCHED.TOML, build partitions)
  → topology bind (load CAPS.TOML, populate cap table)
  → driver init (per-board)
  → spawn supervisor task (PID 1)
  → spawn brain-link task (TCP/UART)
  → dispatch to scheduler
```

---

## 6. Threading & concurrency model

- **Kernel:** preemptive multi-tasking, 4 CPU SMP, partitioned
  scheduler.
- **User:** cooperative within a task; preemption between tasks
  per scheduler policy.
- **Synchronisation primitives:** spin-locks (kernel only;
  bounded), atomic refcount, wait queues. **No futexes / no
  user-mode mutexes** in safety paths — capabilities + IPC
  replace.
- **Async runtime:** brain side uses asyncio; kernel side uses
  cooperative `core::future` only in user space, never in kernel.

---

## 7. Memory map (logical)

```
0x0000_0000_0000_0000 ─┐
                       │ User space (per-task; Sv39)
                       │   text + data + heap + stack
0x0000_003F_FFFF_FFFF ─┘
0x0000_0040_0000_0000 ─┐ Reserved / unmapped
                       │
0xFFFF_FFC0_0000_0000 ─┐
                       │ Kernel space
                       │   .text + .rodata
                       │   .data + .bss
                       │   kernel heap
                       │   per-CPU stacks
                       │   device MMIO (ioremap'd)
0xFFFF_FFFF_FFFF_FFFF ─┘
```

PMP (RV64) / TF-A (ARM) enforces region permissions; user space
cannot access kernel without explicit syscall.

---

## 8. ABI freeze policy

`crates/abi/` holds the user-space contract:

- syscall numbers
- syscall arg / return types
- `repr(C)` data structures crossing the boundary
- error codes

Within a major release series, no ABI break. ABI changes only at
major boundaries; deprecation cycle defined in RFC-0016.

---

## 9. Deployment topology

A typical PHANES-Inside production deployment:

```
┌─────────────────────────────────────────────────────────────────┐
│  CLOUD / FLEET CONTROL (operator + dashboard)                   │
│  • aggregates fleet telemetry                                   │
│  • issues missions                                              │
│  • OTA distribution                                             │
└────────────────────────┬────────────────────────────────────────┘
                         │ TLS (Cloudflare / Fastly / self-host)
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│  EDGE GATEWAY (per-site phanes-brain)                           │
│  • Python brain, x86 / ARM mini-PC                              │
│  • LM Studio (VLM + LLM) local inference                        │
│  • per-robot session (auth_envelope HMAC)                       │
└────────────────────────┬────────────────────────────────────────┘
                         │ TCP / UART / LoRa  (PHANES protocol)
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│  ROBOT (PHANES kernel + skills)                                 │
│  • RV64 / ARM SBC                                               │
│  • motors, sensors, payload                                     │
└─────────────────────────────────────────────────────────────────┘
```

---

## 10. Glossary

- **Cap<T>** — typed capability (RFC-0003).
- **CAPS.TOML / SCHED.TOML** — signed static topology (RFC-0005).
- **MBL** — Model Bundle, AI artefact format (RFC-0007).
- **HW-ROT** — Hardware Root of Trust (RFC-0011).
- **PSIRT** — Product Security Incident Response Team (RFC-0016).
- **LTS** — Long-Term Support branch (RFC-0016).
- **ASIL** — Automotive Safety Integrity Level (ISO 26262;
  RFC-0015).
