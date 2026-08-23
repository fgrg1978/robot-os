# Item-2 — kernel cross-arch migration plan

Status: **plan** — concrete steps for the multi-session refactor
that lets the PHANES kernel build against `arch-aarch64` or
`arch-x86_64` instead of the riscv-only `robot_os_arch`.

## Today

```
┌────────┐    ┌──────────────────┐
│ kernel │ ─→ │ robot_os_arch    │ (riscv-only:
│        │    │  - mmu (Sv39)    │  csr, pmp,
│        │    │  - csr macros    │  sbi, trap)
│        │    │  - pmp           │
│        │    │  - sbi           │
│        │    │  - trap          │
│        │    └──────────────────┘
└────────┘
```

11 source files import `robot_os_arch::`, 36 direct call-sites
in `kernel/src/main.rs` alone (`cpu::hart_id()`, `csr::*`,
`trap::*` etc). The Phase 2 work landed `arch-api` (trait
surface) + `arch-aarch64` + `arch-x86_64` impls — but the kernel
doesn't compile against them yet.

## Target

```
                    cfg(target_arch = …)
                          │
                 ┌────────┴───────┐
┌────────┐  ┌───────────────────────────────┐
│ kernel │→│ robot_os_arch (FACADE)         │
│        │  │  - re-exports current ISA crate│
│        │  └───────────────────────────────┘
└────────┘   ↓ riscv64    ↓ aarch64       ↓ x86_64
       arch-riscv64   arch-aarch64    arch-x86_64
            ↑              ↑                ↑
            └──── arch-api (Cpu/Interrupts/Mmu/Boot/Vector) ───┘
```

## Migration stages

Each stage compiles + passes the riscv kernel boot test
(`make qemu-full-smp`) at the end, so any stage can be
committed without breaking the production path.

### Stage 0 — current state (committed today)

- `arch-api` defines `Cpu`/`Interrupts`/`Mmu`/`Boot`/`Vector` traits.
- `arch-aarch64` + `arch-x86_64` implement those traits + 14/18
  helper modules each (boot, mmu_setup, gic/apic, timer/tsc, …).
- `crates/arch` is the riscv impl; *also* exposes traits via
  `robot_os_arch::api_impl::Riscv64` per B0.2.

### Stage 1 — rename `crates/arch` → `crates/arch-riscv64`

Pure rename, no API change. Touches:
- `crates/arch/Cargo.toml`: `name = "robot_os_arch_riscv64"`
- `Cargo.toml` workspace member path: `crates/arch` → `crates/arch-riscv64`
- 11 source files: `use robot_os_arch::…` → `use robot_os_arch_riscv64::…`
- Workspace deps: `robot_os_arch = …` → `robot_os_arch_riscv64 = …`

Verifiable: `make build` clean, `make qemu-full-smp` boots.
Cost: ~1 session of mechanical search-and-replace + verify.

### Stage 2 — introduce `crates/arch` as a thin facade

New crate, zero functional code:

```rust
// crates/arch/src/lib.rs
#![no_std]

#[cfg(target_arch = "riscv64")]
pub use robot_os_arch_riscv64::*;

#[cfg(target_arch = "aarch64")]
pub use robot_os_arch_aarch64::*;

#[cfg(target_arch = "x86_64")]
pub use robot_os_arch_x86_64::*;
```

Plus a `Cargo.toml` that lists all three as `cfg`-gated deps.

Revert Stage 1's caller renames: code goes back to
`use robot_os_arch::…`, but now points at the facade. On riscv64
behaviour is identical (`*` re-export). On other targets the
facade re-exports a different module set.

Verifiable: same as Stage 1 (riscv path unchanged).
Cost: ~1 session.

### Stage 3 — identify cross-arch-portable vs ISA-only call sites

Walk every `robot_os_arch::…` call site in the kernel and
classify into three buckets:

- **A. Portable, works through the facade today.** The riscv64
  impl exposes a symbol with this exact path; aarch64 + x86_64
  need a same-named symbol with semantically-equivalent
  behaviour, after which the facade's wildcard re-export
  resolves it for every target with zero call-site change.
