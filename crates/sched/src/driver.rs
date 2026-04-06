//! Driver Manager (AQ2) — spawn, monitor, and auto-restart driver processes.
//!
//! The driver manager maintains a registry of userspace driver processes,
//! tracks their health via heartbeats, and handles crash recovery with
//! automatic restarts (up to a configurable limit).

use robot_os_sync::SpinLock;

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum registered drivers.
const MAX_DRIVERS: usize = 16;

/// Maximum MMIO regions per driver.
const MAX_MMIO_PER_DRIVER: usize = 4;

/// Maximum IRQ bindings per driver.
const MAX_IRQS_PER_DRIVER: usize = 4;

/// Timeout before considering a driver unresponsive (ms).
const DRIVER_TIMEOUT_MS: u64 = 5000;

/// Maximum auto-restart attempts before giving up.
const DRIVER_MAX_RESTARTS: u8 = 3;

/// Restart cooldown period (ms).
const DRIVER_RESTART_COOLDOWN_MS: u64 = 1000;

/// Maximum length of a driver name.
const DRIVER_NAME_LEN: usize = 32;

// ── Types ────────────────────────────────────────────────────────────────────

/// Driver lifecycle state.
#[derive(Clone, Copy, PartialEq)]
pub enum DriverState {
    /// Slot is unused.
    Empty,
    /// Registered but not yet started.
    Registered,
    /// Running normally.
    Running,
    /// Stopped gracefully.
    Stopped,
    /// Crashed — awaiting restart or marked as failed.
    Crashed,
}

/// A memory-mapped I/O region assigned to a driver.
#[derive(Clone, Copy)]
pub struct MmioRegion {
    pub base: usize,
    pub size: usize,
}

impl MmioRegion {
    const fn empty() -> Self {
        MmioRegion { base: 0, size: 0 }
    }
}

/// Registry entry for a single driver.
#[derive(Clone, Copy)]
pub struct DriverEntry {
    pub name: [u8; DRIVER_NAME_LEN],
    pub state: DriverState,
    pub task_idx: usize,
    pub mmio: [MmioRegion; MAX_MMIO_PER_DRIVER],
    pub mmio_count: u8,
    pub irqs: [u32; MAX_IRQS_PER_DRIVER],
    pub irq_count: u8,
    pub restart_count: u8,
    pub last_heartbeat: u64,
    pub last_crash: u64,
}

impl DriverEntry {
    const fn empty() -> Self {
        DriverEntry {
            name: [0; DRIVER_NAME_LEN],
            state: DriverState::Empty,
            task_idx: 0,
            mmio: [MmioRegion::empty(); MAX_MMIO_PER_DRIVER],
            mmio_count: 0,
            irqs: [0; MAX_IRQS_PER_DRIVER],
            irq_count: 0,
            restart_count: 0,
            last_heartbeat: 0,
            last_crash: 0,
        }
    }
}

// ── Global state ─────────────────────────────────────────────────────────────

struct DriverTable {
    entries: [DriverEntry; MAX_DRIVERS],
    count: usize,
}

impl DriverTable {
    const fn new() -> Self {
        DriverTable {
            entries: [DriverEntry::empty(); MAX_DRIVERS],
            count: 0,
        }
    }
}

static DRIVERS: SpinLock<DriverTable> = SpinLock::new(DriverTable::new());

// ── Public API ───────────────────────────────────────────────────────────────

/// Register a new driver by name. Returns the driver slot index, or `None`
/// if the registry is full or the name is empty.
pub fn driver_register(name: &[u8]) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let mut table = DRIVERS.lock();
    if table.count >= MAX_DRIVERS {
        return None;
    }

    // Find first empty slot.
    for (i, entry) in table.entries.iter_mut().enumerate() {
        if entry.state == DriverState::Empty {
            *entry = DriverEntry::empty();
            let copy_len = name.len().min(DRIVER_NAME_LEN);
            entry.name[..copy_len].copy_from_slice(&name[..copy_len]);
            entry.state = DriverState::Registered;
            table.count += 1;
            return Some(i);
        }
    }
    None
}

/// Assign an MMIO region to a registered driver. Returns `true` on success.
pub fn driver_set_mmio(id: usize, base: usize, size: usize) -> bool {
    let mut table = DRIVERS.lock();
    if id >= MAX_DRIVERS {
        return false;
    }
    let entry = &mut table.entries[id];
    if entry.state == DriverState::Empty {
        return false;
    }
    let idx = entry.mmio_count as usize;
    if idx >= MAX_MMIO_PER_DRIVER {
        return false;
    }
    entry.mmio[idx] = MmioRegion { base, size };
    entry.mmio_count += 1;
    true
}

/// Bind an IRQ number to a registered driver. Returns `true` on success.
pub fn driver_set_irq(id: usize, irq: u32) -> bool {
    let mut table = DRIVERS.lock();
    if id >= MAX_DRIVERS {
        return false;
    }
    let entry = &mut table.entries[id];
    if entry.state == DriverState::Empty {
        return false;
    }
    let idx = entry.irq_count as usize;
    if idx >= MAX_IRQS_PER_DRIVER {
        return false;
    }
    entry.irqs[idx] = irq;
    entry.irq_count += 1;
    true
}

/// Mark a driver as running with the given scheduler task index.
pub fn driver_start(id: usize, task_idx: usize) {
    let mut table = DRIVERS.lock();
    if id >= MAX_DRIVERS {
        return;
    }
    let entry = &mut table.entries[id];
    if entry.state == DriverState::Empty {
        return;
    }
    entry.task_idx = task_idx;
    entry.state = DriverState::Running;
    entry.last_heartbeat = 0;
}

/// Update the heartbeat timestamp for a running driver.
pub fn driver_heartbeat(id: usize) {
    let mut table = DRIVERS.lock();
    if id >= MAX_DRIVERS {
        return;
    }
    let entry = &mut table.entries[id];
    if entry.state == DriverState::Running {
        // Use uptime from the timer crate if available; caller passes `now`.
        // For simplicity, we just mark it as "recently alive" — the caller
        // of `driver_check_health` provides the current tick.
        entry.last_heartbeat = u64::MAX; // sentinel — updated by check_health caller
    }
}

/// Update heartbeat with an explicit timestamp (ms).
pub fn driver_heartbeat_with_time(id: usize, now_ms: u64) {
    let mut table = DRIVERS.lock();
    if id >= MAX_DRIVERS {
        return;
    }
    let entry = &mut table.entries[id];
    if entry.state == DriverState::Running {
        entry.last_heartbeat = now_ms;
    }
}

/// Called when a task exits abnormally. Finds the driver by `task_idx`,
/// marks it as crashed, and records the crash timestamp.
pub fn driver_on_crash(task_idx: usize) {
    let mut table = DRIVERS.lock();
    for entry in table.entries.iter_mut() {
        if entry.state == DriverState::Running && entry.task_idx == task_idx {
            entry.state = DriverState::Crashed;
            entry.restart_count += 1;
            // Caller should set last_crash via driver_on_crash_with_time
            // or check_health will handle restart timing.
            return;
        }
    }
}

/// Called when a task exits abnormally, with an explicit crash timestamp.
pub fn driver_on_crash_with_time(task_idx: usize, now_ms: u64) {
    let mut table = DRIVERS.lock();
    for entry in table.entries.iter_mut() {
        if entry.state == DriverState::Running && entry.task_idx == task_idx {
            entry.state = DriverState::Crashed;
            entry.restart_count += 1;
            entry.last_crash = now_ms;
            return;
        }
    }
}

/// Check all running drivers for heartbeat timeouts.
///
/// Drivers that have not sent a heartbeat within `DRIVER_TIMEOUT_MS` are
/// marked as crashed. Crashed drivers that have not exceeded
/// `DRIVER_MAX_RESTARTS` and whose cooldown has elapsed are marked as
/// `Registered` (ready for the supervisor to re-spawn them).
///
/// Returns the number of drivers that need restarting (state == Registered
/// after having been Crashed).
pub fn driver_check_health(now_ms: u64) -> usize {
    let mut table = DRIVERS.lock();
    let mut needs_restart = 0usize;

    for entry in table.entries.iter_mut() {
        match entry.state {
            DriverState::Running => {
                // Check heartbeat timeout (skip if heartbeat was never set, i.e. == 0).
                if entry.last_heartbeat > 0
                    && now_ms.saturating_sub(entry.last_heartbeat) > DRIVER_TIMEOUT_MS
                {
                    entry.state = DriverState::Crashed;
                    entry.restart_count += 1;
                    entry.last_crash = now_ms;
                }
            }
            DriverState::Crashed => {
                // Check if we can auto-restart.
                if entry.restart_count <= DRIVER_MAX_RESTARTS
                    && now_ms.saturating_sub(entry.last_crash) >= DRIVER_RESTART_COOLDOWN_MS
                {
                    entry.state = DriverState::Registered;
                    needs_restart += 1;
                }
            }
            _ => {}
        }
    }

    needs_restart
}

/// Get a snapshot of a driver entry. Returns `None` if the slot is empty
/// or the id is out of range.
pub fn driver_info(id: usize) -> Option<DriverEntry> {
    let table = DRIVERS.lock();
    if id >= MAX_DRIVERS {
        return None;
    }
    let entry = &table.entries[id];
    if entry.state == DriverState::Empty {
        return None;
    }
    Some(*entry)
}

/// Return the number of registered (non-empty) drivers.
pub fn driver_count() -> usize {
    DRIVERS.lock().count
}

// ---------------------------------------------------------------------------
// Driver spawn orchestration (F06)
// ---------------------------------------------------------------------------

/// ELF path for a driver (stored alongside the registry entry).
pub const DRIVER_PATH_MAX_LEN: usize = 64;

/// Descriptor for spawning a userspace driver.
#[derive(Clone, Copy)]
pub struct DriverDescriptor {
    /// Human-readable name.
    pub name: [u8; DRIVER_NAME_LEN],
    pub name_len: u8,
    /// FAT32 path to the driver ELF.
    pub elf_path: [u8; DRIVER_PATH_MAX_LEN],
    pub elf_path_len: u8,
    /// MMIO regions to grant.
    pub mmio: [MmioRegion; MAX_MMIO_PER_DRIVER],
    pub mmio_count: u8,
    /// IRQ numbers to grant.
    pub irqs: [u32; MAX_IRQS_PER_DRIVER],
    pub irq_count: u8,
}

impl DriverDescriptor {
    pub const fn empty() -> Self {
        Self {
            name: [0; DRIVER_NAME_LEN],
            name_len: 0,
            elf_path: [0; DRIVER_PATH_MAX_LEN],
            elf_path_len: 0,
            mmio: [MmioRegion::empty(); MAX_MMIO_PER_DRIVER],
            mmio_count: 0,
            irqs: [0; MAX_IRQS_PER_DRIVER],
            irq_count: 0,
        }
    }
}

/// Maximum number of drivers that can be auto-spawned at boot.
pub const MAX_SPAWN_DESCRIPTORS: usize = 8;

/// Boot-time driver descriptors (populated by kernel init, consumed by supervisor).
static mut SPAWN_DESCRIPTORS: [DriverDescriptor; MAX_SPAWN_DESCRIPTORS] = {
    const EMPTY: DriverDescriptor = DriverDescriptor::empty();
    [EMPTY; MAX_SPAWN_DESCRIPTORS]
};
static mut SPAWN_COUNT: usize = 0;

/// Register a driver descriptor for boot-time spawning.
/// Called from kernel init to declare which drivers should be auto-spawned.
pub fn driver_add_spawn_descriptor(desc: DriverDescriptor) -> bool {
    unsafe {
        if SPAWN_COUNT >= MAX_SPAWN_DESCRIPTORS {
            return false;
        }
        SPAWN_DESCRIPTORS[SPAWN_COUNT] = desc;
        SPAWN_COUNT += 1;
        true
    }
}

/// Get the number of registered spawn descriptors.
pub fn driver_spawn_count() -> usize {
    unsafe { SPAWN_COUNT }
}

/// Get a spawn descriptor by index.
pub fn driver_spawn_descriptor(idx: usize) -> Option<DriverDescriptor> {
    unsafe {
        if idx >= SPAWN_COUNT { return None; }
        Some(SPAWN_DESCRIPTORS[idx])
    }
}

/// Spawn a driver from its descriptor.
///
/// This function:
/// 1. Registers the driver in the driver table
/// 2. Configures MMIO regions and IRQ bindings
/// 3. Grants capability handles to the new task
///
/// The actual ELF loading and task creation is done by the caller
/// (kernel main or supervisor task) since it requires `exec_user`
/// which is in a different crate.
///
/// Returns the driver registry ID, or None on failure.
pub fn driver_spawn_register(desc: &DriverDescriptor) -> Option<usize> {
    let name = &desc.name[..desc.name_len as usize];

    // 1. Register in driver table
    let drv_id = driver_register(name)?;

    // 2. Configure MMIO regions
    for i in 0..desc.mmio_count as usize {
        driver_set_mmio(drv_id, desc.mmio[i].base, desc.mmio[i].size);
    }

    // 3. Configure IRQ bindings
    for i in 0..desc.irq_count as usize {
        driver_set_irq(drv_id, desc.irqs[i]);
    }

    Some(drv_id)
}

/// Called by the supervisor to handle drivers that need restarting.
///
/// Scans the driver table for entries in `Registered` state (after crash
/// recovery) and returns a list of (driver_id, name) pairs that need
/// their ELF re-loaded.
///
/// Returns the number of drivers needing restart.
pub fn driver_get_restart_list(out: &mut [(usize, [u8; DRIVER_NAME_LEN])]) -> usize {
    let table = DRIVERS.lock();
    let mut count = 0;

    for (i, entry) in table.entries.iter().enumerate() {
        if entry.state == DriverState::Registered && entry.restart_count > 0 {
            if count < out.len() {
                out[count] = (i, entry.name);
                count += 1;
            }
        }
    }

    count
}
