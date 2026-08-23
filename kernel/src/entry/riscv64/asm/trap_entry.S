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
#
# K-C16: the kernel `tp` (= hart id) is re-established from the per-hart trap
# vector on every trap taken from U-mode.  See the block comment above
# `trap_hart_vectors` for the full rationale.

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

# Number of harts with a dedicated trap-vector slot.
# NOT hand-duplicated: `{max_harts}` is substituted by the
# `global_asm!(..., max_harts = const MAX_HARTS)` block in
# kernel/src/main.rs — same technique as context_switch.S's `task_satp_off`.
# kernel/src/main.rs::MAX_HARTS is the single source; do not replace this
# with a literal.
.equ MAX_HARTS, {max_harts}

.section .text

# ── K-C16: per-hart trap vectors ────────────────────────────────────────────
#
# THE BUG.  In this kernel `tp` carries the hart id: `boot.S` sets `tp =
# hart_id` and `sched::smp::current_cpu_id()` is literally `mv rd, tp`, so
# every `PER_CPU[current_cpu_id()]` access depends on it.  The trap entry used
# to save the incoming `tp` into the frame and never re-establish a kernel
# value, which means that after ANY trap from U-mode the kernel ran on the
# `tp` ring 3 handed it.  A user task that writes `tp` and issues an `ecall`
# makes the kernel believe it is on another hart: wrong ready queue, wrong
# "current task", wrong scheduler state — a privilege-boundary hole reachable
# from ring 3 with no privileges at all.  It only failed to explode because no
# program in this tree happens to touch `tp` (there is no TLS here).  That is
# a convention, not a defence.
#
# WHERE THE HART ID COMES FROM.  RISC-V gives S-mode no `mhartid`, and this
# kernel already spends `sscratch` on the kernel SP (`csrrw sp, sscratch, sp`
# at the top of the entry), so the usual "sscratch points at a per-hart
# struct" trick is not available without rewriting every writer of sscratch —
# including two `csrw sscratch, sp` sites in Rust (crates/sched/src/process.rs
# sret_to_user / sret_to_user_forked) that belong to another lane.  Worse, any
# sscratch-based tag has a bootstrap hole: the FIRST trap after `sret_to_user`
# would see an untagged value.
#
# `stvec` has none of those problems.  It is a per-hart CSR, it is written
# exactly once per hart at boot (boot.S for secondaries, `trap_init()` in
# kernel/src/main.rs for the boot hart), it is not writable from U-mode, and
# it is already correct at the very first trap a task ever takes.  So each
# hart gets its OWN vector slot and the entry recovers the id from `stvec`.
#
# LAYOUT.  One 8-byte slot per hart:
#
#     slot k:  [ .word k ][ j trap_entry_common ]     <- stvec points HERE (+4)
#              ^ base+8k                ^ base+8k+4
#
# The id word sits BELOW the entry point on purpose: `.balign 8` alone then
# pins its offset at -4 from `stvec`, whatever the assembler does with the
# `j` (a compressed `c.j` would shift a trailing word to +2 and we would read
# the next slot's opcode as a hart id).  `stvec` ends up 4-byte aligned, which
# is what Direct mode and `trap_init()`'s `trap_addr & 0x3 == 0` assert want.
#
# THE BOOT HART.  `trap_init()` installs the plain `trap_vector` symbol, so
# the boot hart cannot use a slot chosen at link time (its id is only known at
# run time, and it is not necessarily 0).  `trap_vector` therefore carries the
# sentinel id -1 and the entry falls back to `boot_hart_id`, a .data word
# boot.S fills in from a0 before it ever calls into Rust.  Nothing patches
# .text — `enforce_wx()` maps .text RX later on and a store there would fault.
#
# COST: 3 instructions per U-mode trap on a secondary hart (csrr, lw, bgez),
# 3 more on the boot hart (la = auipc+addi, ld).  Zero on S-mode traps, zero
# on the context-switch path.  The load hits .text/.data, both mapped R in the
# kernel PT and shared into every user PT by copy_kernel_entries_to_user(), so
# it is safe to execute before satp is switched back.  Against the ~1879 ns
# syscall floor this is noise.
.balign 8
.global trap_hart_vectors
trap_hart_vectors:
.set hart_slot_id, 0
.rept MAX_HARTS
    .balign 8
    .word hart_slot_id                 # this slot's hart id, at stvec-4
    j     trap_entry_common
    .set  hart_slot_id, hart_slot_id + 1
.endr

.align 4
.global trap_vector
.global trap_return

#
# trap_vector - generic entry, installed by trap_init() on the boot hart.
#
# Same shape as a per-hart slot, but the id word is the sentinel -1: the hart
# id is not known at link time, so the entry reads it from `boot_hart_id`.
#
.balign 8
    .word -1                           # sentinel, at trap_vector-4
trap_vector:
    j trap_entry_common

#
# trap_entry_common - Entry point for all traps (shared body)
#
# CPU sets sepc, scause, stval, then jumps to this hart's stvec slot, which
# jumps here.
#
trap_entry_common:
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

    # K-C16: re-establish the kernel tp (= hart id) for the hart we are
    # ACTUALLY on, instead of trusting the tp ring 3 handed us.  The user's
    # own tp is already safe in the frame at 32(sp) and is given back
    # untouched on the way out — user code may keep whatever tp it likes, it
    # just may not impose it on the kernel.
    # x5/t0 is already saved in the frame at this point, so it is free.
    csrr t0, stvec              # per-hart CSR; U-mode cannot write it
    lw   tp, -4(t0)             # id word sits 4 bytes below every vector slot
    bgez tp, save_csrs          # >= 0: per-hart slot, id is the literal
    la   t0, boot_hart_id       # -1: generic vector => boot hart
    ld   tp, 0(t0)
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

    # K-C16b (separate finding from K-C16, same file): restore tp ONLY when
    # returning to U-mode.
    #
    # Returning to U-mode, the frame holds the user's own tp — give it back.
    # Returning to S-mode the frame holds the tp of the hart this trap was
    # TAKEN on, which is not necessarily the hart we are returning on: a task
    # preempted by a timer IRQ can be migrated, and context_switch.S then
    # restores the correct tp for the new hart from task.context.tp (the K-A11
    # invariant).  Reloading the stale frame value here would undo exactly
    # that and drop the kernel back into the K-A11 corruption by another
    # route.  The tp the hart is currently running with is the correct one, so
    # leave it alone.
    # t0 still holds the saved sstatus; t1 is restored from the frame below.
    andi t1, t0, SSTATUS_SPP
    bnez t1, 1f                 # SPP=1 -> returning to S-mode: keep kernel tp
    ld x4, 32(sp)               # SPP=0 -> returning to U-mode: user's own tp
1:

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
    # x4 (tp) restored above, U-mode returns only — see K-C16b
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

.size trap_entry_common, . - trap_entry_common

# K-C16: boot hart id, published by boot.S (a0 from OpenSBI) before it calls
# kernel_main.  Read only by the generic `trap_vector` path above.  Lives in
# .data — .bss is zeroed by clear_bss in boot.S and .text becomes RX once
# enforce_wx() runs, so neither would do.
.section .data
.balign 8
.global boot_hart_id
boot_hart_id:
    .quad 0
