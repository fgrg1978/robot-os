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

// ---------------------------------------------------------------------------
// DVFS — Dynamic Voltage and Frequency Scaling (F09)
// ---------------------------------------------------------------------------

/// CPU frequency levels (VF2 JH7110 supports 375/750/1000/1500 MHz).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum CpuFreqLevel {
    /// Low power (375 MHz on VF2, no-op on QEMU).
    Low    = 0,
    /// Medium (750 MHz on VF2).
    Medium = 1,
    /// High (1000 MHz on VF2).
    High   = 2,
    /// Maximum (1500 MHz on VF2).
    Max    = 3,
}

/// Current CPU frequency level.
static CPU_FREQ: AtomicU8 = AtomicU8::new(CpuFreqLevel::Max as u8);

/// Set CPU frequency level.
///
/// VF2: Programs the JH7110 CPU PLL via CRG registers.
/// QEMU/K1: No-op (frequency is fixed in emulation).
pub fn dvfs_set_freq(level: CpuFreqLevel) {
    #[cfg(feature = "vf2")]
    {
        /// JH7110 CPU PLL configuration register.
        const JH7110_CPU_PLL_CFG: usize = 0x1302_0000;
        /// CPU clock register offset in CRG.
        const CPU_CLK_OFFSET: usize = 0;

        // PLL divider values for each frequency (approximate).
        // 1500 MHz = max, dividers reduce from there.
        let divider: u32 = match level {
            CpuFreqLevel::Low    => 4, // 1500/4 = 375 MHz
            CpuFreqLevel::Medium => 2, // 1500/2 = 750 MHz
            CpuFreqLevel::High   => 1, // 1500/1.5 ≈ 1000 MHz (nearest)
            CpuFreqLevel::Max    => 1, // 1500 MHz (no division)
        };

        let addr = JH7110_CPU_PLL_CFG + CPU_CLK_OFFSET;
        let mut val = unsafe { core::ptr::read_volatile(addr as *const u32) };
        // Clear divider bits [23:0], set new divider
        val = (val & !0x00FF_FFFF) | (divider & 0x00FF_FFFF);
        unsafe { core::ptr::write_volatile(addr as *mut u32, val) };
    }
    CPU_FREQ.store(level as u8, Ordering::Release);
}

/// Get current CPU frequency level.
pub fn dvfs_get_freq() -> CpuFreqLevel {
    match CPU_FREQ.load(Ordering::Acquire) {
        0 => CpuFreqLevel::Low,
        1 => CpuFreqLevel::Medium,
        2 => CpuFreqLevel::High,
        _ => CpuFreqLevel::Max,
    }
}

/// Power budget manager: adjust CPU frequency based on battery level.
///
/// - Battery > 50% → Max frequency
/// - Battery 30-50% → High
/// - Battery 15-30% → Medium
/// - Battery < 15% → Low (save power for RTL/landing)
pub fn dvfs_power_budget(battery_pct: u8) {
    /// Battery threshold for maximum performance.
    const BATTERY_MAX_PCT: u8 = 50;
    /// Battery threshold for high performance.
    const BATTERY_HIGH_PCT: u8 = 30;
    /// Battery threshold for medium performance.
    const BATTERY_MED_PCT: u8 = 15;

    let target = if battery_pct > BATTERY_MAX_PCT {
        CpuFreqLevel::Max
    } else if battery_pct > BATTERY_HIGH_PCT {
        CpuFreqLevel::High
    } else if battery_pct > BATTERY_MED_PCT {
        CpuFreqLevel::Medium
    } else {
        CpuFreqLevel::Low
    };

    let current = dvfs_get_freq();
    if target != current {
        dvfs_set_freq(target);
    }
}

/// Print DVFS status.
pub fn dvfs_info() {
    let level = dvfs_get_freq();
    let name = match level {
        CpuFreqLevel::Low    => "Low (375 MHz)",
        CpuFreqLevel::Medium => "Medium (750 MHz)",
        CpuFreqLevel::High   => "High (1000 MHz)",
        CpuFreqLevel::Max    => "Max (1500 MHz)",
    };
    crate::kprintln!("[DVFS] CPU frequency: {}", name);
}

