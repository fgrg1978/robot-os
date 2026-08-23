//! Local APIC driver (xAPIC, MMIO at 0xFEE00000).
//!
//! Implements the two pieces of the `Interrupts` trait that
//! `api_impl::X86_64` used to leave as `unimplemented!()`:
//!
//!   - `set_timer_deadline(ticks)` → LVT timer in TSC-deadline
//!     mode if the CPU supports it, otherwise one-shot Initial-
//!     Count mode at the current bus frequency.
//!   - `send_ipi(target_hart)` → ICR write targeting the
//!     destination's APIC ID.
//!
//! This is the x86_64 mirror of `arch-aarch64::gic`. The APIC has
//! to be set up *once* (calibrate timer + enable spurious vector)
//! before either function works — that happens in
//! [`init_local_apic`], which the kernel boot path will call after
//! ACPI MADT parsing tells it where the APIC base is.
//!
//! Limitations:
//!   - xAPIC only. x2APIC support (MSR-based, since Sandy Bridge)
//!     is a B2.apic.x2 follow-up — it changes the access path
//!     but not the surface.
//!   - Single APIC (BSP only). Per-CPU APIC init for SMP lands
//!     with B2.boot.real once we can actually start a second hart
//!     via APIC INIT/SIPI.

#![allow(dead_code)] // wired up by api_impl once kernel boot exists

#[cfg(target_arch = "x86_64")]
use core::ptr::{read_volatile, write_volatile};

/// Default physical address of the local APIC MMIO region per
/// the architecture spec. The actual base is reported in
/// `IA32_APIC_BASE` MSR bits [35:12]; on QEMU and most real
/// platforms it's the default.
pub const APIC_BASE_PA: usize = 0xFEE0_0000;

/// Register offsets from the APIC base. Each register is 16-byte
/// aligned and 32 bits wide in the xAPIC layout.
mod reg {
    pub const ID:              usize = 0x020; // Local APIC ID
    pub const VERSION:         usize = 0x030; // Local APIC Version
    pub const TPR:             usize = 0x080; // Task Priority
    pub const EOI:             usize = 0x0B0; // End of Interrupt
    pub const LDR:             usize = 0x0D0; // Logical Destination
    pub const DFR:             usize = 0x0E0; // Destination Format
    pub const SVR:             usize = 0x0F0; // Spurious Interrupt Vector
    pub const ICR_LO:          usize = 0x300; // Interrupt Command (low)
    pub const ICR_HI:          usize = 0x310; // Interrupt Command (high)
    pub const LVT_TIMER:       usize = 0x320;
    pub const TIMER_INIT_CNT:  usize = 0x380;
    pub const TIMER_CUR_CNT:   usize = 0x390;
    pub const TIMER_DIV_CFG:   usize = 0x3E0;
}

// ── SVR bits ─────────────────────────────────────────────────
/// Spurious vector enable bit (bit 8). Without this the APIC is
/// "soft-disabled" and most LVT entries are ignored.
pub const SVR_ENABLE: u32 = 1 << 8;
/// Spurious interrupt vector — by convention 0xFF (highest).
pub const SPURIOUS_VECTOR: u32 = 0xFF;

// ── LVT_TIMER fields ─────────────────────────────────────────
/// Timer mode in bits [18:17].
///   00 = One-shot (write Initial Count)
///   01 = Periodic
///   10 = TSC-Deadline (write IA32_TSC_DEADLINE MSR)
pub const TIMER_MODE_ONESHOT:   u32 = 0b00 << 17;
pub const TIMER_MODE_PERIODIC:  u32 = 0b01 << 17;
pub const TIMER_MODE_TSC_DEADLINE: u32 = 0b10 << 17;
/// LVT mask bit (bit 16). 1 = interrupt suppressed.
pub const LVT_MASKED: u32 = 1 << 16;
/// Default vector for timer IRQs.
pub const TIMER_VECTOR: u32 = 0x40;

// ── TIMER_DIV_CFG values (bits [3, 1:0] — packed weirdly) ────
/// Divide by 16 — common default that gives a usable tick window.
pub const TIMER_DIV_16: u32 = 0b0011;

