# RFC-0003: Capability-Typed IPC (`Cap<T>`) — Constitutional

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

Every kernel resource (motor channel, I2C bus, network socket,
filesystem root, OTA slot, signal target, …) is gated by a typed
capability `Cap<T>`. Capabilities are unforgeable references issued
only by the kernel. A task can only invoke an operation on a resource
if it holds the matching `Cap<T>` in its capability table; otherwise
the call fails to type-check at compile time (where the type is
known) or fails the kernel's lookup at runtime (where caps are
transferred between tasks).

This is the **constitutional rule** for all kernel-resource access,
replacing the current POSIX-style integer handles (`fd: i32`, `tid:
u32`, `idx: usize`).

## Motivation

The current design uses ambient authority: any task that can call a
syscall can target any resource by guessing or knowing the integer
handle. Concretely:

- A bug in `behavior` task can call `motor_cmd_publish(0, 0)` and
  stop motors that don't belong to it.
- A compromised `ota-recv` task can `pipe_write` into another task's
  pipe by guessing the index.
- A future userspace driver, if compromised, has access to *all*
  hardware that the syscall layer permits — not just its own.

Capabilities mechanically enforce: **no cap, no access**. Authority
is held in objects, not in syscall numbers. Consequences:

- **AI model isolation**. A loaded VLA model receives explicit caps
  to specific cameras and motors. It cannot reach GPS, IMU, or the
  filesystem unless granted.
- **Driver isolation (AQ3 prep)**. When drivers move to userspace,
  each gets exactly the caps it needs.
- **ISO 26262 ASIL-D Freedom-From-Interference**. We can demonstrate
  FFI mechanically: a task with cap-set `S` cannot interfere with a
  resource not in `S`. Cert auditor signs.
- **Verification**. Type-system invariant: "no path uses a resource
  without a matching cap" is checked by `rustc`, not audited by
  humans.
- **Scales to fleet**. Caps cross network in Phase 2 (KeyKOS-style
  remote caps).

This RFC defines the mechanism. RFC-0005 defines the topology format
that grants caps at boot. RFC-0011 wires caps to secure boot.

## Detailed design

### The `Cap<T>` type

```rust
// crates/ipc/src/cap.rs

/// An unforgeable reference to a resource of type-tag `T`.
///
/// Construction is restricted to `Cap::__from_kernel()` which is
/// `pub(crate)` and only callable by the kernel topology loader.
/// Userspace code receives `Cap<T>` values via `cap_recv()` (RFC-0003.5)
/// or as arguments from a parent task; it cannot fabricate them.
#[repr(transparent)]
pub struct Cap<T> {
    handle: u32,                // index into the task's cap-table
    _phantom: PhantomData<T>,
}

impl<T> Cap<T> {
    /// SAFETY: callable only by kernel boot / topology loader.
    pub(crate) unsafe fn __from_kernel(handle: u32) -> Self {
        Self { handle, _phantom: PhantomData }
    }

    pub fn handle(&self) -> u32 { self.handle }
}

// Cap<T> is Copy because it's just an index — but its semantics are
// "reference to resource", so multiple holders is fine. Revocation
// is per-task: zeroing the cap-table slot invalidates all holders.
impl<T> Copy for Cap<T> {}
impl<T> Clone for Cap<T> { fn clone(&self) -> Self { *self } }
```

### Type tags — what the cap is *for*

Tags are zero-sized markers that distinguish cap kinds at compile
time. They live in their respective subsystem crates:

```rust
// crates/robot/src/lib.rs
pub enum MotorChL {}        // left motor
pub enum MotorChR {}
pub enum EncoderL {}
pub enum EncoderR {}
pub enum PayloadSpray {}

// crates/drivers/src/i2c/api.rs
pub enum I2cBus0 {}
pub enum I2cBus1 {}

// crates/net/src/lib.rs
pub enum NetEpoch {}        // permission to call socket_create
pub enum TcpSocket<const PORT: u16> {}

// crates/fs/src/lib.rs
pub enum FsRoot {}          // call vfs_open from this root
pub enum FileRO {}
pub enum FileRW {}

// crates/ota/src/lib.rs
pub enum OtaSlotA {}
pub enum OtaSlotB {}
pub enum OtaCommit {}
```

