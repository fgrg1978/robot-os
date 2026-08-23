# M-mode boot stub for no-OpenSBI builds (F04: PMP boot integration).
#
# When Robot OS is loaded directly by a bootloader (not OpenSBI), the hart
# starts in M-mode.  This stub:
#   1. Initialises a minimal M-mode stack.
#   2. Calls pmp_early_init(ram_base, ram_size) to enforce the boot PMP policy.
#   3. Configures mstatus/mepc for an S-mode entry, then `mret` into _start.
#
# Register conventions on entry (from firmware / SBI HSM):
#   a0 = hart_id
#   a1 = dtb_ptr
#
# After mret, boot.S takes over in S-mode with a0/a1 intact.

# Enable ISA extensions for LLVM 19+ assembler
.option arch, +m, +a, +c, +zicsr, +zifencei, +zaamo, +zalrsc, +zmmul

# ── QEMU virt RAM constants (compile-time; real size read from DTB at runtime) ──
# These are used ONLY to set up the permissive boot PMP before DTB is parsed.
.equ QEMU_RAM_BASE, 0x80000000      # Physical RAM base on QEMU virt
.equ QEMU_RAM_SIZE, 0x08000000      # 128 MiB (QEMU default; safe upper bound)

# mstatus bits
.equ MSTATUS_MPP_S,  (1 << 11)     # MPP = S-mode (01)
.equ MSTATUS_MPIE,   (1 << 7)      # Machine Previous Interrupt Enable

.section .text.boot
.global _start_mmode
.align 2

_start_mmode:
    # Preserve hart_id (a0) and dtb_ptr (a1) across the Rust call.
    mv  s0, a0
    mv  s1, a1

    # ── Set up a temporary M-mode stack ────────────────────────────────────
    # Reuse the same boot stack (_stack_end) defined in the linker script.
    # At this point no S-mode code has run yet, so the stack is empty.
    la  sp, _stack_end

    # ── Configure mstatus: MPP=S, MPIE=1 ───────────────────────────────────
    # This means mret will drop into S-mode with interrupts previously enabled.
    li  t0, (MSTATUS_MPP_S | MSTATUS_MPIE)
    csrw mstatus, t0

    # ── Set mepc = _start (S-mode boot entry in boot.S) ────────────────────
    la  t0, _start
    csrw mepc, t0

    # ── Disable M-mode interrupts before PMP setup ──────────────────────────
    csrw mie, zero

    # ── Call pmp_early_init(ram_base, ram_size) ─────────────────────────────
    # Uses QEMU constants; after mret the DTB will provide the real size.
    li  a0, QEMU_RAM_BASE
    li  a1, QEMU_RAM_SIZE
    call pmp_early_init

    # ── Restore hart_id / dtb_ptr for S-mode boot entry ────────────────────
    mv  a0, s0
    mv  a1, s1

    # ── mret: drop into S-mode at _start ────────────────────────────────────
    mret

.size _start_mmode, . - _start_mmode
