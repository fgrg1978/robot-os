# RFC-0023: Scalable Resource Limits via Compile-Time Profiles

> **Status:** superseded by RFC-0026  
> **Authors:** Fernando Rodriguez
> **Created:** 2026-05-26
> **Last updated:** 2026-05-26
> **Supersedes:** —
> **Superseded by:** —

## Summary

The PHANES kernel today uses small static array caps (`MAX_TASKS = 64`,
`TCP_MAX_CONNS = 8`, `MAX_FDS = 16`, …) sized for a single-robot demo,
not a real deployment.  This RFC introduces a **single centralised
limits module** (`crates/limits/`) with **three compile-time profiles**
(`embedded` / `edge` / `fleet`) selected via cargo features.  Same code,
different scale, same zero-alloc-runtime guarantee.  No heap allocator
required at runtime; the safety case (RFC-0017) is preserved.

Default profile becomes `edge`, sized to support **~200 userspace apps
+ 256 TCP conns + 100 cameras + 100 LiDAR streams** per kernel — the
realistic ceiling for the user's stated target (1000-robot fleet
× 100 MB/s peak, each robot running ~200 apps).

Two limits cannot scale with a constant bump and require **separate
RFCs**:
- PMP regions (hardware-limited to 16 on RV64 M-mode) — RFC-0024.
- `CapKind` 4-bit field (only 16 capability types) — RFC-0025 (ABI v2).

## Motivation

### Today's caps (audit, 2026-05-26)

| Resource | Site | Current | Adequate for |
|----------|------|---------|--------------|
| Schedulable tasks | `crates/sched/src/task.rs:6` | 64 (esp32c3: 8) | ~10 system tasks + handful of user apps |
| TCP connections | `crates/net/src/tcp.rs:19` | 8 | One brain link + OTA + spare |
| Sockets (all) | `crates/net/src/socket.rs:8` | 16 | One brain link + a few UDP services |
| File descriptors | `crates/fs/src/vfs.rs:15` | 16 | Demo |
| IPC channels | `crates/ipc/src/channel.rs:18` | 16 | A few inter-task pipes |
| IPC pipes | `crates/ipc/src/pipe.rs:6` | 32 | Demo |
| IPC ports | `crates/ipc/src/port.rs:16` | 32 | Demo |
| IPC leases | `crates/ipc/src/lease.rs:40` | 16 | Demo |
| Services | `crates/service/src/lib.rs:10` | 32 | A handful of named services |
| Pub/sub topics | `crates/pubsub/src/lib.rs:19` | 32 | A handful |
| Subs per topic | `crates/pubsub/src/lib.rs:21` | 8 | Demo |
| Caps (total pool) | `crates/topology/src/types.rs:25` | 1024 | ~30 apps × 32 caps each |
| Kernel heap | `kernel/src/main.rs:137` | 4 MiB (esp32c3: 64 KiB) | Demo |

For the target deployment shape (200 apps + 100 sensors per robot,
1000 robots in fleet, ~100 MB/s peak per robot):

- Every cap in the table above is **too small by 1-2 orders of
  magnitude**.  Bumping is necessary.
- Some caps (PMP regions, `CapKind` width) **cannot be bumped** without
  redesign.

### Why not heap-allocated growable structures?

We deliberately use static arrays at compile time because:

1. **WCET predictability** — every kernel data-structure walk has
   a bounded loop count known to the compiler.  `Vec::push` can
   trigger a realloc-and-copy that breaks ISR latency guarantees.
2. **OOM-by-design avoidance** — a kernel that can run out of memory
   in the middle of a syscall fails the safety case (RFC-0017).
   With fixed pools we always fail at *allocation*, never mid-flight.
3. **Cert-eligibility** — ISO 26262 ASIL-B prohibits unbounded heap
   in safety-critical paths.

So the answer is **not** "make it dynamic"; the answer is
**"make the static size right for the deployment, and prove the
memory budget closes"**.

## Detailed design

### Three profiles

A single `crates/limits/` crate exports every cap as a `pub const`
selected by cargo feature:

