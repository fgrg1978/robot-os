# Trap Entry Point — ESP32-C3
# RV32IMC, M-mode only (no U-mode, no sscratch swap)
#
# TrapFrame layout (144 bytes):
#   x0-x31: 32 regs × 4 bytes = 128 bytes (offsets 0-124)
#   mepc:    offset 128
#   mstatus: offset 132
#   mcause:  offset 136
#   mtval:   offset 140
#
# Field names in Rust TrapFrame (sepc/sstatus/scause/stval) are aliases;
# the M-mode CSRs are stored at the same offsets.

.option arch, +m, +c, +zicsr, +zifencei, +zmmul

.section .text
.align 4
.global trap_vector

trap_vector:
    # ESP32-C3: M-mode only, no U-mode — no sscratch swap needed.
    # Allocate trap frame: 32 regs × 4 + 4 CSRs × 4 = 144 bytes
    addi sp, sp, -144

    # Save general-purpose registers (x0 is always 0, skip but reserve)
    sw x1,   4(sp)    # ra
    # x2 (sp) saved below
    sw x3,  12(sp)    # gp
    sw x4,  16(sp)    # tp
    sw x5,  20(sp)    # t0
    sw x6,  24(sp)    # t1
    sw x7,  28(sp)    # t2
    sw x8,  32(sp)    # s0/fp
    sw x9,  36(sp)    # s1
    sw x10, 40(sp)    # a0
    sw x11, 44(sp)    # a1
    sw x12, 48(sp)    # a2
    sw x13, 52(sp)    # a3
    sw x14, 56(sp)    # a4
    sw x15, 60(sp)    # a5
    sw x16, 64(sp)    # a6
    sw x17, 68(sp)    # a7 (syscall number)
    sw x18, 72(sp)    # s2
    sw x19, 76(sp)    # s3
    sw x20, 80(sp)    # s4
    sw x21, 84(sp)    # s5
    sw x22, 88(sp)    # s6
    sw x23, 92(sp)    # s7
    sw x24, 96(sp)    # s8
    sw x25, 100(sp)   # s9
    sw x26, 104(sp)   # s10
    sw x27, 108(sp)   # s11
    sw x28, 112(sp)   # t3
    sw x29, 116(sp)   # t4
    sw x30, 120(sp)   # t5
    sw x31, 124(sp)   # t6

    # Save original SP (before frame allocation)
    addi t0, sp, 144
    sw t0, 8(sp)       # regs[2] = kernel SP before frame

    # Save M-mode CSRs (mapped to same TrapFrame fields as S-mode)
    csrr t0, mepc
    sw t0, 128(sp)     # sepc field ← mepc

    csrr t0, mstatus
    sw t0, 132(sp)     # sstatus field ← mstatus

    csrr t0, mcause
    sw t0, 136(sp)     # scause field ← mcause

    csrr t0, mtval
    sw t0, 140(sp)     # stval field ← mtval

    # Call Rust trap handler: a0 = pointer to TrapFrame
    mv a0, sp
    call trap_handler

    # Restore M-mode CSRs
    lw t0, 128(sp)
    csrw mepc, t0

    lw t0, 132(sp)
    csrw mstatus, t0

    # Restore general-purpose registers
    lw x1,   4(sp)    # ra
    # x2 (sp) restored last
    lw x3,  12(sp)    # gp
    lw x4,  16(sp)    # tp
    lw x5,  20(sp)    # t0
    lw x6,  24(sp)    # t1
    lw x7,  28(sp)    # t2
    lw x8,  32(sp)    # s0/fp
    lw x9,  36(sp)    # s1
    lw x10, 40(sp)    # a0
    lw x11, 44(sp)    # a1
    lw x12, 48(sp)    # a2
    lw x13, 52(sp)    # a3
    lw x14, 56(sp)    # a4
    lw x15, 60(sp)    # a5
    lw x16, 64(sp)    # a6
    lw x17, 68(sp)    # a7
    lw x18, 72(sp)    # s2
    lw x19, 76(sp)    # s3
    lw x20, 80(sp)    # s4
    lw x21, 84(sp)    # s5
    lw x22, 88(sp)    # s6
    lw x23, 92(sp)    # s7
    lw x24, 96(sp)    # s8
    lw x25, 100(sp)   # s9
    lw x26, 104(sp)   # s10
    lw x27, 108(sp)   # s11
    lw x28, 112(sp)   # t3
    lw x29, 116(sp)   # t4
    lw x30, 120(sp)   # t5
    lw x31, 124(sp)   # t6

    # Restore SP last
    lw x2,  8(sp)

    # Return from M-mode trap
    mret

.size trap_vector, . - trap_vector
