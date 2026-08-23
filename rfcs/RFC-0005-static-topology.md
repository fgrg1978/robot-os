# RFC-0005: Static Topology Format (CAPS.TOML + SCHED.TOML)

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

PHANES boots from two declarative configuration files in the boot
partition: `/fat/CAPS.TOML` (capability grants per task — RFC-0003)
and `/fat/SCHED.TOML` (scheduling classes and per-task scheduling
parameters — RFC-0004). The loader parses these at boot, populates
each task's cap-table, and admits tasks under the active scheduler's
admission control. The files are signed (RFC-0011) so a corrupted or
malicious topology cannot bypass the safety model.

## Motivation

A statically-declared topology is the simplest path to:

- **Cert auditability.** Auditors read TOML, not Rust code, to
  understand which task can do what.
- **Reproducible deployments.** Same kernel binary + different TOML =
  different deployment. Wheeled, drone, humanoid, ECU.
- **Verifiable safety claims.** A TLA+ model can ingest the TOML and
  prove FFI properties for the specific deployment, not just for the
  generic kernel.
- **No runtime-allocation surprises.** Every task and every cap is
  known at boot time; the kernel never `Box::new`s a task struct.

## Detailed design

### File location and format

```
/fat/CAPS.TOML       — capabilities per task
/fat/SCHED.TOML      — scheduling classes, parameters per task
/fat/CAPS.TOML.SIG   — Ed25519 signature (RFC-0011)
/fat/SCHED.TOML.SIG  — Ed25519 signature (RFC-0011)
```

TOML chosen for: human-readable, comment-friendly, no ambiguity in
parsing, mature Rust crates (`toml-rs`).

### `SCHED.TOML` schema

```toml
# Top-level scheduling classes — one block per class.
[class.safety_critical]
cpu_budget_min_pct  = 15        # guaranteed minimum per CPU per window
cpu_budget_max_pct  = 100       # cap (100 = unbounded)
policy              = "fifo"    # one of: fifo, edf, rr, cfs, sporadic
priority_range      = [0, 7]    # within the class
preemption          = "always"  # always | timer-only | never

[class.hard_rt]
cpu_budget_min_pct  = 30
cpu_budget_max_pct  = 50
policy              = "edf"
admission_control   = true      # reject tasks that violate Liu-Layland

[class.soft_rt]
cpu_budget_min_pct  = 20
cpu_budget_max_pct  = 40
policy              = "rr"
priority_range      = [8, 15]
time_slice_ms       = 10

[class.best_effort]
cpu_budget_min_pct  = 5
cpu_budget_max_pct  = 100
policy              = "cfs"
priority_range      = [16, 30]

[class.idle]
cpu_budget_min_pct  = 0
cpu_budget_max_pct  = 5
policy              = "sporadic"
priority_range      = [31, 31]

# Per-task scheduling parameters — one block per task.
[task.rt_motor]
class       = "safety_critical"
priority    = 4
pinned_cpu  = 0                 # affinity (optional)
stack_kib   = 16

[task.sensor_ahrs]
class       = "hard_rt"
period_us   = 10_000             # 100 Hz
deadline_us = 9_000              # finish in 9 ms
budget_us   = 2_000              # CBS reservation
pinned_cpu  = 1
stack_kib   = 32

[task.behavior]
class    = "soft_rt"
priority = 12
stack_kib = 32

[task.shell]
class    = "best_effort"
priority = 20
stack_kib = 16

[task.ota_recv]
class    = "soft_rt"
priority = 12
pinned_cpu = 2
stack_kib = 16

# Window for adaptive partitioning budget accounting.
[partition]
window_ms = 1
```

### `CAPS.TOML` schema

```toml
# Per-task capability grants. The loader matches task names against
# SCHED.TOML and populates each task's cap_table at boot.
[task.rt_motor]
caps = [
    { kind = "motor",       target = "motor.0",     perm = "rw" },
    { kind = "motor",       target = "motor.1",     perm = "rw" },
    { kind = "encoder",     target = "encoder.0",   perm = "r" },
    { kind = "encoder",     target = "encoder.1",   perm = "r" },
    { kind = "channel-sub", target = "/cmd/motor",  perm = "r" },
    { kind = "channel-pub", target = "/state/motor",perm = "w" },
]

[task.sensor_ahrs]
caps = [
    { kind = "i2c",         target = "bus.0/0x68",  perm = "rw" },  # IMU
    { kind = "i2c",         target = "bus.0/0x76",  perm = "rw" },  # Baro
    { kind = "channel-pub", target = "/sensors/imu",perm = "w" },
    { kind = "channel-pub", target = "/sensors/baro",perm = "w" },
]

[task.behavior]
caps = [
    { kind = "channel-sub", target = "/sensors/imu",  perm = "r" },
    { kind = "channel-sub", target = "/sensors/baro", perm = "r" },
    { kind = "channel-pub", target = "/cmd/motor",    perm = "w" },
    { kind = "service-call", target = "policy.run",   perm = "rw" },
]

[task.ota_recv]
caps = [
    { kind = "net-listen",  target = "tcp:8080",          perm = "rw" },
    { kind = "fs-write",    target = "/fat/KERN_*.TMP",   perm = "w" },
    { kind = "fs-write",    target = "/fat/BOOTMETA.*",   perm = "w" },
    { kind = "ota-commit",  target = "B",                 perm = "w" },
]
# Note: ota_recv has NO motor caps. A compromised OTA receiver
# cannot move the robot. This is the core safety invariant.

[task.shell]
caps = [
    { kind = "console-rw",  target = "uart.0",   perm = "rw" },
    { kind = "shell-cmd",   target = "*",        perm = "rw" },
]
```