- **B. RISC-V only.** Concept doesn't exist outside RISC-V (or
  is named/structured radically differently). Wrap call sites
  in `cfg(target_arch = "riscv64")` so the aarch64 / x86_64
  build skips them.
- **C. Portable name, per-ISA semantics.** Same operation
  conceptually but the impl needs to dispatch by ISA. Goes
  through `arch-api` trait + per-ISA `api_impl::*Mmu`/`*Cpu`/
  `*Vector`-style adapter.

#### Audit table — `cargo grep robot_os_arch::` 2026-05-17

| Symbol                    | N calls | Bucket | Notes |
|---------------------------|--------:|:------:|-------|
| `mmu::PAGE_SIZE`          | 12      | A      | All 3 ISAs use 4 KiB. ✅ on aarch64+x86_64 since S3.b1. |
| `mmu::PAGE_SHIFT`         | (via b1)| A      | Added on aarch64+x86_64 in S3.b1. |
| `mmu::page_align_{up,dn}` | (via b1)| A      | Same. |
| `mmu::is_page_aligned`    | (via b1)| A      | Same. |
| `mmu::PteFlags`           |  3      | C      | Bit layout differs; route via `arch-api::Mmu::encode_pte` + `PagePerms`. |
| `mmu::Pte` / `make_satp`  |  N      | B      | Sv39-specific structure / satp register. |
| `mmu` (module access)     |  6      | A/B    | Mixed — split per call. |
| `cpu::hart_id`            |  7      | A      | aarch64=MPIDR.Aff0; x86_64=APIC ID; already in `arch-api::Cpu`. |
| `cpu::wfi`                |  6      | A      | aarch64=WFI; x86_64=HLT; same trait method, different asm. |
| `csr::write_sstatus`      |  7      | B      | `sstatus` is a RISC-V CSR. |
| `csr::read_sstatus`       |  5      | B      | Same. |
| `csr::SSTATUS_SIE`        |  5      | B      | Same. |
| `csr::read_satp`          |  2      | B      | `satp` is RISC-V Sv*. |
| `csr` (module)            |  4      | B      | Whole module gated. |
| `sbi::shutdown`           |  4      | B      | SBI is RISC-V M-mode. aarch64/x86_64 use PSCI / ACPI. |
| `sbi::reboot`             |  3      | B      | Same. |
| `sbi::hart_start`         |  1      | B      | Same; aarch64 uses PSCI CPU_ON, x86_64 uses INIT-SIPI-SIPI. |
| `sbi` (module)            |  1      | B      | Whole module gated. |
| `pmp` (module)            |  3      | B      | PMP is RISC-V M-mode-only. aarch64=MPU, x86_64=segment limits — different abstractions. |
| `rvv::dot_f32_scalar`     |  4      | A      | Rename to `vector::dot_f32_scalar`, mirror in all 3 ISAs. |
| `rvv::dot_f32_rvv`        |  4      | B      | RVV-specific impl. Future: dispatch via `arch-api::Vector::dot_f32`. |
| `trap` (module)           |  1      | C      | Per-ISA TrapFrame layout (see below). |

**Migration order (least → most invasive):**
1. **S3.b1** ✅ — add the 4 portable page helpers on aarch64+x86_64.
2. **S3.b2** ← this document — classify everything.
3. **S3.b3** — `cpu::hart_id` + `cpu::wfi` parity on aarch64+x86_64 (low-hanging A bucket; aarch64 has these in `arch-aarch64::cpu` already, x86_64 needs the wrappers).
4. **S3.b4** — `vector::dot_f32_scalar` rename + mirror, then gate `dot_f32_rvv` callers.
5. **S3.b5** — `cfg`-gate B-bucket call sites in the kernel: csr, sbi, pmp, satp, Pte/make_satp.
6. **S3.b6** — `mmu::encode_pte` via `arch-api::Mmu` trait + per-ISA `PagePerms` (replaces direct `PteFlags`).
7. **S3.b7** — TrapFrame design — see below.

