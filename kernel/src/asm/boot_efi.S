// UE01 — EFI entry stub for robot-os (RISC-V 64, PE/COFF).
//
// The PE/COFF loader in UEFI firmware calls our entry point with:
//     a0 = EFI_HANDLE    (image handle)
//     a1 = EFI_SYSTEM_TABLE* (system table)
// We save the stack pointer, set up our own stack, then jump to
// `efi_main` (defined in crates/efi/src/lib.rs). efi_main returns
// by tail-calling `efi_handoff` which is defined below.
//
// IMPORTANT: everything here must be position-independent. UEFI
// firmware relocates the image to whatever load address it picks.

.section .text._start, "ax"
.global _start
.type   _start, @function
_start:
    // Stash firmware-provided args in saved registers (s0/s1) so a temp
    // stack setup doesn't clobber them.
    mv      s0, a0           // s0 = image_handle
    mv      s1, a1           // s1 = system_table

    // Set up a dedicated EFI-stage stack (8 KiB) so we don't depend on
    // whatever SP UEFI gave us (which may vanish after ExitBootServices).
    la      sp, _efi_stack_top

    // Restore args and call efi_main(image_handle, system_table).
    mv      a0, s0
    mv      a1, s1
    call    efi_main

    // Should never return. Spin if it does.
1:  wfi
    j       1b
.size   _start, . - _start


// ── Stack for the EFI stage ───────────────────────────────────────────────
.section .bss._efi_stack, "aw", @nobits
.align  4
_efi_stack_bottom:
    .skip   0x2000               // 8 KiB
_efi_stack_top:


// ── efi_handoff — jump to kernel_main(0, BootInfo*) ───────────────────────
//
// Called from Rust-side efi_main after ExitBootServices. Resets the stack
// to the kernel's reserved boot stack and tail-jumps to kernel_main with:
//     a0 = 0                (hart id — firmware is the boot hart)
//     a1 = boot_info_ptr    (passed in as first arg to this function)
.section .text.efi_handoff, "ax"
.global efi_handoff
.type   efi_handoff, @function
efi_handoff:
    // a0 on entry = boot_info_ptr — move into a1 for kernel_main.
    mv      a1, a0
    li      a0, 0                // hart_id = 0

    // Switch to the kernel boot stack.
    la      sp, _boot_stack_top_efi

    // Jump (not call — do not push a return address).
    la      t0, kernel_main
    jr      t0

1:  wfi
    j       1b
.size   efi_handoff, . - efi_handoff


// ── Kernel boot stack (small — kernel_main will switch once PMM is up) ────
.section .bss._boot_stack_efi, "aw", @nobits
.align  4
_boot_stack_bottom_efi:
    .skip   0x4000               // 16 KiB
_boot_stack_top_efi:
