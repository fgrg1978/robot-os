//! GICv3 — ARM Generic Interrupt Controller v3.
//!
//! Programs the distributor (GICD), the calling CPU's
//! redistributor (GICR), and the CPU interface (ICC_*
//! system registers). Sufficient to take interrupts via
//! `ICC_IAR1_EL1` and acknowledge them via `ICC_EOIR1_EL1`;
//! actual exception handling (vector table, SP/saved-state)
//! is a separate follow-up.
//!
//! # Address layout (QEMU virt + similar Arm reference designs)
//!
//!     0x0800_0000 ─ GICD (distributor)          ─ 64 KiB
//!     0x080A_0000 ─ GICR  (redistributor frames) ─ 128 KiB × NUM_CPUS
//!
//! Real silicon (e.g. NXP S32G3, Ampere Altra) varies; the
//! kernel parses the device tree to discover the bases at
//! boot. The compile-time constants here are QEMU defaults
//! used by `aarch64-hello`.

// ──────────────────────────────────────────────────────────────────────────
// MMIO bases (QEMU virt)
// ──────────────────────────────────────────────────────────────────────────

/// GIC distributor base for `qemu-system-aarch64 -M virt`.
pub const GICD_BASE: usize = 0x0800_0000;

/// GIC redistributor base for `qemu-system-aarch64 -M virt`.
/// Each PE occupies a 128 KiB stride from this base.
pub const GICR_BASE: usize = 0x080A_0000;

/// Stride (in bytes) between consecutive PE redistributor frames.
/// GICv3 packs RD_base (64 KiB) + SGI_base (64 KiB).
pub const GICR_STRIDE: usize = 0x2_0000;

/// Bounded poll count for the GICR_WAKER ChildrenAsleep wait.
/// Real silicon clears within a handful of cycles; QEMU virt
/// may not model the bit at all (see `init_redistributor`).
pub const GICR_WAKE_MAX_SPINS: u32 = 1_000_000;

// ──────────────────────────────────────────────────────────────────────────
// Distributor register offsets (GIC Architecture Specification, §12)
// ──────────────────────────────────────────────────────────────────────────

const GICD_CTLR:       usize = 0x0000;
const GICD_TYPER:      usize = 0x0004;
const GICD_IGROUPR0:   usize = 0x0080;
const GICD_ISENABLER0: usize = 0x0100;
const GICD_ICENABLER0: usize = 0x0180;
const GICD_IPRIORITYR0:usize = 0x0400;

/// GICD_CTLR.EnableGrp1NS — bit 1 (non-secure access).
const GICD_CTLR_ENABLE_GRP1_NS: u32 = 1 << 1;
/// GICD_CTLR.ARE_NS — Affinity Routing Enable, non-secure (bit 4).
/// Mandatory for GICv3 to use the ICC_* system-register interface.
const GICD_CTLR_ARE_NS: u32 = 1 << 4;
/// GICD_CTLR.RWP — Register Write Pending (read-only).
const GICD_CTLR_RWP: u32 = 1 << 31;

// ──────────────────────────────────────────────────────────────────────────
// Redistributor register offsets
// ──────────────────────────────────────────────────────────────────────────

/// Within RD_base (first 64 KiB of a PE's frame).
const GICR_CTLR:  usize = 0x0000;
const GICR_WAKER: usize = 0x0014;

/// GICR_WAKER.ProcessorSleep — bit 1.
const GICR_WAKER_PROCESSOR_SLEEP:  u32 = 1 << 1;
/// GICR_WAKER.ChildrenAsleep — bit 2 (read-only, mirrors above).
const GICR_WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

// SGI_base (second 64 KiB) holds PPI/SGI registers — offsets are
// added on top of `RD_base + 0x10000`.
const GICR_SGI_OFFSET:        usize = 0x1_0000;
const GICR_IGROUPR0:          usize = 0x0080;
const GICR_ISENABLER0:        usize = 0x0100;
const GICR_ICENABLER0:        usize = 0x0180;
const GICR_IPRIORITYR0:       usize = 0x0400;

