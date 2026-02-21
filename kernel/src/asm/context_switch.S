#
# Context Switch (RISC-V Assembly)
#
# Switches execution from one Task to another by saving/restoring
# callee-saved registers only (caller-saved are on the stack already,
# per RISC-V calling convention).
#
# fn context_switch(old: *mut Task, new: *mut Task)
#   a0 = old task pointer (NULL on first call)
#   a1 = new task pointer
#
# Task::context (TaskContext) is at offset 0 of Task (repr(C)).

.option arch, +m, +a, +c, +zicsr, +zifencei, +zaamo, +zalrsc, +zmmul

# TaskContext field offsets (8 bytes each, RV64)
# Must match crates/sched/src/task.rs TaskContext layout.
.equ CTX_RA,   0
.equ CTX_SP,   8
.equ CTX_S0,  16
.equ CTX_S1,  24
.equ CTX_S2,  32
.equ CTX_S3,  40
.equ CTX_S4,  48
.equ CTX_S5,  56
.equ CTX_S6,  64
.equ CTX_S7,  72
.equ CTX_S8,  80
.equ CTX_S9,  88
.equ CTX_S10, 96
.equ CTX_S11, 104
.equ CTX_PC,  112

# Task struct offset (must match offset_of!(Task, task_satp))
.equ TASK_SATP_OFFSET, 200

.section .text
.align 4
.global context_switch

context_switch:
    # If old == NULL, skip saving (first context switch)
    beqz a0, restore_new_task

save_old_task:
    # Save callee-saved registers to old->context (offset 0)
    sd ra,  CTX_RA(a0)
    sd sp,  CTX_SP(a0)
    sd s0,  CTX_S0(a0)
    sd s1,  CTX_S1(a0)
    sd s2,  CTX_S2(a0)
    sd s3,  CTX_S3(a0)
    sd s4,  CTX_S4(a0)
    sd s5,  CTX_S5(a0)
    sd s6,  CTX_S6(a0)
    sd s7,  CTX_S7(a0)
    sd s8,  CTX_S8(a0)
    sd s9,  CTX_S9(a0)
    sd s10, CTX_S10(a0)
    sd s11, CTX_S11(a0)

    # Save PC: when old is rescheduled, execution resumes after this call.
    sd ra,  CTX_PC(a0)

restore_new_task:
    # Restore callee-saved registers from new->context
    ld ra,  CTX_RA(a1)
    ld sp,  CTX_SP(a1)
    ld s0,  CTX_S0(a1)
    ld s1,  CTX_S1(a1)
    ld s2,  CTX_S2(a1)
    ld s3,  CTX_S3(a1)
    ld s4,  CTX_S4(a1)
    ld s5,  CTX_S5(a1)
    ld s6,  CTX_S6(a1)
    ld s7,  CTX_S7(a1)
    ld s8,  CTX_S8(a1)
    ld s9,  CTX_S9(a1)
    ld s10, CTX_S10(a1)
    ld s11, CTX_S11(a1)

    # Load PC
    ld t0,  CTX_PC(a1)

    # Switch page table only if SATP actually changes.
    # Kernel tasks share the same PT — skip the expensive sfence.vma.
    ld t1,  TASK_SATP_OFFSET(a1)
    csrr t2, satp
    beq t1, t2, ctx_skip_pt_switch
    csrw satp, t1
    sfence.vma zero, zero

ctx_skip_pt_switch:
    jr t0              # Jump to new task's PC

.size context_switch, . - context_switch