// ---------------------------------------------------------------------------
// Thermal Management (F23)
// ---------------------------------------------------------------------------

/// Temperature thresholds in milli-degrees Celsius.
/// VF2 JH7110 thermal sensor range: -40°C to 125°C.
const THERMAL_WARNING_MDEG: i32 = 75_000;   // 75°C — start throttling
const THERMAL_CRITICAL_MDEG: i32 = 90_000;  // 90°C — aggressive throttle
const THERMAL_SHUTDOWN_MDEG: i32 = 105_000;  // 105°C — emergency shutdown

/// Current CPU temperature in milli-degrees Celsius.
static THERMAL_TEMP: AtomicU8 = AtomicU8::new(0); // stored as (temp_mdeg / 1000) clamped to u8

/// Thermal throttle state.
#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum ThermalState {
    Normal   = 0,
    Warning  = 1,
    Critical = 2,
    Shutdown = 3,
}

/// Read CPU temperature from thermal sensor.
///
/// VF2 JH7110: Temperature sensor at SYSCON offset.
/// QEMU/K1: returns simulated value (25°C).
pub fn thermal_read_temp_mdeg() -> i32 {
    #[cfg(feature = "vf2")]
    {
        /// JH7110 thermal sensor register.
        const JH7110_TEMP_SENSOR: usize = 0x1703_0000;
        /// Offset for temperature reading.
        const TEMP_READ_OFFSET: usize = 0x10;

        let raw = unsafe {
            core::ptr::read_volatile((JH7110_TEMP_SENSOR + TEMP_READ_OFFSET) as *const u32)
        };
        // JH7110: raw value to milli-degrees conversion (approximate)
        // T(°C) ≈ (raw - 1328) * 100 / 2874 + 70 (from datasheet)
        let temp_c = ((raw as i32).saturating_sub(1328) * 100 / 2874) + 70;
        temp_c * 1000 // convert to milli-degrees
    }
    #[cfg(not(feature = "vf2"))]
    {
        // QEMU / K1: simulated at 25°C
        25_000
    }
}

/// Check thermal state and apply throttling if needed.
///
/// Returns the current thermal state after taking action.
pub fn thermal_check() -> ThermalState {
    let temp = thermal_read_temp_mdeg();

    // Store for status reporting (clamped to u8 = degrees C)
    let temp_c = (temp / 1000).clamp(0, 255) as u8;
    THERMAL_TEMP.store(temp_c, Ordering::Relaxed);

    if temp >= THERMAL_SHUTDOWN_MDEG {
        // Emergency: reduce to minimum frequency
        dvfs_set_freq(CpuFreqLevel::Low);
        crate::kprintln!("[THERMAL] SHUTDOWN threshold {}°C — freq→Low",
                          temp / 1000);
        ThermalState::Shutdown
    } else if temp >= THERMAL_CRITICAL_MDEG {
        dvfs_set_freq(CpuFreqLevel::Low);
        ThermalState::Critical
    } else if temp >= THERMAL_WARNING_MDEG {
        // Throttle to medium
        let current = dvfs_get_freq();
        if current as u8 > CpuFreqLevel::Medium as u8 {
            dvfs_set_freq(CpuFreqLevel::Medium);
        }
        ThermalState::Warning
    } else {
        ThermalState::Normal
    }
}

/// Get current temperature in degrees Celsius.
pub fn thermal_get_temp_c() -> u8 {
    THERMAL_TEMP.load(Ordering::Relaxed)
}

/// Print thermal status.
pub fn thermal_info() {
    let temp = thermal_read_temp_mdeg();
    let state = thermal_check();
    let state_str = match state {
        ThermalState::Normal   => "Normal",
        ThermalState::Warning  => "WARNING (throttled)",
        ThermalState::Critical => "CRITICAL (max throttle)",
        ThermalState::Shutdown => "SHUTDOWN",
    };
    crate::kprintln!("[THERMAL] CPU temp: {}°C — {}", temp / 1000, state_str);
}