A function declares the caps it consumes:

```rust
pub fn motor_cmd(cap: Cap<MotorChL>, speed: i16) -> Result<(), CapErr>;
pub fn i2c_read(cap: Cap<I2cBus0>, addr: u8, reg: u8, out: &mut [u8])
    -> Result<(), CapErr>;
pub fn vfs_open(cap: Cap<FsRoot>, path: &[u8], flags: u32)
    -> Result<Cap<FileRW>, CapErr>;
```

Without `Cap<MotorChL>`, calling `motor_cmd` is a type error. End of
story.

### The cap-table — kernel-side

Each task has a small fixed-size array of cap-table entries. The
cap's `handle` is an index into this table.

```rust
// crates/sched/src/task.rs
pub struct Task {
    // ... existing fields ...
    pub cap_table: [CapEntry; CAP_TABLE_SIZE],   // CAP_TABLE_SIZE = 64
}

#[repr(C)]
pub struct CapEntry {
    /// 0 = empty slot. Otherwise: a bitfield encoding (kind, target_id,
    /// permissions, generation) — see below.
    pub bits: u64,
}

const CAP_TABLE_SIZE: usize = 64;
```

The 64-bit `bits` encoding (working proposal):

```text
 63                                                                      0
 ┌───────┬──────────────┬────────────────────────────┬─────────┬─────────┐
 │ kind  │ generation   │       target id            │  perm   │   tag   │
 │ 8 bit │   16 bit     │        32 bit              │  4 bit  │  4 bit  │
 └───────┴──────────────┴────────────────────────────┴─────────┴─────────┘
```

- **kind** — coarse category (motor, i2c, file, socket, signal, …)
- **generation** — monotonic counter; an old cap that was revoked has
  stale generation, kernel rejects on lookup
- **target id** — concrete resource id within the kind (e.g. motor
  number, file inode)
- **perm** — read / write / read+write / exec / commit
- **tag** — reserved for future use (versioning, hash prefix)

When a task invokes an operation, the kernel:
1. Looks up `cap_table[cap.handle]`.
2. Validates `kind` matches the operation.
3. Validates `generation` is current.
4. Validates `perm` permits the operation.
5. Dispatches to the resource using `target_id`.

This lookup is **one indirect array access + bit comparisons** —
identical cost to today's fd-table lookup. Hot-path is unchanged.

### Caps are granted at boot, not allocated dynamically

The default model is **static topology** (RFC-0005): the boot loader
reads `CAPS.TOML` and populates each task's `cap_table` before the
task ever runs. This is the cert-friendly mode.

For dynamic cases (a guest task that connects later, or model OTA),
the kernel exposes a controlled grant mechanism:

```rust
/// Grant a capability from the issuer's table to the target task.
/// Requires the issuer to hold a `CapMaster<T>` for the target type.
pub fn cap_grant<T>(
    issuer_cap: Cap<CapMaster<T>>,
    target_tid: u32,
    cap_to_grant: Cap<T>,
) -> Result<(), CapErr>;

/// Revoke. Bumps the generation counter on the issuer side, leaving
/// stale copies of the cap dangling (kernel will reject them).
pub fn cap_revoke<T>(
    issuer_cap: Cap<CapMaster<T>>,
    target_tid: u32,
    cap_handle: u32,
) -> Result<(), CapErr>;
```

`CapMaster<T>` is itself a cap; a task can issue caps for `T` only
if it holds a `Cap<CapMaster<T>>`. This makes the *issuer* itself
permission-checked.

### Caps cross processes (Phase 2)

A cap can be transferred via IPC. The receiver gets a fresh handle in
its own cap-table that points to the same underlying resource:

```rust
pub fn cap_send<T>(
    channel: Cap<Channel<CapMessage<T>>>,
    cap: Cap<T>,
) -> Result<(), CapErr>;

pub fn cap_recv<T>(
    channel: Cap<Channel<CapMessage<T>>>,
) -> Result<Cap<T>, CapErr>;
```

The kernel transfers the cap-table entry, allocating a free slot in
the receiver. Generation is preserved.

### Caps cross machines (Phase 4)