### Boot sequence

```text
1. Boot ROM verifies TF-A signature                       (RFC-0011)
2. TF-A verifies U-Boot SPL signature                     (RFC-0011)
3. U-Boot SPL → U-Boot → kernel.bin (signature verified)  (RFC-0011)
4. kernel_main() runs:
     - Phase 1 (UART, traps)
     - Phase 2 (PMM, VMM, heap)
     - Phase 3 (interrupts)
     - Phase 6 (FAT32 mount)
5. *** Topology load (this RFC) ***
     a. Read /fat/SCHED.TOML and /fat/SCHED.TOML.SIG
     b. Verify Ed25519 signature against SECURE_BOOT_PUBKEY
     c. Parse TOML → in-memory class + task table
     d. Read /fat/CAPS.TOML and /fat/CAPS.TOML.SIG
     e. Verify signature
     f. Parse TOML → in-memory cap-table per task
     g. Run admission control on EDF tasks (check utilisation bound)
     h. If admission fails, refuse to boot (or fall back to recovery slot)
6. Spawn each declared task:
     - Allocate task struct + stack from PMM (sized by stack_kib)
     - Populate cap_table from parsed entries
     - Insert into appropriate scheduler class
     - Mark Ready
7. Enable timer interrupt
8. scheduler::start() — never returns
```

### Validation rules

The loader rejects (refuses to boot) if any of:

- TOML parses but is structurally invalid (unknown field, wrong type)
- A task references a class that doesn't exist
- A task references a cap kind that the kernel doesn't support
- Admission control fails (EDF utilisation > 1)
- A `pinned_cpu` exceeds online CPU count
- A `stack_kib` is below the minimum (4 KiB) or above the cap
  (256 KiB)
- A task name is duplicated
- The signature doesn't verify (RFC-0011)

On rejection, the kernel logs the reason to UART and falls back to
the immutable recovery slot (`KERN_R.BIN`, RFC-0011) which has its
own embedded minimal topology.

### Default deployments

We ship reference TOMLs for each robot type:

```
/fat/topology/
├── wheeled.caps.toml
├── wheeled.sched.toml
├── drone.caps.toml
├── drone.sched.toml
├── humanoid.caps.toml
├── humanoid.sched.toml
├── ackermann.caps.toml
├── ackermann.sched.toml
├── ecu.caps.toml          # automotive ECU template
└── ecu.sched.toml
```

Build-time tooling generates the appropriate pair into `/fat/CAPS.TOML`
and `/fat/SCHED.TOML` based on the active Cargo feature.

### Tooling

- `phctl topology validate` — parse and admission-check a TOML pair
  off-target (host-side).
- `phctl topology diff` — compare two topologies for human review.
- `phctl topology graph` — emit a Graphviz dot of the cap-grant
  graph (which task talks to whom).

### Live updates

Static topology means no runtime grants in v0.1. Live updates are
deferred to a future RFC (Phase 3+) that will define a controlled
mechanism for OTA-delivered topology updates with version+rollback.

## Drawbacks

- **All-or-nothing.** A single typo in TOML means the kernel won't
  boot. Mitigated by `phctl topology validate` running in CI on every
  change.
- **Verbose.** A 30-task deployment yields a few hundred lines of
  TOML. Acceptable: it's the source of truth.
- **No runtime adaptation.** A task that's idle still holds its
  budget allocation. Future work: dynamic class membership.

## Rationale and alternatives

**Alternative A — JSON.** Less human-friendly; no comments. Rejected.

**Alternative B — Hubris-style: compile into binary, no runtime
parse.** Faster boot, but every change requires rebuild. We want
auditors and customers to read the deployment without recompiling.
Rejected.

**Alternative C — KConfig / Kbuild.** Linux's mechanism. Heavy, C-
oriented, and our build system is `cargo`. Rejected.

**Alternative D (chosen) — TOML + signed file + CI validator.**
Human-readable, version-controllable, signable.

## Prior art

- **Hubris** `app.toml` — same philosophy: declarative deployment
  config in TOML, parsed at build time. We borrow heavily but parse at
  boot for cert traceability.
- **OpenStack Nova flavors / compute config.** Same idea, very
  different scale.
- **Linux systemd unit files.** Per-service declarative config.
  Closer to our SCHED.TOML model.
- **AUTOSAR ARXML.** XML-heavy declarative deployment for automotive.
  We pick TOML over XML for readability.

## Unresolved questions

- **TOML schema versioning.** Do we embed a `version = "0.1"` field
  in the file? Working assumption: yes, and the loader rejects
  unknown major versions.
- **Heredoc-style multi-line caps.** A task with 30 caps gets
  visually noisy. Working assumption: accept it; tooling generates
  the file.
- **Wildcards in target.** `target = "/fat/KERN_*.TMP"` — how
  permissive is the glob? Working assumption: shell-style globs
  limited to `*` and `?`, no recursion.
- **Conflict between SCHED and CAPS.** What if a task is in
  CAPS.TOML but not SCHED.TOML, or vice versa? Working assumption:
  both must declare the same task set. Loader fails on mismatch.

## Future possibilities

- Phase 3: signed runtime topology updates (delta TOML pushed via
  OTA).
- Phase 4: topology generators that produce ARXML for AUTOSAR
  interop.
- Phase 4: visual topology editor (web UI) that emits validated
  TOML.
- Phase 4: TLA+ ingestion: the same TOML feeds the model checker.
