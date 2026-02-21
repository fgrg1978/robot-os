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

    # Stack = secondary_stacks[hart_id] + stack_size
    la t0, secondary_stacks
    la t1, _secondary_stack_size
    ld t1, 0(t1)                    # load const from .data (Rust SECONDARY_STACK_SIZE)
    mul t2, s0, t1
    add t0, t0, t2
    add sp, t0, t1

    # Set trap vector
    la t0, trap_vector
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
.extern trap_vector

# Boot lock (atomic)
.section .data
.align 2
.global boot_lock
boot_lock:
    .word 0