For fleet protocol, caps tunnel over an authenticated network channel
(see `secure_channel` + RFC-0011). The wire format is:

```
┌─ kind (1B)
├─ generation (2B)
├─ target id (4B)
├─ perm (1B)
├─ remote tid (4B)
├─ remote nonce (8B)
└─ HMAC(key, all of the above)
```

Receiver verifies HMAC + nonce monotonicity (RFC-0011) before
installing the cap.

### Migration plan from today's IPC

The current `crates/ipc/src/` exposes:
- `pipe_*` (idx-based pipes)
- `signal_*` (tid-based signals)
- `channel_*` (idx-based typed channels)
- `port_*` (port-based)
- `service_*` (service registry)
- `fast_ipc_*` (synchronous fastpath)
- `lease_*` (transient borrow)
- `zerocopy_*` (refcounted buffers)
- `shm_*` (shared memory)
- `io_ring_*` (async batched)
- `irq_bind_*` (userspace IRQ)
- `rpc_*` (RPC layer)
- `trace_*` (event ring)

Migration order (phase 1):

1. **`pipe_*` → `Cap<PipeRead> / Cap<PipeWrite>`**. Replace idx args.
2. **`signal_*` → `Cap<SignalTo<T>>`**. Replace tid args.
3. **`channel_*` → `Cap<ChannelPub<T>> / Cap<ChannelSub<T>>`**.
   Already typed; just gate authority.
4. **`socket_*` → `Cap<NetEpoch>` to acquire, returns
   `Cap<TcpSocket<P>>`**.
5. **`fs::vfs_open` → `Cap<FsRoot>` argument; returns
   `Cap<FileRO> / Cap<FileRW>`**.
6. **`fast_ipc_call(server_tid, msg)` →
   `fast_ipc_call(Cap<Service<S>>, msg)`**. Hot path identical;
   cap-table lookup replaces tid lookup. **Same cycle count.**
7. **`lease_*` → `LeaseCap<T>`**. TTL is intrinsic.
8. **`zerocopy_*` → `Cap<ZeroCopyPool>` to acquire;
   `Cap<Buffer<T>>` per buffer**. Refcount unchanged.
9. **`io_ring_*` → ring is `Cap<IoRing>`**. Lock-free MPSC unchanged.

For each migrated API: keep old API behind `#[deprecated]` for one
release; new API in `_v2.rs` modules; drop old API at v0.2.0.

### Performance

The capability check is a single bounds-check + bitfield compare:

```text
fn dispatch<T>(cap: Cap<T>, op: Op) -> Result<R, CapErr> {
    let entry = current_task().cap_table[cap.handle as usize];   // 1 load
    if entry.kind() != T::KIND          { return Err(NoCap); }   // 1 cmp
    if entry.generation() != T::CURRENT { return Err(NoCap); }   // 1 cmp
    if !entry.perm().allows(op)         { return Err(NoCap); }   // 1 cmp
    do_op(entry.target_id(), op)
}
```

**Cost: ~5 cycles + 1 memory load** on RV64 cold cache. With cache
hot (typical), 2–3 cycles. **No more expensive than the current
`if fd >= 0 && fd < MAX_SOCKETS` check.**

For comparison: seL4 capability lookup on the fastpath is ~10
cycles ARM Cortex-A. This proposal is in the same league.

### Boot-time setup

The topology loader (RFC-0005) populates each task's `cap_table` by
parsing `CAPS.TOML` and invoking `Cap::__from_kernel(handle)` for
each entry. Once the loader returns, capabilities are fixed for that
task's lifetime (in static-topology mode).

Example `CAPS.TOML` (excerpt):

```toml
[task.rt_motor]
caps = [
    { kind = "motor", target = "motor.0", perm = "rw" },
    { kind = "motor", target = "motor.1", perm = "rw" },
    { kind = "encoder", target = "encoder.0", perm = "r" },
    { kind = "encoder", target = "encoder.1", perm = "r" },
    { kind = "channel-sub", target = "/cmd/motor", perm = "r" },
]

[task.behavior]
caps = [
    { kind = "channel-pub", target = "/cmd/motor", perm = "w" },
    { kind = "channel-sub", target = "/sensors/imu", perm = "r" },
    { kind = "service-call", target = "policy.run", perm = "rw" },
]

[task.ota_recv]
caps = [
    { kind = "net-listen", target = "tcp:8080", perm = "rw" },
    { kind = "fs", target = "/fat/KERN_*.TMP", perm = "w" },
    { kind = "ota-commit", target = "B", perm = "w" },
]
# Note: ota_recv has NO motor caps. A compromised OTA receiver
# cannot move the robot. This is the core safety invariant.
```

