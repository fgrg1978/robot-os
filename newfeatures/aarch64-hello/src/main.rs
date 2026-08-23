//! PHANES B1.boot — minimal aarch64 boot binary.
//!
//! Run with:  make qemu-aarch64-hello
//!
//! Boots on `qemu-system-aarch64 -M virt -cpu cortex-a72`,
//! prints two lines via the PL011 UART at 0x0900_0000, and parks
//! on `WFI`. Proves that the `arch-aarch64` crate (B1) reaches
//! actual instruction execution — not just compile-time
//! validation.

#![no_std]
#![no_main]

use core::arch::global_asm;

// ──────────────────────────────────────────────────────────────────────────
// Boot trampoline.
//
// QEMU `-kernel <elf>` enters at the ELF entry point with:
//   - At EL2 (with `-M virt`, no firmware → direct EL2 entry).
//   - x0 = address of the device-tree blob (FDT).
//   - All other state implementation-defined.
//
// We do the minimum needed to run Rust:
//   1. Park secondary CPUs (`MPIDR_EL1.Aff0 != 0` → loop on WFE).
//   2. Set the stack to the top of the BSS-allocated stack area.
//   3. Zero `.bss` (Rust assumes it's zeroed; the loader doesn't
//      do this for us).
//   4. Call `rust_main`; if it returns, park on WFI.
// ──────────────────────────────────────────────────────────────────────────

// ──────────────────────────────────────────────────────────────────────────
// Exception vector table — 16 entries × 0x80 bytes = 2 KiB, aligned to
// 2 KiB per Arm ARM §D7. We use one common IRQ handler (vec_irq) and a
// halt for every other vector — sufficient to prove the IRQ entry path
// without sweeping all 16 cases (sync / fiq / serror handlers are
// follow-up work).
// ──────────────────────────────────────────────────────────────────────────

