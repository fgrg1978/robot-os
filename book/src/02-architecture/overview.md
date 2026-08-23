# Architecture overview

For the system diagram and layer map, see
[`docs/plan/ARCHITECTURE.md`](../../../docs/plan/ARCHITECTURE.md). This
chapter introduces the same content in narrative form.

## The four big ideas

PHANES makes four bets that distinguish it from other robotics OSes.

### 1. Capabilities, not file descriptors

Every kernel resource is reached through a typed `Cap<T>`. The type
parameter encodes what kind of resource the cap addresses; mismatched
kinds are a compile-time error.

```rust
let chan: Cap<Channel> = topology::channel("control");
let sensor: Cap<Sensor> = topology::sensor("imu");
ipc::send(chan, &payload);          // ✅
// ipc::send(sensor, &payload);     // ❌ compile error
```

Caps are unforgeable by design and verified at every dereference. See
[Capability-typed IPC](./caps-and-ipc.md).

### 2. Five-class hierarchical scheduler

A single scheduler is wrong for safety and best-effort code at the
same time. PHANES partitions the CPU among five classes, each with
its own policy:

| Class           | Policy   |
|-----------------|----------|
| Safety-critical | EDF + CBS |
| Hard real-time  | EDF + CBS |
| Soft real-time  | RR        |
| Best-effort     | CFS       |
| Idle            | Sporadic  |

Adaptive Partitioning sets a per-class budget; CBS prevents over-run.
See [Multi-policy scheduler](./scheduler.md).

### 3. Static, signed topology

A robot's capability assignment isn't discovered at runtime — it's
**baked into a signed `CAPS.TOML`** that the kernel validates at boot.
No dynamic resource discovery in safety mode means the auditor can
prove what the system *can* do without running it. See [Static
topology](./topology.md).

### 4. AI as a first-class kernel service

Models live in `.MBL` (Model Bundle) files: signed, versioned,
capability-isolated. The kernel's AI runtime loads them with the same
ceremony as any other capability. See [AI runtime](./ai-runtime.md).

## What PHANES is **not**

- Not a Linux kernel — it's bare-metal Rust.
- Not a microkernel in the classical sense — drivers run in-kernel by
  default, with optional userspace driver migration in Phase 2+.
- Not POSIX — POSIX-shaped APIs exist for ergonomics (open / close /
  read / write), but the canonical API is capability-typed.
- Not single-board — the same Rust source builds for RV64 + ARM-A +
  ARM-R + x86_64 (Phase 2+).

## Next

[Capability-typed IPC](./caps-and-ipc.md).
