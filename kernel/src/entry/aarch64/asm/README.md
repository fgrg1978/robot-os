# aarch64 kernel boot + trap asm — placeholder

This directory will hold the kernel-side boot, trap-vector, and
context-switch asm for the **aarch64 build of the PHANES kernel**.

## Status: NOT YET POPULATED

The aarch64 build of the real kernel doesn't exist yet — that's
Item 2 Stage 5 (`rfcs/item-2-kernel-cross-arch-plan.md`). Today
all aarch64 asm lives in two demo crates:

| Reference asm | Lives in | Purpose |
|---------------|----------|---------|
| Boot trampoline EL2→EL1 | `crates/arch-aarch64::boot::{drop_to_el1, eret_to_el0}` | Hoisted helper, reused by demos |
| Stage-1 MMU identity map | `crates/arch-aarch64::mmu_setup::enable_identity_map` | VMSAv8 4 KB granule, 3-level |
| GICv3 vector table (VBAR_EL1) | `crates/arch-aarch64::gic` + asm in `crates/aarch64-hello/src/main.rs` | IRQ + sync exception entry |
| NEON FP context save/restore | `crates/arch-aarch64::fp_state` | Q0-Q31 |
| Demo user_main + SVC trap-up | `crates/aarch64-hello/src/main.rs` | EL0 user-mode demo |

## When Stage 5 happens, this directory will hold

- `boot.S` — kernel entry from EFI / U-Boot / SEMIHOSTING, EL2→EL1
  trampoline, BSS clear, jump into `kernel_main`. Pulled from
  `aarch64-hello` + `arch-aarch64::boot`.
- `trap_entry.S` — VBAR_EL1 vector table, register-save into the
  `entry::aarch64::TrapFrame` defined in `../mod.rs`, tail-call into
  `aarch64_trap_entry(&mut TrapFrame)`.
- `context_switch.S` — save/restore the 31 GPRs + ELR_EL1 + SPSR_EL1
  + Q0-Q31 across a `swap_task_state` call.

## Why we keep this README

So the structure of `kernel/src/entry/` already shows the intended
layout, even before the asm is wired in. Makes it impossible to
write a phase-5 PR that "forgets" the aarch64 surface — the
directory is sitting here waiting.