// ── ICR bits ────────────────────────────────────────────────
/// Vector field in bits [7:0].
pub const fn icr_vector(v: u8) -> u32 { v as u32 }
/// Delivery mode "Fixed" (000) = use the vector field as-is.
pub const ICR_DELIVERY_FIXED: u32 = 0b000 << 8;
/// Delivery mode "INIT" — used for SMP bring-up.
pub const ICR_DELIVERY_INIT:  u32 = 0b101 << 8;
/// Delivery mode "Start-Up" (SIPI).
pub const ICR_DELIVERY_SIPI:  u32 = 0b110 << 8;
/// Destination mode physical (0) vs logical (1).
pub const ICR_DEST_PHYSICAL: u32 = 0;
/// Level: 1 = assert. (Only meaningful for INIT-deassert.)
pub const ICR_LEVEL_ASSERT: u32 = 1 << 14;
/// Delivery status (RO bit 12). Polled to check IPI completion.
pub const ICR_DELIVERY_PENDING: u32 = 1 << 12;
/// Destination shorthand: 00 = no shorthand (use ICR_HI target),
/// 11 = "All Excluding Self".
pub const ICR_DEST_NO_SHORTHAND: u32 = 0b00 << 18;

/// Single-process state. The kernel sets the APIC base via
/// [`init_local_apic`] before any timer / IPI call; pre-init
/// reads return 0 from the dummy mapping and post-init reads
/// hit the real APIC.
static mut APIC_BASE: usize = APIC_BASE_PA;
/// Calibrated timer ticks per second of wall-clock time. 0 means
/// "not yet calibrated" — `set_timer_deadline` then falls back
/// to writing `deadline_ticks` directly into Initial Count.
static mut TIMER_HZ: u64 = 0;
/// Active mode — set by `init_local_apic` / `init_x2apic`.
/// Atomic so the per-op dispatcher (read_reg / write_reg) can
/// see updates without taking a lock.
static MODE: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(MODE_XAPIC);
const MODE_XAPIC:  u8 = 0;
const MODE_X2APIC: u8 = 1;

// ── x2APIC MSR addresses (Intel SDM Vol. 4 §10.12) ───────────
// xAPIC MMIO offsets map 1:1 to MSR addresses 0x800+(offset/16).
const X2APIC_MSR_BASE:     u32 = 0x800;
const X2APIC_MSR_EOI:      u32 = 0x80B;
const X2APIC_MSR_ICR:      u32 = 0x830; // single 64-bit MSR (no HI)
const X2APIC_MSR_LVT_TIMER:u32 = 0x832;
const X2APIC_MSR_INIT_CNT: u32 = 0x838;
const X2APIC_MSR_DIV_CFG:  u32 = 0x83E;

/// `IA32_APIC_BASE` MSR (0x1B). Bit 11 = APIC global enable;
/// bit 10 = x2APIC mode (EXTD); bits [35:12] = base PA (xAPIC
/// only — ignored in x2APIC mode).
const IA32_APIC_BASE_MSR: u32 = 0x1B;
const APIC_BASE_GLOBAL_ENABLE: u64 = 1 << 11;
const APIC_BASE_EXTD:          u64 = 1 << 10;

#[cfg(target_arch = "x86_64")]
use crate::msr::{rdmsr, wrmsr};

/// `true` if CPUID reports x2APIC support (leaf 1 ECX bit 21).
#[cfg(target_arch = "x86_64")]
pub fn cpu_has_x2apic() -> bool {
    let cpuid = unsafe { core::arch::x86_64::__cpuid(1) };
    (cpuid.ecx >> 21) & 1 != 0
}

/// Enable x2APIC mode for this CPU.
///
/// After this returns, register accesses go through MSRs at
/// 0x800+(offset/16) instead of MMIO at APIC_BASE+offset. ICR
/// becomes a single 64-bit MSR (no separate HI). Destination
/// IDs widen from 8 to 32 bits.
///
/// # Safety
/// CPU must support x2APIC (check [`cpu_has_x2apic`] first).
/// Must be called once per CPU during boot.
#[cfg(target_arch = "x86_64")]
pub unsafe fn enable_x2apic() {
    unsafe {
        let mut base = rdmsr(IA32_APIC_BASE_MSR);
        base |= APIC_BASE_GLOBAL_ENABLE | APIC_BASE_EXTD;
        wrmsr(IA32_APIC_BASE_MSR, base);
        MODE.store(MODE_X2APIC, core::sync::atomic::Ordering::Release);
    }
}

