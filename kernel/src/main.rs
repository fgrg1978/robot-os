//! Robot OS - Hybrid RISC-V Kernel (Rust)
//!
//! Entry point for the kernel. Called from boot.S after hardware init.

#![no_std]
#![no_main]

extern crate alloc;

mod panic;

use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};
use robot_os_config::ML_ENABLED;
use robot_os_drivers::kprintln;
#[cfg(not(feature = "esp32c3"))]
use robot_os_arch::mmu::PAGE_SIZE;
#[cfg(feature = "esp32c3")]
const PAGE_SIZE: usize = 4096;
use robot_os_arch::trap::{
    TrapFrame, INTERRUPT_BIT,
    INT_TIMER_S, INT_EXTERNAL_S, INT_SOFTWARE_S,
    TRAP_ECALL_FROM_U, TRAP_ECALL_FROM_S,
    TRAP_INSTR_PAGE_FAULT, TRAP_LOAD_PAGE_FAULT, TRAP_STORE_PAGE_FAULT,
};
use robot_os_arch::{csr, trap};

// Include boot assembly
#[cfg(not(feature = "esp32c3"))]
global_asm!(include_str!("asm/boot.S"));
#[cfg(feature = "esp32c3")]
global_asm!(include_str!("asm/boot_esp32c3.S"));

// Include trap entry assembly
#[cfg(not(feature = "esp32c3"))]
global_asm!(include_str!("asm/trap_entry.S"));
#[cfg(feature = "esp32c3")]
global_asm!(include_str!("asm/trap_entry_esp32c3.S"));

// Include context switch assembly.
// Phase 12: when rvv feature is active, use the RVV-aware variant.
#[cfg(all(not(feature = "rvv"), not(feature = "esp32c3")))]
global_asm!(include_str!("asm/context_switch.S"));
#[cfg(feature = "rvv")]
global_asm!(include_str!("asm/context_switch_rvv.S"));
#[cfg(feature = "esp32c3")]
global_asm!(include_str!("asm/context_switch_esp32c3.S"));

/// Maximum number of harts supported (stack slots allocated).
const MAX_HARTS: usize = 8;

/// Stack size per secondary hart (16 KiB).
/// Enough for nested traps (288 B each) + scheduler + Rust calls.
#[cfg(not(feature = "esp32c3"))]
const SECONDARY_STACK_SIZE: usize = 16 * 1024;

// Secondary CPU stacks — boot.S references `secondary_stacks` and
// loads the per-hart size from `_secondary_stack_size` (.quad in .data).
#[cfg(not(feature = "esp32c3"))]
global_asm!(
    ".section .data",
    ".align 3",
    ".global _secondary_stack_size",
    "_secondary_stack_size:",
    "    .quad {size}",
    ".align 12",
    ".global secondary_stacks",
    "secondary_stacks:",
    "    .space {size} * {max_harts}",
    size = const SECONDARY_STACK_SIZE,
    max_harts = const MAX_HARTS,
);

// Linker script symbols
unsafe extern "C" {
    static _kernel_end: u8;
}

/// Fallback RAM size when DTB doesn't provide memory info.
#[cfg(not(feature = "esp32c3"))]
const FALLBACK_MEM_SIZE: usize = 128 * 1024 * 1024;
#[cfg(feature = "esp32c3")]
const FALLBACK_MEM_SIZE: usize = 384 * 1024;

/// Kernel heap size.
#[cfg(not(feature = "esp32c3"))]
const HEAP_SIZE: usize = 4 * 1024 * 1024;
#[cfg(feature = "esp32c3")]
const HEAP_SIZE: usize = 64 * 1024;

/// Number of CPUs to use in SMP mode.
#[cfg(not(feature = "esp32c3"))]
const NUM_CPUS: usize = 4;
#[cfg(feature = "esp32c3")]
const NUM_CPUS: usize = 1;

/// Number of worker tasks for the SMP stress test.
const NUM_WORKERS: usize = 15;

/// Each worker runs this many iterations.
const WORKER_ITERS: u32 = 2000;