## Drawbacks

- **Migration cost.** ~3–6 months engineering to migrate the
  existing `crates/ipc/`. Mitigated by deprecation period.
- **Cognitive overhead.** Every new API needs to declare its caps.
  Mitigated by a small set of canonical caps; copy-paste from
  examples.
- **Discipline at the boundary.** If a future API forgets to take a
  cap, ambient authority returns. Mitigated by a clippy lint
  (`disallowed_methods`) blocking calls into uncapped subsystems.
- **Wire-format complexity for cross-machine caps.** Real, but the
  wire format is small and HMAC-bounded (RFC-0011).

## Rationale and alternatives

**Alternative A — keep POSIX-style.** Already shown insufficient:
audit found bypasses (`sys_i2c_write` had no cap_check, etc.),
ASIL-D FFI argument is hand-waving without mechanical enforcement.

**Alternative B — Linux capabilities (`cap_t`).** Process-level
flags. Coarse-grained ("CAP_NET_ADMIN") rather than object-level
("Cap<TcpSocket<8080>>"). Doesn't isolate per-resource. Rejected.

**Alternative C — full seL4 capability model.** Verified, but heavy
(every kernel object — frame, page-table, endpoint, untyped — is a
cap). The CapDL loader is non-trivial. Verification programme is
years. We adopt seL4's *philosophy* via Rust types instead of
verified C. Pragmatic middle.

**Alternative D (chosen) — Hubris-style typed caps + Rust
type-system.** Compile-time check for the static cases, runtime
table for the dynamic cases. Best ratio of
correctness-to-implementation-cost.

## Prior art

- **seL4** (Klein et al., SOSP 2009). The reference for verified
  caps. We aim for the same property at a different point in the
  cost / verification curve.
- **Hubris** (Cliff Biffle, Oxide, 2021+). Static topology + typed
  message handles + per-task isolation. PHANES adopts the same
  shape and adds the dynamic grant path needed for AI model OTA.
- **Genode** (Feske et al., 2008+). Capability-based component OS.
  Provides a working RPC layer over caps that we can study.
- **EROS / KeyKOS** (Shapiro, Hardy, 1990s+). Earlier
  capability-based systems; foundational papers on capability
  semantics.
- **Nooks / Vino**. Driver isolation work that motivates the AQ3
  goal.

## Unresolved questions

- **CAP_TABLE_SIZE** — 64 enough? 128? Trade memory vs. flexibility.
  Working assumption: 64; revisit if we hit it during impl.
- **Generation field width** — 16 bit allows 65535 revocations
  per resource; tight for very dynamic systems. Working assumption:
  16 bits + on-rollover-make-cap-permanently-invalid.
- **Cap revocation semantics** — should `cap_revoke` notify holders?
  The cleanest model is silent: holders' calls just start failing.
  Working assumption: silent.
- **Phantom data and `Send` / `Sync`** — when a cap is sent across
  threads, what trait bounds apply? Working assumption: `Cap<T>:
  Copy + Send + Sync` always; the underlying resource handles
  thread safety.
- **Naming** — `Cap<T>` or `Handle<T>` or `Object<T>`? Choosing
  `Cap` because it signals the security property explicitly.

## Future possibilities

- **Phase 2:** caps as IPC arguments (`cap_send` / `cap_recv` over
  channels and pipes).
- **Phase 4:** caps over network (fleet protocol). Wire format
  defined in RFC-0011; implementation in a later RFC.
- **Phase 4:** persistent caps (caps that survive reboot, stored
  signed in NVRAM). Useful for "this robot is paired to this fleet
  brain forever".
- **Phase 5:** verified cap-table operations via Kani / Loom: prove
  no path produces a forged cap.
