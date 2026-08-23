# Context Switch — ESP32-C3
# RV32IMC, M-mode, no MMU (no SATP switch)
#
# fn context_switch(old: *mut Task, new: *mut Task)
#   a0 = old task pointer (NULL on first call)
#   a1 = new task pointer
#
# TaskContext field offsets (4 bytes each, RV32):
#   ra  = 0
#   sp  = 4
#   s0  = 8
#   s1  = 12
#   s2  = 16
#   s3  = 20
#   s4  = 24
#   s5  = 28
#   s6  = 32
#   s7  = 36
#   s8  = 40
#   s9  = 44
#   s10 = 48
#   s11 = 52
#   pc  = 56

.option arch, +m, +c, +zicsr, +zifencei, +zmmul

.section .text
.align 4
.global context_switch

context_switch:
    # If old == NULL, skip saving (first context switch)
    beqz a0, restore_new_task

save_old_task:
    # Save callee-saved registers to old->context (offset 0)
    sw ra,   0(a0)    # context.ra
    sw sp,   4(a0)    # context.sp
    sw s0,   8(a0)    # context.s0
    sw s1,  12(a0)    # context.s1
    sw s2,  16(a0)    # context.s2
    sw s3,  20(a0)    # context.s3
    sw s4,  24(a0)    # context.s4
    sw s5,  28(a0)    # context.s5
    sw s6,  32(a0)    # context.s6
    sw s7,  36(a0)    # context.s7
    sw s8,  40(a0)    # context.s8
    sw s9,  44(a0)    # context.s9
    sw s10, 48(a0)    # context.s10
    sw s11, 52(a0)    # context.s11

    # Save PC: return address of caller
    sw ra,  56(a0)    # context.pc = return address

    # Mark old task's context as safely saved now that every callee-saved
    # register + PC has been written to old->context. Same protocol as
    # context_switch.S — single-core here so there's no cross-hart race to
    # close, but the Rust-side spin-gate in do_schedule() is unconditional
    # across all targets, so this task's context_saving flag must still be
    # cleared or it would stay stuck at `true` forever after the first
    # block/preempt, deadlocking the scheduler.
    mv  s0, a1              # preserve new task ptr across the call
    call mark_context_saved # mark_context_saved(a0 = old task ptr)
    mv  a1, s0               # restore new task ptr for restore_new_task

restore_new_task:
    # Restore callee-saved registers from new->context
    lw ra,   0(a1)    # context.ra
    lw sp,   4(a1)    # context.sp
    lw s0,   8(a1)    # context.s0
    lw s1,  12(a1)    # context.s1
    lw s2,  16(a1)    # context.s2
    lw s3,  20(a1)    # context.s3
    lw s4,  24(a1)    # context.s4
    lw s5,  28(a1)    # context.s5
    lw s6,  32(a1)    # context.s6
    lw s7,  36(a1)    # context.s7
    lw s8,  40(a1)    # context.s8
    lw s9,  44(a1)    # context.s9
    lw s10, 48(a1)    # context.s10
    lw s11, 52(a1)    # context.s11

    # Load PC
    lw t0,  56(a1)    # context.pc

    # No SATP switch (ESP32-C3 has no MMU)

    jr t0              # Jump to new task's PC

.size context_switch, . - context_switch
