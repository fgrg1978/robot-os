# Static topology

> Authoritative spec: [RFC-0005](../appendix/rfcs.md).  
> Implementation: `crates/topology/` (W2) + `default_minimal()` (W3).

PHANES does not discover capabilities at runtime. Every task's caps,
every channel between tasks, and every scheduler partition is
declared in two signed TOML files loaded at boot:

- **`CAPS.TOML`** — who has what (capability assignment)
- **`SCHED.TOML`** — who runs how (scheduler partition + policy)

Both are Ed25519-signed against an OTP-anchored key (RFC-0011); a
broken signature halts boot before any user task is spawned.

## Why static?

- **Cert auditors** prove *what the system can do* by reading the
  TOML — no need to trace every code path. The set of capabilities
  is enumerable and finite.
- **Safety**: an attacker who compromises one task can only do what
  that task's caps already allowed. There is no
  `capability::create_arbitrary()` API.
- **Ergonomics**: declaratively specifying the topology is far
  clearer than constructor-spaghetti scattered across init code.

## Example skeleton

```toml
# SCHED.TOML
[class.safety_critical]
cpu_budget_min_pct  = 20
cpu_budget_max_pct  = 100
policy              = "fifo"
priority_range      = [0, 7]
preemption          = "always"

[class.hard_rt]
cpu_budget_min_pct  = 30
cpu_budget_max_pct  = 50
policy              = "edf"
admission_control   = true

[class.best_effort]
cpu_budget_min_pct  = 20
cpu_budget_max_pct  = 100
policy              = "cfs"

[sched]
partition_window_us = 10000   # 10 ms window
```

```toml
# CAPS.TOML
[task.rt_motor]
class    = "hard_rt"
priority = 5
caps = [
    { kind = "motor",       target = "motor.0",     perm = "rw" },
    { kind = "motor",       target = "motor.1",     perm = "rw" },
    { kind = "encoder",     target = "encoder.0",   perm = "r" },
    { kind = "channel-sub", target = "/cmd/motor",  perm = "r" },
    { kind = "channel-pub", target = "/state/motor",perm = "w" },
]

[task.safety_supervisor]
class    = "safety_critical"
priority = 0
caps = [
    { kind = "channel-sub", target = "/sensors/imu", perm = "r" },
    { kind = "gpio",        target = "estop_relay",  perm = "rw" },
]
```

Each task's caps populate its per-task `CapTable` (RFC-0003) at spawn
time. The kernel then refuses any IPC the task tries that wasn't
declared.

## The parser

`crates/topology/src/parser.rs` is a hand-rolled minimal TOML subset
parser: sections, comments, scalars, ranges, and arrays-of-inline-
tables. No allocator, no `unsafe`. ~570 lines. Bounded at compile
time: max 8 classes × 64 tasks × 1024 caps total per topology.

Why not `toml-rs`? Two reasons:
1. The kernel-side crate is `no_std` with no allocator.
2. Cert auditors prefer minimum-surface-area dependencies — a
   hand-rolled parser of exactly the grammar we need is easier to
   review than a general-purpose library.

Tested with the verbatim example from RFC-0005 plus 23 edge cases
(`crates/topology-tests`, 24 tests).

## Signature verification

`crates/topology/src/verify.rs` wraps the existing
`robot_os_crypto::ed25519::sig_verify`. Each TOML file ships with a
`.SIG` sidecar containing the 64-byte signature. The trusted public
key lives in OTP / eFuse on production hardware; a compile-time
constant for QEMU / dev.

```rust
verify_signature(&toml_bytes, &sig_bytes, &TRUSTED_PUBKEY)?;
parse_caps(&toml_bytes, &mut topology)?;
topology.admission_check()?;
topology::init(topology)?;
```

Any failure halts the kernel before spawning the first user-space
task. The TLA+ spec `formal/tla/topology_load.tla` proves this
end-to-end (`SpawnImpliesLoaded` invariant; 55 states verified).

## Default minimal topology

For QEMU / dev builds without a signed TOML on disk, the kernel
ships a built-in fallback in `crates/topology/src/builder.rs`:

```rust
pub fn default_minimal() -> Topology<'static>
```

It contains the five RFC-0004 scheduler classes (budgets summing to
exactly 100 %) plus two seed tasks (`supervisor`, `brain_link`). The
boot path falls back to this when no signed TOML is present:

```rust
// In kernel/src/main.rs
match topology::init(topology::default_minimal()) {
    Ok(()) => kprintln!("[TOPO] Topology installed: {} classes, {} tasks", ...),
    Err(e) => panic!("topology init failed: {:?}", e),
}
```

You see exactly this line at boot:

```
[TOPO] Topology installed: 5 classes, 2 tasks
```

## Memory budget

Worst case (production deployment): 8 classes + 64 tasks + 1024 caps
total + 64 KiB source-text buffer ≈ **~96 KiB static BSS**.

`MaybeStr<'a>` borrows from the source buffer — no allocator needed.
The fixed-pool `caps_pool` lets a single task hold up to 256 caps
while the pool itself bounds the total.

## See also

- [RFC-0005](../appendix/rfcs.md) — full spec
- [Capability-typed IPC](./caps-and-ipc.md) — how caps are used at
  runtime
- `crates/topology/` — implementation
- `crates/topology-tests/` — 24 host tests
- `formal/tla/topology_load.tla` — TLA+ spec (TLC verified)
