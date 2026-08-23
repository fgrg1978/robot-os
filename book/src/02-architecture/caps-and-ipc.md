# Capability-typed IPC

> Authoritative spec: [RFC-0003](../appendix/rfcs.md).

Every interaction between user-space code and a kernel resource — IPC
channels, hardware MMIO, event ports, sensors, AI sessions — is reached
through a **capability handle**.

## Wire format vs typed form

There are two forms of the same handle:

| Form          | Type                       | Where seen                   |
|---------------|-----------------------------|------------------------------|
| Wire          | `CapHandle` (`u32`)         | Across syscall boundary, in topology files, in IPC payloads |
| Typed         | `Cap<T>`                    | Inside the kernel, inside skills written in Rust            |

Both encode identically; the typed form simply carries the kind tag at
compile time.

```rust
// Wire form:
let raw: CapHandle = CapHandle::pack(CapKind::Channel, CapPerms::RW, 0x42, 0x1234);

// Typed form (zero-cost wrapper around the same bits):
let typed: Cap<targets::Channel> = Cap::from_raw(raw);
```

## What kinds of caps exist?

15 kinds in `ABI v1`:

| Kind          | Purpose                                  |
|---------------|------------------------------------------|
| `Channel`     | Bidirectional IPC                        |
| `Shm`         | Shared memory region                     |
| `Port`        | Event delivery point                     |
| `Irq`         | Hardware IRQ binding                     |
| `MmioRegion`  | Direct hardware MMIO range               |
| `IoRing`      | io_ring submission/completion queues     |
| `Sensor`      | Sensor descriptor                        |
| `Gpio`        | Single GPIO pin                          |
| `I2c`         | I2C bus + address                        |
| `Pwm`         | PWM channel                              |
| `Motor`       | Motor channel                            |
| `File`        | File descriptor (FAT32 / tmpfs)          |
| `Socket`      | Network socket                           |
| `Task`        | Process / task handle                    |
| `AiSession`   | AI inference session                     |

## The cap-table

Each task has a private cap-table. A grant inserts a slot; a revoke
empties it. Granting bumps the slot's generation (modulo 256, skipping
0), so old handles to the freed slot become stale.

```rust
let mut table = CapTable::empty();
let chan: Cap<Channel> = table.grant(CapPerms::RW, channel_id).unwrap();
ipc::send(chan, &msg)?;
// ... later
table.revoke(chan);
ipc::send(chan, &msg)?;  // ❌ Errno::ECAPSTALE
```

## Forgery resistance

To forge a working handle an attacker must guess:

- The slot index (16 bits)
- The current generation (8 bits, but only ~half are live at any time)
- The kind tag (4 bits)
- All required permission bits (4 bits)

Total entropy ≈ 26 bits per attempt. Combined with the runtime's
per-task isolation (a task can only forge handles for its **own**
table), this makes random-guess forgery infeasible.

The Kani harness `cap_forge_impossible_empty_slot` proves no `u32` is
acceptable on an empty cap-table.

## See also

- [RFC-0003](../appendix/rfcs.md) — full spec
- `crates/ipc/src/cap.rs` — implementation
- `formal/tla/cap_table.tla` — TLA+ spec of generation invariants
- `formal/proofs/INVARIANTS.md` — invariant ledger (INV-1 .. INV-4)