// ──────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn mmio_read32(addr: usize) -> u32 {
    core::ptr::read_volatile(addr as *const u32)
}

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn mmio_write32(addr: usize, val: u32) {
    core::ptr::write_volatile(addr as *mut u32, val);
}

/// Wait for any pending distributor register write to complete.
/// Reading GICD_CTLR.RWP clears once the in-flight write is
/// visible to all PEs.
#[cfg(target_arch = "aarch64")]
fn gicd_wait_rwp() {
    unsafe {
        while mmio_read32(GICD_BASE + GICD_CTLR) & GICD_CTLR_RWP != 0 {
            core::hint::spin_loop();
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Init sequence
// ──────────────────────────────────────────────────────────────────────────

/// Initialise the distributor. Must be called once during boot,
/// before any redistributor / CPU-interface init.
#[cfg(target_arch = "aarch64")]
pub fn init_distributor() {
    unsafe {
        // 1. Disable distributor while we configure it.
        mmio_write32(GICD_BASE + GICD_CTLR, 0);
        gicd_wait_rwp();

        // 2. Discover number of SPIs from GICD_TYPER.ITLinesNumber
        //    (bits [4:0]). N → 32 * (N + 1) interrupt IDs total
        //    (including SGIs/PPIs).
        let typer = mmio_read32(GICD_BASE + GICD_TYPER);
        let it_lines = (typer & 0x1F) as usize;

        // 3. For each SPI (INTID >= 32): default group 1 (NS),
        //    priority 0xA0, disabled.
        for i in 1..=it_lines {
            let off = i * 4;
            mmio_write32(GICD_BASE + GICD_IGROUPR0 + off, 0xFFFF_FFFF);
            mmio_write32(GICD_BASE + GICD_ICENABLER0 + off, 0xFFFF_FFFF);
        }
        // 4. Default priority for SPI lines (4 bytes per INTID).
        for i in 8..(8 + it_lines * 8) {
            mmio_write32(GICD_BASE + GICD_IPRIORITYR0 + i * 4, 0xA0A0_A0A0);
        }

        // 5. Enable distributor, Group 1 NS, Affinity Routing.
        mmio_write32(
            GICD_BASE + GICD_CTLR,
            GICD_CTLR_ENABLE_GRP1_NS | GICD_CTLR_ARE_NS,
        );
        gicd_wait_rwp();
    }
}

/// Initialise the calling PE's redistributor. Pass the CPU's
/// affinity index (0..NUM_HARTS); the function picks the right
/// 128 KiB frame.
///
/// The wake-up sequence (GICR_WAKER) is GICv3-mandated — without
/// it the per-CPU SGI/PPI lines stay masked.
#[cfg(target_arch = "aarch64")]
pub fn init_redistributor(cpu_id: usize) {
    let rd_base = GICR_BASE + cpu_id * GICR_STRIDE;
    let sgi_base = rd_base + GICR_SGI_OFFSET;

    unsafe {
        // 1. Clear ProcessorSleep, wait ChildrenAsleep == 0.
        //
        // Bounded by `GICR_WAKE_MAX_SPINS` because some
        // simulations (QEMU virt at the time of writing)
        // don't model the wake-up handshake — they boot the
        // PE awake and ChildrenAsleep is stuck at 0 or stuck
        // at 1 depending on implementation. An infinite wait
        // would wedge boot.
        let mut waker = mmio_read32(rd_base + GICR_WAKER);
        waker &= !GICR_WAKER_PROCESSOR_SLEEP;
        mmio_write32(rd_base + GICR_WAKER, waker);
        let mut spins = 0u32;
        while mmio_read32(rd_base + GICR_WAKER) & GICR_WAKER_CHILDREN_ASLEEP != 0
            && spins < GICR_WAKE_MAX_SPINS
        {
            core::hint::spin_loop();
            spins += 1;
        }

        // 2. PPI/SGI defaults: group 1 NS, disabled, priority 0xA0.
        mmio_write32(sgi_base + GICR_IGROUPR0, 0xFFFF_FFFF);
        mmio_write32(sgi_base + GICR_ICENABLER0, 0xFFFF_FFFF);
        for i in 0..8 {
            mmio_write32(
                sgi_base + GICR_IPRIORITYR0 + i * 4,
                0xA0A0_A0A0,
            );
        }
        // GICR_CTLR — leave at reset (we don't use LPIs).
        let _ = mmio_read32(rd_base + GICR_CTLR);
    }
}

/// Initialise the per-CPU interface via ICC system registers.
/// Must run AFTER `init_redistributor` for the calling PE.
#[cfg(target_arch = "aarch64")]
pub fn init_cpu_interface() {
    unsafe {
        // ICC_SRE_EL1.SRE = 1 — use the system-register interface
        // (the MMIO cpu interface, GICC_*, isn't even mapped in
        // GICv3 by default).
        let mut sre: u64;
        core::arch::asm!(
            "mrs {0}, ICC_SRE_EL1",
            out(reg) sre,
            options(nomem, nostack, preserves_flags),
        );
        sre |= 1;
        core::arch::asm!(
            "msr ICC_SRE_EL1, {0}",
            "isb",
            in(reg) sre,
            options(nomem, nostack, preserves_flags),
        );

        // ICC_PMR_EL1 = 0xFF — accept any priority.
        core::arch::asm!(
            "msr ICC_PMR_EL1, {0}",
            in(reg) 0xFFu64,
            options(nomem, nostack, preserves_flags),
        );

        // ICC_BPR1_EL1 = 0 — no preemption-priority grouping.
        core::arch::asm!(
            "msr ICC_BPR1_EL1, {0}",
            in(reg) 0u64,
            options(nomem, nostack, preserves_flags),
        );

        // ICC_IGRPEN1_EL1.Enable = 1 — enable Group 1 NS.
        core::arch::asm!(
            "msr ICC_IGRPEN1_EL1, {0}",
            "isb",
            in(reg) 1u64,
            options(nomem, nostack, preserves_flags),
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Per-interrupt control (SPIs only — PPIs/SGIs use redistributor)
// ──────────────────────────────────────────────────────────────────────────

/// Enable SPI `intid` (must be ≥ 32). PPI/SGI use the
/// per-PE redistributor SGI_base register and need a separate
/// helper once we wire those.
#[cfg(target_arch = "aarch64")]
pub fn enable_spi(intid: u32) {
    debug_assert!(intid >= 32, "use enable_ppi for INTIDs below 32");
    let reg = (intid / 32) as usize * 4;
    let bit = 1u32 << (intid % 32);
    unsafe {
        mmio_write32(GICD_BASE + GICD_ISENABLER0 + reg, bit);
        gicd_wait_rwp();
    }
}

/// Enable a per-PE SGI (intid 0..15) or PPI (intid 16..31)
/// on `cpu_id`'s redistributor.
#[cfg(target_arch = "aarch64")]
pub fn enable_ppi(cpu_id: usize, intid: u32) {
    debug_assert!(intid < 32, "use enable_spi for INTIDs >= 32");
    let sgi_base = GICR_BASE + cpu_id * GICR_STRIDE + GICR_SGI_OFFSET;
    let bit = 1u32 << intid;
    unsafe {
        mmio_write32(sgi_base + GICR_ISENABLER0, bit);
    }
}

/// Acknowledge the highest-priority pending Group 1 interrupt
/// and return its INTID. The companion `eoir1` MUST be called
/// after handling.
#[cfg(target_arch = "aarch64")]
pub fn iar1() -> u32 {
    let id: u64;
    unsafe {
        core::arch::asm!(
            "mrs {0}, ICC_IAR1_EL1",
            out(reg) id,
            options(nomem, nostack, preserves_flags),
        );
    }
    id as u32
}

/// Signal end-of-interrupt for `intid` returned by `iar1`.
#[cfg(target_arch = "aarch64")]
pub fn eoir1(intid: u32) {
    unsafe {
        core::arch::asm!(
            "msr ICC_EOIR1_EL1, {0}",
            in(reg) intid as u64,
            options(nomem, nostack, preserves_flags),
        );
    }
}