/// Init the local APIC in x2APIC mode + program common defaults.
/// Combines [`enable_x2apic`] + the SVR / TPR / timer-divide
/// writes that `init_local_apic` does for xAPIC.
#[cfg(target_arch = "x86_64")]
pub unsafe fn init_x2apic(timer_hz: u64) {
    unsafe {
        enable_x2apic();
        TIMER_HZ = timer_hz;
        // SVR = enable + spurious vector. MSR is APIC_BASE+0xF0/16 = 0x80F.
        wrmsr(0x80F, (SVR_ENABLE | SPURIOUS_VECTOR) as u64);
        // TPR = 0 (accept all).
        wrmsr(0x808, 0);
        // Timer divide /16.
        wrmsr(X2APIC_MSR_DIV_CFG, TIMER_DIV_16 as u64);
    }
}

/// Initialise the local APIC: set base address, enable via SVR,
/// program timer divide, and (optionally) calibrate.
///
/// `apic_base_pa` should come from the ACPI MADT (B2.acpi) — pass
/// [`APIC_BASE_PA`] for the architectural default if MADT isn't
/// parsed yet. `timer_hz` is the calibrated bus frequency; pass
/// 0 to skip calibration (the kernel can fill it in later via
/// [`set_timer_hz`]).
///
/// # Safety
///
/// Must be called exactly once, by the BSP, before any other
/// function in this module.
#[cfg(target_arch = "x86_64")]
pub unsafe fn init_local_apic(apic_base_pa: usize, timer_hz: u64) {
    unsafe {
        APIC_BASE = apic_base_pa;
        TIMER_HZ = timer_hz;
        // Enable APIC via SVR: software-enable + spurious vector 0xFF.
        write_reg(reg::SVR, SVR_ENABLE | SPURIOUS_VECTOR);
        // TPR = 0 → accept all priority IRQs.
        write_reg(reg::TPR, 0);
        // Timer divide = 16 (common default).
        write_reg(reg::TIMER_DIV_CFG, TIMER_DIV_16);
    }
}

/// Update the calibrated timer frequency post-boot (e.g. after a
/// PIT or HPET calibration loop has measured the APIC bus).
#[cfg(target_arch = "x86_64")]
pub fn set_timer_hz(hz: u64) {
    // SAFETY: single-writer assumption — only the BSP calls this
    // during boot/calibration.
    unsafe { TIMER_HZ = hz; }
}

/// Program the local-APIC timer to fire once in
/// `deadline_ticks_since_now` units of the calibrated frequency.
///
/// The interrupt vector is fixed at [`TIMER_VECTOR`] (0x40); the
/// kernel IDT entry for that vector should call into the
/// scheduler. If [`TIMER_HZ`] is zero (no calibration), the
/// argument is written into Initial Count directly — that's the
/// "raw APIC bus ticks" interpretation. Works transparently in
/// both xAPIC and x2APIC mode via the [`MODE`] dispatch.
#[cfg(target_arch = "x86_64")]
pub fn set_timer_deadline(deadline_ticks_since_now: u64) {
    // SAFETY: TIMER_HZ + APIC_BASE are single-writer; ok to read.
    let hz = unsafe { TIMER_HZ };
    let init = if hz == 0 {
        deadline_ticks_since_now as u32
    } else {
        let cnt = deadline_ticks_since_now
            .saturating_mul(hz / TIMER_DIV_16 as u64);
        if cnt > u32::MAX as u64 { u32::MAX } else { cnt as u32 }
    };
    unsafe {
        if MODE.load(core::sync::atomic::Ordering::Acquire) == MODE_X2APIC {
            wrmsr(X2APIC_MSR_LVT_TIMER,
                  (TIMER_MODE_ONESHOT | TIMER_VECTOR) as u64);
            wrmsr(X2APIC_MSR_INIT_CNT, init as u64);
        } else {
            write_reg(reg::LVT_TIMER, TIMER_MODE_ONESHOT | TIMER_VECTOR);
            write_reg(reg::TIMER_INIT_CNT, init);
        }
    }
}

