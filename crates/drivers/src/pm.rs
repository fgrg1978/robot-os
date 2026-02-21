/// Power management — idle/suspend states with WFI.
///
/// QEMU/VF2/K1: WFI instruction for low-power idle.
/// VF2: clock gating skeleton via JH7110 CRG (Clock Reset Generator).


use core::sync::atomic::{AtomicU8, Ordering};

/// Power management state.
#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum PmState {
    Active  = 0,
    Idle    = 1,
    Suspend = 2,
}

impl PmState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => PmState::Idle,
            2 => PmState::Suspend,
            _ => PmState::Active,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            PmState::Active  => "Active",
            PmState::Idle    => "Idle",
            PmState::Suspend => "Suspend",
        }
    }
}

static PM_STATE: AtomicU8 = AtomicU8::new(PmState::Active as u8);

/// Initialise power management (set state to Active).
pub fn pm_init() {
    PM_STATE.store(PmState::Active as u8, Ordering::Release);
    crate::kprintln!("[PM] Initialized — state: Active");
}

/// Enter idle state — executes WFI to save power until next interrupt.
///
/// Returns immediately after an interrupt wakes the hart.
pub fn pm_idle() {
    PM_STATE.store(PmState::Idle as u8, Ordering::Release);
    unsafe { core::arch::asm!("wfi") };
    PM_STATE.store(PmState::Active as u8, Ordering::Release);
}

/// Suspend the system — enters a deep WFI loop.
///
/// On QEMU this is effectively the same as idle (WFI), since QEMU virt
/// does not model true suspend states.  The hart will wake on any interrupt.
pub fn pm_suspend() {
    PM_STATE.store(PmState::Suspend as u8, Ordering::Release);
    crate::kprintln!("[PM] System suspended -- WFI");
    unsafe { core::arch::asm!("wfi") };
    // Woken by interrupt
    PM_STATE.store(PmState::Active as u8, Ordering::Release);
    crate::kprintln!("[PM] Resumed from suspend");
}

/// Resume to active state (called from interrupt handler or explicit wake).
pub fn pm_resume() {
    PM_STATE.store(PmState::Active as u8, Ordering::Release);
}

/// Get current power management state.
pub fn pm_get_state() -> PmState {
    PmState::from_u8(PM_STATE.load(Ordering::Acquire))
}

/// Print power management status.
pub fn pm_info() {
    let state = pm_get_state();
    crate::kprintln!("[PM] State: {}", state.as_str());
    // Read mtime for uptime estimate
    let mtime: u64 = crate::clint::get_time();
    crate::kprintln!("[PM] Uptime: {} ticks (mtime)", mtime);
}

/// Clock-gate a peripheral (enable or disable its clock).
///
/// VF2: writes to JH7110 CRG (Clock Reset Generator) registers.
/// QEMU: no-op (no clock gating hardware).
pub fn pm_clock_gate(_peripheral: u32, _enable: bool) {
    #[cfg(feature = "vf2")]
    {
        // JH7110 CRG (System CRG) base address
        const JH7110_CRG_BASE: usize = 0x1302_0000;
        // Clock enable register stride: each peripheral has a 4-byte register.
        // Bit 31 = clock enable, bits 29:24 = mux select, bits 23:0 = divider.
        let offset = (_peripheral as usize) * 4;
        let addr = JH7110_CRG_BASE + offset;
        let mut val = unsafe { core::ptr::read_volatile(addr as *const u32) };
        if _enable {
            val |= 1 << 31;  // set clock enable
        } else {
            val &= !(1 << 31); // clear clock enable
        }
        unsafe { core::ptr::write_volatile(addr as *mut u32, val) };
    }
    // QEMU / K1: no-op
}
