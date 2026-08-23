# Boot entry point for Robot OS — ESP32-C3
# RV32IMC, M-mode, single core, no OpenSBI, no DTB
#
# The ESP32-C3 ROM bootloader jumps here after loading the image.
# No secondary harts, no atomic boot lock needed.

.option arch, +m, +c, +zicsr, +zifencei, +zmmul

.section .text.boot
.global _start

_start:
    # Disable M-mode interrupts
    csrw mie, zero
    csrw mip, zero

    # tp = hart_id = 0 (single core)
    li tp, 0

    # Stack at top of RAM
    la sp, _stack_end

    # Clear BSS (4-byte stride for RV32)
    la t0, _bss_start
    la t1, _bss_end
clear_bss:
    bgeu t0, t1, clear_bss_done
    sw zero, 0(t0)
    addi t0, t0, 4
    j clear_bss
clear_bss_done:

    # Call Rust kernel_main(hart_id=0, dtb_ptr=0)
    li a0, 0
    li a1, 0
    call kernel_main

    # If kernel_main returns, halt
halt:
    wfi
    j halt

.size _start, . - _start