```rust
// crates/limits/src/lib.rs
#![no_std]

#[cfg(feature = "profile-embedded")]   pub use embedded::*;
#[cfg(feature = "profile-edge")]       pub use edge::*;
#[cfg(feature = "profile-fleet")]      pub use fleet::*;
// Default = edge.
#[cfg(not(any(feature = "profile-embedded",
              feature = "profile-edge",
              feature = "profile-fleet")))]
pub use edge::*;

mod embedded { /* small caps */ }
mod edge     { /* default caps */ }
mod fleet    { /* large caps */ }
```

Every existing site (`crates/sched/src/task.rs:6`, etc.) is rewritten:

```rust
// Before:
pub const MAX_TASKS: usize = 64;

// After:
pub use robot_os_limits::MAX_TASKS;
```

The cargo feature is propagated from the kernel crate the same way
`qemu` / `vf2` / `k1` already are.

### The three profile values

| Cap | embedded | **edge (default)** | fleet | Memory cost (edge) |
|-----|----------|--------------------|-------|---------------------|
| `MAX_TASKS` | 32 | **512** | 4096 | 512 × 384 B TCB = 192 KiB |
| `TCP_MAX_CONNS` | 4 | **128** | 1024 | 128 × ~1.2 KiB = 150 KiB |
| `MAX_SOCKETS` | 8 | **256** | 2048 | 256 × 512 B = 128 KiB |
| `MAX_FDS` (per-proc) | 16 | **64** | 256 | per-proc, in PCB |
| `MAX_FDS_TOTAL` | 64 | **2048** | 16384 | global; tracked in PCB heap |
| `MAX_CHANNELS` (ipc) | 16 | **512** | 4096 | 512 × 256 B = 128 KiB |
| `MAX_PIPES` | 16 | **512** | 4096 | 512 × 4 KiB ring = 2 MiB |
| `MAX_PORTS` | 16 | **256** | 2048 | 256 × 128 B = 32 KiB |
| `MAX_LEASES` | 16 | **128** | 1024 | 128 × 256 B = 32 KiB |
| `MAX_SERVICES` | 16 | **256** | 2048 | 256 × 64 B = 16 KiB |
| `MAX_TOPICS` | 16 | **256** | 2048 | 256 × 512 B = 128 KiB |
| `MAX_SUBS_PER_TOPIC` | 4 | **32** | 256 | inline in topic |
| `MAX_CAPS_TOTAL` | 256 | **16 384** | 131 072 | 16 K × 64 B = 1 MiB |
| `KERNEL_HEAP_SIZE` | 256 KiB | **32 MiB** | 256 MiB | direct |
| `USER_STACK_SIZE` | 16 KiB | 16 KiB | 16 KiB | per task |