/// Send an inter-processor interrupt with vector [`TIMER_VECTOR`]
/// to `target_apic_id`. The destination ID is 8-bit in xAPIC
/// mode and 32-bit in x2APIC mode — caller passes a u32 to keep
/// one API across modes; xAPIC silently truncates the high bits.
#[cfg(target_arch = "x86_64")]
pub fn send_ipi(target_apic_id: u32) {
    send_ipi_with_vector(target_apic_id, TIMER_VECTOR as u8);
}

/// Like [`send_ipi`] but with a caller-chosen vector — useful
/// when the kernel wants to multiplex multiple IPI causes
/// (reschedule, TLB shootdown, halt).
///
/// `target_apic_id` is a u32 because x2APIC widens destination
/// IDs to 32 bits. Callers in xAPIC mode pass a value 0..=255.
#[cfg(target_arch = "x86_64")]
pub fn send_ipi_with_vector(target_apic_id: u32, vector: u8) {
    let low = icr_vector(vector)
        | ICR_DELIVERY_FIXED
        | ICR_DEST_PHYSICAL
        | ICR_LEVEL_ASSERT
        | ICR_DEST_NO_SHORTHAND;
    unsafe {
        if MODE.load(core::sync::atomic::Ordering::Acquire) == MODE_X2APIC {
            // Single 64-bit MSR write — no delivery-pending poll
            // needed; x2APIC ICR writes are serialising by spec.
            let icr = ((target_apic_id as u64) << 32) | (low as u64);
            wrmsr(X2APIC_MSR_ICR, icr);
        } else {
            // xAPIC: 8-bit dest ID in bits [31:24] of ICR_HI, then
            // ICR_LO write triggers the IPI. Poll until cleared.
            write_reg(reg::ICR_HI, (target_apic_id & 0xFF) << 24);
            write_reg(reg::ICR_LO, low);
            while read_reg(reg::ICR_LO) & ICR_DELIVERY_PENDING != 0 {
                core::hint::spin_loop();
            }
        }
    }
}

/// Signal end-of-interrupt to the local APIC. The IDT handler
/// must call this before iret, otherwise lower-priority IRQs
/// stay masked at this PE.
#[cfg(target_arch = "x86_64")]
pub fn eoi() {
    unsafe {
        if MODE.load(core::sync::atomic::Ordering::Acquire) == MODE_X2APIC {
            wrmsr(X2APIC_MSR_EOI, 0);
        } else {
            write_reg(reg::EOI, 0);
        }
    }
}

/// Read this PE's local APIC ID (low 8 bits of register 0x020,
/// shifted out of bits [31:24]).
#[cfg(target_arch = "x86_64")]
pub fn local_apic_id() -> u8 {
    unsafe { (read_reg(reg::ID) >> 24) as u8 }
}

// ── Internal MMIO helpers ────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn write_reg(offset: usize, val: u32) {
    let p = (APIC_BASE + offset) as *mut u32;
    unsafe { write_volatile(p, val); }
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn read_reg(offset: usize) -> u32 {
    let p = (APIC_BASE + offset) as *const u32;
    unsafe { read_volatile(p) }
}

// ── Host-build stubs (cross-target compile) ──────────────────

#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn init_local_apic(_apic_base_pa: usize, _timer_hz: u64) {}
#[cfg(not(target_arch = "x86_64"))]
pub fn set_timer_hz(_hz: u64) {}
#[cfg(not(target_arch = "x86_64"))]
pub fn set_timer_deadline(_d: u64) {}
#[cfg(not(target_arch = "x86_64"))]
pub fn send_ipi(_t: u32) {}
#[cfg(not(target_arch = "x86_64"))]
pub fn send_ipi_with_vector(_t: u32, _v: u8) {}
#[cfg(not(target_arch = "x86_64"))]
pub fn cpu_has_x2apic() -> bool { false }
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn enable_x2apic() {}
#[cfg(not(target_arch = "x86_64"))]
pub unsafe fn init_x2apic(_hz: u64) {}
#[cfg(not(target_arch = "x86_64"))]
pub fn eoi() {}
#[cfg(not(target_arch = "x86_64"))]
pub fn local_apic_id() -> u8 { 0 }