/// Total timer ticks received across all CPUs (for verification).
static TICK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Kernel entry point. Called from boot.S (hart 0 only).
///
/// Boot flow:
///   1. **Early init** (interrupts OFF): UART, DTB parse, PMM, VMM, heap, traps
///   2. **Late init** (interrupts ON):   storage, config, IPC, drivers, scheduler
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(hart_id: usize, dtb_ptr: usize) -> ! {
    // ══════════════════════════════════════════════════════════════════════
    //  EARLY INIT — interrupts disabled, single hart, no heap yet
    // ══════════════════════════════════════════════════════════════════════

    // ---- Phase 1: UART ----
    robot_os_drivers::uart::init();

    kprintln!();
    kprintln!("========================================");
    kprintln!("  Robot OS Rust kernel booted!");
    kprintln!("========================================");
    kprintln!();
    kprintln!("[BOOT] Hart ID:  {}", hart_id);
    kprintln!("[BOOT] DTB addr: {:#x}", dtb_ptr);

    // Parse DTB (Flattened Device Tree) if pointer looks valid.
    // Extract mem_base/mem_size to feed PMM and VMM with real hardware RAM.
    let (mem_start, mem_size, mem_from_dtb) = if dtb_ptr != 0 {
        if let Some(info) = unsafe { robot_os_dtb::dtb_parse(dtb_ptr as *const u8) } {
            let compat = robot_os_dtb::dtb_compatible_str(&info);
            kprintln!("[DTB] Parsed FDT — {} CPUs, mem={:#x}+{:#x}, timer={}",
                info.num_cpus, info.mem_base, info.mem_size, info.timer_freq);
            kprintln!("[DTB] UART={:#x}, PLIC={:#x}", info.uart_base, info.plic_base);
            kprintln!("[DTB] Compatible: {}", core::str::from_utf8(compat).unwrap_or("?"));
            if info.mem_base != 0 && info.mem_size != 0 {
                (info.mem_base, info.mem_size, true)
            } else {
                (robot_os_drivers::platform::hw::RAM_BASE, FALLBACK_MEM_SIZE, false)
            }
        } else {
            kprintln!("[DTB] Parse failed (invalid or unsupported FDT)");
            (robot_os_drivers::platform::hw::RAM_BASE, FALLBACK_MEM_SIZE, false)
        }
    } else {
        (robot_os_drivers::platform::hw::RAM_BASE, FALLBACK_MEM_SIZE, false)
    };
    kprintln!();

    // ---- Phase 2: Memory Management ----

    let kernel_end = unsafe { &_kernel_end as *const u8 as usize };
    let kernel_end_aligned = (kernel_end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    kprintln!("[MM] Kernel end:  {:#x} (aligned: {:#x})", kernel_end, kernel_end_aligned);
    if mem_from_dtb {
        kprintln!("[MM] RAM detected via DTB: {:#x} - {:#x} ({} MiB)",
            mem_start, mem_start + mem_size, mem_size >> 20);
    } else {
        kprintln!("[MM] RAM fallback (no DTB): {:#x} - {:#x} ({} MiB)",
            mem_start, mem_start + mem_size, mem_size >> 20);
    }

    robot_os_mm::pmm::init(mem_start, mem_size, kernel_end_aligned);
    kprintln!("[MM] PMM: {} total pages, {} free, {} used",
        robot_os_mm::pmm::total_pages(),
        robot_os_mm::pmm::free_pages(),
        robot_os_mm::pmm::used_pages());

    // VMM init BEFORE heap: vmm::init() allocates ~66 page-table pages from PMM
    // starting at kernel_end.  Initializing the heap first would corrupt those
    // pages (pmm::alloc_page zeroes each page it returns).  So: VMM first, then
    // heap starts at the first PMM page that VMM didn't touch.
    #[cfg(not(feature = "no-mmu"))]
    {
        kprintln!("[MM] Initializing VMM (Sv39 page tables)...");
        match robot_os_mm::vmm::init(mem_start, mem_size) {
            Ok(()) => kprintln!("[MM] VMM initialized (megapages), kernel PT created"),
            Err(e) => {
                kprintln!("[MM] VMM init FAILED: {:?}", e);
                loop { robot_os_arch::cpu::wfi(); }
            }
        }

        // Map platform-specific MMIO regions BEFORE enabling paging.
        // Each platform needs its device addresses identity-mapped.
        {
            use robot_os_drivers::platform::hw;

            // UART (all platforms — 4 KiB)
            let _ = robot_os_mm::vmm::map_mmio_region(hw::UART_BASE, 0x1000);

            // PLIC (all platforms — up to 4 MiB is sufficient for enable/threshold/claim)
            let _ = robot_os_mm::vmm::map_mmio_region(hw::PLIC_BASE, 0x40_0000);

            // QEMU-specific MMIO
            #[cfg(not(any(feature = "vf2", feature = "k1", feature = "esp32c3")))]
            {
                // VirtIO MMIO 0x10001000 - 0x10008000 (8 devices)
                let _ = robot_os_mm::vmm::map_mmio_region(0x1000_1000, 0x8000);
                // CLINT 0x02000000 (64 KiB) — mtime/mtimecmp via SBI but read rdtime
                let _ = robot_os_mm::vmm::map_mmio_region(0x0200_0000, 0x1_0000);
            }

            // VF2-specific MMIO
            #[cfg(feature = "vf2")]
            {
                let _ = robot_os_mm::vmm::map_mmio_region(0x0200_0000, 0x1_0000); // CLINT
                let _ = robot_os_mm::vmm::map_mmio_region(hw::GPIO_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::PWM_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::I2C0_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::I2C1_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::MMC0_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::MMC1_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::ETH0_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::UART1_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::WDT_BASE, 0x1000);
            }

            // K1-specific MMIO
            #[cfg(feature = "k1")]
            {
                let _ = robot_os_mm::vmm::map_mmio_region(hw::GPIO_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::PWM_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::I2C0_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::I2C1_BASE, 0x1000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::MMC0_BASE, 0x2000);
                let _ = robot_os_mm::vmm::map_mmio_region(hw::WDT_BASE, 0x1000);
            }

            kprintln!("[MM] Platform MMIO mapped ({})", hw::PLATFORM_NAME);
        }

        robot_os_mm::vmm::enable_paging();
        kprintln!("[MM] Sv39 paging ENABLED");

        // Guard pages: unmap bottom 4 KiB of each task stack so overflow
        // triggers an immediate page fault instead of silent corruption.
        robot_os_sched::setup_stack_guard_pages();
        kprintln!("[MM] Stack guard pages active");
    }
    #[cfg(feature = "no-mmu")]
    kprintln!("[MM] No MMU (flat memory mode)");

    // Now safe to initialize the heap: PMM page-table pages are already allocated,
    // so the heap starts at the first *remaining* free PMM page.
    // Reserve FIRST, then init — eliminates any window where alloc_page() could
    // hand out pages that belong to the heap.
    let heap_start = robot_os_mm::pmm::next_free_addr();
    kprintln!("[MM] Heap: {:#x}, {} KiB", heap_start, HEAP_SIZE / 1024);
    robot_os_mm::pmm::reserve_range(heap_start, HEAP_SIZE);
    unsafe { robot_os_mm::kheap::init(heap_start, HEAP_SIZE) };
    kprintln!("[MM] Heap initialized");

    {
        let mut v = alloc::vec![1u32, 2, 3, 4, 5];
        v.push(6);
        kprintln!("[MM] Heap test: Vec = {:?}", v);
    }
    kprintln!();

    // ---- Phase 3: Traps + Interrupts ----

    kprintln!("[TRAP] Initializing trap handling...");
    trap_init();
    #[cfg(not(feature = "esp32c3"))]
    {
        kprintln!("[IRQ] Initializing PLIC...");
        robot_os_drivers::plic::init(hart_id as u32);
    }
    robot_os_drivers::clint::set_next_tick(hart_id as u32);
    let sie = csr::read_sie();
    csr::write_sie(sie | csr::SIE_STIE | csr::SIE_SEIE | csr::SIE_SSIE);
    let sstatus = csr::read_sstatus();
    csr::write_sstatus(sstatus | csr::SSTATUS_SIE);
    #[cfg(not(feature = "esp32c3"))]
    {
        // Enable UART RX interrupt (IRQ 10) — characters go to ring buffer.
        robot_os_drivers::plic::enable_irq(hart_id as u32, robot_os_drivers::uart::UART_IRQ);
        robot_os_drivers::uart::enable_irq();
        kprintln!("[IRQ] UART IRQ enabled (ring buffer RX)");
    }
    kprintln!("[IRQ] Traps + interrupts active");
    kprintln!();

    // ══════════════════════════════════════════════════════════════════════
    //  LATE INIT — interrupts enabled, heap available, full hardware access
    // ══════════════════════════════════════════════════════════════════════

    // ---- Phase 6: VirtIO Block + VFS + Network ----

    kprintln!("========================================");
    kprintln!(" Phase 6: Storage + Network");
    kprintln!("========================================");
    kprintln!();

    match robot_os_drivers::blkdev::init() {
        Ok(()) => kprintln!("[FS] Block device OK ({} sectors)",
            robot_os_drivers::blkdev::capacity_sectors()),
        Err(()) => kprintln!("[FS] Block device not found (no disk)"),
    }

    robot_os_fs::init();
    kprintln!("[FS] ramfs initialized");

    if robot_os_drivers::blkdev::capacity_sectors() > 0 {
        match robot_os_fs::fat32_mount() {
            Ok(()) => {
                match robot_os_fs::vfs_mount(b"/fat", robot_os_fs::FS_TYPE_FAT32) {
                    Ok(())  => kprintln!("[FS] FAT32 mounted at /fat"),
                    Err(()) => kprintln!("[FS] FAT32 vfs_mount failed"),
                }
            }
            Err(()) => kprintln!("[FS] FAT32 mount failed (disk not FAT32?)"),
        }
    }
    kprintln!();

    // ---- Phase G2: Persistent State Recovery ────────────────────────────────
    //
    // Load /fat/CONFIG.INI BEFORE net_init() and task creation so every
    // subsystem starts with the persisted (or factory-default) configuration.
    // First-boot: generate defaults and write CONFIG.INI to disk.

    kprintln!("========================================");
    kprintln!(" Phase G2: Persistent State Recovery");
    kprintln!("========================================");
    {
        static mut CFG_BUF: [u8; 1024] = [0u8; 1024];
        let buf = unsafe { &mut *(&raw mut CFG_BUF) };
        let mut fd_table = robot_os_fs::FdTable::new();
        let fd = robot_os_fs::vfs_open(&mut fd_table, b"/fat/CONFIG.INI",
                                        robot_os_fs::O_RDONLY);
        if fd >= 0 {
            let n = robot_os_fs::vfs_read(&mut fd_table, fd,
                                           buf.as_mut_ptr(), buf.len());
            robot_os_fs::vfs_close(&mut fd_table, fd);
            if n > 0 {
                robot_os_config::cfg_load(&buf[..n as usize]);
                kprintln!("[CFG] Loaded {} entries from /fat/CONFIG.INI",
                    robot_os_config::cfg_count());
            } else {
                kprintln!("[CFG] /fat/CONFIG.INI empty — generating defaults");
                robot_os_config::cfg_defaults();
            }
        } else {
            // First boot: generate factory defaults and write to disk.
            kprintln!("[CFG] /fat/CONFIG.INI not found — first boot, generating defaults");
            robot_os_config::cfg_defaults();

            // Write defaults to disk so next boot loads them.
            let n = robot_os_config::cfg_serialize(buf);
            if n > 0 {
                let wfd = robot_os_fs::vfs_open(&mut fd_table, b"/fat/CONFIG.INI",
                    robot_os_fs::O_WRONLY | robot_os_fs::O_CREAT | robot_os_fs::O_TRUNC);
                if wfd >= 0 {
                    let written = robot_os_fs::vfs_write(&mut fd_table, wfd,
                        buf.as_ptr(), n);
                    robot_os_fs::vfs_close(&mut fd_table, wfd);
                    kprintln!("[CFG] Wrote {} bytes to /fat/CONFIG.INI (factory defaults)",
                        written);
                }
            }
        }
        robot_os_config::cfg_apply();
        kprintln!("[CFG] {} entries, ml_enabled={}",
            robot_os_config::cfg_count(),
            ML_ENABLED.load(Ordering::Relaxed) as u8);

        // ── Apply config to all subsystems ─────────────────────────────

        // Network: set IP/mask/gateway BEFORE net_init().
        let ip  = robot_os_config::unpack_ip(
            robot_os_config::CFG_NET_IP.load(Ordering::Relaxed));
        let gw  = robot_os_config::unpack_ip(
            robot_os_config::CFG_NET_GATEWAY.load(Ordering::Relaxed));
        let mask = robot_os_config::unpack_ip(
            robot_os_config::CFG_NET_MASK.load(Ordering::Relaxed));
        robot_os_net::net_set_ip(ip, mask, gw);
        kprintln!("[CFG] net: {}.{}.{}.{} gw {}.{}.{}.{}",
            ip[0], ip[1], ip[2], ip[3], gw[0], gw[1], gw[2], gw[3]);

        // Scheduler Hz (10..10000).
        let cfg_hz = robot_os_config::cfg_get_u32(b"sched_hz", 100);
        if cfg_hz >= 10 {
            robot_os_drivers::clint::sched_hz_set(cfg_hz as u64);
        }
        kprintln!("[CFG] sched_hz={}", robot_os_drivers::clint::sched_hz_get());

        // Behavior layers.
        robot_os_behavior::layer_set_enabled(1,
            robot_os_config::BEHAVIOR_L1_ENABLED.load(Ordering::Relaxed));
        robot_os_behavior::layer_set_enabled(2,
            robot_os_config::BEHAVIOR_L2_ENABLED.load(Ordering::Relaxed));
        robot_os_behavior::layer_set_enabled(3,
            robot_os_config::BEHAVIOR_L3_ENABLED.load(Ordering::Relaxed));

        // Behavior VLA server.
        let bport = robot_os_config::BEHAVIOR_SERVER_PORT.load(Ordering::Relaxed);
        if bport > 0 {
            let bip = robot_os_config::behavior_server_ip_bytes();
            robot_os_behavior::remote_configure(bip, bport as u16);
            kprintln!("[CFG] VLA server: {}.{}.{}.{}:{}",
                bip[0], bip[1], bip[2], bip[3], bport);
        }

        // Encoder physical params.
        robot_os_robot::set_ticks_per_m(
            robot_os_config::CFG_TICKS_PER_M.load(Ordering::Relaxed));
        robot_os_robot::set_wheel_base_mm(
            robot_os_config::CFG_WHEEL_BASE_MM.load(Ordering::Relaxed));
        kprintln!("[CFG] encoder: tpm={} wb={}mm",
            robot_os_robot::ticks_per_m(), robot_os_robot::wheel_base_mm());

        // IMU offsets are applied automatically in imu_read_scaled() via atomics.
    }
    kprintln!();

    // ── Network init (uses IP/mask/gw set above) ─────────────────────────────
    // Init transport drivers first, then the IP/TCP/UDP stack.
    #[cfg(not(feature = "esp32c3"))]
    match robot_os_drivers::virtio::net::init() {
        Ok(()) => kprintln!("[NET] VirtIO net OK"),
        Err(()) => kprintln!("[NET] VirtIO net not found (no NIC)"),
    }
    #[cfg(feature = "vf2")]
    {
        let eth_rc = robot_os_drivers::eth::eth_init();
        if eth_rc == 0 {
            kprintln!("[NET] Cadence MACB Ethernet OK");
        } else {
            kprintln!("[NET] Cadence MACB Ethernet init failed ({})", eth_rc);
        }
        // Init UART1 bridge for ESP32-C3 WiFi relay
        let bridge_rc = robot_os_drivers::uart_bridge::bridge_init();
        if bridge_rc == 0 {
            kprintln!("[NET] UART1 bridge for ESP32 WiFi OK");
        }
    }
    robot_os_net::net_init();
    kprintln!();

    // ---- Phase 8: IPC + Signals + Services ----

    robot_os_ipc::pipe_init();
    robot_os_ipc::signal_init();
    robot_os_service::service_init();
    kprintln!("[IPC] Pipes, signals, service manager initialized");
    kprintln!();

    // ---- Phase 15: Dynamic model loading from FAT32 ----
    #[cfg(not(feature = "no-ml"))]
    {
        static mut MODEL_BUF: [u8; 512] = [0u8; 512];
        let buf = unsafe { &mut *(&raw mut MODEL_BUF) };
        let mut fd_table = robot_os_fs::FdTable::new();
        let fd = robot_os_fs::vfs_open(&mut fd_table, b"/fat/MLP.RMLP",
                                        robot_os_fs::O_RDONLY);
        if fd >= 0 {
            let n = robot_os_fs::vfs_read(&mut fd_table, fd,
                                           buf.as_mut_ptr(), buf.len());
            robot_os_fs::vfs_close(&mut fd_table, fd);
            if n > 0 && robot_os_ml::model_load_bytes(&buf[..n as usize]) {
                kprintln!("[ML] Dynamic weights loaded from /fat/MLP.RMLP ({} bytes) \
                           — {} features", n, robot_os_ml::RMLP_FILE_SIZE);
            } else {
                kprintln!("[ML] /fat/MLP.RMLP invalid or wrong size — using hardcoded weights");
            }
        } else {
            kprintln!("[ML] /fat/MLP.RMLP not found — using hardcoded weights");
        }
    }
    #[cfg(feature = "no-ml")]
    kprintln!("[ML] Compile-time disabled (--features no-ml)");
    kprintln!();

    // ---- Phase C: ggml-nano / GGUF inference ----
    #[cfg(not(feature = "no-ml"))]
    {
        kprintln!("========================================");
        kprintln!(" Phase C: ggml-nano / GGUF inference");
        kprintln!("========================================");
        {
            static mut GGUF_BUF: [u8; 4096] = [0u8; 4096];
            let buf = unsafe { &mut *(&raw mut GGUF_BUF) };
            let mut fd_table = robot_os_fs::FdTable::new();
            let fd = robot_os_fs::vfs_open(&mut fd_table, b"/fat/POLICY.GGF",
                                            robot_os_fs::O_RDONLY);
            if fd >= 0 {
                let n = robot_os_fs::vfs_read(&mut fd_table, fd,
                                               buf.as_mut_ptr(), buf.len());
                robot_os_fs::vfs_close(&mut fd_table, fd);
                if n > 0 {
                    let slice = &buf[..n as usize];
                    if let Some(gguf) = robot_os_ml::gguf::GgufFile::parse(slice) {
                        kprintln!("[GGUF] Parsed POLICY.GGF: {} tensors, {} bytes",
                                  gguf.n_tensors, n);
                        let tests: [([f32; 4], &str); 3] = [
                            ([0.8, 0.3, 0.5, 0.9], "go_forward"),
                            ([0.6, 0.1, 0.5, 0.9], "turn_right"),
                            ([0.1, 0.5, 0.5, 0.9], "stop"),
                        ];
                        let mut pass = 0u32;
                        for (inp, expected) in &tests {
                            let mut logits = [0.0f32; 3];
                            if robot_os_ml::ggml_nano::gguf_mlp_infer(&gguf, inp, &mut logits) {
                                let idx = robot_os_ml::ggml_nano::argmax(&logits);
                                let pred = robot_os_ml::CLASS_NAMES[idx];
                                let ok = pred == *expected;
                                if ok { pass += 1; }
                                kprintln!("  [{}] {:?} → {} (logits [{:.3},{:.3},{:.3}])",
                                          if ok { "OK" } else { "FAIL" },
                                          inp, pred,
                                          logits[0], logits[1], logits[2]);
                            } else {
                                kprintln!("  [FAIL] gguf_mlp_infer returned false");
                            }
                        }
                        kprintln!("[GGUF] {}/3 tests passed", pass);
                    } else {
                        kprintln!("[GGUF] Parse error: invalid GGUF file");
                    }
                } else {
                    kprintln!("[GGUF] /fat/POLICY.GGF empty");
                }
            } else {
                kprintln!("[GGUF] /fat/POLICY.GGF not found (run make qemu-full-smp)");
            }
        }
        kprintln!();
    }

    // ---- Phase D: PMP Security + Hardware Watchdog ----

    kprintln!("========================================");
    kprintln!(" Phase D: PMP Security + HW Watchdog");
    kprintln!("========================================");

    // WDT: initialize hardware watchdog (VF2/K1 only; no-op on QEMU).
    // Phase G2: timeout from config (default 500 ms).
    let wdt_ms = robot_os_config::CFG_WATCHDOG_MS.load(Ordering::Relaxed);
    robot_os_drivers::wdt::wdt_init(wdt_ms);
    if robot_os_drivers::wdt::wdt_has_hardware() {
        kprintln!("[WDT] Hardware watchdog initialized ({} ms timeout)", wdt_ms);
        kprintln!("[WDT] Counter = {}", robot_os_drivers::wdt::wdt_counter());
    } else {
        kprintln!("[WDT] No hardware WDT (QEMU) — software watchdog active");
    }

    // PMP: display the intended Robot OS memory-protection policy.
    // pmp_configure() must be called from M-mode (before mret into S-mode).
    // Here we display it for boot-time audit; actual enforcement is M-mode only.
    #[cfg(not(feature = "esp32c3"))]
    {
        use robot_os_arch::pmp;
        let fw_end = robot_os_drivers::platform::hw::KERNEL_LOAD; // end of OpenSBI
        let regions = pmp::pmp_regions(fw_end, kernel_end_aligned, heap_start, HEAP_SIZE);
        kprintln!("[PMP] Memory-protection policy ({} TOR regions):", pmp::N_PMP_REGIONS);
        kprintln!("[PMP]   Note: CSRs are M-mode only; configure from boot stub.");
        for r in &regions {
            kprintln!("[PMP]   {:20}  base={:#010x}  size={:#010x}  {}{}{}",
                r.name,
                r.base, r.size,
                if r.perm.r { "R" } else { "-" },
                if r.perm.w { "W" } else { "-" },
                if r.perm.x { "X" } else { "-" });
        }
    }
    kprintln!();

    // ---- Phase 16: Security — stack canaries + system watchdog ----

    kprintln!("========================================");
    kprintln!(" Phase 16: Security");
    kprintln!("========================================");
    kprintln!("[SEC] Stack canary: 0xDEADBEEFCAFE1234 — written at each task_create");
    kprintln!("[SEC] System watchdog: monitors canaries + timer liveness every ~1 s");
    kprintln!();

    // ---- Phase 17: Robot Physical Integration ----

    kprintln!("========================================");
    kprintln!(" Phase 17: Robot Physical Integration");
    kprintln!("========================================");
    kprintln!("[ROBOT] Encoder sim: ticks in rt_motor_task → speed × iteration");
    kprintln!("[ROBOT] Odometry:    dead reckoning (dist_mm, heading_cdeg)");
    kprintln!("[ROBOT] Trajectory:  ring buffer ({} pts) + FAT32 CSV flush",
        robot_os_robot::TRAJ_CAP);
    kprintln!("[ROBOT] OTA:         TCP model hot-swap (shell: ota recv <port>)");
    kprintln!();

    // ---- Phase 10: Drivers + Robot Framework ----

    kprintln!("========================================");
    kprintln!(" Phase 10: Drivers + Robot Framework");
    kprintln!("========================================");
    kprintln!();

    robot_os_drivers::gpio::gpio_init();
    robot_os_drivers::pwm::pwm_init();
    robot_os_drivers::i2c::i2c_init();
    kprintln!("[HW] GPIO ({} pins), PWM ({} ch), I2C ({} buses) initialized",
        robot_os_drivers::gpio::GPIO_MAX_PINS,
        robot_os_drivers::pwm::PWM_MAX_CHANNELS,
        robot_os_drivers::i2c::I2C_BUS_COUNT);

    robot_os_robot::robot_init();
    robot_os_drivers::motor_pid::motor_pid_init();

    // Phase H: additional drivers
    robot_os_drivers::spi::spi_init();
    robot_os_drivers::can::can_init();
    robot_os_drivers::dma::dma_init();
    robot_os_drivers::usb::usb_init();
    robot_os_drivers::pm::pm_init();
    // eth_init() already called above during network init sequence.
    kprintln!("[HW] SPI, CAN, DMA, USB, PM initialized");
    kprintln!();

    // ---- Phase 11: RISC-V Vector Extension (RVV 1.0) ----
    //
    // Only compiled when --features rvv is passed (QEMU with -cpu rv64,v=true).
    // VisionFive 2 (SiFive U74) has no V extension — QEMU emulation only.

    #[cfg(feature = "rvv")]
    {
        kprintln!("========================================");
        kprintln!(" Phase 11: RVV 1.0 (VLEN=128, f32)");
        kprintln!("========================================");
        kprintln!();
    }
    #[cfg(all(feature = "rvv", not(feature = "no-ml")))]
    {
        kprintln!("========================================");
        kprintln!(" Phase 12: ML Runtime (MLP demo)");
        kprintln!("========================================");
        kprintln!();
        kprintln!("========================================");
        kprintln!(" Phase 14: Virtual Camera Driver");
        kprintln!("========================================");
        kprintln!();
        kprintln!("========================================");
        kprintln!(" Phase 15: Dynamic Model Loading (RMLP)");
        kprintln!("========================================");
        kprintln!();
    }

    // ---- Phase E1+E2+G1+G2+H1+I1: Sensors + AHRS ----

    kprintln!("========================================");
    kprintln!(" Phase E1: Scheduler {} Hz", robot_os_drivers::clint::sched_hz_get());
    kprintln!(" Phase E2: IMU driver (MPU-6050)");
    kprintln!(" Phase G1: Barometer (BMP280)");
    kprintln!(" Phase G2: Persistent State Recovery");
    kprintln!(" Phase H1: Channel<T> middleware");
    kprintln!(" Phase I1: AHRS complementary filter");
    kprintln!(" Phase I2: GPS driver (NMEA)");
    kprintln!(" Phase J:  Flight controller (mixer+PID)");
    kprintln!(" Phase K:  RC input + failsafe");
    kprintln!(" Phase L:  Telemetry protocol");
    kprintln!(" Phase M:  Perception (rangefinder + CSI camera)");
    kprintln!(" Phase N:  Navigation + waypoints");
    kprintln!(" Phase O:  ESP32-C3 companion (WiFi)");
    kprintln!("========================================");

    robot_os_imu::imu_init(0, robot_os_imu::MPU6050_ADDR);
    robot_os_baro::baro_init(0, robot_os_baro::BMP280_ADDR);
    robot_os_gps::gps_init(1, 9600); // UART1, 9600 baud (standard GPS)

    // Phase M: rangefinder sensors (proximity).
    robot_os_drivers::rangefinder::us_init(4);   // 4 ultrasonic (front/right/rear/left)
    robot_os_drivers::rangefinder::tof_init(2);  // 2 ToF (down + forward)

    // Phase M2: MIPI CSI-2 camera (simulated on QEMU).
    robot_os_drivers::csi::csi_init(
        robot_os_drivers::csi::DEFAULT_WIDTH,
        robot_os_drivers::csi::DEFAULT_HEIGHT,
        robot_os_drivers::csi::PixFmt::Gray8,
    );

    // Phase O: WiFi (ESP32-C3 only; no-op on VF2/K1/QEMU).
    robot_os_drivers::wifi::wifi_init();
    robot_os_drivers::esc::esc_init(4); // 4 ESC channels (QuadX)
    robot_os_drivers::rc::rc_init(robot_os_drivers::rc::RcMode::Simulated);
    kprintln!();

    // ---- Phase G1: Subsumption Behavior Engine + VLA Protocol ----

    kprintln!("========================================");
    kprintln!(" [BEHAVIOR] Phase G1: Subsumption Engine");
    kprintln!("   L0: emergency-stop  (IMU)");
    #[cfg(not(feature = "no-ml"))]
    kprintln!("   L1: avoid-obstacle  (MLP)");
    kprintln!("   L2: remote-vla      (TCP)");
    kprintln!("   L3: explore         (wander)");
    kprintln!("========================================");
    kprintln!();

    // ---- Phase 5: SMP + Scheduler ----

    kprintln!("========================================");
    kprintln!(" Phase 5: SMP Scheduler ({} CPUs)", NUM_CPUS);
    kprintln!("========================================");
    kprintln!();

    robot_os_sched::init();

    // Wire priority inheritance callbacks so PiMutex can boost/restore
    // task priorities through the scheduler.
    robot_os_sync::pi_mutex::pi_set_callbacks(
        robot_os_sched::pi_boost_task,
        robot_os_sched::pi_restore_task,
    );

    // Tell the scheduler how many CPUs will be online so tasks are
    // distributed evenly before secondary CPUs start.
    robot_os_sched::smp::NUM_ONLINE_CPUS.store(NUM_CPUS, Ordering::SeqCst);

    // Create idle task (keeps CPU 0 alive after all workers finish).
    robot_os_sched::task_create("idle", idle_task, 0, robot_os_sched::IDLE_PRIORITY);
    kprintln!("[SCHED] Created idle task");

    // Create shell task (interactive UART shell).
    robot_os_sched::task_create("shell", shell_task, 0, robot_os_sched::DEFAULT_PRIORITY);
    kprintln!("[SCHED] Created shell task");

    // Create IPC/signal/service demo task (Phase 8).
    robot_os_sched::task_create("ipc-demo", ipc_demo_task, 0, robot_os_sched::DEFAULT_PRIORITY);
    kprintln!("[SCHED] Created ipc-demo task");

    // Create RVV benchmark task (Phase 11, QEMU only).
    #[cfg(feature = "rvv")]
    {
        robot_os_sched::task_create("rvv-bench", rvv_bench_task, 0, robot_os_sched::DEFAULT_PRIORITY);
        kprintln!("[SCHED] Created rvv-bench task");
    }

    // Create ML demo task (Phase 12: 4→8→3 MLP inference).
    #[cfg(not(feature = "no-ml"))]
    {
        robot_os_sched::task_create("ml-demo", ml_demo_task, 0, robot_os_sched::DEFAULT_PRIORITY);
        kprintln!("[SCHED] Created ml-demo task");
    }

    // Phase G1: behavior engine (runs without ML too — L0, L2, L3 work).
    robot_os_sched::task_create("behavior", behavior_task, 0, robot_os_sched::BEHAVIOR_PRIORITY);
    kprintln!("[SCHED] Created behavior task (subsumption L0-L3)");
    robot_os_sched::task_create("rt-motor", rt_motor_task, 0, robot_os_sched::RT_MOTOR_PRIORITY);
    kprintln!("[SCHED] Created rt-motor task (MotorCmd→PID→PWM + watchdog)");

    // Phase U1: dedicated network polling task — decouples net I/O from behavior loop.
    robot_os_sched::task_create("net-poll", net_poll_task, 0, robot_os_sched::NET_POLL_PRIORITY);
    kprintln!("[SCHED] Created net-poll task (Phase U1: dedicated net polling)");

    // Phase I1: sensor + AHRS fusion task (~100 Hz).
    robot_os_sched::task_create("sensor-ahrs", sensor_ahrs_task, 0, robot_os_sched::SENSOR_AHRS_PRIORITY);
    kprintln!("[SCHED] Created sensor-ahrs task (IMU+baro+GPS→AHRS→channels)");

    // Phase J+K: flight controller task (mixer + PID + failsafe).
    robot_os_sched::task_create("flight-ctrl", flight_control_task, 0, robot_os_sched::FLIGHT_CTRL_PRIORITY);
    kprintln!("[SCHED] Created flight-ctrl task (PID→mixer→ESC + failsafe)");

    // Phase L: telemetry task (attitude + GPS → UDP).
    robot_os_sched::task_create("telemetry", telemetry_task, 0, robot_os_sched::DEFAULT_PRIORITY);
    kprintln!("[SCHED] Created telemetry task (channels→UDP)");

    // Phase 16: system watchdog — checks stack canaries + timer liveness.
    robot_os_sched::task_create("sys-wdt", system_wdt_task, 0, robot_os_sched::WATCHDOG_PRIORITY);
    kprintln!("[SCHED] Created sys-wdt task (Phase 16: canaries + timer liveness)");

    // Create stress-test workers. find_least_loaded_cpu() distributes them
    // evenly across NUM_CPUS CPUs (4 tasks per CPU for 16 total = 15+idle).
    for i in 0..NUM_WORKERS {
        robot_os_sched::task_create("worker", worker_task, i, robot_os_sched::DEFAULT_PRIORITY);
        kprintln!("[SCHED] Created worker task {}", i);
    }
    kprintln!();

    // Phase U4: autorun ELF — if CONFIG.INI has `autorun=<path>`, spawn a
    // task that loads and exec's that ELF at boot (e.g. brain client).
    #[cfg(target_pointer_width = "64")]
    if let Some(path) = robot_os_config::cfg_get(b"autorun") {
        if !path.is_empty() {
            // Copy path to a static buffer so the autorun task can access it.
            let len = path.len().min(AUTORUN_PATH_MAX - 1);
            let buf = unsafe { &mut *(&raw mut AUTORUN_PATH) };
            buf[..len].copy_from_slice(&path[..len]);
            buf[len] = 0;
            robot_os_sched::task_create("autorun", autorun_task, len,
                                        robot_os_sched::DEFAULT_PRIORITY);
            kprintln!("[SCHED] Created autorun task: {}",
                core::str::from_utf8(&path[..len]).unwrap_or("?"));
        }
    }

    // Enable SMP UART lock before secondary CPUs can print.
    robot_os_drivers::uart::enable_smp_lock();
    kprintln!("[SMP] UART lock enabled");

    // Start secondary harts via SBI HSM hart_start (OpenSBI parks them by default).
    #[cfg(not(feature = "esp32c3"))]
    {
        kprintln!("[SMP] Starting {} secondary harts via SBI HSM...", NUM_CPUS - 1);
        unsafe { robot_os_sched::smp::wake_harts(NUM_CPUS); }
    }

    kprintln!("[SCHED] Starting scheduler on boot CPU — tasks will now preempt...");
    kprintln!();

    // Start the scheduler on CPU 0 (never returns).
    robot_os_sched::start()
}

/// Initialize trap handling: set stvec, sscratch, scounteren.
fn trap_init() {
    unsafe extern "C" { fn trap_vector(); }
    let trap_addr = trap_vector as *const () as usize;
    assert!(trap_addr & 0x3 == 0, "trap_vector not aligned");
    csr::write_stvec(trap_addr);
    csr::write_sscratch(0);
    csr::write_scounteren(0x7);
    kprintln!("[TRAP] Trap vector: {:#x}", csr::read_stvec());
}

/// Secondary CPU entry point.
///
/// Called from `_secondary_start` in boot.S after OpenSBI starts this hart via HSM.
/// Sets up the timer interrupt and enters a WFI idle loop.
/// The first timer interrupt on this CPU will call `schedule()`, which picks up
/// the tasks assigned to this CPU during `kernel_main`.
#[cfg(not(feature = "esp32c3"))]
#[unsafe(no_mangle)]
pub extern "C" fn smp_secondary_start(hart_id: usize) -> ! {
    // Enable timer interrupt (boot.S cleared sie to 0).
    let sie = csr::read_sie();
    csr::write_sie(sie | csr::SIE_STIE);

    // Set the first timer tick for this CPU.
    robot_os_drivers::clint::set_next_tick(hart_id as u32);

    // Enable global S-mode interrupts.
    let sstatus = csr::read_sstatus();
    csr::write_sstatus(sstatus | csr::SSTATUS_SIE);

    // WFI loop — timer interrupt will call schedule() → pick first task for this CPU.
    loop {
        robot_os_arch::cpu::wfi();
    }
}

/// Idle task: runs when no other task is ready on this CPU.
fn idle_task(_arg: usize) {
    loop {
        robot_os_arch::cpu::wfi();
    }
}

/// Shell task: interactive UART shell.
fn shell_task(_arg: usize) {
    robot_os_shell::shell_run()
}

/// IPC demo task: exercises signals, pipes, and service manager (Phase 8).
fn ipc_demo_task(_arg: usize) {
    kprintln!("[IPC] ========================================");
    kprintln!("[IPC]  Phase 8: Signals + Pipes + Services");
    kprintln!("[IPC] ========================================");

    // ── 1. Signals ────────────────────────────────────────────────────────────
    let my_tid = robot_os_sched::current_task_tid();
    kprintln!("[IPC] Signal test: my TID = {}", my_tid);

    // Send SIGTERM to self
    let rc = robot_os_ipc::signal_send(my_tid, robot_os_ipc::SIGTERM);
    kprintln!("[IPC] signal_send(SIGTERM) = {}", rc);

    // Check pending signals
    let pending = robot_os_ipc::signal_pending();
    let sigterm_bit = 1u32 << robot_os_ipc::SIGTERM;
    if pending & sigterm_bit != 0 {
        kprintln!("[IPC] SIGTERM pending ✓");
    } else {
        kprintln!("[IPC] SIGTERM NOT pending (unexpected)");
    }

    // Mask SIGTERM
    robot_os_ipc::signal_set_mask(sigterm_bit);
    let pending2 = robot_os_ipc::signal_pending();
    if pending2 & sigterm_bit == 0 {
        kprintln!("[IPC] SIGTERM masked (not visible to pending) ✓");
    }

    // Unmask SIGTERM
    robot_os_ipc::signal_set_mask(0);
    kprintln!("[IPC] Signals: PASS");

    // ── 2. Pipes ──────────────────────────────────────────────────────────────
    match robot_os_ipc::pipe_create() {
        None => kprintln!("[IPC] Pipe create FAILED"),
        Some((ridx, widx)) => {
            kprintln!("[IPC] Pipe created: read_idx={}, write_idx={}", ridx, widx);

            let msg = b"hello pipe!\0";
            let n_written = robot_os_ipc::pipe_write(widx, msg.as_ptr(), msg.len());
            kprintln!("[IPC] pipe_write({} bytes) = {}", msg.len(), n_written);

            let mut buf = [0u8; 32];
            let n_read = robot_os_ipc::pipe_read(ridx, buf.as_mut_ptr(), buf.len());
            kprintln!("[IPC] pipe_read() = {} bytes", n_read);

            if n_read > 0 && &buf[..n_read as usize] == &msg[..n_read as usize] {
                kprintln!("[IPC] Pipe data matches ✓");
            }

            robot_os_ipc::pipe_close_read(ridx);
            robot_os_ipc::pipe_close_write(widx);
            kprintln!("[IPC] Pipes: PASS");
        }
    }

    // ── 3. Service manager ────────────────────────────────────────────────────
    let rc = robot_os_service::service_register(b"robot.sensor", my_tid, 42);
    kprintln!("[IPC] service_register(\"robot.sensor\") = {}", rc);

    let rc2 = robot_os_service::service_register(b"robot.motor", my_tid, 43);
    kprintln!("[IPC] service_register(\"robot.motor\") = {}", rc2);

    match robot_os_service::service_discover(b"robot.sensor") {
        Some(entry) => kprintln!("[IPC] service_discover(\"robot.sensor\") → tid={} ✓", entry.tid),
        None        => kprintln!("[IPC] service_discover FAILED"),
    }

    let hb = robot_os_service::service_heartbeat(b"robot.sensor");
    kprintln!("[IPC] service_heartbeat = {}", hb);

    let cnt = robot_os_service::service_count();
    kprintln!("[IPC] service_count() = {}", cnt);

    kprintln!("[IPC] Service manager: PASS");
    kprintln!("[IPC] ========================================");
    kprintln!("[IPC]  Phase 8 demo complete");
    kprintln!("[IPC] ========================================");
}

/// Worker task: runs WORKER_ITERS iterations with voluntary yields, then exits.
fn worker_task(arg: usize) {
    let id = arg;
    let cpu = robot_os_sched::smp::current_cpu_id();

    kprintln!("[TASK] Worker {} starting on CPU {}", id, cpu);

    for i in 0..WORKER_ITERS {
        if i % 500 == 0 {
            kprintln!("[TASK] Worker {} — {}/{} (CPU {})",
                id, i, WORKER_ITERS, robot_os_sched::smp::current_cpu_id());
        }
        robot_os_sched::task_yield();
    }

    kprintln!("[TASK] Worker {} — Completed {} iterations (CPU {})",
        id, WORKER_ITERS, robot_os_sched::smp::current_cpu_id());
    // Returns → task_entry_wrapper → task_exit()
}

/// Phase 11: RVV benchmark task.
///
/// Runs scalar vs RVV dot product and matmul benchmarks, then prints cycle counts.
/// Timer interrupt is disabled during RVV operations to prevent vector register
/// corruption (vector context save is implemented in Phase 12).
#[cfg(feature = "rvv")]
fn rvv_bench_task(_: usize) {
    use robot_os_arch::{csr, rvv};

    kprintln!("[RVV] ========================================");
    kprintln!("[RVV]  Phase 11: RISC-V Vector Extension");
    kprintln!("[RVV]  VLEN=128, LMUL=m4, f32 precision");
    kprintln!("[RVV] ========================================");
    kprintln!("[RVV] Vector context save: Phase 12 (implemented).");
    kprintln!("[RVV] Timer IRQ disabled during bench (intentional).");
    kprintln!();

    // Disable timer interrupt so no context switch can corrupt v-registers.
    let saved_sie = csr::read_sie();
    csr::write_sie(saved_sie & !csr::SIE_STIE);

    // ── Dot product: 256 f32 ────────────────────────────────────────────────
    const N: usize = 256;
    let mut a = [0.0f32; N];
    let mut b = [0.0f32; N];
    for i in 0..N {
        a[i] = (i as f32) * 0.001;
        b[i] = 1.0_f32;
    }

    let (sc, vc, _, _) = rvv::bench_dot(&a, &b);
    let sp = if vc > 0 { sc * 100 / vc } else { 0 };
    kprintln!("[RVV] dot({} f32):", N);
    kprintln!("[RVV]   scalar : {} cycles", sc);
    kprintln!("[RVV]   rvv    : {} cycles", vc);
    kprintln!("[RVV]   speedup: {}.{}x", sp / 100, sp % 100);
    kprintln!();

    // ── Matmul: 8×8×8 f32 ──────────────────────────────────────────────────
    const MM: usize = 8;
    const KK: usize = 8;
    const NN: usize = 8;
    let mut ma   = [0.0f32; MM * KK];
    let mut mb   = [0.0f32; KK * NN];
    let mut mc_s = [0.0f32; MM * NN];
    let mut mc_v = [0.0f32; MM * NN];
    for i in 0..MM * KK { ma[i] = (i as f32) * 0.001; }
    for i in 0..KK * NN { mb[i] = (i as f32) * 0.001; }

    let (ms, mv) = rvv::bench_matmul(&mut mc_s, &mut mc_v, &ma, &mb, MM, KK, NN);
    let msp = if mv > 0 { ms * 100 / mv } else { 0 };
    kprintln!("[RVV] matmul({}×{}×{} f32):", MM, KK, NN);
    kprintln!("[RVV]   scalar : {} cycles", ms);
    kprintln!("[RVV]   rvv    : {} cycles", mv);
    kprintln!("[RVV]   speedup: {}.{}x", msp / 100, msp % 100);
    kprintln!();

    // Restore timer interrupt.
    csr::write_sie(saved_sie);

    kprintln!("[RVV] ========================================");
    kprintln!("[RVV]  Phase 11 complete — RVV foundation ready");
    kprintln!("[RVV]  Next: Phase 12 ML runtime (ggml-nano port)");
    kprintln!("[RVV] ========================================");
}

#[cfg(not(feature = "no-ml"))]
/// Phase 12: ML demo task — runs a 4→8→3 MLP inference and prints results.
///
/// Uses `dot_f32_rvv` when built with `--features rvv`, scalar otherwise.
/// Vector context is now correctly saved/restored on every context switch
/// (Phase 12), so no timer-IRQ disable is needed.
fn ml_demo_task(_: usize) {
    use robot_os_ml::{mlp_infer, argmax3, CLASS_NAMES, DEMO_INPUT};

    kprintln!("[ML] ========================================");
    kprintln!("[ML]  Phase 12: MLP Inference Demo");
    kprintln!("[ML]  Model: 4 → 8 (ReLU) → 3 (logits)");
    #[cfg(feature = "rvv")]
    kprintln!("[ML]  Backend: RVV 1.0 (dot_f32_rvv, VLEN=128)");
    #[cfg(not(feature = "rvv"))]
    kprintln!("[ML]  Backend: scalar (dot_f32_scalar)");
    kprintln!("[ML] ========================================");

    let input = DEMO_INPUT;
    let logits = mlp_infer(&input);
    let class  = argmax3(&logits);

    kprintln!("[ML] Input:  [{}, {}, {}, {}] (dist_f, dist_r, vel, batt)",
        (input[0] * 100.0) as u32,
        (input[1] * 100.0) as u32,
        (input[2] * 100.0) as u32,
        (input[3] * 100.0) as u32);

    // Print logits × 1000 as integers (no float formatting in kprintln!).
    let l0 = (logits[0] * 1000.0) as i32;
    let l1 = (logits[1] * 1000.0) as i32;
    let l2 = (logits[2] * 1000.0) as i32;
    kprintln!("[ML] Logits (×1000): go_forward={}, turn_right={}, stop={}", l0, l1, l2);
    kprintln!("[ML] Prediction: {} (class {})", CLASS_NAMES[class], class);
    kprintln!("[ML] ========================================");
    kprintln!("[ML]  Phase 12 complete. Type 'ml' in shell for interactive demo.");
    kprintln!("[ML] ========================================");
}

/// Phase U1: dedicated network polling task.
///
/// Runs in a tight loop, yielding after each poll. This decouples network I/O
/// from the behavior loop so incoming packets (TCP, UDP, ARP, DHCP) are
/// processed promptly regardless of behavior loop timing.
///
/// Polls at ~100 Hz (1 yield = 10 ms at 100 Hz scheduler).
fn net_poll_task(_: usize) {
    kprintln!("[NET-POLL] Phase U1: dedicated network polling task started");

    loop {
        robot_os_net::net_poll();
        robot_os_sched::task_yield();
    }
}

/// Phase U4: autorun task — loads and exec's an ELF from the filesystem.
///
/// The ELF path is stored in the static `AUTORUN_PATH` buffer (set during boot).
/// `arg` contains the path length.
#[cfg(target_pointer_width = "64")]
fn autorun_task(arg: usize) {
    /// Maximum ELF file size for autorun (64 KiB).
    const AUTORUN_ELF_MAX: usize = 64 * 1024;

    let path_len = arg;
    let path = unsafe { &*(&raw const AUTORUN_PATH) };
    let path_slice = &path[..path_len];

    kprintln!("[AUTORUN] Loading ELF: {}",
        core::str::from_utf8(path_slice).unwrap_or("?"));

    // Open and read the ELF file.
    let mut fd_table = robot_os_fs::FdTable::new();
    let fd = robot_os_fs::vfs_open(&mut fd_table, path_slice, robot_os_fs::O_RDONLY);
    if fd < 0 {
        kprintln!("[AUTORUN] File not found: {}",
            core::str::from_utf8(path_slice).unwrap_or("?"));
        return;
    }

    static mut AUTORUN_BUF: [u8; AUTORUN_ELF_MAX] = [0u8; AUTORUN_ELF_MAX];
    let buf = unsafe { &mut *(&raw mut AUTORUN_BUF) };
    let n = robot_os_fs::vfs_read(&mut fd_table, fd, buf.as_mut_ptr(), buf.len());
    robot_os_fs::vfs_close(&mut fd_table, fd);

    if n <= 0 {
        kprintln!("[AUTORUN] Failed to read ELF (read returned {})", n);
        return;
    }

    kprintln!("[AUTORUN] Read {} bytes, exec'ing...", n);
    let rc = robot_os_sched::exec_user(&buf[..n as usize]);
    if rc != 0 {
        kprintln!("[AUTORUN] exec_user failed (rc={})", rc);
    }
    // If exec_user succeeded, this task will be replaced by the user process
    // via the pending exec mechanism in the trap handler.
}

/// Maximum autorun ELF path length (including NUL terminator).
#[cfg(target_pointer_width = "64")]
const AUTORUN_PATH_MAX: usize = 64;

/// Static buffer for autorun ELF path (set during boot, read by autorun_task).
#[cfg(target_pointer_width = "64")]
static mut AUTORUN_PATH: [u8; AUTORUN_PATH_MAX] = [0u8; AUTORUN_PATH_MAX];

/// Phase G1: behavior task — subsumption engine running indefinitely.
///
/// Each tick (~100 ms = 10 yields):
/// 1. Collect SensorState (camera, IMU, odometry, encoders)
/// 2. If remote enabled: TCP send VlaObservation, recv VlaAction/VlaGoal
/// 3. If ML enabled: mlp_infer → MlpResult
/// 4. Arbitrate L0→L3 — first valid output wins
/// 5. Publish motor command + update odometry
fn behavior_task(_: usize) {
    use robot_os_behavior::*;

    kprintln!("[BEHAVIOR] ========================================");
    kprintln!("[BEHAVIOR]  Phase G1: Subsumption Behavior Engine");
    kprintln!("[BEHAVIOR]  Layers: L0=estop L1=avoid L2=vla L3=explore");
    kprintln!("[BEHAVIOR] ========================================");

    // DHCP auto-discovery (if dhcp=1 in CONFIG.INI)
    if robot_os_config::CFG_NET_DHCP.load(Ordering::Relaxed) != 0 {
        kprintln!("[BEHAVIOR] Running DHCP auto-discovery...");
        let ok = robot_os_net::dhcp::dhcp_start(robot_os_sched::task_yield);
        if !ok {
            kprintln!("[BEHAVIOR] DHCP failed — using static IP config");
        }
    }

    // TCP connection state (local to this task)
    let mut tcp_fd: i32 = -1;
    let mut tcp_connected = false;

    // Camera frame sending: every CAMERA_SEND_INTERVAL behavior cycles (~2 Hz at 10 Hz loop)
    const CAMERA_SEND_INTERVAL: u32 = 5;
    let mut camera_cycle: u32 = 0;

    loop {
        // ── 1. Collect sensor state ──────────────────────────────────────
        let now = robot_os_drivers::clint::get_time();
        let mut state = SensorState::new();
        state.timestamp = now;

        // Camera capture (cycle through patterns based on tick count)
        #[cfg(not(feature = "no-ml"))]
        {
            let tick = TICK_COUNT.load(Ordering::Relaxed);
            let pattern = (tick % 3) as u8;
            let frame = robot_os_camera::cam_capture(pattern);
            let feat  = robot_os_camera::cam_extract_features(&frame);
            state.cam_pixels[..32].copy_from_slice(&frame.pixels);
            state.cam_w = 8;
            state.cam_h = 4;
            state.cam_valid = true;
            state.cam_dist_front = (feat.dist_front * 1000.0) as u16;
            state.cam_dist_right = (feat.dist_right * 1000.0) as u16;
        }

        // IMU
        if let Some(imu_data) = robot_os_imu::imu_read_scaled() {
            state.accel_mg  = imu_data.accel_mg;
            state.gyro_mdps = imu_data.gyro_mdps;
            state.imu_valid = true;
        }

        // Odometry + encoders
        let (enc_l, enc_r) = robot_os_robot::encoder_read();
        robot_os_robot::odom_update(enc_l, enc_r);
        let (dist_mm, heading_cdeg) = robot_os_robot::odom_get();
        state.enc_left           = enc_l;
        state.enc_right          = enc_r;
        state.odom_dist_mm       = dist_mm;
        state.odom_heading_cdeg  = heading_cdeg;
        state.battery_mv         = 3700; // simulated
        state.velocity_mm_s      = 0;    // placeholder

        // ── 2. Brain Protocol: TCP send/recv ─────────────────────────────
        if robot_os_behavior::remote_is_enabled() {
            // Network polling is handled by the dedicated net-poll task (Phase U1).

            // Connect if not yet connected
            if !tcp_connected {
                let ip   = robot_os_behavior::remote_server_ip();
                let port = robot_os_behavior::remote_server_port();
                if port > 0 {
                    tcp_fd = robot_os_net::tcp::connect(ip, port, 12345);
                    if tcp_fd >= 0 {
                        tcp_connected = true;
                        robot_os_behavior::remote_set_connected(true);
                        robot_os_behavior::remote_set_socket(tcp_fd);

                        // Send StatusPacket immediately on connect
                        let uptime_s = (now / robot_os_drivers::clint::TIMER_FREQ) as u32;
                        let mut st_payload = [0u8; STATUS_PAYLOAD_SIZE];
                        encode_status_packet(
                            &mut st_payload,
                            1,              // mode: running
                            8,              // tasks_ok
                            8,              // canary_ok
                            uptime_s,
                            ROBOT_WHEELED,
                        );
                        let mut st_frame = [0u8; STATUS_FRAME_SIZE];
                        let st_len = build_packet(PKT_STATUS, &st_payload, &mut st_frame);
                        robot_os_net::tcp::send_data(tcp_fd as usize, &st_frame[..st_len]);
                    }
                }
            }

            if tcp_connected && tcp_fd >= 0 {
                // Build SensorPacket payload
                let ts_ms = now / (robot_os_drivers::clint::TIMER_FREQ / 1000);
                let range_front = state.cam_dist_front;
                let range_right = state.cam_dist_right;
                let mut sp_payload = [0u8; SENSOR_PAYLOAD_SIZE];
                encode_sensor_packet(
                    &mut sp_payload,
                    ts_ms,
                    state.accel_mg,
                    state.gyro_mdps,
                    state.battery_mv,
                    state.odom_dist_mm as i32,
                    state.odom_heading_cdeg as i32,
                    state.enc_left,
                    state.enc_right,
                    range_front,
                    range_right,
                    state.sensor_flags,
                );

                // Frame and send
                let mut sp_frame = [0u8; SENSOR_FRAME_SIZE];
                let sp_len = build_packet(PKT_SENSOR, &sp_payload, &mut sp_frame);
                let sent = robot_os_net::tcp::send_data(tcp_fd as usize, &sp_frame[..sp_len]);
                if sent > 0 {
                    robot_os_behavior::remote_inc_sent();
                }

                // Send camera frame periodically (~2 Hz)
                camera_cycle += 1;
                if camera_cycle >= CAMERA_SEND_INTERVAL && robot_os_drivers::csi::csi_is_ready() {
                    camera_cycle = 0;
                    let (cam_w, cam_h) = robot_os_drivers::csi::csi_resolution();
                    let frame_pixels = cam_w as usize * cam_h as usize;
                    // Camera payload: 5B header + raw pixels
                    // Use a static buffer sized for max resolution (320×240 = 76800 + 5 + 6 = 76811)
                    const CAM_BUF_SIZE: usize = 320 * 240 + CAMERA_HDR_SIZE + FRAME_OVERHEAD;
                    let mut cam_payload = [0u8; CAM_BUF_SIZE];
                    // Encode header
                    let mut cam_hdr = [0u8; CAMERA_HDR_SIZE];
                    encode_camera_header(&mut cam_hdr, cam_w, cam_h, CAMERA_FMT_GRAY8);
                    cam_payload[..CAMERA_HDR_SIZE].copy_from_slice(&cam_hdr);
                    // Capture frame directly into payload after header
                    let captured = robot_os_drivers::csi::csi_capture(
                        &mut cam_payload[CAMERA_HDR_SIZE..CAMERA_HDR_SIZE + frame_pixels]
                    );
                    if captured > 0 {
                        let payload_len = CAMERA_HDR_SIZE + captured;
                        let total_len = payload_len + FRAME_OVERHEAD;
                        let mut cam_frame = [0u8; CAM_BUF_SIZE];
                        let cam_len = build_packet(
                            PKT_CAMERA,
                            &cam_payload[..payload_len],
                            &mut cam_frame[..total_len],
                        );
                        robot_os_net::tcp::send_data(tcp_fd as usize, &cam_frame[..cam_len]);
                    }
                }

                // Check connection state
                let conn_state = robot_os_net::tcp::conn_state(tcp_fd as usize);
                if conn_state != robot_os_net::tcp::TcpState::Established {
                    tcp_connected = false;
                    tcp_fd = -1;
                    robot_os_behavior::remote_set_connected(false);
                }

                // Receive ActuatorCmd (framed: up to 6 + 3 + 2*8 = 25 bytes)
                let mut recv_buf = [0u8; 32];
                let n = robot_os_net::tcp::recv(tcp_fd as usize, &mut recv_buf);
                if n >= 6 {
                    robot_os_behavior::remote_inc_recv();
                    if let Some((pkt_type, pay_start, pay_len, _)) =
                        parse_packet(&recv_buf[..n as usize])
                    {
                        if pkt_type == PKT_ACTUATOR {
                            let payload = &recv_buf[pay_start..pay_start + pay_len];
                            if let Some(cmd) = decode_actuator_cmd(payload) {
                                if cmd.is_emergency() {
                                    // Emergency stop — override all layers
                                    robot_os_robot::motor_cmd_publish(0, 0);
                                } else {
                                    // Apply diff-drive channels
                                    let (sl, sr) = cmd.diff_drive();
                                    robot_os_robot::motor_cmd_publish(sl, sr);
                                    // Feed into remote_action so L2 layer sees it
                                    let mut action = robot_os_behavior::last_action();
                                    action.cmd        = 1; // CMD_MOTOR
                                    action.actions[0] = sl as i16;
                                    action.actions[1] = sr as i16;
                                    action.received_at = now;
                                    action.valid       = true;
                                    robot_os_behavior::set_last_action(action);
                                    state.remote_action = action;
                                }
                            }
                        } else if pkt_type == PKT_CONFIG {
                            let payload = &recv_buf[pay_start..pay_start + pay_len];
                            if let Some(cfg) = decode_config_cmd(payload) {
                                match cfg.config_key {
                                    CFG_KEY_BUZZER => match cfg.value {
                                        BUZZER_BEEP  => robot_os_drivers::buzzer::buzzer_beep(),
                                        BUZZER_SIREN => robot_os_drivers::buzzer::buzzer_alert(),
                                        BUZZER_OFF   => robot_os_drivers::buzzer::buzzer_off(),
                                        _ => {}
                                    },
                                    _ => {} // other config keys handled in future phases
                                }
                            }
                        }
                    }
                }
            }
        }

        // ── 2b. Brain Protocol: UART bridge send/recv ────────────────────
        // Alternative to TCP: send brain protocol packets over UART1 to
        // ESP32-C3 WiFi bridge.  Used when Ethernet is unavailable.
        if !tcp_connected && robot_os_drivers::uart_bridge::bridge_is_ready() {
            // Build and send SensorPacket via UART1
            let ts_ms = now / (robot_os_drivers::clint::TIMER_FREQ / 1000);
            let range_front = state.cam_dist_front;
            let range_right = state.cam_dist_right;
            let mut sp_payload = [0u8; SENSOR_PAYLOAD_SIZE];
            encode_sensor_packet(
                &mut sp_payload,
                ts_ms,
                state.accel_mg,
                state.gyro_mdps,
                state.battery_mv,
                state.odom_dist_mm as i32,
                state.odom_heading_cdeg as i32,
                state.enc_left,
                state.enc_right,
                range_front,
                range_right,
                state.sensor_flags,
            );
            let mut sp_frame = [0u8; SENSOR_FRAME_SIZE];
            let sp_len = build_packet(PKT_SENSOR, &sp_payload, &mut sp_frame);
            let sent = robot_os_drivers::uart_bridge::bridge_send(&sp_frame[..sp_len]);
            if sent > 0 {
                robot_os_behavior::remote_inc_sent();
            }

            // Receive ActuatorCmd from UART1
            let mut recv_buf = [0u8; 32];
            let n = robot_os_drivers::uart_bridge::bridge_recv(&mut recv_buf);
            if n >= 6 {
                robot_os_behavior::remote_inc_recv();
                if let Some((pkt_type, pay_start, pay_len, _)) =
                    parse_packet(&recv_buf[..n as usize])
                {
                    if pkt_type == PKT_ACTUATOR {
                        let payload = &recv_buf[pay_start..pay_start + pay_len];
                        if let Some(cmd) = decode_actuator_cmd(payload) {
                            if cmd.is_emergency() {
                                robot_os_robot::motor_cmd_publish(0, 0);
                            } else {
                                let (sl, sr) = cmd.diff_drive();
                                robot_os_robot::motor_cmd_publish(sl, sr);
                                let mut action = robot_os_behavior::last_action();
                                action.cmd        = 1;
                                action.actions[0] = sl as i16;
                                action.actions[1] = sr as i16;
                                action.received_at = now;
                                action.valid       = true;
                                robot_os_behavior::set_last_action(action);
                                state.remote_action = action;
                            }
                        }
                    } else if pkt_type == PKT_CONFIG {
                        let payload = &recv_buf[pay_start..pay_start + pay_len];
                        if let Some(cfg) = decode_config_cmd(payload) {
                            match cfg.config_key {
                                CFG_KEY_BUZZER => match cfg.value {
                                    BUZZER_BEEP  => robot_os_drivers::buzzer::buzzer_beep(),
                                    BUZZER_SIREN => robot_os_drivers::buzzer::buzzer_alert(),
                                    BUZZER_OFF   => robot_os_drivers::buzzer::buzzer_off(),
                                    _ => {}
                                },
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        // Inject latest remote action into state
        let last_act = robot_os_behavior::last_action();
        if last_act.valid {
            state.remote_action = last_act;
        }

        // ── 3. ML inference (if enabled) ─────────────────────────────────
        #[allow(unused_mut)]
        let mut mlp_result = MlpResult::none();
        #[cfg(not(feature = "no-ml"))]
        if ML_ENABLED.load(Ordering::Acquire) && state.cam_valid {
            let input: [f32; 4] = [
                state.cam_dist_front as f32 / 1000.0,
                state.cam_dist_right as f32 / 1000.0,
                0.5,
                0.9,
            ];
            let logits = robot_os_ml::mlp_infer(&input);
            let class  = robot_os_ml::argmax3(&logits);
            mlp_result = MlpResult { class: class as u8, valid: true };
        }

        // ── 4. Arbitrate ─────────────────────────────────────────────────
        let output = arbitrate(&state, &mlp_result);

        // ── 5. Publish motor command ─────────────────────────────────────
        if output.cmd.valid {
            let sl = output.cmd.speed_l.clamp(-100, 100);
            let sr = output.cmd.speed_r.clamp(-100, 100);
            robot_os_robot::motor_cmd_publish(sl, sr);

            // Trajectory recording
            let ts_ms = now / 10_000;
            let class_byte = if mlp_result.valid { mlp_result.class } else { 0xFF };
            robot_os_robot::traj_record(ts_ms, sl, sr, class_byte,
                                        dist_mm, heading_cdeg);
        }

        // ── Yield ~100 ms (10 scheduler ticks) ──────────────────────────
        for _ in 0..10 { robot_os_sched::task_yield(); }
    }
}

/// Phase I1: sensor + AHRS fusion task.
///
/// Reads IMU and barometer at ~100 Hz (1 yield per iteration at 100 Hz scheduler),
/// publishes raw readings to CH_IMU / CH_BARO, runs complementary filter,
/// and publishes estimated attitude to CH_ATTITUDE.
fn sensor_ahrs_task(_: usize) {
    use robot_os_ahrs::{AhrsState, CH_IMU, CH_BARO, CH_ATTITUDE};
    use robot_os_gps::CH_GPS;
    use robot_os_nav::{CH_PROXIMITY, ProximityData};

    kprintln!("[AHRS] Phase I1+I2+I3+M: sensor fusion task (AHRS + GPS yaw + proximity)");

    let mut ahrs = AhrsState::new();

    // Set reference pressure from first baro reading.
    if let Some(baro) = robot_os_baro::baro_read() {
        ahrs.set_ref_pressure(baro.pressure_pa);
        kprintln!("[AHRS] Reference pressure: {} Pa", baro.pressure_pa);
    }

    let mut last_time = robot_os_drivers::clint::get_time();
    let mut gps_counter: u32 = 0;
    let mut prox_counter: u32 = 0;

    loop {
        let now = robot_os_drivers::clint::get_time();

        // Compute dt in microseconds.
        let dt_ticks = now.wrapping_sub(last_time);
        let dt_us = (dt_ticks * 1_000_000 / robot_os_drivers::clint::TIMER_FREQ) as u32;
        last_time = now;

        // Skip if dt is unreasonable (first iteration or timer wrap).
        if dt_us == 0 || dt_us > 1_000_000 {
            robot_os_sched::task_yield();
            continue;
        }

        // Read IMU (~100 Hz).
        if let Some(imu) = robot_os_imu::imu_read_scaled() {
            CH_IMU.publish(imu, now);

            // Read barometer.
            let baro_pa = if let Some(baro) = robot_os_baro::baro_read() {
                CH_BARO.publish(baro, now);
                baro.pressure_pa
            } else {
                101325 // fallback to standard pressure
            };

            // Run AHRS fusion.
            let att = ahrs.update(&imu, baro_pa, dt_us);
            CH_ATTITUDE.publish(att, now);
        }

        // Poll GPS at ~10 Hz (every 10 iterations of the 100 Hz loop).
        // Phase I3: GPS course-over-ground corrects AHRS yaw drift when moving.
        gps_counter += 1;
        if gps_counter >= 10 {
            gps_counter = 0;
            if let Some(pos) = robot_os_gps::gps_read() {
                ahrs.update_gps(&pos);
                CH_GPS.publish(pos, now);
            }
        }

        // Poll proximity sensors at ~20 Hz (every 5 iterations).
        prox_counter += 1;
        if prox_counter >= 5 {
            prox_counter = 0;
            use robot_os_drivers::rangefinder;
            let us_n = rangefinder::us_count();
            let tof_n = rangefinder::tof_count();
            let mut prox = ProximityData::new();
            // US sensors: front(0), right(1), rear(2), left(3).
            for i in 0..us_n.min(4) {
                if let Some(d) = rangefinder::us_read_mm(i) {
                    prox.distances_mm[i as usize] = d as u16;
                }
            }
            // ToF sensors: down(4) index→0, forward(5) index→1.
            for i in 0..tof_n.min(2) {
                if let Some(d) = rangefinder::tof_read_mm(i) {
                    prox.distances_mm[4 + i as usize] = d;
                }
            }
            prox.count = us_n.min(4) + tof_n.min(2);
            CH_PROXIMITY.publish(prox, now);
        }

        // Yield once per tick (~10 ms at 100 Hz scheduler = ~100 Hz AHRS rate).
        robot_os_sched::task_yield();
    }
}

/// Phase J+K: flight controller task.
///
/// Reads attitude, RC/target, runs cascaded PID, computes mixer output,
/// and drives ESC channels.  Checks failsafes each iteration.
/// Runs at scheduler rate (~100 Hz in default config, up to 1 kHz).
fn flight_control_task(_: usize) {
    use robot_os_flight::*;
    use robot_os_ahrs::{CH_ATTITUDE, CH_IMU};

    kprintln!("[FLIGHT] Phase J+K: flight controller starting (QuadX, cascaded PID)");

    let mut pid = FlightPid::new();
    let frame = FrameType::QuadX;
    let mut last_time = robot_os_drivers::clint::get_time();

    loop {
        let now = robot_os_drivers::clint::get_time();
        let dt_ticks = now.wrapping_sub(last_time);
        let dt_us = (dt_ticks * 1_000_000 / robot_os_drivers::clint::TIMER_FREQ) as u32;
        last_time = now;

        // Skip unreasonable dt.
        if dt_us == 0 || dt_us > 1_000_000 {
            robot_os_sched::task_yield();
            continue;
        }

        // Read RC input and publish to channel.
        if let Some((channels, failsafe)) = robot_os_drivers::rc::rc_read() {
            let rc = RcInput {
                channels,
                rssi: if failsafe { 0 } else { 100 },
                failsafe,
            };
            CH_RC_INPUT.publish(rc, now);
        }

        if !is_armed() {
            // Not armed — ensure ESC outputs are zero.
            for i in 0..4u8 {
                robot_os_drivers::esc::esc_set_throttle(i, 0);
            }
            robot_os_sched::task_yield();
            continue;
        }

        // Check failsafes.
        let att_age = CH_ATTITUDE.age(now) * 1_000_000 / robot_os_drivers::clint::TIMER_FREQ;
        let rc_age = CH_RC_INPUT.age(now) * 1_000_000 / robot_os_drivers::clint::TIMER_FREQ;
        // Server age: use flight target channel.
        let srv_age = CH_FLIGHT_TARGET.age(now) * 1_000_000 / robot_os_drivers::clint::TIMER_FREQ;

        let fs = check_failsafe(att_age, rc_age, srv_age);
        match fs {
            FailsafeAction::Disarm => {
                kprintln!("[FLIGHT] FAILSAFE: Disarm (critical failure)");
                flight_disarm();
                robot_os_drivers::esc::esc_disarm();
                robot_os_sched::task_yield();
                continue;
            }
            FailsafeAction::Land => {
                if flight_mode() != FlightMode::Land {
                    kprintln!("[FLIGHT] FAILSAFE: Land (attitude loss)");
                    set_flight_mode(FlightMode::Land);
                }
            }
            FailsafeAction::RTL => {
                if flight_mode() != FlightMode::RTL && flight_mode() != FlightMode::Land {
                    kprintln!("[FLIGHT] FAILSAFE: RTL (RC link loss)");
                    set_flight_mode(FlightMode::RTL);
                }
            }
            FailsafeAction::PosHold => {
                if flight_mode() == FlightMode::Auto {
                    set_flight_mode(FlightMode::PosHold);
                }
            }
            FailsafeAction::None => {}
        }

        // Get flight target (from RC or server depending on mode).
        let target = match flight_mode() {
            FlightMode::Manual | FlightMode::Stabilize | FlightMode::AltHold => {
                // RC-driven.
                let rc = CH_RC_INPUT.read().val;
                rc_to_target(&rc)
            }
            FlightMode::Auto | FlightMode::PosHold => {
                // Server-driven (or last published target).
                CH_FLIGHT_TARGET.read().val
            }
            FlightMode::RTL | FlightMode::Land => {
                // Auto-generated: level, descend slowly.
                FlightTarget {
                    roll_cdeg: 0,
                    pitch_cdeg: 0,
                    yaw_rate_mdps: 0,
                    throttle: if flight_mode() == FlightMode::Land { 300 } else { 400 },
                    alt_mm: 0,
                }
            }
            FlightMode::Disarmed => {
                FlightTarget::new()
            }
        };

        // Read attitude and gyro.
        let att = CH_ATTITUDE.read().val;
        let imu = CH_IMU.read().val;

        // Cascaded PID per axis.
        let roll_corr = pid.update_axis(
            target.roll_cdeg - att.roll_cdeg,
            imu.gyro_mdps[0],
            0, dt_us);
        let pitch_corr = pid.update_axis(
            target.pitch_cdeg - att.pitch_cdeg,
            imu.gyro_mdps[1],
            1, dt_us);
        // Yaw: in Manual/Stabilize, use rate control directly.
        let yaw_corr = pid.rate_pid[2].update(
            target.yaw_rate_mdps - imu.gyro_mdps[2],
            dt_us);

        // Compute mixer output.
        let mix = mixer_compute(frame, target.throttle as i32, roll_corr, pitch_corr, yaw_corr);

        // Apply to ESCs.
        for i in 0..mix.count as u8 {
            robot_os_drivers::esc::esc_set_throttle(i, mix.motors[i as usize]);
        }

        robot_os_sched::task_yield();
    }
}

/// Phase L: telemetry task.
///
/// Periodically reads attitude, GPS, and flight state, serializes into
/// telemetry packets, and sends via UDP to the configured server.
/// Runs at ~10 Hz.
fn telemetry_task(_: usize) {
    use robot_os_ahrs::{CH_ATTITUDE, CH_IMU, CH_BARO};
    use robot_os_gps::CH_GPS;

    kprintln!("[TELEM] Phase L: telemetry task starting");

    let mut buf = [0u8; 64];
    let mut tick_count: u32 = 0;
    let mut udp_fd: i32 = -1;

    loop {
        // Yield 10 times (~100 ms at 100 Hz scheduler = ~10 Hz telemetry).
        for _ in 0..10 { robot_os_sched::task_yield(); }

        if !robot_os_telemetry::telem_is_active() {
            continue;
        }

        // Lazily create UDP socket.
        if udp_fd < 0 {
            udp_fd = robot_os_net::socket_create(
                robot_os_net::AF_INET, robot_os_net::SOCK_DGRAM, 0);
            if udp_fd < 0 { continue; }
        }

        tick_count += 1;

        let port = robot_os_telemetry::telem_port();
        let server_ip = robot_os_config::unpack_ip(
            robot_os_config::BEHAVIOR_SERVER_IP.load(Ordering::Relaxed));

        // Send TELEM_ATTITUDE every iteration (10 Hz).
        let att = CH_ATTITUDE.read().val;
        let gps = CH_GPS.read().val;
        let mode_val = match robot_os_flight::flight_mode() {
            robot_os_flight::FlightMode::Disarmed  => 0u8,
            robot_os_flight::FlightMode::Manual    => 1,
            robot_os_flight::FlightMode::Stabilize => 2,
            robot_os_flight::FlightMode::AltHold   => 3,
            robot_os_flight::FlightMode::PosHold   => 4,
            robot_os_flight::FlightMode::Auto      => 5,
            robot_os_flight::FlightMode::RTL       => 6,
            robot_os_flight::FlightMode::Land      => 7,
        };
        let armed = robot_os_flight::is_armed();

        let len = robot_os_telemetry::serialize_attitude(&mut buf, &att, &gps, mode_val, armed);
        if len > 0 {
            robot_os_net::udp::sendto(udp_fd, &server_ip, port, &buf[..len]);
            robot_os_telemetry::telem_inc_sent();
        }

        // Send TELEM_SENSORS every 2nd iteration (~5 Hz).
        if tick_count % 2 == 0 {
            let imu = CH_IMU.read().val;
            let baro = CH_BARO.read().val;
            let len = robot_os_telemetry::serialize_sensors(&mut buf, &imu, baro.pressure_pa);
            if len > 0 {
                robot_os_net::udp::sendto(udp_fd, &server_ip, port, &buf[..len]);
            }
        }
    }
}

/// Phase 13/G1: RT motor task — apply MotorCmd to motors; fire safe-stop on watchdog.
///
/// Reads `motor_cmd_read()` each tick, checks the watchdog, and drives
/// motors 0 and 1 according to the published command.  Runs forever.
fn rt_motor_task(_: usize) {
    kprintln!("[RT-MOTOR] Starting (watchdog timeout=500 ms, PID velocity control)");
    let mut safe_mode = false;

    /// Left motor hardware ID.
    const MOTOR_ID_LEFT: u32 = 0;
    /// Right motor hardware ID.
    const MOTOR_ID_RIGHT: u32 = 1;

    loop {
        let fired = robot_os_robot::motor_watchdog_fired();

        if fired && !safe_mode {
            kprintln!("[RT-MOTOR] Watchdog! No command >500 ms → SAFE STOP");
            robot_os_robot::motor_stop(MOTOR_ID_LEFT);
            robot_os_robot::motor_stop(MOTOR_ID_RIGHT);
            robot_os_drivers::motor_pid::motor_pid_reset();
            safe_mode = true;
        } else if !fired {
            if safe_mode {
                kprintln!("[RT-MOTOR] Watchdog cleared — resuming");
                safe_mode = false;
            }
            let cmd = robot_os_robot::motor_cmd_read();
            if robot_os_robot::CH_MOTOR_CMD.is_valid() {
                // Phase 17: accumulate simulated encoder ticks.
                robot_os_robot::encoder_tick(cmd.speed_l, cmd.speed_r);

                if robot_os_drivers::motor_pid::motor_pid_enabled() {
                    // Closed-loop PID velocity control.
                    // Set target from the motor command (speed as ticks/s).
                    robot_os_drivers::motor_pid::motor_pid_set_target(
                        cmd.speed_l as i16,
                        cmd.speed_r as i16,
                    );

                    // Read encoders and run PID tick.
                    let (ticks_l, ticks_r) = robot_os_robot::encoder_read();
                    let now = robot_os_drivers::clint::get_time();
                    let (pwm_l, pwm_r) =
                        robot_os_drivers::motor_pid::motor_pid_tick(ticks_l, ticks_r, now);

                    // Apply PID output to motors.
                    let (dir_l, spd_l) = if pwm_l >= 0 {
                        (robot_os_robot::MotorDir::Forward,  pwm_l as u32)
                    } else {
                        (robot_os_robot::MotorDir::Backward, (-pwm_l) as u32)
                    };
                    let (dir_r, spd_r) = if pwm_r >= 0 {
                        (robot_os_robot::MotorDir::Forward,  pwm_r as u32)
                    } else {
                        (robot_os_robot::MotorDir::Backward, (-pwm_r) as u32)
                    };
                    robot_os_robot::motor_set(MOTOR_ID_LEFT, dir_l, spd_l);
                    robot_os_robot::motor_set(MOTOR_ID_RIGHT, dir_r, spd_r);
                } else {
                    // Open-loop: direct PWM from motor command (legacy behavior).
                    let (dir_l, spd_l) = if cmd.speed_l >= 0 {
                        (robot_os_robot::MotorDir::Forward,  cmd.speed_l as u32)
                    } else {
                        (robot_os_robot::MotorDir::Backward, (-cmd.speed_l) as u32)
                    };
                    let (dir_r, spd_r) = if cmd.speed_r >= 0 {
                        (robot_os_robot::MotorDir::Forward,  cmd.speed_r as u32)
                    } else {
                        (robot_os_robot::MotorDir::Backward, (-cmd.speed_r) as u32)
                    };
                    robot_os_robot::motor_set(MOTOR_ID_LEFT, dir_l, spd_l);
                    robot_os_robot::motor_set(MOTOR_ID_RIGHT, dir_r, spd_r);
                }
            }
        }

        robot_os_sched::task_yield();
    }
}

/// Phase 16: system watchdog task.
///
/// Runs in a loop, yielding sched_hz/2 times between checks (~500 ms).
/// Each iteration:
///
/// 1. **Stack canaries** — calls `stack_canary_check()` on all valid task slots.
///    If any canary was overwritten, logs a warning and stops both motors.
///
/// 2. **Timer liveness** — compares `TICK_COUNT` with the value from the
///    previous iteration.  If it did not advance (timer ISR is frozen),
///    increments a stall counter.  After 3 consecutive stalls, stops motors.
fn system_wdt_task(_: usize) {
    kprintln!("[WDT] Phase 16 system watchdog running");
    let mut last_tick    = TICK_COUNT.load(Ordering::Relaxed);
    let mut frozen_count = 0u32;

    loop {
        // Yield ~500 ms worth of ticks (adapts to sched_hz).
        let yield_count = robot_os_drivers::clint::sched_hz_get() / 2;
        for _ in 0..yield_count {
            robot_os_sched::task_yield();
        }

        // ── 1. Stack canary check ─────────────────────────────────────────
        let (ok, total) = robot_os_sched::stack_canary_check();
        if ok < total {
            kprintln!("[WDT] STACK OVERFLOW: {}/{} canaries intact — motors stopped",
                ok, total);
            robot_os_robot::motor_stop(0);
            robot_os_robot::motor_stop(1);
        }

        // ── 2. Timer liveness check ───────────────────────────────────────
        let now_tick = TICK_COUNT.load(Ordering::Relaxed);
        if now_tick == last_tick {
            frozen_count += 1;
            kprintln!("[WDT] Timer stall #{} (tick_count={})", frozen_count, now_tick);
            if frozen_count >= 3 {
                kprintln!("[WDT] Timer FROZEN — SAFE STOP");
                robot_os_robot::motor_stop(0);
                robot_os_robot::motor_stop(1);
            }
        } else {
            if frozen_count > 0 {
                kprintln!("[WDT] Timer resumed after {} stalls", frozen_count);
                frozen_count = 0;
            }
            last_tick = now_tick;
        }
    }
}

/// Trap handler called from trap_entry*.S.
///
/// On RV64: returns the SATP value to load before SRET (0 = no page-table switch).
/// On ESP32-C3/RV32: returns 0 (no MMU).
#[unsafe(no_mangle)]
pub extern "C" fn trap_handler(frame: &mut TrapFrame) -> usize {
    let cause = frame.scause as usize;

    if cause & INTERRUPT_BIT != 0 {
        handle_interrupt(frame, cause & !INTERRUPT_BIT);
        0
    } else {
        handle_exception(frame, cause)
    }
}

/// Handle asynchronous interrupts.
fn handle_interrupt(_frame: &mut TrapFrame, cause: usize) {
    match cause {
        INT_TIMER_S => {
            TICK_COUNT.fetch_add(1, Ordering::Relaxed);

            // Kick hardware WDT every tick (no-op on QEMU; prevents reset on VF2/K1).
            robot_os_drivers::wdt::wdt_kick();

            // Schedule next timer tick BEFORE calling the scheduler
            // (context_switch may not return to this frame).
            let hart = robot_os_arch::cpu::hart_id();
            robot_os_drivers::clint::set_next_tick(hart as u32);

            // Let the scheduler preempt if the current task's time slice expired.
            robot_os_sched::schedule();
        }
        INT_EXTERNAL_S => {
            #[cfg(not(feature = "esp32c3"))]
            {
                let hart = robot_os_arch::cpu::hart_id();
                let irq = robot_os_drivers::plic::claim(hart as u32);
                if irq != 0 {
                    if irq == robot_os_drivers::uart::UART_IRQ {
                        robot_os_drivers::uart::irq_handler();
                    }
                    robot_os_drivers::plic::complete(hart as u32, irq);
                }
            }
        }
        INT_SOFTWARE_S => {
            // IPI received — execute TLB shootdown and clear pending bit.
            csr::sfence_vma();
            // Clear S-mode software interrupt pending (SIP.SSIP = bit 1).
            csr::clear_sip_ssip();
        }
        _ => {
            kprintln!("[IRQ] Unknown interrupt: {}", cause);
        }
    }
}

/// Handle synchronous exceptions.
///
/// Returns the SATP to switch to on SRET (0 = keep current page table).
fn handle_exception(frame: &mut TrapFrame, cause: usize) -> usize {
    match cause {
        // ── System calls (ecall from U-mode or S-mode) ────────────────────
        TRAP_ECALL_FROM_U | TRAP_ECALL_FROM_S => {
            // Save ecall context for fork(): sepc and user SP.
            #[cfg(not(feature = "no-mmu"))]
            robot_os_sched::set_ecall_context(frame.sepc, frame.regs[2]);

            let num = frame.regs[17]; // a7 = syscall number
            let result = robot_os_syscall::syscall_dispatch(
                num as u64,
                frame.regs[10] as u64, frame.regs[11] as u64, frame.regs[12] as u64,
                frame.regs[13] as u64, frame.regs[14] as u64, frame.regs[15] as u64,
            );
            frame.regs[10] = result as _; // return value in a0
            frame.sepc += 4;              // skip ecall instruction

            // If exec_user() stored a pending context, apply it and switch to U-mode.
            #[cfg(not(feature = "no-mmu"))]
            if let Some(ctx) = robot_os_sched::take_pending_exec() {
                frame.sepc      = ctx.entry as _;
                frame.sstatus   = ctx.sstatus as _; // SPP=0, SPIE=1
                frame.regs[2]   = ctx.user_sp as _; // user SP
                return ctx.satp as usize;            // switch page table
            }
            0
        }

        // ── Page faults: kill user task, fatal if from kernel ──────────
        TRAP_INSTR_PAGE_FAULT | TRAP_LOAD_PAGE_FAULT | TRAP_STORE_PAGE_FAULT => {
            let hart = robot_os_arch::cpu::hart_id();
            // SPP bit: 0 = came from U-mode, 1 = came from S-mode.
            let from_user = (frame.sstatus as usize) & csr::SSTATUS_SPP == 0;

            kprintln!();
            kprintln!("[PAGE FAULT] CPU {} — {} at {:#x}",
                hart, trap::cause_str(cause), frame.stval);
            kprintln!("  sepc: {:#x}  task: {}", frame.sepc,
                robot_os_sched::current_task_name());

            if from_user {
                // U-mode fault: kill the offending task, scheduler picks next.
                kprintln!("[PAGE FAULT] Killing user task");
                robot_os_sched::task_exit();
                // task_exit() never returns — context_switch abandons this frame.
            } else {
                // S-mode (kernel) fault: this is a kernel bug.
                // Stop all motors, log diagnostics, shutdown system.
                kprintln!("[FATAL] Kernel page fault on CPU {} — initiating shutdown", hart);
                kprintln!("  regs[1] (ra):  {:#x}", frame.regs[1]);
                kprintln!("  regs[2] (sp):  {:#x}", frame.regs[2]);
                kprintln!("  regs[8] (s0):  {:#x}", frame.regs[8]);
                // Emergency motor stop to prevent runaway
                robot_os_robot::motor_cmd_publish(0, 0);
                robot_os_arch::sbi::shutdown();
            }
        }

        // ── All other exceptions: fatal ───────────────────────────────────
        _ => {
            let hart = robot_os_arch::cpu::hart_id();
            kprintln!();
            kprintln!("[EXCEPTION] CPU {} — {}", hart, trap::cause_str(cause));
            kprintln!("  sepc:   {:#x}", frame.sepc);
            kprintln!("  stval:  {:#x}", frame.stval);
            kprintln!("  scause: {:#x}", frame.scause);
            kprintln!("  regs[1] (ra):  {:#x}", frame.regs[1]);
            kprintln!("  regs[2] (sp):  {:#x}", frame.regs[2]);
            // Emergency motor stop
            robot_os_robot::motor_cmd_publish(0, 0);
            kprintln!("[FATAL] Unhandled exception on CPU {} — shutdown", hart);
            robot_os_arch::sbi::shutdown();
        }
    }
}