global_asm!(r#"
.section .text.vectors
.balign 2048
.globl _vectors
_vectors:
    // Current EL with SP_EL0 — we don't use this stack mode.
    b _vec_panic
    .balign 0x80
    b _vec_panic
    .balign 0x80
    b _vec_panic
    .balign 0x80
    b _vec_panic
    .balign 0x80

    // Current EL with SP_ELx (the mode we boot in).
    b _vec_sync         // 0x200 — sync (SVC + data/inst abort)
    .balign 0x80
    b _vec_irq          // 0x280 — IRQ
    .balign 0x80
    b _vec_panic        // 0x300 — FIQ
    .balign 0x80
    b _vec_panic        // 0x380 — SError
    .balign 0x80

    // Lower EL using AArch64 — entry path for traps from EL0
    // (SVC, page faults, IRQs taken while running user code).
    b _vec_sync         // 0x400 — sync (SVC from EL0, EL0 aborts)
    .balign 0x80
    b _vec_irq          // 0x480 — IRQ from EL0
    .balign 0x80
    b _vec_panic        // 0x500 — FIQ from EL0
    .balign 0x80
    b _vec_panic        // 0x580 — SError from EL0
    .balign 0x80

    // Lower EL using AArch32 — we don't run 32-bit userspace.
    b _vec_panic
    .balign 0x80
    b _vec_panic
    .balign 0x80
    b _vec_panic
    .balign 0x80
    b _vec_panic

_vec_panic:
    wfi
    b _vec_panic

// Sync exception entry. Same save/restore frame as _vec_irq, but
// calls `rust_sync_handler(esr, elr, far)` so Rust can decode
// the exception class and either ERET (if recoverable, e.g. SVC)
// or halt. The handler is responsible for advancing ELR past the
// trapping instruction for synchronous exceptions where that's
// required (SVC).
_vec_sync:
    sub  sp, sp, #192
    stp  x0,  x1,  [sp, #(0*16)]
    stp  x2,  x3,  [sp, #(1*16)]
    stp  x4,  x5,  [sp, #(2*16)]
    stp  x6,  x7,  [sp, #(3*16)]
    stp  x8,  x9,  [sp, #(4*16)]
    stp  x10, x11, [sp, #(5*16)]
    stp  x12, x13, [sp, #(6*16)]
    stp  x14, x15, [sp, #(7*16)]
    stp  x16, x17, [sp, #(8*16)]
    stp  x18, lr,  [sp, #(9*16)]
    mrs  x0, ELR_EL1
    mrs  x1, SPSR_EL1
    stp  x0, x1, [sp, #(10*16)]

    // Call Rust: x0=ESR_EL1, x1=ELR_EL1, x2=FAR_EL1
    mrs  x0, ESR_EL1
    mrs  x1, ELR_EL1
    mrs  x2, FAR_EL1
    bl   rust_sync_handler

    // Rust may have advanced ELR_EL1 (e.g. for SVC the saved
    // ELR points AT the `svc` insn — we need it AFTER, but
    // hardware already does that for SVC, so usually no-op).
    ldp  x0, x1, [sp, #(10*16)]
    msr  ELR_EL1, x0
    msr  SPSR_EL1, x1
    ldp  x0,  x1,  [sp, #(0*16)]
    ldp  x2,  x3,  [sp, #(1*16)]
    ldp  x4,  x5,  [sp, #(2*16)]
    ldp  x6,  x7,  [sp, #(3*16)]
    ldp  x8,  x9,  [sp, #(4*16)]
    ldp  x10, x11, [sp, #(5*16)]
    ldp  x12, x13, [sp, #(6*16)]
    ldp  x14, x15, [sp, #(7*16)]
    ldp  x16, x17, [sp, #(8*16)]
    ldp  x18, lr,  [sp, #(9*16)]
    add  sp, sp, #192
    eret

// IRQ entry. AAPCS-clean save of caller-saved regs (x0..x18, lr),
// then ELR_EL1 + SPSR_EL1 for the kernel state we're returning to.
// Total frame: 22 × 8 = 176 bytes, rounded to 192 for 16-byte SP
// alignment.
_vec_irq:
    sub  sp, sp, #192
    stp  x0,  x1,  [sp, #(0*16)]
    stp  x2,  x3,  [sp, #(1*16)]
    stp  x4,  x5,  [sp, #(2*16)]
    stp  x6,  x7,  [sp, #(3*16)]
    stp  x8,  x9,  [sp, #(4*16)]
    stp  x10, x11, [sp, #(5*16)]
    stp  x12, x13, [sp, #(6*16)]
    stp  x14, x15, [sp, #(7*16)]
    stp  x16, x17, [sp, #(8*16)]
    stp  x18, lr,  [sp, #(9*16)]
    mrs  x0, ELR_EL1
    mrs  x1, SPSR_EL1
    stp  x0, x1, [sp, #(10*16)]

    bl   rust_irq_handler

    ldp  x0, x1, [sp, #(10*16)]
    msr  ELR_EL1, x0
    msr  SPSR_EL1, x1
    ldp  x0,  x1,  [sp, #(0*16)]
    ldp  x2,  x3,  [sp, #(1*16)]
    ldp  x4,  x5,  [sp, #(2*16)]
    ldp  x6,  x7,  [sp, #(3*16)]
    ldp  x8,  x9,  [sp, #(4*16)]
    ldp  x10, x11, [sp, #(5*16)]
    ldp  x12, x13, [sp, #(6*16)]
    ldp  x14, x15, [sp, #(7*16)]
    ldp  x16, x17, [sp, #(8*16)]
    ldp  x18, lr,  [sp, #(9*16)]
    add  sp, sp, #192
    eret
"#);

global_asm!(r#"
.section .text.boot
.globl _start
_start:
    // Park secondaries. Aff0 = MPIDR_EL1[7:0].
    mrs     x9, MPIDR_EL1
    and     x9, x9, #0xFF
    cbnz    x9, _park

    // EL2→EL1 trampoline. QEMU virt boots us at EL2 by default
    // (we need EL2 above us so HVC can be serviced by QEMU's
    // emulated PSCI). The trampoline programs HCR_EL2.RW=1,
    // grants EL1 the physical timer + GICv3 sysregs, primes
    // SCTLR_EL1 with the reserved-bits mask, and ERETs into the
    // rest of boot at EL1h with DAIF masked.
    mrs     x10, CurrentEL
    lsr     x10, x10, #2
    cmp     x10, #2
    b.ne    _start_at_el1
    bl      _phanes_drop_to_el1
_start_at_el1:

    // Stack at the top of the static area we reserve below.
    adrp    x9, _stack_top
    add     x9, x9, #:lo12:_stack_top
    mov     sp, x9

    // Zero .bss: from __bss_start to __bss_end, by 8-byte words.
    adrp    x9,  __bss_start
    add     x9,  x9,  #:lo12:__bss_start
    adrp    x10, __bss_end
    add     x10, x10, #:lo12:__bss_end
1:  cmp     x9, x10
    b.ge    2f
    str     xzr, [x9], #8
    b       1b
2:
    bl      rust_main

_park:
    wfi
    b       _park

// (EL2→EL1 trampoline body now lives in
// `arch-aarch64::boot::_phanes_drop_to_el1`. `_start` /
// `_start_secondary` below `bl` into it via that exported symbol.)

// 16 KiB boot stack. Lives in .bss so we don't bloat the binary.
.section .bss
.align 12
.globl _stack_bottom
.globl _stack_top
_stack_bottom:
    .skip 16384
_stack_top:

// 16 KiB stack for hart 1 (brought up via PSCI in B1.gic.smp).
.align 12
.globl _hart1_stack_bottom
.globl _hart1_stack_top
_hart1_stack_bottom:
    .skip 16384
_hart1_stack_top:
"#);

// Secondary CPU entry point. PSCI CPU_ON lands us here with
// `context_id` (the stack-top physical address) in x0. We set
// the stack pointer, then jump to `rust_main_secondary`.
global_asm!(r#"
.section .text.boot
.globl _start_secondary
_start_secondary:
    // Park if this isn't a "real" wake — Aff0 != 1 means PSCI
    // sent the wrong PE here.
    mrs     x9, MPIDR_EL1
    and     x9, x9, #0xFF
    cmp     x9, #1
    b.ne    _park_secondary

    // QEMU PSCI brings the secondary up at the highest implemented
    // EL (EL2 on virt). Drop to EL1 the same way the primary did,
    // preserving the context_id in x0 across ERET (ERET preserves
    // GPRs; only PSTATE changes).
    mrs     x10, CurrentEL
    lsr     x10, x10, #2
    cmp     x10, #2
    b.ne    _sec_at_el1
    bl      _phanes_drop_to_el1
_sec_at_el1:

    // x0 carries the stack-top phys address (context_id from PSCI).
    mov     sp, x0
    bl      rust_main_secondary

_park_secondary:
    wfi
    b       _park_secondary
"#);

// ──────────────────────────────────────────────────────────────────────────
// PL011 UART (just enough to print).
// ──────────────────────────────────────────────────────────────────────────

const PL011_BASE: usize = 0x0900_0000;
const PL011_DR:   usize = 0x000;
const PL011_FR:   usize = 0x018;
const PL011_FR_TXFF: u32 = 1 << 5; // Transmit FIFO full.

fn pl011_putc(c: u8) {
    let fr = (PL011_BASE + PL011_FR) as *const u32;
    let dr = (PL011_BASE + PL011_DR) as *mut u32;
    unsafe {
        while core::ptr::read_volatile(fr) & PL011_FR_TXFF != 0 {}
        core::ptr::write_volatile(dr, c as u32);
    }
}

fn pl011_puts(s: &str) {
    for b in s.bytes() {
        pl011_putc(b);
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Rust entry. After this returns, the trampoline parks on WFI.
// ──────────────────────────────────────────────────────────────────────────

#[no_mangle]
extern "C" fn rust_main() {
    pl011_puts("[AARCH64] hello from PHANES on cortex-a72\n");

    // Prove we can call into `arch-aarch64`. `ARCH_ID` is a
    // const, so no asm runs here yet — but the linker had to
    // resolve the symbol through the arch-aarch64 crate, which
    // proves the cross-crate dependency works on the real target.
    pl011_puts("[AARCH64] arch-api ARCH_ID: ");
    pl011_puts(robot_os_arch_api::arch_name(
        robot_os_arch_aarch64::ARCH_ID,
    ));
    pl011_puts("\n");

    // Exercise one real asm helper end-to-end: read MPIDR_EL1
    // via `arch-aarch64::cpu::hart_id()` and print the low nibble.
    let hart = robot_os_arch_aarch64::cpu::hart_id();
    pl011_puts("[AARCH64] hart_id() returned: 0x");
    let nibble = (hart & 0xF) as u8;
    let hex = if nibble < 10 {
        b'0' + nibble
    } else {
        b'a' + (nibble - 10)
    };
    pl011_putc(hex);
    pl011_puts("\n");

    // B1.gic — GICv3 init smoke. Distributor + this PE's
    // redistributor + CPU interface are programmed in order.
    // We then read ICC_IAR1_EL1; with no IRQ pending it must
    // return the "spurious" magic 1023 (0x3FF). DAIF stays
    // masked so no exceptions fire — this just verifies the
    // GICv3 programming path executes end-to-end.
    //
    // Requires `gic-version=3` on the QEMU machine line; without
    // it QEMU virt models GICv2 and the redistributor MMIO
    // window at 0x080A_0000 isn't mapped.
    // Identify the CPU we're running on via MIDR_EL1 + MPIDR_EL1
    // (consumer of the new `midr` + `mpidr` modules). On QEMU
    // virt -cpu cortex-a72 the expected output is
    //   [CPU]  impl=ARM part=Cortex-A72 (0xd08) aff=0.0.0.0
    {
        let midr = robot_os_arch_aarch64::midr::read_midr();
        let mpidr = robot_os_arch_aarch64::mpidr::read_mpidr();
        pl011_puts("[CPU]  impl=");
        pl011_puts(midr.implementer_name());
        pl011_puts(" part=");
        pl011_puts(midr.part_name());
        pl011_puts(" aff=");
        let to_hex_nib = |n: u8| if n < 10 { b'0' + n } else { b'a' + (n - 10) };
        pl011_putc(to_hex_nib(midr.part_num as u8 >> 4));
        // Print aff0 only (others always 0 on cortex-a72 -smp 2)
        pl011_puts(" hart=");
        pl011_putc(b'0' + (mpidr.aff0 & 0xF));
        pl011_puts("\n");
    }

    robot_os_arch_aarch64::gic::init_distributor();
    robot_os_arch_aarch64::gic::init_redistributor(0);
    robot_os_arch_aarch64::gic::init_cpu_interface();
    let iar = robot_os_arch_aarch64::gic::iar1();
    pl011_puts("[GIC] init OK, IAR1=0x");
    let to_hex = |n: u8| if n < 10 { b'0' + n } else { b'a' + (n - 10) };
    pl011_putc(to_hex(((iar >> 8) & 0xF) as u8));
    pl011_putc(to_hex(((iar >> 4) & 0xF) as u8));
    pl011_putc(to_hex((iar & 0xF) as u8));
    pl011_puts(" (3ff = spurious, no IRQ pending)\n");
    if iar != 1023 {
        robot_os_arch_aarch64::gic::eoir1(iar);
    }

    // B1.gic.handler — install vector table, enable IRQ in DAIF,
    // then fire a Software-Generated Interrupt to ourselves
    // (SGI 0 via ICC_SGI1R_EL1 with TargetList = self). The IRQ
    // entry path saves context, calls `rust_irq_handler` (below),
    // and `eret`s back here.
    extern "C" {
        static _vectors: u8;
    }
    let vbar = unsafe { &_vectors as *const u8 as u64 };
    robot_os_arch_aarch64::sysregs::set_vbar_el1(vbar);

    // Enable SGI 0 (per-PE) in the redistributor SGI_base. SGI
    // intids 0..15 live at GICR_SGI_OFFSET + ISENABLER0[bit].
    unsafe {
        let isenabler =
            (0x080A_0000usize + 0x1_0000 + 0x100) as *mut u32;
        core::ptr::write_volatile(isenabler, 1u32 << 0);
    }

    robot_os_arch_aarch64::sysregs::enable_irq();
    pl011_puts("[IRQ]  vectors installed, DAIF.I cleared\n");

    // ICC_SGI1R_EL1 self-target: TargetList lives in bits [15:0]
    // (one bit per Aff0 within the addressed Aff1/Aff2/Aff3
    // cluster). For self on PE0, Aff*=0 + bit 0 of the target
    // list. INTID=0 in bits [27:24], IRM=0 in bit 40.
    let sgi: u64 = 1u64;
    unsafe {
        core::arch::asm!(
            "msr ICC_SGI1R_EL1, {0}",
            "isb",
            in(reg) sgi,
            options(nomem, nostack, preserves_flags),
        );
    }

    // Brief poll so the IRQ has time to fire before we WFI.
    for _ in 0..1_000_000 {
        core::hint::spin_loop();
    }

    // B1.gic.sync — trigger a Supervisor Call from EL1.
    // ESR_EL1.EC should report 0x15 (SVC from AArch64), the sync
    // handler prints it and ERETs back here.
    pl011_puts("[SYNC] triggering SVC #0\n");
    unsafe {
        core::arch::asm!("svc #0", options(nostack));
    }
    pl011_puts("[SYNC] returned from SVC handler\n");

    // B1.gic.smp — bring up hart 1 via PSCI CPU_ON, then send a
    // cross-core SGI to it. The _drop_to_el1 trampoline ran
    // earlier in boot, so we're at EL1 with EL2 still above us to
    // service HVC #0 (QEMU virt routes PSCI through HVC).
    extern "C" {
        static _start_secondary: u8;
        static mut _hart1_stack_top: u8;
    }
    let entry = unsafe { &_start_secondary as *const u8 as u64 };
    let stack_top = &raw mut _hart1_stack_top as u64;
    let rc = robot_os_arch_aarch64::psci::cpu_on(1, entry, stack_top);
    pl011_puts("[SMP]  PSCI CPU_ON(1) rc=");
    let smp_to_hex = |n: u8| if n < 10 { b'0' + n } else { b'a' + (n - 10) };
    pl011_putc(if rc < 0 { b'-' } else { b'+' });
    pl011_putc(smp_to_hex((rc.unsigned_abs() & 0xF) as u8));
    pl011_puts("\n");

    // Wait for hart 1 to flag itself ready (it sets HART1_READY
    // only after its own per-PE GIC init), then send the SGI.
    while !HART1_READY.load(core::sync::atomic::Ordering::Acquire) {
        core::hint::spin_loop();
    }
    pl011_puts("[SMP]  hart 1 ready — sending cross-core SGI\n");

    // ICC_SGI1R_EL1: TargetList bit 1 = Aff0=1 within Aff1=0.
    let sgi: u64 = 1u64 << 1;
    unsafe {
        core::arch::asm!(
            "msr ICC_SGI1R_EL1, {0}",
            "isb",
            in(reg) sgi,
            options(nomem, nostack, preserves_flags),
        );
    }

    // Let hart 1's IRQ handler print before we move on.
    for _ in 0..2_000_000 {
        core::hint::spin_loop();
    }

    // B1.gic.timer — program the generic timer to fire ~250 ms
    // from now, enable PPI 30 (EL1 physical timer interrupt) in
    // this PE's redistributor, then spin. When the comparator
    // catches up to CNTPCT_EL0 the IRQ should fire and our
    // handler print "[IRQ] fired! INTID=0x01e" (30 decimal).
    // The freq/4 incantation is now `arm_deadline_us(250_000)` —
    // 250 ms. Backend math (read CNTFRQ, scale ns → ticks, set
    // CNTP_CVAL_EL0, set CNTP_CTL_EL0.ENABLE) lives in the
    // shared `timer` module.
    robot_os_arch_aarch64::timer::arm_deadline_us(250_000);
    robot_os_arch_aarch64::gic::enable_ppi(0, 30);
    pl011_puts("[TIMER] PPI 30 armed for +250 ms\n");

    // Spin long enough that the timer IRQ has every chance to
    // dispatch (TCG timing is non-deterministic — overcompensate).
    for _ in 0..5_000_000 {
        core::hint::spin_loop();
    }

    // B1.mmu.boot — turn on VMSAv8-64 stage-1 translation at EL1
    // with an identity map covering everything we touch:
    //   [0x00000000, 0x40000000)  Device-nGnRE  (GIC + PL011 + flash)
    //   [0x40000000, 0x80000000)  Normal WB IS  (RAM, code, .bss)
    // Single L0 entry → L1 with two 1 GiB block descriptors.
    // Survival = PL011 still works after SCTLR.M=1 = identity map ok.
    enable_mmu_identity();
    pl011_puts("[MMU]  M+I+C enabled — pl011 reachable through translation\n");

    // B1.user.boot — drop into EL0 and observe user_main's SVC.
    //   - Lower-EL-AArch64 sync + IRQ slots in `_vectors` route
    //     to `_vec_sync` / `_vec_irq` (was: `_vec_panic`).
    //   - `drop_to_user_mode()` writes SP_EL0/ELR_EL1/SPSR_EL1
    //     and ERETs into user_main at its real PA — possible
    //     because B1.user.split builds L1→L2→L3 with per-page AP
    //     so the user_main + USER_STACK pages carry AP=01 while
    //     the kernel keeps AP=00 on every other page.
    //   - user_main fires SVC #0xbe; the sync handler prints
    //     `imm=0x00be (from EL0 user_main)` and ERETs back.
    drop_to_user_mode(user_main as *const () as u64);
    // Unreachable — drop_to_user_mode ends in `eret` and we
    // never come back to EL1 here. (When user_main fires SVC
    // the sync handler returns *to user_main*, not here.)
    pl011_puts("[USER] unreachable — drop_to_user_mode returned!\n");
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)); }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// EL0 user-mode demo (B1.user.boot).
//
// `user_main` runs at EL0 with its own stack. It triggers a
// synchronous trap (SVC #0xbe) so the kernel can observe it,
// then parks on WFI. SCTLR_EL1.nTWI=1 (set by EL2 trampoline)
// means WFI at EL0 doesn't trap — it just waits.
// ──────────────────────────────────────────────────────────────────────────

/// 4 KiB user stack — small but sufficient for SVC + WFI demo.
/// Lives in its own linker section (`.user_bss`) on a fresh 4 KiB
/// boundary so the L3 entry we widen to AP[2:1]=01 covers exactly
/// USER_STACK and nothing else.
#[repr(C, align(4096))]
struct UserStack([u8; 4096]);
#[link_section = ".user_bss"]
static mut USER_STACK: UserStack = UserStack([0; 4096]);

/// Address of the top of `USER_STACK`. AAPCS wants 16-byte
/// alignment, which the `repr(align(16))` already guarantees.
fn user_stack_top() -> u64 {
    unsafe { (&raw mut USER_STACK as *mut u8).add(4096) as u64 }
}

// user_main lives in raw asm + a custom `.user_text` section so
// the linker actually keeps it on its own 4 KiB page (Rust's
// `#[link_section]` on extern fns isn't honoured by rustc here —
// the function ends up back in default .text under build-std).
global_asm!(r#"
.section .user_text, "ax"
.globl user_main
user_main:
    svc #0xbe
1:  wfi
    b 1b
"#);

extern "C" {
    fn user_main() -> !;
}

fn drop_to_user_mode(user_pc: u64) -> ! {
    pl011_puts("[USER] preparing drop to EL0\n");
    let sp = user_stack_top();
    pl011_puts("[USER] dropping to EL0 — user_main entry\n");
    // ERET sequence lives in arch-aarch64::boot now — same four
    // instructions, just shared so any future kernel can call it.
    unsafe { robot_os_arch_aarch64::boot::eret_to_el0(user_pc, sp) }
}

// ──────────────────────────────────────────────────────────────────────────
// MMU identity-map demo (B1.mmu.boot).
//
// Page-table descriptors at level 1 with 4 KiB granule are 1 GiB
// blocks. Layout per Arm ARM §D8.3:
//   [0]       valid=1
//   [1]       =0 → block descriptor
//   [5:2]     AttrIndx into MAIR_EL1
//   [7:6]     AP[2:1]   (00 = EL1 RW, kernel-only)
//   [9:8]     SH        (00 = non, 11 = Inner shareable)
//   [10]      AF        (Access Flag — set so first access doesn't fault)
//   [47:30]   output address bits [47:30]
//
// `T0SZ = 25` (39-bit input) starts the table walk at level 1 —
// so TTBR0_EL1 points straight at our L1 table; no L0 needed.
// (T0SZ ≤ 24 would require a level-0 walk and an extra table.)
// ──────────────────────────────────────────────────────────────────────────

// Page tables now use `robot_os_arch_aarch64::mmu_setup::PageTable`.
use robot_os_arch_aarch64::mmu_setup::PageTable;

/// Level-1 root table — TTBR0_EL1 points here.
static mut L1_TABLE: PageTable = PageTable::zero();
/// Level-2 table covering the first 1 GiB of DRAM in 2 MiB chunks.
static mut L2_TABLE: PageTable = PageTable::zero();
/// Level-3 table covering the first 2 MiB of DRAM in 4 KiB pages.
static mut L3_TABLE: PageTable = PageTable::zero();

/// DRAM base on QEMU virt cortex-a72. The 1 GiB starting here is
/// mapped Normal WB IS through L1[1] → L2 → L3.
const BASE_PA: u64 = 0x4000_0000;

fn enable_mmu_identity() {
    use robot_os_arch_aarch64::mmu_setup::{enable_identity_map, IdentityMapConfig};

    unsafe {
        // The page-table layout (L1[0] Device + L1[1] → L2 + L2[0]
        // → L3 with the .user_text page AP=01 and the .user_bss
        // page AP=01+UXN+PXN) is the canonical B1.user.split setup.
        // arch-aarch64::mmu_setup owns the encoding so this binary
        // only has to declare its tables + user-page addresses.
        enable_identity_map(IdentityMapConfig {
            l1: &raw mut L1_TABLE,
            l2: &raw mut L2_TABLE,
            l3: &raw mut L3_TABLE,
            base_pa: BASE_PA,
            user_code_pa: Some(user_main as *const () as u64),
            user_stack_pa: Some(&raw const USER_STACK as u64),
        });
    }
}

/// Sync exception handler. Decodes ESR_EL1 + prints; for SVC #0
/// it returns normally (hardware has already advanced ELR past
/// the `svc` instruction). For anything else it halts after
/// printing the cause so we don't loop on a hard fault.
#[no_mangle]
extern "C" fn rust_sync_handler(esr: u64, elr: u64, far: u64) {
    // (Tried wrapping the handler in fp_state::save/restore as a
    // consumer-exercise commit. The very first SVC happens BEFORE
    // enable_mmu_identity's Rust-side CPACR_EL1.FPEN write — and
    // for reasons that need debugging, the trampoline's earlier
    // CPACR write isn't actually enabling FP in time. Save FP
    // hangs the first SVC. Rolled back; consumer of fp_state will
    // be the real kernel once its boot order is correct.)

    // ESR_EL1.EC is bits [31:26]. Common codes:
    //   0x15 = SVC from AArch64
    //   0x21 = Instruction abort, current EL
    //   0x25 = Data abort, current EL
    //   0x26 = SP alignment fault
    //   0x2C = FP/SIMD trap
    let ec = (esr >> 26) & 0x3F;
    let iss = esr & 0x1FF_FFFF;
    pl011_puts("[SYNC] ESR_EL1=0x");
    let to_hex = |n: u8| if n < 10 { b'0' + n } else { b'a' + (n - 10) };
    for shift in (0..64).step_by(4).rev() {
        pl011_putc(to_hex(((esr >> shift) & 0xF) as u8));
    }
    pl011_puts(" EC=0x");
    pl011_putc(to_hex(((ec >> 4) & 0xF) as u8));
    pl011_putc(to_hex((ec & 0xF) as u8));
    pl011_puts(" ELR=0x");
    for shift in (0..64).step_by(4).rev() {
        pl011_putc(to_hex(((elr >> shift) & 0xF) as u8));
    }
    pl011_puts("\n");
    let _ = far;
    let _ = iss;

    // ESR_EL1.ISS for SVC: bits [15:0] = imm16 from the SVC
    // instruction. Useful so we can tell SVC #0 (from EL1 in
    // _start) apart from SVC #0xbe (from EL0 in user_main).
    match ec {
        0x15 => {
            let imm = (esr & 0xFFFF) as u16;
            pl011_puts("[SYNC] SVC from AArch64 — imm=0x");
            pl011_putc(to_hex(((imm >> 12) & 0xF) as u8));
            pl011_putc(to_hex(((imm >> 8) & 0xF) as u8));
            pl011_putc(to_hex(((imm >> 4) & 0xF) as u8));
            pl011_putc(to_hex((imm & 0xF) as u8));
            // Tag whether the SVC came from EL0 (B1.user.boot) or
            // EL1 (B1.gic.sync) so the boot log is unambiguous.
            // The vector entry that ran tells us this, but here
            // we just print the imm — see vector ordering in
            // _vectors.
            if imm == 0xBE {
                pl011_puts(" (from EL0 user_main)\n");
            } else {
                pl011_puts(" (from EL1)\n");
            }
        }
        _ => {
            pl011_puts("[SYNC] unhandled — halting\n");
            loop {
                unsafe { core::arch::asm!("wfi", options(nomem, nostack)); }
            }
        }
    }
}

/// Called from `_vec_irq` after caller-saved regs + ELR/SPSR are
/// stacked. Reads ICC_IAR1_EL1, prints the interrupt ID, then
/// signals end-of-interrupt via ICC_EOIR1_EL1.
#[no_mangle]
extern "C" fn rust_irq_handler() {
    let hart = robot_os_arch_aarch64::cpu::hart_id();
    let id = robot_os_arch_aarch64::gic::iar1();
    pl011_puts("[IRQ-");
    let h_to_hex = |n: u8| if n < 10 { b'0' + n } else { b'a' + (n - 10) };
    pl011_putc(h_to_hex((hart & 0xF) as u8));
    pl011_puts("] fired! INTID=0x");
    pl011_putc(h_to_hex(((id >> 8) & 0xF) as u8));
    pl011_putc(h_to_hex(((id >> 4) & 0xF) as u8));
    pl011_putc(h_to_hex((id & 0xF) as u8));
    pl011_puts("\n");
    if id < 1020 {
        robot_os_arch_aarch64::gic::eoir1(id);
    }
}

/// Set by `rust_main_secondary` once hart 1 has finished its
/// per-PE GIC + CPU interface init and is ready to receive IPIs.
static HART1_READY: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Hart 1 entry. Initialises only the per-PE pieces (the
/// distributor was already programmed by hart 0). Installs the
/// same exception vectors, enables IRQs, flags itself ready, then
/// parks on WFI awaiting IPIs.
#[no_mangle]
extern "C" fn rust_main_secondary() {
    pl011_puts("[SMP]  hart 1 alive, initialising per-PE GIC\n");

    // Per-PE GIC: redistributor wake-up + CPU interface enable.
    // The distributor itself was already programmed by hart 0.
    robot_os_arch_aarch64::gic::init_redistributor(1);
    robot_os_arch_aarch64::gic::init_cpu_interface();
    // Enable SGI 0 on this PE.
    robot_os_arch_aarch64::gic::enable_ppi(1, 0);

    // Same vector table as hart 0 — VBAR_EL1 is per-CPU.
    extern "C" {
        static _vectors: u8;
    }
    let vbar = unsafe { &_vectors as *const u8 as u64 };
    robot_os_arch_aarch64::sysregs::set_vbar_el1(vbar);
    robot_os_arch_aarch64::sysregs::enable_irq();

    HART1_READY.store(true, core::sync::atomic::Ordering::Release);
    pl011_puts("[SMP]  hart 1 ready, parking on WFI\n");
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)); }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Panic handler — required by no_std.
// ──────────────────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    pl011_puts("[AARCH64] PANIC\n");
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)); }
    }
}