#### TrapFrame — the hardest call-site

The kernel passes `&mut TrapFrame` to its trap handler.
Options:

- **(a) Generic kernel:** `fn trap_handler<T: TrapFrame>(frame: &mut T)`.
  Forces every trap-touching call site to be generic. Code
  bloat per ISA (monomorphisation) and reads poorly.
- **(b) Common-subset struct:** A struct holding only the
  fields all three ISAs have (PC, SP, fault-cause, syscall
  number). Per-ISA registers (e.g. RISC-V's gp/tp, aarch64's
  x29/x30, x86_64's RBP/RSP) get saved/restored by the entry
  asm but aren't visible to the shared handler. Loses fidelity
  for things like the procfs trap dump.
- **(c) Per-arch entry modules:** Each ISA has its own
  `kernel::entry::riscv64::trap_entry()` that builds the
  ISA-native TrapFrame and calls into shared handler logic
  via small ISA-specific shim functions
  (`syscall_arg_n()`, `pc()`, `set_return_value()`). The
  shared logic never sees TrapFrame directly. **Recommended.**

(c) is the "shared logic, ISA-aware boundary" pattern used by
seL4, Hubris, and Linux's `pt_regs` accessor functions — and
it sidesteps the monomorphisation tax of (a).

Cost: ~1-2 sessions per remaining batch.

### Stage 4 — kernel build matrix expansion

Add aarch64 + x86_64 build targets to the CI matrix. Initially
they will fail (lots of ISA-only code paths the kernel still
calls directly). Each failure is a follow-up commit that either
routes through arch-api or gates behind `cfg`.

Cost: ~3-5 sessions of incremental wiring.

### Stage 5 — actually boot the kernel on aarch64 / x86_64

By this point the kernel compiles for all three targets but
only riscv64 boots. Each non-riscv boot requires:
- arch-aarch64: hook the existing boot helpers (drop_to_el1 +
  mmu_setup + eret_to_el0) into the kernel boot path
- arch-x86_64: PVH note (blocked on B2.boot.real; see commit
  message of 5c3a0be for the QEMU PVH detection issue)

Cost: depends on the unblockers, ≥2 sessions per target.

## Pre-existing exercised consumers (already work today)

These can be migrated to the facade in Stage 2 and continue
working unchanged:

- `aarch64-hello` already uses `robot_os_arch_aarch64::*`
  directly — facade migration would only matter if a kernel
  shared the same source.
- `x86_64-hello` similarly direct.

## Order of attack

1. **Stage 1 + 2 together in one session** (rename + facade)
   gives an immediate "kernel can talk to arch-aarch64 / x86_64
   via a single dep" win, even though most call sites still
   route to the riscv crate behind the scenes.
2. **Stage 3** stretches across many sessions; pick one
   sub-module per session (e.g. "move mmu::PAGE_SIZE callers to
   arch-api::Mmu::PAGE_SIZE").
3. **Stage 4 + 5** are the long tail; they depend on the
   x86_64 PVH detection blocker getting resolved.

## Risks

- TrapFrame layout per-ISA is the biggest design call. Get it
  wrong in Stage 3 and Stage 4 collapses.
- The `csr!` macro in `arch-riscv64::csr` is used 4× — those
  are RISC-V-only by design and will need `cfg`-gating at every
  call site or a per-ISA equivalent macro.
- `crates/mm` has 6 usages — heaviest dependency. Migrating
  those is its own multi-commit arc.

## Done definition

`make build` AND `cd aarch64-hello && cargo build` AND
`cd x86_64-hello && cargo build` all pass.

`make qemu-full-smp` boots the kernel (riscv).
`make aarch64-hello && make qemu-aarch64-hello` boots the
aarch64 binary.
`scripts/x86_64-boot-podman.sh` boots the x86_64 binary
(unblocks once the PVH detection bug is resolved).
