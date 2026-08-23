//! Per-arch trap entry — Item 2 Stage 3 batch 7 (RFC option (c)).
//!
//! Each ISA owns its native TrapFrame and trap-entry asm wrapper.
//! The shared kernel trap-handler logic never sees TrapFrame
//! directly — it operates through the [`TrapContext`] trait so
//! the same source (syscall dispatch, page-fault diagnostic,
//! interrupt routing) compiles for every target without
//! generics-monomorphisation tax (`&mut dyn TrapContext`).
//!
//! ## Why option (c) over (a) or (b)
//!
//! See `rfcs/item-2-kernel-cross-arch-plan.md` §"TrapFrame —
//! the hardest call-site" for the full rationale. Short version:
//!
//! - (a) `fn handle<T: TrapContext>(frame: &mut T)` forces every
//!   trap-touching site to be generic → monomorphisation bloat
//!   per ISA.
//! - (b) "thinnest common subset" struct loses fidelity (we use
//!   ISA-specific fault registers in COW/demand-paging logic).
//! - (c) per-arch entry + small dyn-dispatched shim is what
//!   seL4, Hubris, and Linux's `pt_regs` accessors do — minimal
//!   bloat, full per-ISA fidelity, shared logic stays portable.
//!
//! ## Status
//!
//! Scaffolding: the trait + per-arch impls + skeleton entry
//! functions exist and compile on every target. Wiring the
//! kernel's existing `trap_handler` in `kernel/src/main.rs` to
//! call through this trait is S3.b7.next — leaving the existing
//! riscv64 path untouched today so the production boot keeps
//! working byte-for-byte.

pub mod aarch64;
pub mod riscv64;
pub mod x86_64;

/// Active per-arch entry impl. Routes via `cfg(target_arch)` so
/// callers write `entry::current::TrapFrame` without an `if/else`
/// ladder. Mirror of how `crates/arch` facade-routes the ISA
/// crates. `allow(unused_imports)` is intentional scaffolding:
/// the kernel's existing `trap_handler` still talks to
/// `robot_os_arch::trap::TrapFrame` directly. Task #210
/// (S3.b7.next) flips the shared handler to consume
/// `&mut dyn TrapContext`, at which point these aliases become
/// the canonical access path and the allow goes away.
#[allow(unused_imports)]
#[cfg(target_arch = "riscv64")]
pub use riscv64 as current;

#[allow(unused_imports)]
#[cfg(target_arch = "aarch64")]
pub use aarch64 as current;

#[allow(unused_imports)]
#[cfg(target_arch = "x86_64")]
pub use x86_64 as current;

// ── TrapContext trait ──────────────────────────────────────────
//
// Everything the shared trap-handler needs from a per-arch
// TrapFrame, narrowed to a portable surface. Keep it small —
// each new method we add multiplies the per-ISA work for S3.b7
// follow-ups.

/// Class of trap, abstracted over the ISA-native cause encoding.
/// `cause()` returns the raw ISA value (so per-arch diagnostic
/// printing still works); `class()` is the portable categoriser
/// the shared handler dispatches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapClass {
    /// Hardware interrupt (timer, IPI, external). The shared
    /// handler routes by IRQ number which the per-arch layer
    /// reports separately via `irq_number()`.
    Interrupt,
    /// User-mode system call (ecall / svc / syscall / int 0x80).
    /// `syscall_arg(n)` + `set_syscall_return(v)` + `advance_pc()`
    /// handle the calling convention.
    Syscall,
    /// Page fault. `fault_addr()` + `cause()` + per-arch detail
    /// in the shared `handle_page_fault` routine.
    PageFault,
    /// Any other synchronous exception (illegal instruction,
    /// alignment, etc.). The shared handler prints + kills the
    /// task; per-arch detail comes from `cause()`.
    OtherException,
}

/// Per-arch TrapFrame surface the shared kernel logic uses.
/// `&mut dyn TrapContext` is the cross-arch type the shared
/// handler receives.
pub trait TrapContext {
    /// Raw ISA cause register value (scause / esr_el1 / vector
    /// number + error code packed). Used for diagnostics; the
    /// portable dispatch uses [`class`](Self::class).
    fn cause(&self) -> usize;

    /// Categorise the trap into a portable [`TrapClass`].
    fn class(&self) -> TrapClass;

    /// IRQ number (when `class() == Interrupt`). On RISC-V this
    /// is `cause & !INTERRUPT_BIT`; on aarch64 the IAR1 value;
    /// on x86_64 the IDT vector index.
    fn irq_number(&self) -> usize;

    /// Faulting virtual address (when `class() == PageFault`).
    /// `stval` / `far_el1` / `cr2`.
    fn fault_addr(&self) -> usize;

    /// Faulting instruction's PC (`sepc` / `elr_el1` / `rip`).
    fn pc(&self) -> usize;

    /// Set the post-trap PC. Used to advance past `ecall` / `svc`
    /// / `int` so the syscall doesn't re-trigger forever, or to
    /// redirect to a new entry point for `exec_user`.
    fn set_pc(&mut self, pc: usize);

    /// Read user stack pointer at the point of trap (`x2` /
    /// `sp_el0` / `rsp`). Used by `fork()` for COW page-table
    /// duplication.
    fn user_sp(&self) -> usize;

    /// True iff the trap came from user mode. RISC-V reads
    /// `sstatus.SPP`; aarch64 reads `spsr_el1.M[3:0]`; x86_64
    /// reads CS RPL.
    fn came_from_user(&self) -> bool;

    /// System-call number (`a7` / `x8` / `rax`).
    fn syscall_number(&self) -> usize;

    /// Read syscall argument `n` (0..=5). RISC-V `a0..a5`,
    /// aarch64 `x0..x5`, x86_64 `rdi rsi rdx r10 r8 r9`.
    fn syscall_arg(&self, n: usize) -> usize;

    /// Set the syscall return value (a0 / x0 / rax).
    fn set_syscall_return(&mut self, v: usize);
}
