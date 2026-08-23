# RFC-0002: Modular Module Pattern (Constitutional)

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

Every replaceable subsystem in PHANES exposes a Rust `trait` (the API),
each implementation lives in its own file under `policies/` or
`impls/`, the active implementation is selected by a Cargo feature at
compile time, and a `runtime/registry.rs` scaffold is in place so a
future RFC can add dynamic loading without touching consumers. This is
the **constitutional rule** that all subsystem code follows.

## Motivation

Without this rule, the project drifts into either of two failure modes:

- **Monolith.** Schedulers, drivers, allocators, codecs all wired
  directly. Adding an alternative implementation requires touching
  consumers everywhere.
- **Plugin chaos.** Every subsystem invents its own dispatch
  mechanism. Onboarding a new contributor means learning N different
  conventions.

Adopting one canonical pattern from day one fixes both.

It also unlocks downstream goals:

- **Build-time multi-deployment.** A wheeled robot doesn't ship the
  drone flight controller; an automotive ECU doesn't ship the GGUF
  inference engine. Each deployment is a Cargo feature combination.
- **Verification.** Verify the trait API once; each impl is a separate
  proof obligation.
- **Cert.** Auditor reads one `trait Scheduler` definition and one
  `impl` per file, vs. spelunking through 5000 lines of intertwined
  scheduler logic.
- **Future runtime loading.** When the registry table goes from
  static to dynamic in a later phase, consumer code is unchanged.

## Detailed design

### File layout per replaceable subsystem

```
crates/<subsystem>/src/
├── api.rs               ← the trait + types every impl must satisfy
├── lib.rs               ← cfg-selects active impl; re-exports
├── runtime/
│   ├── mod.rs           ← runtime registry entry-point (Phase 4)
│   └── registry.rs      ← dynamic table (empty in Phase 1; populated later)
├── common/              ← shared primitives across impls
│   └── ...
└── impls/  (or policies/)
    ├── <impl_a>.rs      ← #[cfg(feature = "<sub>-<a>")]
    ├── <impl_b>.rs      ← #[cfg(feature = "<sub>-<b>")]
    └── ...
```

### The trait — the stable API

Each subsystem defines exactly one trait that every implementation
must satisfy. The trait is the contract; consumers depend only on it.

Worked example for the scheduler subsystem (see RFC-0004 for the
real definition):

```rust
// crates/sched/src/api.rs
pub trait Scheduler: Sync {
    fn pick_next(&self, cpu: usize, now: u64) -> Option<TaskIdx>;
    fn enqueue(&self, idx: TaskIdx, prio: Priority, class: SchedClass);
    fn dequeue(&self, idx: TaskIdx);
    fn tick(&self, cpu: usize, now: u64);
    fn admit(&self, params: &SchedParams) -> bool;
    fn stats(&self) -> SchedStats;
}
```

The trait is **stable**. Adding methods is a breaking change governed
by the same RFC process as adding a syscall.

### Cargo features — compile-time selection

Each subsystem owns its own `<sub>-<impl>` feature namespace. Exactly
one `<sub>-<impl>` may be active per build (the validator in
`build.rs` enforces this). A meta-feature like `sched-partition` may
opt-in to multiple impls for the partition combinator.

```toml
[features]
default = ["sched-priority"]

sched-priority   = []
sched-edf        = []
sched-rr         = []
sched-cfs        = []
sched-sporadic   = []
sched-partition  = ["sched-priority", "sched-edf", "sched-rr", "sched-cfs"]
```

The kernel's top-level `Cargo.toml` then composes deployment presets
out of subsystem features:

```toml
[features]
qemu     = ["sched-priority", "uart-ns16550a", "i2c-mock", ...]
vf2      = ["sched-partition", "uart-jh7110",  "i2c-jh7110", ...]
k1       = ["sched-partition", "uart-k1",      "i2c-k1",     ...]
deployment-car-ecu = ["sched-partition", "sched-edf", "sched-cfs",
                      "uart-jh7110", "tsn-net"]
```

### `lib.rs` dispatch — zero-cost static

```rust
#![no_std]

pub mod api;
pub mod common;
pub mod runtime;

#[cfg(feature = "sched-priority")]
pub mod policies { pub mod priority; }
#[cfg(feature = "sched-edf")]
pub mod policies { pub mod edf; }
// ...

#[cfg(all(feature = "sched-partition", not(feature = "sched-edf-only")))]
pub type ActiveScheduler = policies::partition::PartitionScheduler;
#[cfg(all(feature = "sched-priority", not(feature = "sched-edf"),
          not(feature = "sched-partition")))]
pub type ActiveScheduler = policies::priority::PriorityScheduler;
// ... one cfg per allowed combination

pub static SCHEDULER: ActiveScheduler = ActiveScheduler::new();
```

Consumers only ever call `crates::sched::SCHEDULER.pick_next(...)`.
The kernel doesn't know — and shouldn't — which implementation is
active. The compiler resolves the dispatch statically; zero overhead.

### `build.rs` validation — the rule of one

