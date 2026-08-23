#
# Context Switch with RVV 1.0 vector register save/restore (Phase 12)
#
# Same interface as context_switch.S but additionally saves/restores v0-v31,
# vl, vtype, and vstart via rvv_ctx_save() / rvv_ctx_restore() (defined in
# crates/arch/src/rvv.rs).  Only linked when --features rvv is active.
#
# fn context_switch(old: *mut Task, new: *mut Task)
#   a0 = old task pointer (NULL on first call)
#   a1 = new task pointer
#
# Task::context (TaskContext) is at offset 0 of Task.
#
# ── `tp` under rvv: deliberately absent, and now harmless (K-C16) ────────────
#
# This file has no CTX_TP offset and never touches `tp`, unlike
# context_switch.S, which saves and restores it to keep `current_cpu_id()`
# consistent with the hart a task was migrated to (K-A11). That asymmetry is
# NOT an oversight to be "fixed" by copying the K-A11 code here — read this
# before you do:
#
#   * `tp` is the hart id. Under rvv nothing ever changes it after boot.S sets
#     it, so it is a per-hart invariant by construction: a hart's `tp` is
#     always its own id, whatever task happens to be running. `current_cpu_id()`
#     is therefore correct here for the same reason it is correct in
#     context_switch.S, by a different route.
#   * The consequence is that K-A11's `task.context.tp = target_cpu` bookkeeping
#     in scheduler.rs is dead code under this build. Harmless — it writes a
#     field nothing reads.
#   * Before K-C16 this was NOT harmless. trap_entry.S restored `tp` from the
#     trap frame on the way back from U-mode, so a ring-3 task that wrote `tp`
#     poisoned the kernel's idea of which hart it was on — and under rvv there
#     was nothing anywhere that would ever correct it again. K-C16 re-establishes
#     the kernel `tp` from the per-hart trap vector on every entry from U, which
#     closes that for this build too.
#
# So: leave `tp` out of this file. If you ever add it, the K-A11 invariant must
# hold on both paths, and the rvv spin-gate exemption in do_schedule() (which
# exists because this file never clears context_saving — context_switch.S does
# it with its fence+sb save tail, K-C23) has to be revisited at the same time.
#
# Vector save strategy:
#   s0 is used as scratch to preserve a1 (new task ptr) across `call`.
#   s0 is saved to old->context BEFORE we overwrite it, so the old task's s0
#   value is preserved.  When control returns to that task, ld s0, CTX_S0(a1)
#   restores it from the new task's saved context.

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

# Task struct offset — auto-generated from offset_of!(Task, task_satp)
.equ TASK_SATP_OFFSET, {task_satp_off}

.section .text
.align 4
.global context_switch

context_switch:
    beqz a0, rvv_restore_only

save_old_task:
    # Save callee-saved registers to old->context (offset 0).
    # s0 is saved first so we can reuse it as scratch below.
    sd ra,  CTX_RA(a0)
    sd sp,  CTX_SP(a0)
    sd s0,  CTX_S0(a0)    # saved before we use s0 as scratch
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
    sd ra,  CTX_PC(a0)    # context.pc = return address

    # Save vector state for old task.
    # Preserve a1 (new task ptr) across the call in s0 (callee-saved).
    mv  s0, a1
    call rvv_ctx_save          # rvv_ctx_save(a0 = old task ptr)

    # Restore vector state for new task.
    mv  a0, s0                 # a0 = new task ptr
    call rvv_ctx_restore       # rvv_ctx_restore(a0 = new task ptr)
    mv  a1, s0                 # restore a1 for restore_new_task
    j   restore_new_task

rvv_restore_only:
    # First context switch: old == NULL, skip saving.
    # Restore vector state for new task only.
    mv  s0, a1
    mv  a0, a1
    call rvv_ctx_restore
    mv  a1, s0

restore_new_task:
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

    ld t0,  CTX_PC(a1)

    # Switch page table only if SATP actually changes.
    ld t1,  TASK_SATP_OFFSET(a1)
    csrr t2, satp
    beq t1, t2, rvv_skip_pt_switch
    csrw satp, t1
    sfence.vma zero, zero

rvv_skip_pt_switch:
    jr t0

.size context_switch, . - context_switch
