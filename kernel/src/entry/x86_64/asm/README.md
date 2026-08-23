# x86_64 kernel boot + trap asm — placeholder

Mirror of `../aarch64/asm/README.md` for the x86_64 ISA. Empty
today; populated in Item 2 Stage 5.

## Status: NOT YET POPULATED + ONE BLOCKER

Same shape as aarch64 — the kernel-side build for x86_64 doesn't
exist yet. PLUS task **#152 (B2.boot.attempt-N)** is parked: the
demo `crates/x86_64-hello` doesn't boot on macOS QEMU 10.1 nor
Linux QEMU 8.2 via PVH / multiboot1 / multiboot2 / ELFCLASS32-patched
ELF. Until we sort that, Stage 5 for x86_64 stays blocked even
when we have the kernel cross-arch wiring done for aarch64.

## Reference asm sources

| Reference asm | Lives in | Purpose |
|---------------|----------|---------|
| Multiboot1 header + 32→64 trampoline | `crates/x86_64-hello/src/main.rs` (`.multiboot` section) | Entry from QEMU `-kernel` |
| PML4 / PDP / PD identity map | `crates/x86_64-hello/src/main.rs` (`.text.boot`) | First 1 GiB identity |
| GDT + IDT setup | `crates/arch-x86_64::gdt` + `idt` | Segments + interrupt gates |
| TSS, syscall MSRs, xsave | `crates/arch-x86_64::tss` + `syscall` + `xsave` | EL0→EL1 syscall path |
| APIC + IOAPIC + ACPI MADT | `crates/arch-x86_64::apic` + `ioapic` + `acpi` | Interrupt + CPU enumeration |

## When Stage 5 happens, this directory will hold

- `boot.S` — Multiboot1 entry (or whatever boot protocol fixes
  #152), 32-bit prot mode → CR3/CR4/EFER long-mode transition →
  jump into `kernel_main`. Pulled from `x86_64-hello`.
- `trap_entry.S` — IDT vector stubs that push the `entry::x86_64::TrapFrame`
  defined in `../mod.rs` (including CR2 for page faults), tail-call into
  `x86_64_trap_entry(&mut TrapFrame)`.
- `context_switch.S` — save/restore the SystemV calling-convention
  GPRs + RIP + RFLAGS + FXSAVE area across `swap_task_state`.