```rust
fn main() {
    let active: Vec<_> = ["priority", "edf", "rr", "cfs", "sporadic", "partition"]
        .iter()
        .filter(|f| std::env::var(format!("CARGO_FEATURE_SCHED_{}",
                                          f.to_uppercase())).is_ok())
        .collect();
    let partition = std::env::var("CARGO_FEATURE_SCHED_PARTITION").is_ok();
    if active.is_empty() {
        panic!("activate exactly one sched-* feature");
    }
    if !partition && active.len() > 1 {
        panic!("only one sched-* feature is allowed without sched-partition");
    }
}
```

A misconfigured build fails loudly at compile time. Never silently
picks one.

### `runtime/registry.rs` — placeholder for dynamic loading

```rust
// Phase 1: empty stub. Phase 4: real implementation.
pub static REGISTRY: spin::Mutex<Registry> = spin::Mutex::new(Registry::empty());

pub struct Registry {
    pub modules: heapless::Vec<&'static dyn Scheduler, 8>,
    pub active: AtomicUsize,
}

impl Registry {
    pub const fn empty() -> Self { ... }

    /// Phase 4: register a new impl loaded at runtime.
    /// Phase 1: never called.
    pub unsafe fn register(&mut self, sched: &'static dyn Scheduler) -> ModId { ... }

    /// Phase 4: hot-swap the active impl (quiesce, drain, switch).
    pub unsafe fn switch_active(&self, mod_id: ModId) { ... }
}
```

In Phase 1, `REGISTRY.modules` is empty and consumers use the static
`SCHEDULER` directly. In Phase 4 we add an ELF or WASM loader that
calls `REGISTRY.register()`; at that point the kernel can hot-swap.
Consumers don't change.

## Subsystems that follow this pattern

In priority order:

| Phase | Subsystem | Reason |
|-------|-----------|--------|
| 1 | `crates/sched/` | Constitutional (RFC-0004) |
| 1 | `crates/ipc/` | Constitutional (RFC-0003 caps) |
| 1 | `crates/mm/` (allocator) | Multiple impls per workload |
| 1 | `crates/drivers/uart/` | Per-SoC |
| 1 | `crates/drivers/i2c/` | Per-SoC |
| 1 | `crates/drivers/blk/` | VirtIO / MMC / NVMe / mock |
| 1 | `crates/drivers/net/` | VirtIO / MACB / RTL / mock |
| 2 | `crates/ml/` | Scalar / RVV / NPU / Hailo / Coral |
| 2 | `crates/crypto/` | Software / NPU-accel / HSM |
| 2 | `crates/codecs/` (DTB, OTA, GGUF) | Format versioning |
| 3 | `crates/fs/` | FAT32 / ext2-mini / squashfs |
| 3 | `crates/net/` (TCP) | Our impl / smoltcp / lwip |
| 3 | `crates/behavior/` | Subsumption / Behavior Trees / VLA |
| 4 | `crates/sched/runtime/` | Activate dynamic loading |

## Drawbacks

- **Discipline overhead.** Every new subsystem must define its trait
  before code lands. Slows down the first commit, pays back at the
  third.
- **Cfg-soup risk.** With many features, the cross-product of
  configurations explodes. Mitigated by the partition / combinator
  pattern (one feature implies a known set) and by the build.rs
  validator.
- **Trait stability constraints.** Adding a method to a trait is a
  breaking change for all consumers. We accept this and treat traits
  as ABI.

## Rationale and alternatives

**Alternative A — direct dispatch, no traits.** Each subsystem
exposes free functions. Simpler today, painful when adding the second
implementation. Rejected.

**Alternative B — single trait per crate, but no per-impl files.**
All scheduler policies in one file. Worked for prototypes. Doesn't
scale: cross-policy changes are easy to make accidentally. Rejected.

**Alternative C — proc-macro-based plugin system.** Define
`#[scheduler]` and let a macro wire things up. Magic; hard to debug
when something goes wrong; harder to read. Rejected.

**Alternative D (chosen) — explicit `cfg` + trait + per-impl file.**
Verbose but readable, debugger-friendly, idiomatic Rust, no magic.

## Prior art

- **Linux**: kernel modules use a similar pattern (driver registers
  itself via `module_init`), but with runtime loading from day one.
- **Hubris**: tasks defined in `app.toml`, code in per-task crates.
  Same philosophy; PHANES adapts it for subsystem-internal
  modularity rather than task-level.
- **Zephyr**: build-time `CONFIG_*` selects subsystems. Same shape,
  C-style `#ifdef` instead of Rust `cfg`.
- **Genode**: components are processes; `runtime` configuration in
  XML. Similar end goal, very different mechanism.

## Unresolved questions

- Should the trait API be `&self` (shareable, no interior mutability
  needed in the trait) or `&mut self` (allows for explicit
  mutation)? Working assumption: `&self` with interior mutability via
  spinlock; mirrors how other kernel subsystems do it.
- Naming: `policies/` for sched, `impls/` for everything else? Or
  uniform `impls/` everywhere? Working assumption: `impls/` uniform;
  `policies/` is a sched-specific synonym we accept for clarity.

## Future possibilities

- Phase 4: dynamic loading via ELF kernel modules (Linux-style) for
  in-kernel subsystems.
- Phase 4: WASM modules for user-level extensions (AI policies,
  behavior tree libraries).
- Phase 5: hot-swap with state migration across module versions
  (research-level).