**Notes**:
- `USER_STACK_SIZE` doesn't change per profile.  Total = `MAX_TASKS × 16
  KiB` = 8 MiB on edge, 64 MiB on fleet — these are lazy-allocated via
  demand paging (E11 already implemented), so the *virtual* commitment
  is large but the *physical* footprint scales with actual use.
- `MAX_FDS_TOTAL` is a new cap (today there's only per-proc); needed
  because the VFS file table is global today.
- `KERNEL_HEAP_SIZE` jump is large (4 MiB → 32 MiB).  Needs to fit on
  the target SoC RAM:
  - VF2: 8 GiB DDR4 → fits trivially.
  - K1: 16 GiB LPDDR4X → fits trivially.
  - ESP32-C3 (`profile-embedded`): 400 KiB SRAM → stays at 256 KiB cap.

### Memory budget closure (edge profile)

Sum of static tables: **~3-4 MiB**.  Plus heap 32 MiB.  Plus lazy task
stacks ~8 MiB (demand-paged).  Total worst-case **~45 MiB** per kernel
instance.  Well within the 8+ GiB RAM target hardware budgets.

### Profile selection

Cargo feature flags, mutually exclusive:

```bash
cargo build --release --features profile-embedded   # ESP32-C3, tiny SoCs
cargo build --release --features profile-edge       # default: VF2, K1, RK3588
cargo build --release --features profile-fleet      # cloud-edge gateways
cargo build --release                               # = profile-edge (default)
```

`esp32c3` feature forces `profile-embedded` (override existing
conditional logic at the cap sites — every esp32c3 cap currently
hardcoded becomes a profile-embedded value).

### Per-cap rationale

For each cap, the RFC requires a justification of the chosen value.
Three categories:

1. **App count-bound**: `MAX_TASKS`, `MAX_SERVICES`, per-proc `MAX_FDS`.
   Sized for the **stated 200-app target × 2 safety margin = 400 → round
   up to nearest power of 2 = 512**.
2. **Wire-bound**: `TCP_MAX_CONNS`, `MAX_SOCKETS`.  Sized for
   **multi-stream protocol (RFC-0021) × cameras + LiDAR + control + OTA
   + spare = ~50 → round up = 128**.  Edge default tolerates ~30 sensor
   streams per robot.
3. **Mesh-bound**: `MAX_CHANNELS`, `MAX_PIPES`, `MAX_TOPICS`,
   `MAX_CAPS_TOTAL`.  Sized for **N apps × M avg-channels-per-app + slop
   = 512 × 5 + 50% = ~4000 → round up = 4096** for caps.  The
   per-resource numbers in the table are smaller because not every app
   uses every primitive.

### What does NOT scale with a constant bump

Three architectural limits cannot be solved by this RFC:

1. **PMP regions (RV64 M-mode)**: hardware limit **16 regions max**.
   Each isolated user process today consumes 4-8 regions.  Practical
   ceiling: ~3 isolated procs simultaneously.  For 200 apps we need
   **MMU-based isolation (Sv39 page tables, already implemented) as the
   primary mechanism**, with PMP reserved for the M-mode trusted
   computing base (kernel + privileged drivers).  Tracked in **RFC-0024
   (MMU-as-Primary-Isolation Migration)** — design RFC, not implemented.

2. **`CapKind` 4-bit field (RFC-0003)**: enum currently has 16 slots,
   all consumed.  Adding new typed cap kinds (e.g. `CapKind::VideoFrame`,
   `CapKind::AudioStream`) requires expanding to 8 bits = 256 slots.
   That changes the in-kernel `Cap` struct layout, which changes the
   capability ABI.  Tracked in **RFC-0025 (Cap ABI v2: 8-bit CapKind)** —
   design RFC, not implemented.

3. **`MAX_SUBS_PER_TOPIC`**: today static array of 8 subscribers per
   topic.  At 200 apps × pub/sub mesh density of 10% = ~20
   subs/topic.  Bumping to 32 (edge) handles it, but at fleet scale
   (`MAX_SUBS_PER_TOPIC = 256`) the per-topic memory cost grows
   quadratically with topic count.  Acceptable now; revisit if topic
   count > 1000 per kernel.

### Implementation plan (6 phases, ~3-5 days total)

| Phase | Scope | Files | Tests | Days |
|-------|-------|-------|-------|------|
| **L1** Audit + centralise | Create `crates/limits/`.  Move every cap to it under `mod edge`.  Add cargo features `profile-embedded` / `profile-edge` / `profile-fleet`.  Default = edge.  All current values preserved (this phase is **pure refactor**, no behavioural change). | ~15 sites edited, 1 new crate | All existing tests still pass | 1 |
| **L2** Bump to edge | Replace small embedded values with the edge column from the table above.  Verify 6/6 builds + all host tests + brain pytest still pass + memory closure check via `objdump --section-headers` on the kernel binary. | `crates/limits/src/edge.rs` rewritten | Verify table sizes don't overflow .bss budget | 1 |
| **L3** Memory closure verification | Build the kernel.  `objdump` + `nm --size-sort` to confirm the increased table sizes match the table in this RFC ±10%.  Add `regression-tests` entry pinning these sizes so future cap bumps don't sneak past memory budgets. | New `crates/regression-tests/src/memory_budget.rs` | New tests | 0.5 |
| **L4** Fleet profile dry-run | Build with `--features profile-fleet`.  Confirm 6/6 configs still compile.  Memory check — fleet must fit on VF2/K1's RAM.  Document worst-case in this RFC. | Documentation only | Build check | 0.5 |
| **L5** WCET regression sweep | Some loops in the kernel walk these tables (e.g. `find_conn` in `tcp.rs`, scheduler queue walks).  With 8× larger tables, walk time grows 8×.  Re-run WCET probes (already in place from #35).  If a critical-path loop blows budget, add a hash-map or bucket structure. | Possibly `tcp.rs`/`scheduler.rs` if regression found | `crates/sched-policy-tests` | 1 |
| **L6** Default flip to edge | Once L1-L5 are green, switch the default profile from "preserve old values" to actual edge.  Document the flip in `MEMORY.md`.  Brain side gets nothing new (this RFC is kernel-only). | Cargo.toml | Verify everything still green | 0.5 |

### Risks

1. **WCET regression**: bigger tables = longer linear walks.  Mitigation:
   most lookups are already O(N) where N is small; if N grows 8× and
   we cross WCET budget, we replace linear scan with hash index.
   `tcp::find_conn` is the most likely candidate.

2. **Memory budget overrun**: confirmed via L3 phase.  If a profile
   exceeds the target SoC RAM, that profile is unsupported on that SoC
   and the kernel build refuses (compile-time `assert!` on table sizes).

3. **Test churn**: existing tests assume specific cap values (e.g.
   "the 9th connection should be rejected because TCP_MAX_CONNS=8").
   L1 must update those tests to use `robot_os_limits::TCP_MAX_CONNS`
   instead of hard-coding `8`.

4. **Brain-side mismatches**: brain's `protocol.py` has its own
   constants (e.g. `MAX_ROBOT_TYPES`).  This RFC is kernel-only;
   brain caps already scale via Python (no static arrays).  No
   coordinated change needed.

## Drawbacks

- **Memory footprint** of edge default (~45 MiB) is large compared to
  the old demo footprint (~5 MiB).  This is intentional: we're sizing
  for production, not demos.  The `embedded` profile preserves the old
  ~5 MiB footprint for ESP32-C3 and similar small targets.

- **Compile-time choice**: a deployed kernel can't switch profiles at
  runtime.  Each fleet must commit to a profile at build time.  This is
  the right trade-off for safety-cert eligibility (the safety case
  doesn't have to enumerate "what if user picks profile X at runtime").

- **No graceful degradation**: hitting `MAX_TASKS` returns an error to
  the caller, same as today.  If 513 apps want to start under edge
  profile, app 513 fails to spawn.  Operator must upgrade to fleet
  profile or reduce app count.

## Rationale and alternatives

**Alternative A — Dynamic allocators (Vec, slab, etc.)**: rejected.
Breaks zero-alloc-runtime, breaks WCET, breaks cert path.  See
"Motivation" above.

**Alternative B — Runtime configurable via CONFIG.INI**: tempting but
no.  Tables would have to be sized for the *max* config value, defeating
the point.  Same memory cost as static fleet profile, plus a runtime
config bug surface.

**Alternative C (chosen) — Compile-time profile**: keeps zero-alloc,
preserves WCET, scales to any reasonable deployment size, and forces
each fleet operator to make a conscious sizing decision.

## Unresolved questions

- **Profile boundaries**: are the three profiles enough, or do we need
  four (`embedded` / `single-robot` / `edge-gateway` / `cloud-fleet`)?
  Defer until we have a real deployment.
- **Per-cap override**: can a user pin `MAX_TASKS = 1024` while leaving
  everything else at edge?  Adds cargo-feature combinatorial explosion.
  Not in this RFC; revisit if needed.
- **Fleet profile WCET**: at 4096 tasks, scheduler queue walks may
  exceed budgets.  L5 phase will measure; if regression, fleet profile
  may require a redesigned scheduler (B-tree of priority queues?).

## Future possibilities

- **Per-app caps**: today caps are kernel-wide.  Add per-app sub-caps
  (one app can't monopolise channels).  Would use the existing
  `CapKind`-typed budgets.
- **Hash-indexed lookup tables**: replace linear scans where regression
  data shows it matters.
- **Dynamic profile switch**: not for runtime, but a "profile selector"
  at boot (read CONFIG.INI, error if mismatched against compiled
  profile).  Helps catch profile-deployment mistakes.

## Prior art

- **Linux `CONFIG_*` kernel build options**: same shape (compile-time
  caps via Kconfig).  Linux has thousands of them; we want ~15.
- **seL4 verified kernel**: also uses compile-time caps for the same
  cert reason.  Their caps are even smaller (single-app focus).
- **Zephyr RTOS**: per-profile build configs (`prj.conf`).  Our cargo
  features are the same idea.
