# Boot entry point for Robot OS (Rust)
# RISC-V 64-bit, S-mode (OpenSBI provides M-mode)
#
# Boot parameters from OpenSBI:
#   a0 = hart_id
#   a1 = dtb_ptr (device tree blob)
#
# Secondary harts are started later via SBI HSM hart_start(),
# which jumps directly to _secondary_start.

# Enable ISA extensions for LLVM 19+ assembler (decomposed M and A extensions)
.option arch, +m, +a, +c, +zicsr, +zifencei, +zaamo, +zalrsc, +zmmul

# Number of harts with a per-hart trap-vector slot / stack slot.
# NOT hand-duplicated: `{max_harts}` is substituted by the
# `global_asm!(..., max_harts = const MAX_HARTS)` block in
# kernel/src/main.rs, so kernel/src/main.rs::MAX_HARTS is the single source.
# Do not replace it with a literal.
.equ MAX_HARTS, {max_harts}

.section .text.boot
.global _start

_start:
    mv s0, a0                      # Save hart_id
    mv s1, a1                      # Save dtb_ptr

    # Boot hart selection: atomic lock, first hart wins
    la t0, boot_lock
    li t1, 1
    amoswap.w.aq t2, t1, (t0)
    bnez t2, secondary_halt        # Others halt (SBI HSM will start them later)

    # Disable S-mode interrupts
    csrw sie, zero

    # tp = hart_id (per-CPU data)
    mv tp, s0

    # K-C16: publish the boot hart id for the generic trap vector.
    # trap_init() in kernel/src/main.rs installs the plain `trap_vector`
    # symbol on this hart, whose embedded id word is the sentinel -1 (the boot
    # hart is only known at run time and is not necessarily 0).  The trap
    # entry falls back to this .data word to rebuild the kernel tp after a
    # trap from U-mode.  Must be stored before any trap can be taken, i.e.
    # before kernel_main; .data is not touched by clear_bss below.
    la t0, boot_hart_id
    sd s0, 0(t0)

    # Stack at top of RAM
    la sp, _stack_end

    # Clear BSS
    la t0, _bss_start
    la t1, _bss_end
clear_bss:
    bgeu t0, t1, clear_bss_done    # bgeu: handles non-8-byte-aligned _bss_end
    sd zero, 0(t0)
    addi t0, t0, 8
    j clear_bss
clear_bss_done:

    # Ensure I-cache sees zeroed BSS (RISC-V has no guaranteed I/D coherence)
    fence.i

    # Call Rust kernel_main(hart_id, dtb_ptr)
    mv a0, s0
    mv a1, s1
    call kernel_main

    # If kernel_main returns, halt
halt:
    wfi
    j halt

# Harts that lost the boot lock wait here until SBI HSM starts them
secondary_halt:
    wfi
    j secondary_halt

.size _start, . - _start

# Secondary CPU entry point
# Called by SBI HSM hart_start: a0 = hart_id (opaque argument)
.section .text.boot
.global _secondary_start
.align 2

_secondary_start:
    mv s0, a0
    mv tp, a0

    csrw sie, zero

    # Range-check the hart id BEFORE anything is computed from it. The sp
    # derived from an id >= MAX_HARTS points outside secondary_stacks into
    # adjacent .bss; parking used to happen only after that sp was already
    # live in the register file, so any future instruction between stack
    # setup and the park that touched the stack would have corrupted .bss.
    # Nothing before this needs more than s0 itself.
    li t1, MAX_HARTS
    bgeu s0, t1, secondary_halt     # no stack slot, no trap-vector slot,
                                    # no PER_CPU entry: park

    # Stack = secondary_stacks[hart_id] + stack_size
    la t0, secondary_stacks
    la t1, _secondary_stack_size
    ld t1, 0(t1)                    # load const from .data (Rust SECONDARY_STACK_SIZE)
    mul t2, s0, t1
    add t0, t0, t2
    add sp, t0, t1

    # Set trap vector — K-C16: this hart's OWN slot, not the generic vector.
    # Each slot carries its hart id in the word just below the entry point, so
    # the trap entry can rebuild the kernel tp after a trap from U-mode
    # instead of trusting the tp ring 3 handed it.  stvec is a per-hart CSR
    # and U-mode cannot write it, which is exactly why the id is kept there.
    la t0, trap_hart_vectors
    slli t2, s0, 3                  # slot stride = 8 bytes
    add t0, t0, t2
    addi t0, t0, 4                  # stvec points at the `j`, id word at -4
    csrw stvec, t0

    csrw sscratch, zero

    .equ SSTATUS_SPP, (1 << 8)     # Supervisor Previous Privilege = S-mode
    li t0, SSTATUS_SPP
    csrs sstatus, t0

    # Call Rust smp_secondary_start(hart_id)
    mv a0, s0
    call smp_secondary_start

    wfi
    j secondary_halt

.size _secondary_start, . - _secondary_start

.extern secondary_stacks
.extern _secondary_stack_size
.extern trap_hart_vectors
.extern boot_hart_id

# Boot lock (atomic)
.section .data
.align 2
.global boot_lock
boot_lock:
    .word 0
