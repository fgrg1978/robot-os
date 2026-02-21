#
# Trap Entry Point (Assembly)
# RISC-V trap handler entry point
#
# Entered when any trap (exception or interrupt) occurs.
# Saves all registers, calls Rust trap_handler, restores state.
#
# Stack Switching for U-mode traps:
#   sscratch holds kernel SP when in U-mode (0 when in S-mode).
#   On trap: swap SP <-> sscratch to get kernel stack.

.option arch, +m, +a, +c, +zicsr, +zifencei, +zaamo, +zalrsc, +zmmul

# TrapFrame layout constants (must match crates/arch/src/trap.rs)
.equ TRAP_FRAME_SIZE, 288          # 32 regs * 8 + 4 CSRs * 8
.equ TF_SEPC,         256          # offset of sepc   in TrapFrame
.equ TF_SSTATUS,      264          # offset of sstatus in TrapFrame
.equ TF_SCAUSE,       272          # offset of scause  in TrapFrame
.equ TF_STVAL,        280          # offset of stval   in TrapFrame
.equ TF_SP,            16          # offset of regs[2] (saved SP)

# sstatus bit constants
.equ SSTATUS_SPP, (1 << 8)        # Supervisor Previous Privilege

.section .text
.align 4
.global trap_vector
.global trap_return

#
# trap_vector - Entry point for all traps
#
# CPU sets sepc, scause, stval, then jumps here (stvec).
#
trap_vector:
    # Swap SP <-> sscratch
    csrrw sp, sscratch, sp

    # If SP == 0, we came from S-mode (sscratch was 0)
    beqz sp, trap_from_smode

    # We came from U-mode, SP = kernel stack, sscratch = user SP
    j trap_save_context

trap_from_smode:
    # Restore: swap back (get real kernel SP from sscratch)
    csrrw sp, sscratch, sp
    # Now SP = kernel stack, sscratch = 0 again

trap_save_context:
    # Allocate trap frame
    addi sp, sp, -TRAP_FRAME_SIZE

    # Save general-purpose registers x0-x31
    # x0 is always 0, skip but reserve space (offset 0)
    sd x1,   8(sp)    # ra
    # x2 (sp) saved below
    sd x3,  24(sp)    # gp
    sd x4,  32(sp)    # tp
    sd x5,  40(sp)    # t0
    sd x6,  48(sp)    # t1
    sd x7,  56(sp)    # t2
    sd x8,  64(sp)    # s0/fp
    sd x9,  72(sp)    # s1
    sd x10, 80(sp)    # a0
    sd x11, 88(sp)    # a1
    sd x12, 96(sp)    # a2
    sd x13, 104(sp)   # a3
    sd x14, 112(sp)   # a4
    sd x15, 120(sp)   # a5
    sd x16, 128(sp)   # a6
    sd x17, 136(sp)   # a7 (syscall number)
    sd x18, 144(sp)   # s2
    sd x19, 152(sp)   # s3
    sd x20, 160(sp)   # s4
    sd x21, 168(sp)   # s5
    sd x22, 176(sp)   # s6
    sd x23, 184(sp)   # s7
    sd x24, 192(sp)   # s8
    sd x25, 200(sp)   # s9
    sd x26, 208(sp)   # s10
    sd x27, 216(sp)   # s11
    sd x28, 224(sp)   # t3
    sd x29, 232(sp)   # t4
    sd x30, 240(sp)   # t5
    sd x31, 248(sp)   # t6

    # Save original SP: check SPP bit to determine origin
    csrr t0, sstatus
    andi t0, t0, SSTATUS_SPP
    bnez t0, save_smode_sp

    # Came from U-mode: original SP is in sscratch
    csrr t0, sscratch
    sd t0, TF_SP(sp)            # regs[2] = user SP
    csrw sscratch, zero         # Mark: now in S-mode
    j save_csrs

save_smode_sp:
    # Came from S-mode: original SP = current SP + frame size
    addi t0, sp, TRAP_FRAME_SIZE
    sd t0, TF_SP(sp)            # regs[2] = kernel SP before frame

save_csrs:
    csrr t0, sepc
    sd t0, TF_SEPC(sp)

    csrr t0, sstatus
    sd t0, TF_SSTATUS(sp)

    csrr t0, scause
    sd t0, TF_SCAUSE(sp)

    csrr t0, stval
    sd t0, TF_STVAL(sp)

    # Call Rust trap handler: a0 = pointer to TrapFrame
    # Returns SATP value in a0 (0 = no PT switch)
    mv a0, sp
    call trap_handler
    mv s2, a0                # Save SATP in callee-saved s2

    # Fall through to trap_return

#
# trap_return - Restore state and return from trap
#
trap_return:
    # Check if returning to U-mode or S-mode
    ld t0, TF_SSTATUS(sp)       # Load saved sstatus
    andi t1, t0, SSTATUS_SPP    # Check SPP bit
    bnez t1, restore_to_smode

    # Returning to U-mode: set sscratch = kernel SP for next trap
    addi t1, sp, TRAP_FRAME_SIZE
    csrw sscratch, t1
    j restore_regs

restore_to_smode:
    # Returning to S-mode: sscratch stays 0
    csrw sscratch, zero

restore_regs:
    # Restore CSRs
    ld t0, TF_SEPC(sp)
    csrw sepc, t0

    ld t0, TF_SSTATUS(sp)
    csrw sstatus, t0

    # Switch page table if needed (s2 = SATP from trap_handler, checked
    # BEFORE s2 is restored from the frame below)
    beqz s2, skip_pt_switch
    csrw satp, s2
    sfence.vma zero, zero

skip_pt_switch:
    # Restore general-purpose registers
    ld x1,   8(sp)    # ra
    # x2 (sp) restored last
    ld x3,  24(sp)    # gp
    ld x4,  32(sp)    # tp
    ld x5,  40(sp)    # t0
    ld x6,  48(sp)    # t1
    ld x7,  56(sp)    # t2
    ld x8,  64(sp)    # s0/fp
    ld x9,  72(sp)    # s1
    ld x10, 80(sp)    # a0
    ld x11, 88(sp)    # a1
    ld x12, 96(sp)    # a2
    ld x13, 104(sp)   # a3
    ld x14, 112(sp)   # a4
    ld x15, 120(sp)   # a5
    ld x16, 128(sp)   # a6
    ld x17, 136(sp)   # a7
    ld x18, 144(sp)   # s2
    ld x19, 152(sp)   # s3
    ld x20, 160(sp)   # s4
    ld x21, 168(sp)   # s5
    ld x22, 176(sp)   # s6
    ld x23, 184(sp)   # s7
    ld x24, 192(sp)   # s8
    ld x25, 200(sp)   # s9
    ld x26, 208(sp)   # s10
    ld x27, 216(sp)   # s11
    ld x28, 224(sp)   # t3
    ld x29, 232(sp)   # t4
    ld x30, 240(sp)   # t5
    ld x31, 248(sp)   # t6

    # Restore SP last
    ld x2, TF_SP(sp)

    # Return from trap
    sret

.size trap_vector, . - trap_vector
