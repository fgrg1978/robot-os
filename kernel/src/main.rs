//! Robot OS - Hybrid RISC-V Kernel (Rust)
//!
//! Entry point for the kernel. Called from boot.S after hardware init.

#![no_std]
#![no_main]

extern crate alloc;

mod panic;

// DEV02 — USB DFU 1.1 recovery mode glue. The init function is
// called from a recovery-trigger path that is not yet wired (no
// USB-OTG controller driver on hand pre-Julio 2026), so the
// module is dead from main's POV.
#[allow(dead_code)]
mod dfu_recovery;

// Item 2 Stage 3 batch 7 — per-arch trap entry modules + TrapContext
// trait. Defines the cross-arch surface for trap handling; the
// existing riscv trap_handler in this file still uses the native
// TrapFrame directly (S3.b7 scaffolding only — refactor is .next).
#[allow(dead_code)]
mod entry;

// DEV03 — USB MSC gadget glue (FAT32-backed LUN over Bulk-Only Transport).
// Pre-hardware: USB device controller wiring is stubbed; pure dispatch
// logic is covered by crates/msc-tests.
#[allow(dead_code)]
mod msc_gadget;

use core::arch::global_asm;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use robot_os_config::ML_ENABLED;
use robot_os_drivers::kprintln;
use robot_os_arch::mmu::PAGE_SIZE;
use robot_os_arch::trap::{
    TrapFrame, INTERRUPT_BIT,
    INT_TIMER_S, INT_EXTERNAL_S, INT_SOFTWARE_S,
    TRAP_ECALL_FROM_U, TRAP_ECALL_FROM_S,
    TRAP_INSTR_PAGE_FAULT, TRAP_LOAD_PAGE_FAULT, TRAP_STORE_PAGE_FAULT,
};
use robot_os_arch::{csr, trap};

// Include boot assembly
// All these .S files are RISC-V; lives in entry/riscv64/asm per Item 2
// Stage 3 batch 7 (per-arch entry organisation).  aarch64 + x86_64 boot/
// trap asm sit in entry/{aarch64,x86_64}/asm — currently stub READMEs;
// will get the real asm in Stage 5 when the kernel boots on those ISAs.
// `max_harts` se inyecta en vez de duplicarse: ambos ficheros dimensionan
// tablas por hart (`secondary_stacks`, los vectores de trap de K-C16) y una
// `.equ` a mano en cada uno es justo la clase de constante que se desincroniza
// en silencio — aqui el sintoma seria un hart escribiendo fuera de su ranura.
global_asm!(
    include_str!("entry/riscv64/asm/boot.S"),
    max_harts = const MAX_HARTS,
);
// F04: M-mode PMP boot stub for no-OpenSBI direct-boot builds.
#[cfg(feature = "no-opensbi")]
global_asm!(include_str!("entry/riscv64/asm/boot_noopensbi.S"));

// Include trap entry assembly
global_asm!(
    include_str!("entry/riscv64/asm/trap_entry.S"),
    max_harts = const MAX_HARTS,
);

// Include context switch assembly.
// Phase 12: when rvv feature is active, use the RVV-aware variant.
// TASK_SATP_OFFSET is injected at compile time via offset_of! — if someone
// adds fields before task_satp, the offset updates automatically.
#[cfg(not(feature = "rvv"))]
global_asm!(
    include_str!("entry/riscv64/asm/context_switch.S"),
    task_satp_off = const core::mem::offset_of!(robot_os_sched::task::Task, task_satp),
    // K-C23: context_saving is cleared straight from the asm (fence + sb) —
    // a Rust helper's Release store publishes the old task's stack as
    // reusable while still running ON that stack, so the helper must not
    // have a frame; injecting the offset keeps the asm store correct even
    // if fields move. (Not passed to context_switch_rvv.S below: that file
    // never touches context_saving — see its header and the rvv gating in
    // do_schedule() — and global_asm! rejects unused operands.)
    context_saving_off = const core::mem::offset_of!(robot_os_sched::task::Task, context_saving),
);
#[cfg(feature = "rvv")]
global_asm!(
    include_str!("entry/riscv64/asm/context_switch_rvv.S"),
    task_satp_off = const core::mem::offset_of!(robot_os_sched::task::Task, task_satp),
);

/// Maximum number of harts supported (stack slots allocated).
const MAX_HARTS: usize = 8;

/// Stack size per secondary hart (16 KiB).
/// Enough for nested traps (288 B each) + scheduler + Rust calls.
const SECONDARY_STACK_SIZE: usize = 16 * 1024;

// Secondary CPU stacks — boot.S references `secondary_stacks` and
// loads the per-hart size from `_secondary_stack_size` (.quad in .data).
//
// _secondary_stack_size lives in .data because it carries a real value.
// secondary_stacks lives in .bss so the 128 KiB buffer doesn't bloat the
// kernel binary on disk; clear_bss in boot.S zeroes it before any hart
// ever touches it (secondaries are still parked in OpenSBI at that point).
global_asm!(
    ".section .data",
    ".align 3",
    ".global _secondary_stack_size",
    "_secondary_stack_size:",
    "    .quad {size}",
    size = const SECONDARY_STACK_SIZE,
);
global_asm!(
    ".section .bss",
    ".align 12",
    ".global secondary_stacks",
    "secondary_stacks:",
    "    .space {size} * {max_harts}",
    size = const SECONDARY_STACK_SIZE,
    max_harts = const MAX_HARTS,
);

// Linker script symbols — section boundaries for W^X enforcement
unsafe extern "C" {
    static _text_start: u8;
    static _text_end: u8;
    static _rodata_start: u8;
    static _rodata_end: u8;
    static _data_start: u8;
    static _kernel_end: u8;
    // Boot stack, pinned by linker.ld to the top of the kernel window:
    //   _stack_end   = ORIGIN(RAM) + LENGTH(RAM)
    //   _stack_start = _stack_end - BOOT_STACK_SIZE
    // It sits ABOVE _kernel_end, and pmm::init() only reserves up to
    // _kernel_end — so without an explicit reserve_range the allocator hands
    // out hart 0's own stack. See the reserve call in Phase 2.
    static _stack_start: u8;
    static _stack_end: u8;
}

/// Fallback RAM size when DTB doesn't provide memory info.
const FALLBACK_MEM_SIZE: usize = 128 * 1024 * 1024;

use robot_os_limits::KERNEL_HEAP_SIZE_BYTES as HEAP_SIZE;

/// Compile-time maximum number of CPUs supported. The runtime count is
/// derived from the DTB (capped at this value); see `num_cpus` local in
/// kernel_main. Must be ≤ MAX_HARTS so all online CPUs have a stack slot.
const MAX_CPUS: usize = 4;

/// Number of worker tasks for the SMP stress test.
const NUM_WORKERS: usize = 15;

/// Each worker runs this many iterations.
const WORKER_ITERS: u32 = 2000;

/// Total timer ticks received across all CPUs (for verification).
static TICK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// K-A1: liveness counter, incremented once per `rt_motor_task` iteration.
/// The timer ISR feeds the hardware WDT only while this advances, so a hung
/// control task (motors frozen at their last command) lets the WDT reset the
/// board instead of being masked by the still-running ISR.
static CONTROL_HEARTBEAT: AtomicU32 = AtomicU32::new(0);
/// Last heartbeat observed by the WDT feeder (hart-0-owned).
static WDT_LAST_HEARTBEAT: AtomicU32 = AtomicU32::new(0);
/// Consecutive ticks the control heartbeat has not advanced (hart-0-owned).
static WDT_STALL_TICKS: AtomicU32 = AtomicU32::new(0);
/// Ticks the control task may stall before the WDT stops being fed. At the
/// 100 Hz tick this is ~400 ms of grace; with the 500 ms HW timeout the board
/// resets within ~900 ms of a genuine control-task hang. Comfortably above the
/// RT-priority control task's real scheduling period, so no false resets.
const WDT_CONTROL_STALL_LIMIT: u32 = 40;

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

    // ---- Phase 1b: Install trap vector EARLY (before any code that could fault).
    // Until stvec is set, any exception jumps to address 0 → triple fault.
    // trap_init() only needs UART (for kprintln) and CSRs — no heap, no MMU.
    trap_init();

    kprintln!();
    kprintln!("========================================");
    kprintln!("  Robot OS Rust kernel booted!");
    kprintln!("========================================");
    kprintln!();
    kprintln!("[BOOT] Hart ID:  {}", hart_id);
    kprintln!("[BOOT] DTB addr: {:#x}", dtb_ptr);

    // The boot hart is whoever won `boot_lock` — boot.S range-checks only
    // SECONDARY harts, and against MAX_HARTS (stack + trap-vector slots),
    // not MAX_CPUS. Every scheduler structure is `[_; MAX_CPUS]` indexed by
    // a raw `PER_CPU[current_cpu_id()]`, so a boot hart id past MAX_CPUS is
    // not degraded service — it is an out-of-bounds write into .bss on the
    // first scheduler touch and a silent board reset. The VF2/JH7110 case
    // is real (S7 + four U74s enumerate 5 harts; see KERNEL_REVIEW_NOTES):
    // if firmware ever elects a boot hart >= MAX_CPUS, halting loudly here
    // with the id on the UART is the only honest outcome. The real fix for
    // such boards is a physical→logical hart map (post-hardware work);
    // until then the id doubles as the index and must be in range.
    if hart_id >= MAX_CPUS {
        kprintln!("[BOOT] FATAL: boot hart id {} >= MAX_CPUS {} — every \
                   PER_CPU access would index out of bounds. Halting. \
                   (Board needs a physical->logical hart map, or boot-hart \
                   selection in firmware.)", hart_id, MAX_CPUS);
        loop { robot_os_arch::cpu::wfi(); }
    }

    // Parse DTB (Flattened Device Tree) if pointer looks valid.
    // Extract mem_base/mem_size to feed PMM and VMM with real hardware RAM.
    // Extract num_cpus to size the SMP scheduler at runtime (capped at MAX_CPUS).
    // Validate timer_freq against the kernel's hardcoded value — a mismatch
    // means every µs/ms calculation in the kernel is off and must be flagged.
    let (mem_start, mem_size, mem_from_dtb, num_cpus) = if dtb_ptr != 0 {
        if let Some(info) = unsafe { robot_os_dtb::dtb_parse(dtb_ptr as *const u8) } {
            let compat = robot_os_dtb::dtb_compatible_str(&info);
            kprintln!("[DTB] Parsed FDT — {} CPUs, mem={:#x}+{:#x}, timer={}",
                info.num_cpus, info.mem_base, info.mem_size, info.timer_freq);
            kprintln!("[DTB] UART={:#x}, PLIC={:#x}", info.uart_base, info.plic_base);
            kprintln!("[DTB] Compatible: {}", core::str::from_utf8(compat).unwrap_or("?"));

            // Validate timer_freq vs hardcoded constant — if they disagree, every
            // time-based calculation (WCET, sleeps, timeouts) is wrong. Warn loudly.
            {
                let kernel_timer_hz = robot_os_drivers::platform::hw::TIMER_FREQ;
                if info.timer_freq != 0 && info.timer_freq != kernel_timer_hz {
                    kprintln!("[DTB] WARNING: timer_freq mismatch — DTB={}Hz kernel={}Hz, \
                        timing calculations will drift", info.timer_freq, kernel_timer_hz);
                }
            }

            // Cap DTB-reported CPUs by compile-time MAX_CPUS (stack slots reserved).
            let cpus = if info.num_cpus > 0 {
                core::cmp::min(MAX_CPUS, info.num_cpus)
            } else {
                MAX_CPUS
            };
            if info.mem_base != 0 && info.mem_size != 0 {
                (info.mem_base, info.mem_size, true, cpus)
            } else {
                (robot_os_drivers::platform::hw::RAM_BASE, FALLBACK_MEM_SIZE, false, cpus)
            }
        } else {
            kprintln!("[DTB] Parse failed (invalid or unsupported FDT)");
            (robot_os_drivers::platform::hw::RAM_BASE, FALLBACK_MEM_SIZE, false, MAX_CPUS)
        }
    } else {
        (robot_os_drivers::platform::hw::RAM_BASE, FALLBACK_MEM_SIZE, false, MAX_CPUS)
    };
    kprintln!("[BOOT] Online CPUs: {} (max compile-time: {})", num_cpus, MAX_CPUS);
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

    // Reserve the boot stack. `pmm::init` only marks [mem_start, _kernel_end)
    // as used, but linker.ld pins the boot stack to the TOP of the kernel
    // window — above _kernel_end — so those pages come back as free and the
    // allocator will eventually hand hart 0 its own stack to write over.
    // Nothing else in the tree referenced _stack_start/_stack_end before this.
    {
        let stack_start = unsafe { &_stack_start as *const u8 as usize };
        let stack_end   = unsafe { &_stack_end   as *const u8 as usize };
        robot_os_mm::pmm::reserve_range(stack_start, stack_end - stack_start);
        kprintln!("[MM] Boot stack reserved: {:#x} - {:#x} ({} KiB)",
            stack_start, stack_end, (stack_end - stack_start) >> 10);
    }

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
            #[cfg(not(any(feature = "vf2", feature = "k1")))]
            {
                // VirtIO MMIO 0x10001000 - 0x10008000 (8 devices)
                let _ = robot_os_mm::vmm::map_mmio_region(0x1000_1000, 0x8000);
                // CLINT 0x02000000 (64 KiB) — mtime/mtimecmp via SBI but read rdtime
                let _ = robot_os_mm::vmm::map_mmio_region(0x0200_0000, 0x1_0000);
                // fw_cfg (--features ramfb only — crates/display/src/ramfb.rs).
                // Found by actually booting with ramfb and getting a page
                // fault at 0x10100008 (the selector register) — this
                // mapping was simply forgotten when FW_CFG_BASE was added.
                #[cfg(feature = "ramfb")]
                let _ = robot_os_mm::vmm::map_mmio_region(hw::FW_CFG_BASE, 0x1000);
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
                // Display (--features hdmi only — crates/display). Added
                // preemptively after the SAME class of missing-mapping bug
                // was caught for the QEMU ramfb path above (real page
                // fault, not a guess) — this would fault identically on
                // real VF2 hardware without it.
                #[cfg(feature = "hdmi")]
                {
                    let _ = robot_os_mm::vmm::map_mmio_region(hw::DC8200_TOP_BASE, 0x1000);
                    let _ = robot_os_mm::vmm::map_mmio_region(hw::DC8200_MAIN_BASE, 0x2000);
                    let _ = robot_os_mm::vmm::map_mmio_region(hw::HDMI_TX_BASE, 0x1000);
                }
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
                // F14: NPU MMIO (1 MiB, covers all command/data registers).
                let _ = robot_os_mm::vmm::map_mmio_region(hw::NPU_BASE, hw::NPU_SIZE);
            }

            kprintln!("[MM] Platform MMIO mapped ({})", hw::PLATFORM_NAME);
        }

        robot_os_mm::vmm::enable_paging();
        kprintln!("[MM] Sv39 paging ENABLED");

        // W^X enforcement: remap kernel sections with correct permissions.
        // Must split megapages that cover the kernel image into 4K pages
        // first, because different sections need different permissions.
        unsafe {
            let text_start = &_text_start as *const u8 as usize;
            let text_end   = &_text_end as *const u8 as usize;
            let ro_start   = &_rodata_start as *const u8 as usize;
            let ro_end     = &_rodata_end as *const u8 as usize;
            let data_start = &_data_start as *const u8 as usize;

            // Split megapages covering the kernel into 4K pages.
            robot_os_mm::vmm::split_mega_range(text_start, kernel_end_aligned);

            // Now remap with per-section permissions.
            robot_os_mm::vmm::enforce_wx(
                text_start, text_end,
                ro_start, ro_end,
                data_start, kernel_end_aligned,
            );
        }
        kprintln!("[MM] W^X enforced: .text=RX .rodata=RO .data=RW");

        // Null pointer guard: unmap page 0 so null derefs fault immediately.
        robot_os_mm::vmm::null_guard();
        kprintln!("[MM] Null pointer guard active (page 0 unmapped)");

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

    // F04: PMP policy audit log.
    // The kernel runs in S-mode and cannot write PMP CSRs directly (M-mode only).
    // For no-opensbi builds, pmp_early_init() ran in M-mode before _start.
    // For OpenSBI builds, OpenSBI configured a permissive PMP; W^X is enforced by VMM.
    // Either way, log the intended stricter policy for operator audit.
    {
        use robot_os_arch::pmp::{pmp_regions, N_PMP_REGIONS};
        use robot_os_drivers::platform::hw::KERNEL_LOAD;
        let pmp = pmp_regions(KERNEL_LOAD, kernel_end_aligned, heap_start, HEAP_SIZE);
        kprintln!("[PMP] Intended policy ({} regions + deny catch-all):", N_PMP_REGIONS);
        for (i, r) in pmp.iter().enumerate() {
            kprintln!("[PMP]  {}: {:20}  {:010x}-{:010x}  {}{}{}",
                i, r.name,
                r.base, r.base + r.size,
                if r.perm.r { 'R' } else { '-' },
                if r.perm.w { 'W' } else { '-' },
                if r.perm.x { 'X' } else { '-' },
            );
        }
        #[cfg(feature = "no-opensbi")]
        kprintln!("[PMP] pmp_early_init() applied at M-mode boot (no-opensbi build)");
        #[cfg(not(feature = "no-opensbi"))]
        kprintln!("[PMP] Running under OpenSBI — W^X enforced by VMM page tables");
        kprintln!();
    }

    // M01: vDSO — allocate the shared timing page that user-space reads directly.
    #[cfg(not(feature = "no-mmu"))]
    {
        robot_os_mm::vdso::vdso_init();
        kprintln!("[VDSO] Shared timing page ready at user VA {:#x}", robot_os_mm::vdso::VDSO_USER_BASE);
    }

    // AQ8: Enable kernel tracing (ring buffer of last 512 events).
    robot_os_ipc::trace_start();
    kprintln!("[TRACE] Kernel tracing enabled ({} event buffer)", robot_os_ipc::TRACE_BUF_SIZE);
    kprintln!();

    // ---- Phase 3: Interrupt controllers + enable interrupts ----
    // (trap_init was already done in Phase 1b before any potentially faulting code.)

    {
        kprintln!("[IRQ] Initializing PLIC...");
        robot_os_drivers::plic::init(hart_id as u32);
    }
    // Enable EXTERNAL + SOFTWARE interrupts now (PLIC, IPI). Timer (STIE) is
    // deferred until just before scheduler::start() — otherwise the timer ISR
    // preempts kernel_main with already-created RT tasks and the boot CPU
    // never reaches wake_harts(), starving every secondary CPU forever.
    let sie = csr::read_sie();
    csr::write_sie(sie | csr::SIE_SEIE | csr::SIE_SSIE);
    let sstatus = csr::read_sstatus();
    csr::write_sstatus(sstatus | csr::SSTATUS_SIE);
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

    // F20: TmpFS — bounded in-RAM temporary filesystem.
    kprintln!("[FS] tmpfs ready — max {} files, {} KiB cap",
        robot_os_fs::TMPFS_MAX_FILES,
        robot_os_fs::TMPFS_MAX_BYTES / 1024);

    // F21: Procfs + sysfs — register built-in virtual-file providers.
    robot_os_fs::procfs_init();
    // A1.next — expose scheduler runtime registry at /sys/scheduler.
    // Read-only introspection of which dispatch backend is active.
    robot_os_fs::procfs_register(
        robot_os_fs::ProcNs::Sys,
        b"scheduler",
        gen_sys_scheduler,
    );
    // A4.next — list registered drivers at /sys/drivers.
    robot_os_fs::procfs_register(
        robot_os_fs::ProcNs::Sys,
        b"drivers",
        gen_sys_drivers,
    );
    kprintln!("[FS] procfs/sysfs ready ({} entries)", robot_os_fs::procfs_count());

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

    // ---- Optional: HDMI framebuffer (VF2 only, --features hdmi) ─────────────
    // Never validated against real hardware — QEMU has no model for this
    // peripheral. See crates/display and docs/KERNEL_REVIEW_NOTES.md,
    // "Framebuffer/HDMI (VF2)". PHY calibration is unconfirmed placeholder
    // data (crates/display/src/hdmi.rs) — expect no signal on a real
    // monitor even with this call wired correctly.
    #[cfg(feature = "hdmi")]
    robot_os_display::display_init();

    // ---- Optional: QEMU ramfb test (--features ramfb) ────────────────────────
    // Unrelated to the real VF2 display driver above — see
    // crates/display/src/ramfb.rs. Needs -device ramfb -display <backend>
    // on the QEMU command line, not this whole session's usual -nographic.
    #[cfg(feature = "ramfb")]
    robot_os_display::qemu_display_init();

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

        // ── OTA boot validation (A/B slot + boot loop detection) ──────
        let boot_meta = robot_os_ota::ota_boot_validate();

        // ── Verify CRC-32 of active firmware slot ─────────────────────
        let active_slot = boot_meta.active_slot;
        let slot_size = boot_meta.slot_size(active_slot);
        if slot_size > 0 {
            if robot_os_ota::ota_verify_slot(active_slot) {
                kprintln!("[OTA] Slot {} CRC OK (fw={}, size={})",
                    if active_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' },
                    boot_meta.slot_version(active_slot), slot_size);
            } else {
                kprintln!("[OTA] ERROR: Slot {} CRC MISMATCH",
                    if active_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' });

                // ── Retrospective recovery: steer the NEXT boot away ────
                // from a corrupted active slot. This check runs from
                // inside `kernel_main` with the bootloader-loaded image
                // already executing in RAM — a bad CRC here cannot mean
                // "refuse to load", only "don't pick this slot again".
                // No reset is triggered: this may be a robot mid-motion
                // or a drone mid-flight, and the code that's running
                // right now works fine regardless of what's on disk.
                let last_good = boot_meta.last_good;
                if last_good != active_slot && robot_os_ota::ota_verify_slot(last_good) {
                    // A different, CRC-verified last-known-good slot
                    // exists — point the next boot at it and persist.
                    let mut new_meta = boot_meta;
                    new_meta.active_slot = last_good;
                    robot_os_ota::ota_write_boot_meta(&new_meta);
                    robot_os_ota::ota_apply_meta(&new_meta);
                    kprintln!("[OTA] ERROR: switching NEXT boot to slot {} \
                               (last_good, CRC verified) — slot {} keeps \
                               running for the rest of this boot but will \
                               not be selected again",
                        if last_good == robot_os_ota::SLOT_A { 'A' } else { 'B' },
                        if active_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' });
                } else if robot_os_ota::ota_verify_slot(robot_os_ota::SLOT_R) {
                    // Neither `active_slot` nor `last_good` verified.
                    // Last candidate: the immutable recovery slot R.
                    //
                    // BOOTMETA's `active_slot`/`last_good` fields can now
                    // encode "r" (see `serialize_boot_meta`/
                    // `parse_boot_meta` in `crates/ota/src/pure.rs`), and
                    // `BootMeta` carries `image_size_r`/`image_crc_r` so R
                    // can be CRC-verified exactly like A/B. In practice
                    // this branch is only reachable once something
                    // populates those `_r` fields — R is factory-flashed
                    // and nothing in this codebase writes them today — but
                    // when it is populated and verifies, steer the NEXT
                    // boot at R and persist. As with the `last_good`
                    // branch above: the image already running in RAM
                    // keeps running for the rest of *this* boot; only the
                    // next boot's selection changes. No reset is
                    // triggered here either.
                    let mut new_meta = boot_meta;
                    new_meta.active_slot = robot_os_ota::SLOT_R;
                    robot_os_ota::ota_write_boot_meta(&new_meta);
                    robot_os_ota::ota_apply_meta(&new_meta);
                    kprintln!("[OTA] ERROR: switching NEXT boot to slot R \
                               (recovery, CRC verified) — slot {} keeps \
                               running for the rest of this boot but will \
                               not be selected again",
                        if active_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' });
                } else {
                    // No BOOTMETA-selectable replacement exists: neither
                    // `last_good` nor R (whose `image_size_r` is 0 on
                    // every BOOTMETA in the field today, since nothing
                    // populates it — see `SLOT_R` doc in
                    // `crates/ota/src/pure.rs`) verified.
                    //
                    // `last_good == active_slot` means the last-known-good
                    // pointer IS the corrupt slot — nothing to fall back
                    // to. Otherwise `last_good`'s own CRC also failed.
                    // Shout loudly and keep booting: the image in RAM is
                    // already running.
                    let last_good_status = if last_good == active_slot {
                        "== active slot, also corrupt"
                    } else {
                        "CRC also failed"
                    };
                    kprintln!("[OTA] ERROR: no verified replacement slot \
                               available (last_good={} {}, R unverified or \
                               empty) — continuing boot on unverified slot \
                               {}; fix via OTA update or manual reflash",
                        if last_good == robot_os_ota::SLOT_A { 'A' } else { 'B' },
                        last_good_status,
                        if active_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' });
                }
            }
        } else {
            kprintln!("[OTA] Slot {} — no firmware recorded",
                if active_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' });
        }

        // ── Secure boot: Ed25519 signature verification (F18) ──────────
        //
        // Policy is fixed at COMPILE TIME by the `secure-boot-enforced`
        // cargo feature (Kconfig `SECURE_BOOT_ENFORCED`), never by a
        // runtime flag. `secure_boot_require_signature()` /
        // `secure_boot_set_require_signature()` exist in `secure_boot.rs`
        // for soft/advisory callers, but this boot gate deliberately does
        // NOT consult them: with the feature on, there must be no runtime
        // variable, debug build, or code path that can relax enforcement
        // — debug and release behave identically. Verification itself
        // always runs (even with the feature off) so the trust state is
        // always visible on the console; only the halt-on-failure part is
        // `#[cfg]`-gated.
        //
        // Single-hart, single-caller context: secondary harts stay parked
        // in OpenSBI HSM until `smp_start_secondary_harts()` far below
        // ("[SMP] Starting {} secondary harts..."), which runs only after
        // this whole block and after task_create — so
        // `secure_boot_verify_slot_detailed()`'s internal `static mut
        // IMG_BUF` (2 MiB, lives in `.bss`) has exactly one possible
        // caller at this point in the boot sequence. No lock needed here.
        let slot_char = if active_slot == robot_os_ota::SLOT_A { 'A' } else { 'B' };
        let (boot_trust, boot_trust_reason) =
            robot_os_ota::secure_boot_verify_slot_detailed(active_slot);
        kprintln!("[SECURE-BOOT] Slot {} signature: {} ({})",
            slot_char, boot_trust.as_str(), boot_trust_reason.as_str());

        #[cfg(feature = "secure-boot-enforced")]
        {
            if boot_trust != robot_os_ota::BootTrust::Verified {
                kprintln!("[SECURE-BOOT] FATAL: slot {} rejected — {} — \
                           secure-boot-enforced is compiled in, refusing to boot",
                    slot_char, boot_trust_reason.as_str());
                loop { robot_os_arch::cpu::wfi(); }
            }
        }
        #[cfg(not(feature = "secure-boot-enforced"))]
        {
            if boot_trust != robot_os_ota::BootTrust::Verified {
                kprintln!("[SECURE-BOOT] WARNING: slot {} not verified — {} \
                           (secure-boot-enforced not compiled in — booting anyway)",
                    slot_char, boot_trust_reason.as_str());
            }
        }

        // ── Authenticated brain channel key (`/fat/LINK.KEY`) ──────────
        // 32-byte pre-shared key for the brain↔kernel HMAC envelope
        // (`robot_os_behavior::auth_envelope`). If present, all
        // brain-protocol TCP frames get wrapped/unwrapped; if absent, the
        // wrap/unwrap functions fall back to identity (legacy plaintext).
        // The brain's matching key lives in env `ROBOT_BRAIN_LINK_KEY`.
        //
        // The key file lives on the FAT volume, which `msc_gadget.rs` also
        // exposes over USB mass storage — so "the file is missing" is a
        // state an attacker can *cause*, not just an accident. Whether that
        // silently downgrades the link to plaintext is therefore a policy
        // decision, and like secure boot it is fixed at COMPILE time by the
        // `link-auth-enforced` feature: with it compiled in there must be no
        // runtime flag, debug build, or code path that relaxes the
        // requirement. The load below always runs so the trust state is
        // visible on the console; only the "refuse to boot" half is gated.
        let mut link_authenticated = false;
        // No initialiser: every branch below assigns it, and a placeholder
        // value here would be dead (rustc says so, and warnings fail CI).
        let link_auth_reason: &str;
        {
            const LINK_KEY_BYTES: usize = robot_os_behavior::auth_envelope::KEY_BYTES;
            let mut key_buf = [0u8; LINK_KEY_BYTES];
            let mut fd_table = robot_os_fs::FdTable::new();
            let fd = robot_os_fs::vfs_open(&mut fd_table, b"/fat/LINK.KEY",
                                            robot_os_fs::O_RDONLY);
            if fd >= 0 {
                let n = robot_os_fs::vfs_read(&mut fd_table, fd,
                                               key_buf.as_mut_ptr(),
                                               key_buf.len());
                robot_os_fs::vfs_close(&mut fd_table, fd);
                if n == LINK_KEY_BYTES as i32 {
                    // SAFETY: init is `unsafe` because it writes the
                    // crate-local key/state via static-mut writes; we call it
                    // exactly once during boot, single-threaded, before any
                    // task that uses wrap/unwrap is spawned.
                    if unsafe { robot_os_behavior::auth_envelope::init(&key_buf) } {
                        kprintln!("[SECCHAN] /fat/LINK.KEY loaded ({} bytes) — \
                                   brain link authenticated", n);
                        link_authenticated = true;
                        link_auth_reason = "key loaded";
                    } else {
                        kprintln!("[SECCHAN] /fat/LINK.KEY rejected by auth_envelope::init");
                        link_auth_reason = "key rejected by auth_envelope::init";
                    }
                } else {
                    kprintln!("[SECCHAN] /fat/LINK.KEY wrong size ({} bytes, want {}) — \
                               running plaintext", n as i64, LINK_KEY_BYTES as i64);
                    link_auth_reason = "key file wrong size";
                }
            } else {
                kprintln!("[SECCHAN] /fat/LINK.KEY absent — brain link runs plaintext");
                link_auth_reason = "key file absent";
            }
        }

        // Policy gate — deliberately NOT consulting any runtime flag, for the
        // same reason the secure-boot gate does not: a build that claims to
        // enforce must have no way to be talked out of it.
        //
        // K-C5: `link-encrypt-enforced` shares this gate. Under that policy
        // every brain-link frame must travel inside an AEAD session keyed
        // from /fat/LINK.KEY, so a keyless boot leaves the robot with a link
        // that is down BY POLICY forever — refusing to boot surfaces the
        // provisioning error here, on the console, instead of in the field
        // as "robot won't talk".
        #[cfg(any(feature = "link-auth-enforced", feature = "link-encrypt-enforced"))]
        {
            if !link_authenticated {
                kprintln!("[SECCHAN] FATAL: brain link unauthenticated — {} — \
                           {} is compiled in, refusing to boot",
                    link_auth_reason,
                    if cfg!(feature = "link-encrypt-enforced") {
                        "link-encrypt-enforced"
                    } else {
                        "link-auth-enforced"
                    });
                loop { robot_os_arch::cpu::wfi(); }
            }
        }
        #[cfg(not(any(feature = "link-auth-enforced", feature = "link-encrypt-enforced")))]
        {
            if !link_authenticated {
                kprintln!("[SECCHAN] WARNING: brain link unauthenticated — {} \
                           (link-auth-enforced not compiled in — booting anyway)",
                    link_auth_reason);
            }
        }

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
    // DEV01.5 — two-node network smoke (opt-in via `--features net-smoke`).
    //
    // Identity comes from the MAC that QEMU assigns per instance, so a single
    // kernel image serves both roles and no per-node disk image is needed:
    // last MAC octet 1 => server 10.0.0.1, 2 => client 10.0.0.2.
    //
    // Must run HERE, between the NIC probe (MAC is readable) and net_init()
    // (which caches the address into the TCP layer via `tcp::init`). Setting
    // the IP after net_init() would update NET_CFG but leave TCP answering on
    // the old address.
    #[cfg(feature = "net-smoke")]
    {
        let mac  = robot_os_drivers::virtio::net::get_mac();
        let node = if mac[5] == 1 { 1u8 } else { 2u8 };
        let ip   = [10, 0, 0, node];
        robot_os_net::net_set_ip(ip, [255, 255, 255, 0], [10, 0, 0, 1]);
        kprintln!("[NETSMOKE] node={} ip=10.0.0.{} mac_octet={}", node, node, mac[5]);
    }

    robot_os_net::net_init();
    kprintln!();

    // Runs HERE, before the scheduler starts preempting (see the "[SCHED]
    // Starting scheduler" line further down). It used to sit next to the TFTP
    // smoke, which is *after* that point: `kernel_main` then competes with ~30
    // tasks and, on a 1-hart QEMU, is starved to brief bursts — 2000 polls need
    // 1.9ms of CPU but took over 150 wall-seconds to accumulate. The TFTP smoke
    // survives there only because a single fetch fits in one burst; this test
    // polls for seconds and never finished. main.rs already warns about this
    // hazard ("late init code may never run to completion").
    // DEV01.5 — two-node TCP smoke over a QEMU `socket` link.
    //
    // Real coverage, not a liveness ping: the client sends a deterministic
    // 256-byte pattern, the server echoes it, and the client compares every
    // byte. A single wrong or missing byte is a FAIL. This exercises ARP,
    // the IPv4 header checksum, the TCP handshake and — the reason it
    // exists — RX checksum validation in BOTH directions, which the TFTP
    // smoke (UDP only) never touches.
    //
    // Emits exactly one verdict line, `[NETSMOKE] PASS` or
    // `[NETSMOKE] FAIL <reason>`, so the harness can assert on it.
    #[cfg(feature = "net-smoke")]
    {
        const PORT:     u16   = 9100;
        const LEN:      usize = 256;
        const SERVER:   [u8; 4] = [10, 0, 0, 1];
        /// Patience budget, in POLLS — not in seconds. MUST be recomputed
        /// whenever this block moves, because the poll rate here varies by two
        /// orders of magnitude: after `sched::start()` the polling context is
        /// starved to ~20k polls/s, while here — before the scheduler preempts
        /// — it runs at ~2.4M/s. The previous 600_000 was sized for the starved
        /// regime (~30 s) and silently became ~0.25 s when the block moved, so
        /// the server gave up before a loaded machine could boot its peer.
        /// ~100M ≈ 40 s at the current placement.
        const POLLS:    u32   = 100_000_000;

        /// Deterministic, position-dependent and non-repeating within a
        /// byte: a length error, a duplicated segment or a reordered one
        /// all change the bytes, unlike a constant fill.
        fn pat(i: usize) -> u8 { (i as u8).wrapping_mul(31).wrapping_add(7) }

        let mac  = robot_os_drivers::virtio::net::get_mac();
        let is_server = mac[5] == 1;

        if is_server {
            let fd = robot_os_net::socket_create(
                robot_os_net::AF_INET, robot_os_net::SOCK_STREAM, 0);
            let mut a = robot_os_net::SockAddr::new();
            a.family = robot_os_net::AF_INET as u16;
            a.port   = PORT;
            if fd < 0
                || robot_os_net::socket_bind(fd, &a) < 0
                || robot_os_net::socket_listen_bound(fd) < 0
            {
                kprintln!("[NETSMOKE] FAIL server-bind");
            } else {
                kprintln!("[NETSMOKE] server listening on {}", PORT);
                let mut cfd = -1;
                for _ in 0..POLLS {
                    robot_os_net::net_poll();
                    let r = robot_os_net::socket_accept(fd);
                    if r >= 0 { cfd = r; break; }
                }
                if cfd < 0 {
                    kprintln!("[NETSMOKE] FAIL no-client");
                } else {
                    // Echo until we have bounced LEN bytes back.
                    let mut buf  = [0u8; LEN];
                    let mut seen = 0usize;
                    for _ in 0..POLLS {
                        robot_os_net::net_poll();
                        let n = robot_os_net::socket_recv(cfd, &mut buf[..LEN - seen]);
                        if n > 0 {
                            let n = n as usize;
                            if robot_os_net::socket_send(cfd, &buf[..n]) < 0 {
                                kprintln!("[NETSMOKE] FAIL server-send");
                                break;
                            }
                            seen += n;
                            if seen >= LEN { break; }
                        }
                    }
                    kprintln!("[NETSMOKE] server echoed {} bytes", seen);
                    robot_os_net::socket_close(cfd);
                }
                robot_os_net::socket_close(fd);
            }
        } else {
            let fd = robot_os_net::socket_create(
                robot_os_net::AF_INET, robot_os_net::SOCK_STREAM, 0);
            let mut a = robot_os_net::SockAddr::new();
            a.family = robot_os_net::AF_INET as u16;
            a.port   = PORT;
            a.addr   = SERVER;
            // Resolve ARP first. `tcp::connect` does no address resolution:
            // on a cache miss the SYN is simply dropped and never retried,
            // so connecting cold would hang in SynSent forever. Pinging
            // until it succeeds both primes the cache and gives the peer
            // time to finish booting — and exercises ARP + ICMP on the way.
            let mut arp_ok = false;
            // Same rate caveat as POLLS: 3000 x 20k is ~25 s at this
            // placement, where the old 300 x 1k was ~0.12 s.
            for _ in 0..3_000 {
                if robot_os_net::net_ping(SERVER) == 0 { arp_ok = true; break; }
                for _ in 0..20_000 { robot_os_net::net_poll(); }
            }
            if arp_ok { kprintln!("[NETSMOKE] arp resolved, connecting"); }
            if !arp_ok {
                kprintln!("[NETSMOKE] FAIL arp");
            } else if robot_os_net::socket_connect(fd, &a, 40000) < 0 {
                // Only reports failure to *start* connecting (no free slot);
                // the handshake itself completes asynchronously below.
                kprintln!("[NETSMOKE] FAIL connect-start");
            } else {
                let mut tx = [0u8; LEN];
                for i in 0..LEN { tx[i] = pat(i); }
                // `socket_send` refuses until the connection is established,
                // so retrying it while polling is how we wait out the
                // three-way handshake without a state-query API.
                let mut sent = false;
                // Also a poll count, also rate-dependent — see POLLS.
                for _ in 0..20_000_000 {
                    robot_os_net::net_poll();
                    if robot_os_net::socket_send(fd, &tx) >= 0 { sent = true; break; }
                }
                if sent { kprintln!("[NETSMOKE] sent {} bytes, awaiting echo", LEN); }
                if !sent {
                    kprintln!("[NETSMOKE] FAIL client-send");
                } else {
                    let mut rx  = [0u8; LEN];
                    let mut got = 0usize;
                    for _ in 0..POLLS {
                        robot_os_net::net_poll();
                        let n = robot_os_net::socket_recv(fd, &mut rx[got..]);
                        if n > 0 { got += n as usize; }
                        if got >= LEN { break; }
                    }
                    if got != LEN {
                        kprintln!("[NETSMOKE] FAIL short-echo got={} want={}", got, LEN);
                    } else {
                        let mut bad = usize::MAX;
                        for i in 0..LEN {
                            if rx[i] != pat(i) { bad = i; break; }
                        }
                        if bad == usize::MAX {
                            kprintln!("[NETSMOKE] PASS {} bytes round-tripped", LEN);
                        } else {
                            kprintln!("[NETSMOKE] FAIL mismatch at {} got={} want={}",
                                      bad, rx[bad], pat(bad));
                        }
                    }
                }
            }
            robot_os_net::socket_close(fd);
        }
    }

    // DEV01.6 — boot-time DHCP smoke (opt-in via `--features dhcp-smoke`).
    //
    // QEMU's user-mode backend runs a DHCP server on the gateway, so
    // `-netdev user,...` is all the infrastructure this needs. Asserts we
    // actually reach the Bound state and end up with a non-zero address in the
    // 10.0.2.x pool SLIRP hands out — not merely that dhcp_start() returned.
    //
    // Runs here, before the scheduler preempts, for the same reason as the
    // other smokes: `dhcp_start` polls, and a starved polling context turns a
    // fixed poll budget into a fraction of the time it was sized for.
    #[cfg(feature = "dhcp-smoke")]
    {
        // `dhcp_start` takes a `fn()` it calls once per receive attempt, and
        // only polls once itself per iteration. Pre-scheduler there is nothing
        // to yield TO, so the hook does the waiting instead: 200 attempts x 20k
        // polls is ~1.7s per phase at this placement, which is ample for a
        // server on the same host and still bounded.
        fn dhcp_smoke_wait() {
            for _ in 0..20_000 { robot_os_net::net_poll(); }
        }

        // Start from a deliberately wrong address so a PASS cannot be the
        // CONFIG.INI value surviving untouched.
        robot_os_net::net_set_ip([0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]);

        if !robot_os_net::dhcp::dhcp_start(dhcp_smoke_wait) {
            kprintln!("[DHCPSMOKE] FAIL no-lease");
        } else {
            let ip = robot_os_net::net_get_ip();
            let gw = robot_os_net::net_get_gateway();
            if ip == [0, 0, 0, 0] {
                kprintln!("[DHCPSMOKE] FAIL bound-but-no-address");
            } else if ip[0] != 10 || ip[1] != 0 || ip[2] != 2 {
                // Not fatal in principle, but on QEMU user-mode it means we
                // parsed something other than the lease we were offered.
                kprintln!("[DHCPSMOKE] FAIL unexpected-subnet {}.{}.{}.{}",
                          ip[0], ip[1], ip[2], ip[3]);
            } else {
                kprintln!("[DHCPSMOKE] PASS ip={}.{}.{}.{} gw={}.{}.{}.{}",
                          ip[0], ip[1], ip[2], ip[3], gw[0], gw[1], gw[2], gw[3]);
            }
        }
    }

    // DEV01.4 placement fix: `tftp_client`'s own module doc says "Intended for
    // boot-time use (before the scheduler starts): Blocking poll loop. Not
    // safe to call after `sched::start()`" — yet this smoke used to run after
    // it, racing kernel_main against the i3-probe spinners for CPU0. When it
    // lost, the 5M-poll budget ran at starved speed and the fetch (and its
    // verdict line) simply never happened. Whether it passed depended on boot
    // timing, not on the network stack.
    // DEV01.4 — boot-time TFTP fetch smoke (opt-in via
    // `cargo build --features tftp-smoke`). Pulls `TFTP.BIN`
    // from the default gateway (QEMU user-mode net hosts a
    // built-in TFTP at 10.0.2.2 when started with
    // `-netdev user,tftp=DIR,...`). Result is purely
    // diagnostic; boot continues either way.
    #[cfg(feature = "tftp-smoke")]
    {
        const TFTP_SMOKE_BUF_BYTES: usize = 1024;
        const TFTP_SMOKE_SERVER_IP: [u8; 4] = [10, 0, 2, 2];
        const TFTP_SMOKE_FILENAME: &str = "TFTP.BIN";
        static mut TFTP_SMOKE_BUF: [u8; TFTP_SMOKE_BUF_BYTES] =
            [0u8; TFTP_SMOKE_BUF_BYTES];
        let buf = unsafe { &mut *(&raw mut TFTP_SMOKE_BUF) };
        match robot_os_net::tftp_client::tftp_fetch(
            TFTP_SMOKE_SERVER_IP,
            TFTP_SMOKE_FILENAME,
            buf,
        ) {
            Ok(n) => kprintln!(
                "[TFTP] fetched {} bytes from {}.{}.{}.{} OK",
                n,
                TFTP_SMOKE_SERVER_IP[0], TFTP_SMOKE_SERVER_IP[1],
                TFTP_SMOKE_SERVER_IP[2], TFTP_SMOKE_SERVER_IP[3],
            ),
            Err(e) => kprintln!("[TFTP] fetch failed: {:?}", e),
        }
    }


    // ── OTA auto-recv (early spawn) ──────────────────────────────────────────
    // Spawn the OTA TCP listener task BEFORE any RT-priority tasks. Once
    // rt-motor / flight-ctrl / sensor-ahrs are created and the timer ISR
    // starts preempting kernel_main on the boot CPU, late init code may
    // never run to completion. Spawning early guarantees the listener is
    // registered while the boot CPU is still single-tasking.
    {
        let port = robot_os_config::CFG_OTA_AUTO_RECV_PORT.load(
            core::sync::atomic::Ordering::Relaxed);
        if port != 0 && port <= 65535 {
            // Pinned to CPU 2 — CPUs 0/1 host RT tasks, CPU 2 is quiet.
            robot_os_sched::task_create_affinity(
                "ota-recv",
                robot_os_shell::ota_recv_task_entry,
                port as usize,
                robot_os_sched::NET_POLL_PRIORITY,
                2,
            );
            kprintln!("[OTA] Auto-recv task created on port {} (early)", port);
        }
    }

    // ---- Phase 8: IPC + Signals + Services ----

    robot_os_ipc::pipe_init();
    robot_os_ipc::signal_init();
    robot_os_service::service_init();
    kprintln!("[IPC] Pipes, signals, service manager initialized");
    kprintln!();

    // ── Early-boot synthetic bench capture (CFG_BENCH_BOOT) ───────────────
    // Quiescent context: hart 0 is the only running hart (secondaries wake at
    // scheduler::start), the timer ISR is still OFF (deferred until just
    // before scheduler::start), and all benched subsystems (ipc, fs/tmpfs,
    // net/arp, crypto, auth) are initialised by this point. That removes the
    // cross-hart rdcycle contention + timer preemption that make the SMP
    // behavior-task path noisy. Run once, emit, then halt — no need to reach
    // the (slow / -smp-1-hanging) full task system. Pair with QEMU -icount
    // for cross-run determinism. See crates/config CFG_BENCH_BOOT.
    #[cfg(feature = "qemu")]
    if robot_os_config::CFG_BENCH_BOOT.load(Ordering::Relaxed) {
        const BENCH_BOOT_ITERS: u64 = 100;
        robot_os_bench::run_all_quiescent(BENCH_BOOT_ITERS);
        kprintln!("[BENCH-RES] ── boot-bench complete, halting ──");
        loop {
            unsafe { core::arch::asm!("wfi"); }
        }
    }

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

    // F11.3: Check crash counter — detect boot loops.
    // The counter was incremented on panic (if any). Reset it now that we
    // have reached late-init successfully (clean boot).
    {
        let prev_crashes = robot_os_drivers::wdt::crash_counter_get();
        if prev_crashes >= 3 {
            kprintln!("[WDT] WARNING: {} consecutive crashes detected (boot loop?)", prev_crashes);
            kprintln!("[WDT] Continuing with reduced init — check hardware and config.");
        } else if prev_crashes > 0 {
            kprintln!("[WDT] Recovering from {} previous crash(es)", prev_crashes);
        }
        // Clean boot — reset counter now that late-init is stable.
        robot_os_drivers::wdt::crash_counter_reset();
        kprintln!("[WDT] Crash counter reset (clean boot)");
    }

    // PMP: display the intended Robot OS memory-protection policy.
    // pmp_configure() must be called from M-mode (before mret into S-mode).
    // Here we display it for boot-time audit; actual enforcement is M-mode only.
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
    kprintln!("[ROBOT] OTA:         A/B firmware slots (shell: ota recv/status/verify)");
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
    // E04: Payload abstraction — spray, gripper, camera trigger
    robot_os_behavior::payload::payload_init();
    kprintln!("[PAYLOAD] E04: spray GPIO{}, gripper PWM ch{}, cam-trig GPIO{}",
        robot_os_behavior::payload::PAYLOAD_GPIO_SPRAY,
        robot_os_behavior::payload::PAYLOAD_PWM_GRIPPER,
        robot_os_behavior::payload::PAYLOAD_GPIO_CAM_TRIGGER);

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

    // F14: SpacemiT K1 NPU initialization.
    #[cfg(feature = "k1")]
    {
        let npu_ver = robot_os_drivers::npu::npu_init();
        let (major, minor, patch) = robot_os_drivers::npu::npu_version();
        kprintln!("[NPU] SpacemiT K1 NPU initialized — HW v{}.{}.{} ({:#010x})",
            major, minor, patch, npu_ver);
        // Clock-gate off until first inference to save ~200 mW standby.
        robot_os_drivers::npu::npu_power_gate();
        kprintln!("[NPU] Clock-gated (power off) — will wake on first job");
    }
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

    // Phase O: WiFi (no-op on VF2/K1/QEMU — no on-SoC WiFi peripheral).
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
    kprintln!(" Phase 5: SMP Scheduler ({} CPUs)", num_cpus);
    kprintln!("========================================");
    kprintln!();

    // PHANES Phase 1 W3 — install the static topology before the
    // scheduler so that future task spawns can pull their cap_table +
    // class assignment from RFC-0005 declarations. For now we ship the
    // built-in `default_minimal()` topology; W4+ replaces this with a
    // signed CAPS.TOML / SCHED.TOML loaded from FAT32.
    match robot_os_topology::init(robot_os_topology::default_minimal()) {
        Ok(()) => kprintln!(
            "[TOPO] Topology installed: {} classes, {} tasks",
            robot_os_topology::get().map(|t| t.classes_len()).unwrap_or(0),
            robot_os_topology::get().map(|t| t.tasks_len()).unwrap_or(0),
        ),
        Err(e) => {
            kprintln!("[TOPO] Topology install FAILED: {:?} — halting", e);
            loop { robot_os_arch::cpu::wfi(); }
        }
    }

    robot_os_sched::init();

    // Wire priority inheritance callbacks so PiMutex can boost/restore
    // task priorities through the scheduler.
    robot_os_sync::pi_mutex::pi_set_callbacks(
        robot_os_sched::pi_boost_task,
        robot_os_sched::pi_restore_task,
        // Yield: a contended PiMutex waiter must release the hart, or the
        // owner it just boosted cannot run to finish the critical section.
        robot_os_sched::task_yield,
    );

    // Release ALL of a dying task's IPC resources before its TID — and the
    // pool slot the typed-cap tables are indexed by — can be reused.
    // `crates/ipc` depends on `crates/sched`, so the scheduler cannot call
    // into it directly; the kernel closes the loop here, same shape as the
    // PI callbacks above.
    //
    // W3-F7: this hook used to be `handle_revoke_all`, which cleans only the
    // legacy global handle table. Two other per-task resource classes leaked
    // through it: typed capabilities (`cap_store`, whose own doc claimed
    // task_exit reset it while having zero callers) and shared-memory
    // references. `task_release_all` is the single entry point that does all
    // three; registering anything narrower here re-opens the leak silently,
    // because nothing reports an un-revoked capability.
    robot_os_sched::set_task_exit_hook(task_release_all_resources);

    // Wire WaitQueue block/wake callbacks so Completion/WaitQueue can
    // sleep and wake tasks through the scheduler.
    robot_os_sync::waitqueue::wq_set_callbacks(
        robot_os_sched::wq_block_current,
        robot_os_sched::wq_wake_by_tid,
    );

    // Tell the scheduler how many CPUs are *expected* to come online so the
    // task_create calls below — which run before any secondary hart exists —
    // distribute evenly across all of them. This is an optimistic estimate
    // from the DTB, not a confirmation: secondary harts aren't started until
    // wake_harts() runs near the end of this function (after the UART SMP
    // lock is enabled and every task is enqueued — per-CPU ready queues
    // can't be touched cross-CPU once a hart is live). Once wake_harts()
    // reports how many harts actually started, NUM_ONLINE_CPUS is corrected
    // down to the real count so that later task creation (e.g. fork()) never
    // targets a hart that failed to start.
    robot_os_sched::smp::NUM_ONLINE_CPUS.store(num_cpus, Ordering::SeqCst);

    // Create idle task (keeps CPU 0 alive after all workers finish).
    robot_os_sched::task_create("idle", idle_task, 0, robot_os_sched::IDLE_PRIORITY);
    kprintln!("[SCHED] Created idle task");

    // Shell task: priority 13 (high normal — above workers, runs in RT task sleep gaps).
    // No CPU pin so it works on both 1-CPU and 4-CPU QEMU.
    robot_os_sched::task_create("shell", shell_task, 0, 13);
    kprintln!("[SCHED] Created shell task");

    // I3 experiment (RFC-0031): one-shot lease priority-inversion probe on a
    // plain (CPU-0) task so it runs in a 1-hart boot. Emits one `[I3]` line.
    #[cfg(feature = "qemu")]
    {
        robot_os_sched::task_create_affinity("i3-probe", i3_probe::runner, 0,
                                             i3_probe::PROBE_PRIO, 0);
        kprintln!("[SCHED] Created i3-probe task (RFC-0031 lease inversion)");
    }

    // K-A14 probe: PiMutex donation with holder and waiter on one hart.
    #[cfg(feature = "pi-smoke")]
    {
        robot_os_sched::task_create_affinity("pi-probe", pi_probe::runner, 0,
                                             pi_probe::PROBE_PRIO, 0);
        kprintln!("[SCHED] Created pi-probe task (K-A14 PiMutex donation)");
    }

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
    // Pinned to hart 2: hart 0 is owned by rt-motor + flight-ctrl (prio 8),
    // hart 1 by imu + sensor-ahrs (prio 8/14). Without affinity the
    // scheduler was leaving `behavior` competing for hart 3 against
    // every other prio-14+ task, and the brain TCP dial never fired
    // inside the E2E window. Hart 2 is otherwise idle so we get
    // immediate scheduling.
    robot_os_sched::task_create_affinity(
        "behavior", behavior_task, 0,
        robot_os_sched::BEHAVIOR_PRIORITY, 2);
    kprintln!("[SCHED] Created behavior task (subsumption L0-L3) [hart 2]");
    // Hart 0: dedicated real-time control — PID loop pinned to avoid jitter.
    robot_os_sched::task_create_affinity("rt-motor", rt_motor_task, 0,
        robot_os_sched::RT_MOTOR_PRIORITY, 0);
    kprintln!("[SCHED] Created rt-motor task (MotorCmd→PID→PWM + watchdog) [hart 0]");

    // Dedicated sensor tasks (AQ0: IO-wait, priority-separated)
    robot_os_sched::task_create_affinity("imu", imu_task, 0,
        robot_os_sched::RT_MOTOR_PRIORITY, 1); // RT priority, hart 1
    robot_os_sched::task_create("odom", odom_task, 0, robot_os_sched::BEHAVIOR_PRIORITY);
    robot_os_sched::task_create("sensor-slow", sensor_slow_task, 0, robot_os_sched::DEFAULT_PRIORITY);
    kprintln!("[SCHED] Sensor tasks: imu(RT,100Hz) odom(50Hz) sensor-slow(10Hz)");

    // Fast-IPC slot census, diagnostic only. Gated so production builds and
    // ordinary QEMU runs never create the task at all.
    #[cfg(feature = "ipc-census")]
    {
        robot_os_sched::task_create(
            "ipc-census", ipc_census_task, 0, robot_os_sched::DEFAULT_PRIORITY);
        kprintln!("[SCHED] Created ipc-census task (ipc-trace: fast-IPC slot states)");
    }

    // M05: IO Ring async worker task — processes SQEs submitted via SYS_IO_SUBMIT_ASYNC.
    robot_os_sched::task_create("io-ring-worker", io_ring_worker_task, 0, robot_os_sched::DEFAULT_PRIORITY);
    kprintln!("[SCHED] Created io-ring-worker task (M05: async IO Ring processing)");

    // Phase U1: dedicated network polling task — decouples net I/O from behavior loop.
    //
    // History: previously pinned to hart 2 alongside behavior to keep TCP
    // responsive when harts 0/1 are saturated by RT tasks (rt-motor, imu,
    // sensor-ahrs, flight-ctrl).  Empirically that pairing inverted: net-poll
    // is a busy yield-loop (NET_POLL_PRIORITY=12, lower-number = higher), so
    // it never blocks and effectively starved behavior (prio 14) on hart 2 —
    // behavior managed only ~3 iterations in 40 s under QEMU TCG bench
    // (vs the 10 Hz design point = 400 iterations).  Moved to hart 3 which
    // has no other pinned system task — same TCP responsiveness goal, no
    // starvation of behavior's control loop.  autorun (when present) also
    // lives on hart 3 but is short-lived and yields cooperatively.
    robot_os_sched::task_create_affinity("net-poll", net_poll_task, 0,
        robot_os_sched::NET_POLL_PRIORITY, 3);
    kprintln!("[SCHED] Created net-poll task (IO-wait, 100Hz) [hart 3]");

    // Phase I1: sensor + AHRS fusion task (~100 Hz).
    // Hart 1: sensor fusion — dedicated to avoid contention with motor PID on hart 0.
    robot_os_sched::task_create_affinity("sensor-ahrs", sensor_ahrs_task, 0,
        robot_os_sched::SENSOR_AHRS_PRIORITY, 1);
    kprintln!("[SCHED] Created sensor-ahrs task (IMU+baro+GPS→AHRS→channels) [hart 1]");

    // Phase J+K: flight controller task (mixer + PID + failsafe).
    // Hart 0: flight controller — same hart as rt-motor for cache locality.
    robot_os_sched::task_create_affinity("flight-ctrl", flight_control_task, 0,
        robot_os_sched::FLIGHT_CTRL_PRIORITY, 0);
    kprintln!("[SCHED] Created flight-ctrl task (PID→mixer→ESC + failsafe) [hart 0]");

    // Phase L: telemetry task (attitude + GPS → UDP).
    robot_os_sched::task_create("telemetry", telemetry_task, 0, robot_os_sched::DEFAULT_PRIORITY);
    kprintln!("[SCHED] Created telemetry task (channels→UDP)");

    // Phase 16: system watchdog — checks stack canaries + timer liveness.
    robot_os_sched::task_create("sys-wdt", system_wdt_task, 0, robot_os_sched::WATCHDOG_PRIORITY);
    kprintln!("[SCHED] Created sys-wdt task (Phase 16: canaries + timer liveness)");

    // Create stress-test workers. find_least_loaded_cpu() distributes them
    // evenly across num_cpus CPUs (4 tasks per CPU for 16 total = 15+idle).
    // SKIP workers in test scenarios that need IO bandwidth (OTA E2E, etc.) —
    // 15 busy workers at DEFAULT_PRIORITY starve the listener task on a 4-CPU
    // QEMU and the test never sees any OTA data on the wire.
    // Skip the 15 DEFAULT_PRIORITY stress workers when *either* the
    // OTA receiver or the behavior (brain) server is configured. Same
    // reasoning in both cases: 15 busy-yield workers on 4 QEMU CPUs
    // pile up enough scheduler load to starve the listener / TCP-dial
    // task and the test never sees its event on the wire — the E2E
    // wheeled run found `behavior_task` never reached its loop because
    // 15 workers + ml-demo on CPU 3 kept the run queue full.
    let skip_for_ota = robot_os_config::CFG_OTA_AUTO_RECV_PORT
        .load(Ordering::Relaxed) != 0;
    let skip_for_brain = robot_os_config::BEHAVIOR_SERVER_PORT
        .load(Ordering::Relaxed) != 0;
    if skip_for_ota || skip_for_brain {
        let reason = if skip_for_ota { "ota_auto_recv_port" } else { "behavior_server_port" };
        kprintln!("[SCHED] Skipping {} stress-test workers ({} set)", NUM_WORKERS, reason);
    } else {
        for i in 0..NUM_WORKERS {
            robot_os_sched::task_create("worker", worker_task, i, robot_os_sched::DEFAULT_PRIORITY);
            kprintln!("[SCHED] Created worker task {}", i);
        }
    }
    kprintln!();

    // ── AT: Pub/sub initialization ───────────────────────────────────────────
    // Wire the wake callback so topic subscribers are woken when data arrives.
    robot_os_pubsub::set_wake_callback(|task_idx| {
        robot_os_sched::wake_by_channel(task_idx as u32);
    });

    // Create default topics for inter-task communication.
    /// Message size for IMU topic (accel[3] + gyro[3] = 24 bytes, padded to 64).
    const TOPIC_IMU_MSG_SIZE: u16 = 64;
    /// Message size for battery topic (voltage, current, etc).
    const TOPIC_BATTERY_MSG_SIZE: u16 = 16;
    /// Message size for motor command topic.
    const TOPIC_MOTOR_CMD_MSG_SIZE: u16 = 16;
    /// Message size for status topic.
    const TOPIC_STATUS_MSG_SIZE: u16 = 32;

    robot_os_pubsub::topic_create(b"/sensors/imu", TOPIC_IMU_MSG_SIZE);
    robot_os_pubsub::topic_create(b"/sensors/battery", TOPIC_BATTERY_MSG_SIZE);
    robot_os_pubsub::topic_create(b"/cmd/motor", TOPIC_MOTOR_CMD_MSG_SIZE);
    robot_os_pubsub::topic_create(b"/status", TOPIC_STATUS_MSG_SIZE);
    kprintln!("[PUBSUB] Initialized 4 default topics");

    // A truncated CONFIG.INI value means the kernel is running a configuration
    // nobody wrote. Say so before anything acts on it — the symptom otherwise
    // shows up somewhere unrelated (a cut autorun path reads as "file not
    // found", a cut IP as an unreachable host).
    {
        let cut = robot_os_config::cfg_truncated_count();
        if cut > 0 {
            kprintln!("[CONFIG] WARNING: {} value(s) in CONFIG.INI exceeded {} \
                       bytes and were truncated — the running config is NOT \
                       what is on disk", cut, robot_os_config::MAX_VAL);
        }
    }

    // Phase U4: autorun ELF — if CONFIG.INI has `autorun=<path>`, spawn a
    // task that loads and exec's that ELF at boot (e.g. brain client).
    if let Some(path) = robot_os_config::cfg_get(b"autorun") {
        if !path.is_empty() {
            // Copy path to a static buffer so the autorun task can access it.
            let len = path.len().min(AUTORUN_PATH_MAX - 1);
            let buf = unsafe { &mut *(&raw mut AUTORUN_PATH) };
            buf[..len].copy_from_slice(&path[..len]);
            buf[len] = 0;
            // Autorun is a one-shot ELF loader that must run promptly at boot,
            // then exec_user replaces it with the user process. At
            // DEFAULT_PRIORITY (16) it was starved indefinitely by the
            // always-ready system tasks (net-poll=12, behavior/sensor=14) on
            // whatever hart it landed on. Give it a higher priority (10, below
            // rt-motor=8) and pin it to hart 3 (no pinned system task there) so
            // the loader actually gets scheduled. Once it exec's, the user
            // process inherits a normal priority.
            const AUTORUN_PRIORITY: u32 = 10;
            const AUTORUN_HART: i8 = 3;
            robot_os_sched::task_create_affinity("autorun", autorun_task, len,
                                        AUTORUN_PRIORITY, AUTORUN_HART);
            kprintln!("[SCHED] Created autorun task: {}",
                core::str::from_utf8(&path[..len]).unwrap_or("?"));
        }
    }

    // E11.AQ3 validation smoke: if a userspace gpio_drv was autorun'd, exercise
    // the ring-3 driver round-trip from a kernel task. Pinned to a hart other
    // than the autorun hart (3) so the proxy's busy-wait and gpio_drv's serve
    // loop run concurrently. QEMU-only — it's a validation aid, not production.
    #[cfg(feature = "qemu")]
    {
        const GPIO_SMOKE_PRIORITY: u32 = 13;
        const GPIO_SMOKE_HART: i8 = 1;
        robot_os_sched::task_create_affinity(
            "gpio-aq3-smoke", gpio_user_driver_smoke_task, 0,
            GPIO_SMOKE_PRIORITY, GPIO_SMOKE_HART,
        );
    }

    // reflex-smoke: drive the ring-3 obstacle-avoidance daemon through a real
    // decision by moving the simulated rangefinder underneath it, and assert
    // it reacts. Without this, "reflex runs" only ever meant "reflex printed
    // its banner": with a clear road it correctly does nothing, so a working
    // daemon and a daemon whose sensor reads are all denied produce identical
    // output. That ambiguity is exactly what hid the missing capability grant.
    #[cfg(feature = "reflex-smoke")]
    {
        const REFLEX_SMOKE_PRIORITY: u32 = 13;
        const REFLEX_SMOKE_HART: i8 = 1;   // not hart 3 — that is autorun's
        robot_os_sched::task_create_affinity(
            "reflex-smoke", reflex_smoke_task, 0,
            REFLEX_SMOKE_PRIORITY, REFLEX_SMOKE_HART,
        );
    }

    // (OTA auto-recv listener was spawned earlier, right after net_init().)

    // Enable SMP UART lock before secondary CPUs can print.
    robot_os_drivers::uart::enable_smp_lock();
    kprintln!("[SMP] UART lock enabled");

    // Start secondary harts via SBI HSM hart_start (OpenSBI parks them by default).
    {
        kprintln!("[SMP] Starting {} secondary harts via SBI HSM...", num_cpus - 1);
        let online = unsafe { robot_os_sched::smp::wake_harts(num_cpus) };
        if online != num_cpus {
            kprintln!(
                "[SMP] WARNING: only {}/{} harts started — degraded to {} online CPU(s)",
                online, num_cpus, online
            );
        }
        // Correct NUM_ONLINE_CPUS from the optimistic pre-boot estimate
        // (set above, before task creation, so the boot-time task_create
        // calls could spread across the intended CPU count) to the real
        // count wake_harts() confirmed. This is what protects any task
        // created from here on (e.g. fork() in crates/sched/src/process.rs)
        // from being load-balanced onto a hart that never came up — see
        // NUM_ONLINE_CPUS's doc comment in crates/sched/src/smp.rs.
        robot_os_sched::smp::NUM_ONLINE_CPUS.store(online, Ordering::SeqCst);

        // Rescue tasks that the *pre-boot optimistic* task_create calls
        // (above, before wake_harts() ran) assigned to a hart that then
        // failed to start — those per-CPU ready queues would otherwise sit
        // forever, since this scheduler has no runtime work-stealing
        // (verified: no steal/rebalance/migrate logic anywhere in
        // crates/sched/src/scheduler.rs). This is the only point in the
        // whole boot sequence where ready queues can be moved between CPUs
        // without racing another consumer: the boot hart hasn't called
        // sched::start() yet, hasn't enabled its own timer interrupt yet
        // (a few lines below), and dead harts by definition never run any
        // code at all. See `rebalance_from_offline_cpus`'s doc comment for
        // why it still routes every touch through the locked queue
        // wrappers regardless (an *alive* secondary hart can start ticking
        // independently of the boot hart's progress here).
        if online != num_cpus {
            robot_os_sched::rebalance_from_offline_cpus(online, num_cpus);
        }
    }

    kprintln!("[SCHED] Starting scheduler on boot CPU — tasks will now preempt...");
    kprintln!();

    // Enable timer interrupts NOW (deferred from Phase 3 init): from this
    // point the timer ISR may preempt us, so any code below runs at scheduler
    // dispatch latency. set_next_tick programs the first deadline.
    robot_os_drivers::clint::set_next_tick(hart_id as u32);
    let sie = csr::read_sie();
    csr::write_sie(sie | csr::SIE_STIE);

    // PHANES Phase 1 W4-int.2 — boot-time smoke test for the APS
    // dispatch path. Picks a task via the policy runqueues to confirm
    // the co-enqueue path actually populated them.
    match robot_os_sched::aps_state::smoke_test(hart_id as usize) {
        Ok(tid) => kprintln!("[APS]  smoke OK — pick_next on CPU {} → tid {}", hart_id, tid),
        Err(reason) => kprintln!("[APS]  smoke FAIL on CPU {}: {}", hart_id, reason),
    }

    // PHANES Phase 1 W4-int.5 — exercise the APS dispatch toggle
    // atomically. The previous `[APS] smoke OK` print already verified
    // the policy runqueues are populated; this verifies the flag
    // atomic is usable. A long-running APS-active soak is W4-int.6
    // territory (not in Phase 1's exit criteria).
    let was = robot_os_sched::use_aps_dispatch(true);
    let _ = robot_os_sched::use_aps_dispatch(false);
    kprintln!("[APS]  dispatch toggle on/off OK (prev was {})", was);

    // Authoritative config-driven backend selection — supersedes the
    // diagnostic toggle above.  `SCHED_BACKEND_APS` is emitted by
    // phanes_config from the Kconfig choice in Kconfig.timing; true
    // means the user selected APS, false means Legacy (the default).
    // Called once in the single-threaded boot path, before `start()`.
    if robot_os_limits::SCHED_BACKEND_APS {
        let _prev = robot_os_sched::use_aps_dispatch(true);
        kprintln!("[SCHED] config-selected backend: APS (experimental)");
    } else {
        kprintln!("[SCHED] config-selected backend: Legacy (default)");
    }

    // PHANES Phase 1 A4 — registry smoke. Register the static
    // UartDriver into the driver registry, then look it up by kind
    // and drive a write through `dyn Driver`. Proves the full
    // RFC-0002 path (api → registry → lookup → trait dispatch →
    // hardware) end-to-end. Stays InKernel + isolated to this
    // smoke; the legacy `kprint!` macros are unchanged.
    {
        // Trait methods on `&dyn Driver` are accessible through the
        // return type itself — no explicit `use` needed.
        static UART_DRV: robot_os_drivers::uart_driver::UartDriver =
            robot_os_drivers::uart_driver::UartDriver::new();
        match robot_os_drivers::runtime::registry::REGISTRY
            .lock()
            .register(&UART_DRV)
        {
            Ok(()) => kprintln!("[REG]  UART registered into driver registry"),
            Err(e) => kprintln!("[REG]  UART register FAILED: {:?}", e),
        }
        let probe = robot_os_drivers::runtime::registry::REGISTRY
            .lock()
            .find_by_kind(/*DRV_KIND_UART*/ 0x0004);
        if let Some(drv) = probe {
            let _ = drv.init();
            let msg = b"[REG]  dyn Driver write via registry OK\n";
            match drv.handle_request(
                robot_os_drivers::uart_driver::UART_OP_WRITE,
                msg,
                &mut [],
            ) {
                Ok(_) => {} // bytes already printed by the driver
                Err(e) => kprintln!("[REG]  dyn Driver write FAILED: {:?}", e),
            }
        } else {
            kprintln!("[REG]  find_by_kind(UART) returned None");
        }

        // A3a.2 — register the second concrete `Driver` impl.
        // Validates that the trait + registry handle two unrelated
        // hardware families side by side (different DRV_KIND_*).
        static GPIO_DRV: robot_os_drivers::gpio_driver::GpioDriver =
            robot_os_drivers::gpio_driver::GpioDriver::new();
        match robot_os_drivers::runtime::registry::REGISTRY
            .lock()
            .register(&GPIO_DRV)
        {
            Ok(()) => kprintln!("[REG]  GPIO registered into driver registry"),
            Err(e) => kprintln!("[REG]  GPIO register FAILED: {:?}", e),
        }

        // A3a.3 — register the third concrete `Driver` impl.
        // Bus-oriented family; proves the trait scales across
        // hardware models (char / pin / bus).
        static I2C_DRV: robot_os_drivers::i2c_driver::I2cDriver =
            robot_os_drivers::i2c_driver::I2cDriver::new();
        match robot_os_drivers::runtime::registry::REGISTRY
            .lock()
            .register(&I2C_DRV)
        {
            Ok(()) => kprintln!("[REG]  I2C  registered into driver registry"),
            Err(e) => kprintln!("[REG]  I2C  register FAILED: {:?}", e),
        }

        // A3a.4 — fourth: multi-parameter actuator (PWM).
        static PWM_DRV: robot_os_drivers::pwm_driver::PwmDriver =
            robot_os_drivers::pwm_driver::PwmDriver::new();
        match robot_os_drivers::runtime::registry::REGISTRY
            .lock()
            .register(&PWM_DRV)
        {
            Ok(()) => kprintln!("[REG]  PWM  registered into driver registry"),
            Err(e) => kprintln!("[REG]  PWM  register FAILED: {:?}", e),
        }

        // A3a.5 — fifth: closed-loop controller (motor PID).
        // Pure software (no MMIO) — composes PWM + encoders.
        static MOTOR_DRV: robot_os_drivers::motor_driver::MotorPidDriver =
            robot_os_drivers::motor_driver::MotorPidDriver::new();
        match robot_os_drivers::runtime::registry::REGISTRY
            .lock()
            .register(&MOTOR_DRV)
        {
            Ok(()) => kprintln!("[REG]  MTR  registered into driver registry"),
            Err(e) => kprintln!("[REG]  MTR  register FAILED: {:?}", e),
        }

        // A5.next — exercise SYS_DRV_INVOKE end-to-end through the
        // *real* syscall handler (not just the trait directly).
        // Called from kernel context so `current_user_pt() == 0`
        // and the raw-copy path is taken; the userspace path will
        // be exercised by the brain client task later.
        let msg = b"[A5]   sys_drv_invoke UART write via syscall OK\n";
        // Wire-format constants — mirrors of robot_os_abi::syscall_nr
        // and uart_driver. Avoid a kernel→abi Cargo dep for what is
        // just a boot-time smoke.
        const SYS_DRV_INVOKE_NR: u64 = 311;
        const DRV_KIND_UART_NR: u64 = 0x0004;
        const UART_OP_WRITE_NR: u64 = 0;
        let rc = robot_os_syscall::syscall_dispatch(
            SYS_DRV_INVOKE_NR,
            DRV_KIND_UART_NR,
            UART_OP_WRITE_NR,
            msg.as_ptr() as u64,
            msg.len() as u64,
            /* out_ptr */ 0,
            /* out_cap */ 0,
            /* sepc */ 0, /* user_sp */ 0,
            // Synthetic call from kernel context: there is no real trap frame,
            // and this can never be SYS_FORK, which is the only arm that reads
            // the register file (K-C11).
            &[0u64; 32],
        );
        if rc < 0 {
            kprintln!("[A5]   sys_drv_invoke returned errno {}", rc);
        }




    }

    // WDT: initialize hardware watchdog (VF2/K1 only; no-op on QEMU).
    // Phase G2: timeout from config (default 500 ms).
    // Armed here, immediately before sched::start(), so the gap between
    // "armed" and "first kick" (which only happens from the timer ISR,
    // enabled inside sched::start()) is minimal — arming this earlier in
    // boot left a long unkicked window that could reset the board mid-init.
    let wdt_ms = robot_os_config::CFG_WATCHDOG_MS.load(Ordering::Relaxed);
    robot_os_drivers::wdt::wdt_init(wdt_ms);
    if robot_os_drivers::wdt::wdt_has_hardware() {
        kprintln!("[WDT] Hardware watchdog initialized ({} ms timeout)", wdt_ms);
        kprintln!("[WDT] Counter = {}", robot_os_drivers::wdt::wdt_counter());
    } else {
        kprintln!("[WDT] No hardware WDT (QEMU) — software watchdog active");
    }

    // Re-establish tp = hart_id immediately before entering the scheduler.
    // Rust functions called during kernel_main (including kprintln) may have used
    // tp as a caller-saved scratch register, corrupting current_cpu_id().
    // After this point no Rust functions are called before context_switch.S saves/restores tp.
    unsafe { core::arch::asm!("mv tp, {}", in(reg) hart_id, options(nostack, nomem)); }

    // Start the scheduler on the boot CPU (never returns).
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
#[unsafe(no_mangle)]
pub extern "C" fn smp_secondary_start(hart_id: usize) -> ! {
    // boot.S sets tp = hart_id via `mv tp, a0` before calling here.
    // Re-establish it in case any early Rust call clobbered tp.
    unsafe { core::arch::asm!("mv tp, {}", in(reg) hart_id, options(nostack, nomem)); }

    // boot.S range-checked us against MAX_HARTS — that guards the stack and
    // trap-vector slots, which are `[_; MAX_HARTS]`. The scheduler arrays
    // are `[_; MAX_CPUS]` (smaller), and `wake_harts` never *starts* a hart
    // past MAX_CPUS — but a hart can arrive here without us starting it
    // (firmware HSM state, a resumed hart, a future warm-boot path). Park it
    // before it enables the timer: one tick later it would be inside
    // `schedule()` indexing `PER_CPU[hart_id]` out of bounds. Interrupts are
    // still off (boot.S cleared sie), so parking here is final.
    if hart_id >= MAX_CPUS {
        kprintln!("[SMP] hart {} >= MAX_CPUS {} — parking (no PER_CPU slot)",
                  hart_id, MAX_CPUS);
        loop { robot_os_arch::cpu::wfi(); }
    }

    // Enable timer interrupt (boot.S cleared sie to 0).
    let sie = csr::read_sie();
    csr::write_sie(sie | csr::SIE_STIE);

    // Set the first timer tick for this CPU.
    robot_os_drivers::clint::set_next_tick(hart_id as u32);

    // Enable global S-mode interrupts.
    let sstatus = csr::read_sstatus();
    csr::write_sstatus(sstatus | csr::SSTATUS_SIE);

    // WFI loop — timer interrupt will call schedule() → pick first task for this CPU.
    // Re-set tp = hart_id on every iteration: SBI calls (set_next_tick above) and
    // Rust functions inside the timer ISR are allowed to use tp as a scratch register
    // (RISC-V caller-saved).  The timer fires while wfi is executing; at that instant
    // tp must equal hart_id so current_cpu_id() returns the correct CPU index in schedule().
    loop {
        unsafe { core::arch::asm!("mv tp, {}", in(reg) hart_id, options(nostack, nomem)); }
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
/// Fast-IPC slot census — prints `(pending, accepted, replied)` every few
/// seconds. Diagnostic only, compiled out unless `--features ipc-trace`.
///
/// **WHY a periodic census and not the per-exchange trace.** When a fast-IPC
/// exchange wedges, the client sits on `FastIpcClient(slot)` and the server on
/// `FastIpcServer(tid)`, and the log cannot tell whether the slot is `Pending`
/// (the server lost the wake) or `Accepted` (it took the call and never
/// answered). Those are different bugs. The per-exchange `ipc_trace!` would
/// answer it but costs six UART writes per exchange — enough to move the race:
/// measured, the traced build reaches `ALL PASSED` 8/8 where the untraced one
/// wedges. One three-integer line every few seconds does not.
///
/// Reading a rising `accepted` count that never drains is the signature of a
/// server that stopped replying; a rising `pending` count is a server that
/// stopped being woken.
#[cfg(feature = "ipc-census")]
const IPC_CENSUS_INTERVAL: u64 = 20_000_000; // 2 s at the 10 MHz CLINT

#[cfg(feature = "ipc-census")]
fn ipc_census_task(_arg: usize) {
    let mut last = (0u32, 0u32, 0u32, 0u32);
    loop {
        let now = robot_os_ipc::fast_ipc_census();
        // Only speak when something changed — a steady state prints nothing and
        // costs nothing, so a wedge shows up as the last line before silence.
        if now != last {
            // The scheduler side is what separates the two explanations for a
            // wedge whose reply is already deposited: a `Blocked` client lost
            // its wake, a `Ready` one is being starved. Printed together so the
            // two halves can never be read from different moments.
            let (ready, blocked, running, per_cpu, ready_unq, blk_q, by_rsn) =
                robot_os_sched::task_census();
            kprintln!(
                "[IPC-CENSUS] pending={} accepted={} replied={} used={} (suma={}) | ready={} blocked={} running={} | rq={:?} | LOST ready_unqueued={} blocked_queued={}",
                now.0, now.1, now.2, now.3, now.0 + now.1 + now.2, ready, blocked, running, per_cpu,
                ready_unq, blk_q);
            kprintln!("[IPC-CENSUS] blocked_on: fastclient={} fastserver={} timer={} waitq={} otros={}",
                by_rsn[0], by_rsn[1], by_rsn[2], by_rsn[3], by_rsn[4]);
            let (d, st, mm, ab, enq, late) = robot_os_sched::wake_counters();
            kprintln!("[IPC-CENSUS] wake: dispatched={} stamped={} MISMATCH={} absent={} enq_refused={} late_dispatch={}",
                d, st, mm, ab, enq, late);
            // Las dos mitades, impresas juntas: ranuras vivas por identidad y
            // tareas dormidas en fast-IPC por identidad. Si el `caller` de una
            // ranura `Replied` (codigo 3) coincide con el `tid` de un cliente
            // dormido y su `slot` coincide con el indice, el despertar se
            // perdio pese a todos los contadores. Si no coinciden, la premisa
            // era falsa y el atasco es otra cosa.
            let mut slots = [(0u8, 0u8, 0u32, 0u32); 8];
            let ns = robot_os_ipc::fast_ipc_slot_ids(&mut slots);
            for e in slots.iter().take(ns) {
                kprintln!("[IPC-CENSUS]   slot idx={} state={} caller={} server={}",
                    e.0, e.1, e.2, e.3);
            }
            // Identidades del estado imposible: Ready sin cola. El contador
            // dice cuantas; esto dice QUIENES — que es lo que separa "el
            // servidor esta muerto de hambre" de "una tarea de fondo
            // irrelevante quedo colgada".
            let mut unq = [(0u32, 0u32, 0u32, [0u8; 8]); 8];
            let nu = robot_os_sched::ready_unqueued_ids(&mut unq);
            for e in unq.iter().take(nu) {
                let name = core::str::from_utf8(&e.3).unwrap_or("?");
                kprintln!("[IPC-CENSUS]   READY-UNQUEUED tid={} prio={} home={} name={}",
                    e.0, e.1, e.2, name);
            }
            // Who is each hart actually running? Separates "ready tasks
            // starve behind a current that never yields" from "ready tasks
            // lost by the queues" — the run-6 (2026-08-24) wedge shape.
            let mut cur = [(0u32, 0u32, [0u8; 8]); robot_os_sched::MAX_CPUS];
            robot_os_sched::current_snapshot(&mut cur);
            for (cpu, e) in cur.iter().enumerate() {
                if e.0 == 0 { continue; }
                let name = core::str::from_utf8(&e.2).unwrap_or("?");
                kprintln!("[IPC-CENSUS]   cpu={} cur_tid={} word={:#x} name={}",
                    cpu, e.0, e.1, name);
            }
            // `word` is the raw K-C19 state word (bit 3 = WAKE_STAMP) and
            // `saving` the K-C24 gate: a task stuck at Blocked+saving=true is
            // one every wake can only stamp and no switch-out will ever sweep.
            let mut blk = [(0u32, false, 0u32, 0u32, false); 8];
            let nb = robot_os_sched::blocked_fastipc_ids(&mut blk);
            for e in blk.iter().take(nb) {
                kprintln!("[IPC-CENSUS]   blocked tid={} client={} payload={} word={:#x} saving={}",
                    e.0, e.1, e.2, e.3, e.4);
            }
            last = now;
        }
        // Same timer-block idiom as the sensor tasks: a yield loop would spin
        // hot and compete with the very race this is measuring.
        let dl = robot_os_drivers::clint::get_time() + IPC_CENSUS_INTERVAL;
        robot_os_sched::task_block(robot_os_sched::WaitReason::Timer(dl));
    }
}

/// M05: IO Ring async worker — processes SQEs submitted via SYS_IO_SUBMIT_ASYNC.
///
/// Runs as a best-effort kernel task.  Yields when no work is pending to avoid
/// burning cycles.  When `IO_RING_WORK_PENDING` is set, drains all ring queues.
fn io_ring_worker_task(_: usize) {
    kprintln!("[IORW] M05: IO Ring async worker started");
    loop {
        if robot_os_ipc::io_ring_has_async_work() {
            let _ = robot_os_ipc::io_ring_worker_poll();
        }
        robot_os_sched::task_yield();
    }
}

/// Polls at ~100 Hz (1 yield = 10 ms at 100 Hz scheduler).
fn net_poll_task(_: usize) {
    kprintln!("[NET-POLL] Phase U1: dedicated network polling task started");

    loop {
        robot_os_net::net_poll();
        // AR: TCP tick — drive retransmissions, TIME-WAIT, keep-alive timers.
        robot_os_net::tcp::tcp_tick();
        robot_os_sched::task_yield();
    }
}

/// Phase U4: autorun task — loads and exec's an ELF from the filesystem.
///
/// IMU sensor task — reads IMU at 100 Hz, writes to sensor bus.
/// RT priority: IMU data must be fresh for safety layer L0.
fn imu_task(_: usize) {
    kprintln!("[IMU-TASK] Started (100 Hz, RT priority)");
    const IMU_INTERVAL: u64 = robot_os_drivers::clint::TIMER_FREQ / 100;
    loop {
        if let Some(d) = robot_os_imu::imu_read_scaled() {
            robot_os_behavior::sensor_bus::SENSOR_BUS.update_imu(d.accel_mg, d.gyro_mdps);
            robot_os_behavior::sensor_bus::SENSOR_BUS.update_temp(d.temp_cdeg);
        }
        let dl = robot_os_drivers::clint::get_time() + IMU_INTERVAL;
        robot_os_sched::task_block(robot_os_sched::WaitReason::Timer(dl));
    }
}

/// Odometry + encoder task at 50 Hz.
fn odom_task(_: usize) {
    kprintln!("[ODOM-TASK] Started (50 Hz)");
    const ODOM_INTERVAL: u64 = robot_os_drivers::clint::TIMER_FREQ / 50;
    loop {
        let (el, er) = robot_os_robot::encoder_read();
        robot_os_robot::odom_update(el, er);
        let (d, h) = robot_os_robot::odom_get();
        robot_os_behavior::sensor_bus::SENSOR_BUS.update_odom(d, h, el, er);
        let dl = robot_os_drivers::clint::get_time() + ODOM_INTERVAL;
        robot_os_sched::task_block(robot_os_sched::WaitReason::Timer(dl));
    }
}

/// Slow sensor task: rangefinder + battery + GPIO flags at 10 Hz.
fn sensor_slow_task(_: usize) {
    kprintln!("[SENSOR-SLOW] Started (10 Hz)");
    const SLOW_INTERVAL: u64 = robot_os_drivers::clint::TIMER_FREQ / 10;
    use robot_os_behavior::sensor_bus::SENSOR_BUS;
    loop {
        let f = robot_os_drivers::rangefinder::us_read_mm(0).unwrap_or(0) as u16;
        let r = robot_os_drivers::rangefinder::us_read_mm(1).unwrap_or(0) as u16;
        SENSOR_BUS.update_range(f, r);
        let mv: u16 = if robot_os_drivers::ads1115::ads1115_is_initialized() {
            robot_os_drivers::ads1115::ads1115_read_battery_mv(0, 2).unwrap_or(3700) as u16
        } else { 3700 };
        SENSOR_BUS.update_battery(mv);
        let mut flags: u16 = 0;
        if robot_os_drivers::gpio::gpio_read(13) == 1 { flags |= 0x0001; }
        if robot_os_drivers::gpio::gpio_read(15) == 1 { flags |= 0x0002; }
        if robot_os_drivers::gpio::gpio_read(14) == 1 { flags |= 0x0004; }
        SENSOR_BUS.update_flags(flags);
        SENSOR_BUS.update_timestamp(robot_os_drivers::clint::get_time());
        if robot_os_behavior::offline::offline_is_active() && flags != 0 {
            robot_os_drivers::buzzer::buzzer_beep();
        }
        let dl = robot_os_drivers::clint::get_time() + SLOW_INTERVAL;
        robot_os_sched::task_block(robot_os_sched::WaitReason::Timer(dl));
    }
}

/// /sys/scheduler procfs entry. Reports the active
/// scheduler dispatch backend (Legacy / Aps / reserved variants)
/// per the runtime registry from A1.
fn gen_sys_scheduler(buf: &mut [u8]) -> usize {
    use robot_os_sched::runtime::registry::{active, SchedulerHandle};
    let h = active();
    let name = match h {
        SchedulerHandle::Legacy => "legacy",
        SchedulerHandle::Aps => "aps",
        SchedulerHandle::Fifo => "fifo (reserved)",
        SchedulerHandle::EdfCbs => "edf-cbs (reserved)",
        SchedulerHandle::Rr => "rr (reserved)",
        SchedulerHandle::Cfs => "cfs (reserved)",
        SchedulerHandle::Sporadic => "sporadic (reserved)",
    };
    let s = alloc::format!("active: {}\nraw: {}\n", name, h.as_raw());
    let bytes = s.as_bytes();
    let n = bytes.len().min(buf.len());
    buf[..n].copy_from_slice(&bytes[..n]);
    n
}

/// A4.next — /sys/drivers procfs entry. Walks the RFC-0002
/// driver registry and emits one line per registered driver:
/// `<kind_hex> <name> <isolation> <perms>`. Empty if nothing
/// has registered yet.
fn gen_sys_drivers(buf: &mut [u8]) -> usize {
    use robot_os_drivers::api::DriverIsolation;
    let reg = robot_os_drivers::runtime::registry::REGISTRY.lock();
    let mut s = alloc::string::String::new();
    for kind in 0u32..0x100 {
        if let Some(d) = reg.find_by_kind(kind) {
            let m = d.manifest();
            let iso = match m.isolation {
                DriverIsolation::InKernel => "inkernel",
                DriverIsolation::UserProcess { .. } => "userproc",
                DriverIsolation::Hypervisor => "hypervisor",
            };
            s.push_str(&alloc::format!(
                "0x{:04x} {} {} 0x{:02x}\n",
                m.kind,
                m.name,
                iso,
                m.required_perms.bits(),
            ));
        }
    }
    let bytes = s.as_bytes();
    let n = bytes.len().min(buf.len());
    buf[..n].copy_from_slice(&bytes[..n]);
    n
}

/// The ELF path is stored in the static `AUTORUN_PATH` buffer (set during boot).
/// `arg` contains the path length.
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

    // Provision the capabilities the process will need BEFORE it runs.
    //
    // Every hardware syscall starts with `cap_check`, which for a userspace
    // task scans the global handle table for a matching (owner_tid, kind).
    // Nothing in the tree ever filled that table, so until now every
    // `sensor_read`/`motor_speed` from ring 3 returned E_PERM. That is why
    // reflex and brain_client could be started and still do nothing: reflex
    // treats a failed range read as "no obstacle" (`range_front > 0` guards
    // the comparison), so a blind daemon and a daemon on a clear road produce
    // byte-identical output.
    //
    // Scope, deliberately: read on the sensor types, write on the two drive
    // motors, and nothing else — no GPIO, no I2C, no PWM, no MMIO. The grant
    // uses RO/RW rather than ALL so `duplicate` stays false: even with the
    // ownership checks now in `handle_dup`, a capability that cannot be
    // duplicated cannot be spread by a compromised process at all.
    //
    // The TID does not change across `exec_user` — the autorun kernel task
    // becomes the user process — so granting here is granting to the process
    // that is about to run.
    {
        use robot_os_ipc::{HandleKind, HandlePerms, handle_grant};
        let tid = robot_os_sched::current_task_tid();
        let mut granted = 0u32;
        // Sensor type IDs 0..=9, mirroring SENSOR_TYPE_* in
        // crates/syscall/src/handlers.rs (IMU, ODOM, ENCODER, RANGE,
        // BATTERY, GPS, LIDAR, _, CAMERA, POWER).
        for st in 0u8..=9 {
            if handle_grant(tid, HandleKind::Sensor(st), HandlePerms::RO).is_some() {
                granted += 1;
            }
        }
        for m in 0u32..2 {
            if handle_grant(tid, HandleKind::Motor(m), HandlePerms::RW).is_some() {
                granted += 1;
            }
        }
        kprintln!("[AUTORUN] Granted {} capabilities to tid {} \
                   (10 sensors RO, 2 motors RW)", granted, tid);
    }

    // P1 migration (RFC-0003/RFC-0005, dual mode): mint the TYPED
    // equivalents alongside the legacy grants above. Nothing above is
    // removed or changed — both authorization paths now agree, so ring 3
    // can move to `SYS_MOTOR_*_TYPED` (550-555) without losing access, and
    // `cap_check`'s legacy path keeps working exactly as before for
    // anything that hasn't migrated. Sourced from the topology's
    // declarative "autorun" task (`robot_os_topology::default_minimal`)
    // rather than hard-coding motor ids a second time here — see
    // `crates/ipc/src/cap_seed.rs` for the bridge and its ordering
    // contract (this call happens from inside the already-running autorun
    // task, i.e. strictly after its own pool slot was claimed, which is
    // exactly the property that contract requires).
    {
        let tid = robot_os_sched::current_task_tid();
        let mut minted = 0u32;
        let mut skipped = 0u32;
        match robot_os_topology::get() {
            Some(topo) => match topo.find_task(&robot_os_topology::MaybeStr::from_bytes(b"autorun")) {
                Some(task) => {
                    for cap in topo.caps_of(task) {
                        let target = cap.target.as_str();
                        match robot_os_ipc::cap_seed::seed_one_cap(tid, cap.kind, cap.perms, target) {
                            Some(handle) => {
                                minted += 1;
                                kprintln!(
                                    "[AUTORUN][CAP-SEED] minted kind={:?} target={} perms={:?} \
                                     tid={} handle={:#010x}",
                                    cap.kind, target, cap.perms, tid, handle.as_raw(),
                                );
                            }
                            None => {
                                skipped += 1;
                                kprintln!(
                                    "[AUTORUN][CAP-SEED] skipped kind={:?} target={} \
                                     (no typed minter yet, or target did not parse)",
                                    cap.kind, target,
                                );
                            }
                        }
                    }
                }
                None => kprintln!("[AUTORUN][CAP-SEED] no 'autorun' task in topology — nothing minted"),
            },
            None => kprintln!("[AUTORUN][CAP-SEED] topology not installed — nothing minted"),
        }
        kprintln!(
            "[AUTORUN] Typed caps minted via topology bridge: {} (skipped {}) for tid {}",
            minted, skipped, tid,
        );
    }

    kprintln!("[AUTORUN] Read {} bytes, exec'ing...", n);
    let rc = robot_os_sched::exec_user(&buf[..n as usize]);
    if rc != 0 {
        kprintln!("[AUTORUN] exec_user failed (rc={})", rc);
        return;
    }
    // autorun is a kernel task — it cannot rely on the ecall/SRET return path
    // a user process uses. Like the shell's `exec` command, take the prepared
    // hand-off (K-C21: published on THIS task's own slot, and the taker has
    // already installed the new satp — sret_to_user's own write of the same
    // value is a harmless re-write) and SRET to U-mode directly. Previously
    // this task simply returned here, hitting task_exit() before the pending
    // exec was ever applied — so the user process never started.
    if let Some(ctx) = robot_os_sched::take_current_task_exec_ctx() {
        kprintln!("[AUTORUN] SRET to user-space entry={:#x}", ctx.entry);
        unsafe {
            robot_os_sched::sret_to_user(
                ctx.entry   as usize,
                ctx.user_sp as usize,
                ctx.satp    as usize,
            );
        }
        // sret_to_user() is -> ! — unreachable
    }
}

/// E11.AQ3 validation smoke: exercise the ring-3 GPIO driver round-trip.
///
/// Constructs a `UserDriverProxy` for `DRV_KIND_GPIO` and issues a
/// `GPIO_OP_PING` through it. The proxy enqueues the request on the
/// driver-server queue; the userspace `gpio_drv` process (autorun'd) fetches,
/// handles, and replies; the proxy returns the reply. A successful round-trip
/// proves a driver running in a *user process* serves kernel-side callers.
///
/// Retries until `gpio_drv` has registered (it may still be starting), then
/// prints the result once. QEMU-only (validation aid).
#[cfg(feature = "qemu")]
fn gpio_user_driver_smoke_task(_: usize) {
    use robot_os_drivers::api::{Driver, DriverIsolation, DriverManifest};
    use robot_os_drivers::user_driver_proxy::UserDriverProxy;
    use robot_os_abi::cap::CapPerms;

    const DRV_KIND_GPIO: u32 = 0x0001;
    const GPIO_OP_PING: u32 = 0;
    const PING_REPLY_TAG: u8 = 0xA5;
    const PING_INPUT: u8 = 0x42;
    /// Outer retry budget while gpio_drv comes up. autorun loads the ELF from
    /// FAT32 and exec's it only after boot is well underway, so the smoke must
    /// out-wait that startup. Each failed attempt is either: (a) submit-fail
    /// fast (kind not yet registered) → 100 yields; or (b) submit OK + 1M-iter
    /// proxy busy-wait timeout (gpio_drv didn't reply in this window). With a
    /// generous budget the validation tolerates QEMU-TCG timing variance
    /// across binary-layout shifts.
    const SMOKE_MAX_ATTEMPTS: u32 = 20_000;
    /// Yields between attempts.
    const SMOKE_YIELDS_PER_ATTEMPT: u32 = 100;

    // tid is informational here — request routing is by driver kind.
    let manifest = DriverManifest::new(
        DRV_KIND_GPIO,
        "gpio-user",
        DriverIsolation::UserProcess { tid: 0 },
        CapPerms::RW,
    );
    let proxy = UserDriverProxy::new(manifest);

    let input = [PING_INPUT];
    let mut output = [0u8; 8];
    let mut attempts = 0u32;
    loop {
        match proxy.handle_request(GPIO_OP_PING, &input, &mut output) {
            Ok(n) if n >= 2 && output[0] == PING_REPLY_TAG => {
                kprintln!(
                    "[AQ3] GPIO ring-3 round-trip OK — reply tag={:#04x} echo={:#04x} ({} bytes from user process)",
                    output[0], output[1], n
                );
                return;
            }
            Ok(n) => {
                kprintln!("[AQ3] GPIO ring-3 unexpected reply (len={}, out0={:#04x})", n, output[0]);
                return;
            }
            Err(_) => {
                attempts += 1;
                if attempts >= SMOKE_MAX_ATTEMPTS {
                    kprintln!("[AQ3] GPIO ring-3 round-trip FAILED — no reply from gpio_drv");
                    return;
                }
                for _ in 0..SMOKE_YIELDS_PER_ATTEMPT {
                    robot_os_sched::task_yield();
                }
            }
        }
    }
}

/// reflex-smoke: prove the ring-3 reflex daemon actually reacts to sensors.
///
/// Sequence, all observed through reflex's own stdout:
///   1. wait for the daemon to be up (it prints a banner on entry)
///   2. put an obstacle at 100 mm — below OBSTACLE_CRITICAL_MM (150) — and
///      expect `[reflex] CRITICAL OBSTACLE`
///   3. clear the road to 1500 mm — above OBSTACLE_CLEAR_MM (600) — and
///      expect `[reflex] Clear`
///
/// Step 3 matters as much as step 2. Asserting only the trigger would pass on
/// a daemon wedged permanently in override, which on a robot means motors
/// held in reverse forever.
///
/// QEMU-only: `us_set_distance` writes the simulated distance array that
/// `us_read_mm` serves. On real hardware the value comes from the sensor.
#[cfg(feature = "reflex-smoke")]
fn reflex_smoke_task(_arg: usize) {
    use robot_os_drivers::rangefinder;

    // Let the autorun task load the ELF and reach its main loop. reflex polls
    // at 40 Hz, so a handful of yields is plenty once it is running; the
    // budget is generous because ELF load from FAT32 dominates.
    for _ in 0..2_000_000 { robot_os_sched::task_yield(); }

    kprintln!("[REFLEXSMOKE] placing obstacle at 100mm (critical < 150mm)");
    rangefinder::us_set_distance(0, 100);

    for _ in 0..2_000_000 { robot_os_sched::task_yield(); }

    kprintln!("[REFLEXSMOKE] clearing road to 1500mm (clear > 600mm)");
    rangefinder::us_set_distance(0, 1500);

    for _ in 0..2_000_000 { robot_os_sched::task_yield(); }

    kprintln!("[REFLEXSMOKE] DONE");
}

/// Release every per-task resource when a task dies.
///
/// The exit hook takes a single `fn(u32)`, and two crates need to clean up:
/// `robot_os_ipc` (legacy handles, typed caps, shared-memory references) and
/// `robot_os_net` (sockets). Neither can call the other — `crates/net` is
/// deliberately scheduler-agnostic and `crates/ipc` has no business knowing
/// about sockets — so the kernel is the only place the two can be joined.
///
/// **Ordering is deliberate: IPC first, sockets second.** `socket_release_all`
/// is not pure bookkeeping — for a socket in `Established`/`CloseWait` it
/// TRANSMITS a FIN synchronously through the NIC. Doing that after the
/// capability teardown means a task cannot be holding a half-revoked
/// capability while its connection is still being closed on the wire.
///
/// **Do not move this hook later in `task_exit`.** `cap_store::reset` resolves
/// the TID back to a task-pool slot and only works while `TASK_VALID[idx]` is
/// still true; the hook fires before the Zombie marking and long before
/// `do_schedule` frees the slot. Moved past either point it becomes a silent
/// no-op, which is exactly the failure mode this cleanup exists to prevent.
fn task_release_all_resources(tid: u32) {
    robot_os_ipc::task_release_all(tid);
    robot_os_net::socket_release_all(tid);
}

/// Maximum autorun ELF path length (including NUL terminator).
const AUTORUN_PATH_MAX: usize = 64;

/// Static buffer for autorun ELF path (set during boot, read by autorun_task).
static mut AUTORUN_PATH: [u8; AUTORUN_PATH_MAX] = [0u8; AUTORUN_PATH_MAX];

/// Phase G1: behavior task — subsumption engine running indefinitely.
///
/// Each tick (~100 ms = 10 yields):
/// 1. Collect SensorState (camera, IMU, odometry, encoders)
/// 2. If remote enabled: TCP send VlaObservation, recv VlaAction/VlaGoal
/// 3. If ML enabled: mlp_infer → MlpResult
/// 4. Arbitrate L0→L3 — first valid output wins
/// 5. Publish motor command + update odometry
/// Receive exactly `buf.len()` bytes from TCP `fd`, yielding while waiting,
/// up to `deadline` (CLINT ticks). Returns true iff the buffer filled. Used
/// to read the fixed-size RFC-0019 handshake frames off the stream.
fn tcp_recv_exact(fd: usize, buf: &mut [u8], deadline: u64) -> bool {
    let mut got = 0usize;
    while got < buf.len() {
        if robot_os_drivers::clint::get_time() >= deadline {
            return false;
        }
        let n = robot_os_net::tcp::recv(fd, &mut buf[got..]);
        if n > 0 {
            got += n as usize;
        } else {
            robot_os_sched::task_yield();
        }
    }
    true
}

/// Drive the RFC-0019 responder handshake on an established TCP socket. The
/// brain (initiator) sends HELLO first; we reply HELLO+proof and wait for
/// CONFIRM. Returns the established `EncryptLink` or `None` on failure
/// (timeout, bad proof, send error).
///
/// Deadline is a generous 5 s wall-clock: the X25519 work is slow under QEMU
/// TCG (~830k cycles measured) and SLIRP NAT thread-starves the emulated
/// harts, so a tight deadline would make the handshake flaky for the same
/// reason the TCP handshake deadline had to move 500ms→2s.
fn brain_responder_handshake(fd: usize, psk: [u8; 32], salt: u64)
    -> Option<robot_os_behavior::encrypt_link::EncryptLink>
{
    use robot_os_behavior::encrypt_link::{
        EncryptLink, derive_ephemeral_priv,
        HELLO_INIT_BYTES, HELLO_REPLY_BYTES, CONFIRM_BYTES,
    };
    let eph = derive_ephemeral_priv(&psk, salt);
    let mut link = EncryptLink::new(psk, eph);
    let deadline = robot_os_drivers::clint::get_time()
        + robot_os_drivers::clint::TIMER_FREQ * 5;

    // 1. brain → kernel: [0x02][HELLO][brain_e_pub] (34 B)
    let mut hello = [0u8; HELLO_INIT_BYTES];
    if !tcp_recv_exact(fd, &mut hello, deadline) {
        kprintln!("[BRAIN] hs: recv HELLO timed out");
        return None;
    }
    // 2. kernel → brain: [0x02][HELLO][kernel_e_pub][proof_k] (66 B)
    let mut reply = [0u8; HELLO_REPLY_BYTES];
    if link.handle_initiator_hello(&hello, &mut reply).is_err() {
        kprintln!("[BRAIN] hs: handle_initiator_hello rejected");
        return None;
    }
    let sent = robot_os_net::tcp::send_all_with_yield(fd, &reply, robot_os_sched::task_yield);
    kprintln!("[BRAIN] hs: reply want={} sent={}", HELLO_REPLY_BYTES, sent);
    if sent <= 0 {
        return None;
    }
    // 3. brain → kernel: [0x02][CONFIRM][proof_b] (34 B)
    let mut confirm = [0u8; CONFIRM_BYTES];
    if !tcp_recv_exact(fd, &mut confirm, deadline) {
        kprintln!("[BRAIN] hs: recv CONFIRM timed out");
        return None;
    }
    if link.handle_initiator_confirm(&confirm).is_err() {
        kprintln!("[BRAIN] hs: CONFIRM proof rejected");
        return None;
    }
    Some(link)
}

/// Wrap a brain-protocol `frame` in the auth envelope, then (RFC-0019)
/// encrypt it when `link` is established, and push every byte to the wire.
/// Returns bytes sent (>0 on success). Routing every TCP send through this
/// keeps the wire uniform — never a mix of plaintext and encrypted frames,
/// which the brain's single-mode reader could not demultiplex.
fn send_framed(
    fd: usize,
    frame: &[u8],
    link: &mut Option<robot_os_behavior::encrypt_link::EncryptLink>,
    salt: &mut u64,
) -> i32 {
    use robot_os_behavior::auth_envelope;
    // Largest non-camera frame is a SensorPacket; size buffers for it.
    const SENSOR_FRAME_SIZE: usize = robot_os_behavior::brain_protocol::SENSOR_FRAME_SIZE;
    const ENV_MAX: usize = SENSOR_FRAME_SIZE + auth_envelope::ENVELOPE_OVERHEAD;
    const WIRE_MAX: usize = ENV_MAX + robot_os_behavior::encrypt_link::ENC_OVERHEAD;
    if frame.len() > SENSOR_FRAME_SIZE {
        return 0;
    }
    let mut env = [0u8; ENV_MAX];
    let env_len = auth_envelope::wrap(frame, &mut env);
    if env_len == 0 {
        return 0;
    }
    // Build the inner wire bytes: AEAD-encrypted envelope when the link is
    // established, otherwise the HMAC envelope itself.
    let mut wire = [0u8; WIRE_MAX];
    let inner: &[u8] = match link.as_mut() {
        Some(l) => {
            let nr = robot_os_behavior::encrypt_link::fresh_nonce_rand(*salt);
            *salt = salt.wrapping_add(1);
            let wlen = l.encrypt(&env[..env_len], &nr, &mut wire);
            if wlen == 0 {
                return 0;
            }
            &wire[..wlen]
        }
        None => &env[..env_len],
    };
    // Outermost: RFC-0021 multi-stream framing on STREAM_CONTROL when enabled,
    // so the brain demuxes control vs camera/lidar BEFORE decode. Composes
    // outside the AEAD layer.
    if robot_os_config::CFG_MULTI_STREAM.load(Ordering::Relaxed) {
        use robot_os_multi_stream as ms;
        // RFC-0021 scheduling policy is a COMPILE-TIME choice (Kconfig
        // MULTISTREAM_SCHED_PRIORITY → robot_os_limits const; the unused
        // branch is const-eliminated, zero hot-path overhead). Baseline =
        // FIFO (control + bulk share the link in send order). When PRIORITY
        // is selected, experiment I2 will interleave STREAM_CONTROL ahead of
        // bulk-stream chunks HERE. One-shot log surfaces the compiled policy.
        #[cfg(feature = "qemu")]
        {
            use core::sync::atomic::{AtomicBool, Ordering as O};
            static MS_POLICY_LOGGED: AtomicBool = AtomicBool::new(false);
            if !MS_POLICY_LOGGED.swap(true, O::Relaxed) {
                robot_os_drivers::kprintln!(
                    "[MS] sched policy: {} (compile-time)",
                    if robot_os_limits::MULTISTREAM_SCHED_PRIORITY { "priority" } else { "fifo" },
                );
            }
        }
        let mut ms_buf = [0u8; ms::HEADER_LEN + WIRE_MAX];
        match ms::wrap(ms::STREAM_CONTROL, inner, &mut ms_buf) {
            Ok(ms_len) => robot_os_net::tcp::send_all_with_yield(
                fd, &ms_buf[..ms_len], robot_os_sched::task_yield,
            ) as i32,
            Err(_) => 0,
        }
    } else {
        robot_os_net::tcp::send_all_with_yield(
            fd, inner, robot_os_sched::task_yield,
        ) as i32
    }
}

/// I2 experiment probe (qemu only): measure the control-stream head-of-line
/// hold-off behind a bulk STREAM_CAMERA frame, under the COMPILE-TIME policy
/// (`robot_os_limits::MULTISTREAM_SCHED_PRIORITY`). FIFO sends the whole bulk
/// before the control frame; PRIORITY chunks the bulk and lets the control
/// frame jump ahead after the first chunk. Emits one `[I2]` line. Run once.
#[cfg(feature = "qemu")]
fn i2_holdoff_probe(fd: usize) {
    use robot_os_drivers::wcet::read_cycles;
    use robot_os_multi_stream as ms;
    use robot_os_net::tcp::send_all_with_yield;
    use robot_os_sched::task_yield;

    // K-C5: this probe pushes synthetic bulk straight onto the brain socket,
    // outside both the envelope and the AEAD layers. QEMU-only, but the
    // `qemu,link-encrypt-enforced` combination CI builds would otherwise
    // carry a policy-violating sender. Down by policy, like the UART bridge.
    if robot_os_behavior::auth_envelope::link_policy_denial().is_some() {
        return;
    }

    const BULK: usize = 16 * 1024;     // ~11 MSS segments under stop-and-wait TCP
    const CHUNK: usize = 1460;         // one MSS
    static mut BULKBUF: [u8; BULK] = [0x5Au8; BULK];
    static mut WIRE: [u8; CHUNK + ms::HEADER_LEN] = [0u8; CHUNK + ms::HEADER_LEN];

    // A small control frame (content irrelevant — measuring the hold-off, the
    // stub drains it; it need not parse as a brain packet).
    let ctrl = [0x42u8; 32];
    let mut ctrl_wire = [0u8; 32 + ms::HEADER_LEN];
    let ctrl_len = ms::wrap(ms::STREAM_CONTROL, &ctrl, &mut ctrl_wire).unwrap_or(0);

    let bulk = unsafe { &*core::ptr::addr_of!(BULKBUF) };
    let wire = unsafe { &mut *core::ptr::addr_of_mut!(WIRE) };
    let priority = robot_os_limits::MULTISTREAM_SCHED_PRIORITY;

    // t0 = both bulk and control "ready". Measure when control finishes.
    let t0 = read_cycles();
    let mut off = 0usize;
    let mut ctrl_done = false;
    let mut ctrl_holdoff = 0u64;
    while off < BULK {
        let n = (BULK - off).min(CHUNK);
        let w = ms::wrap(ms::STREAM_CAMERA_BASE, &bulk[off..off + n], wire).unwrap_or(0);
        let _ = send_all_with_yield(fd, &wire[..w], task_yield);
        off += n;
        if priority && !ctrl_done {
            // Control jumps ahead after the first bulk chunk.
            let _ = send_all_with_yield(fd, &ctrl_wire[..ctrl_len], task_yield);
            ctrl_holdoff = read_cycles().wrapping_sub(t0);
            ctrl_done = true;
        }
    }
    if !ctrl_done {
        // FIFO: control waits for the entire bulk frame.
        let _ = send_all_with_yield(fd, &ctrl_wire[..ctrl_len], task_yield);
        ctrl_holdoff = read_cycles().wrapping_sub(t0);
    }
    let bulk_total = read_cycles().wrapping_sub(t0);
    kprintln!(
        "[I2] mode={} bulk_bytes={} chunk={} ctrl_holdoff_cyc={} bulk_total_cyc={}",
        if priority { "priority" } else { "fifo" },
        BULK, CHUNK, ctrl_holdoff, bulk_total,
    );
}

/// I3 experiment probe (RFC-0031, qemu only): measure priority inversion
/// through the lease/capability layer on the **legacy priority** scheduler
/// (the live dispatcher), under the COMPILE-TIME policy
/// `robot_os_limits::LEASE_PRIORITY_INHERITANCE`.
///
/// All tasks are pinned to CPU 0 and placed in priority band 1..6 — ABOVE every
/// standing kernel task (rt-motor / flight-ctrl sit at priority 8), so the
/// scenario runs uncontended and 1-hart dispatch is deterministic (lower number
/// = higher priority; RT_MOTOR_PRIORITY = 8).
///   - lessor   (prio 2) grants a lease then BLOCKS awaiting its return
///   - K spinners (prio 4) run a fixed CPU-bound burst then exit
///   - lessee   (prio 6) accepts the lease, returns it, wakes the lessor
///
/// OFF (baseline A): the lessor blocks; the priority scheduler runs the prio-4
/// spinners to completion before the prio-6 lessee is ever scheduled, so the
/// lease return — and thus the lessor — is held off for ~all the spinner work.
/// With a non-expiring lease (expire_ticks=0) this is unbounded by construction.
///
/// ON (B): when the high-priority lessor blocks on a lease held by a
/// lower-priority lessee, boost the lessee's PRIORITY to the lessor's (classic
/// priority inheritance, reusing `pi_boost_task`), so the lessee runs ahead of
/// the spinners and returns immediately. Inversion collapses to the lessee's
/// critical section. Const-eliminated when OFF.
///
/// Emits one `[I3]` line with `lease_inversion_cyc`. Build-time A/B like I2.
#[cfg(feature = "qemu")]
/// K-A14 probe — proves the PiMutex donation protocol on a SINGLE hart.
///
/// The scenario is the one the old spinning implementation could not survive:
/// a low-priority holder and a higher-priority waiter pinned to the same CPU.
/// The holder does a fixed slab of work WITHOUT yielding, so the only way it
/// can ever reach its `release()` is if the waiter gives up the hart from
/// inside `lock()`. With the old spin-to-completion loop this deadlocked
/// outright; the boost was correct and useless, because the boosted task had
/// no CPU to run on.
///
/// Asserts three things, not just liveness:
///   1. the waiter eventually acquires (no hang),
///   2. the holder was actually boosted to the waiter's priority while it held
///      the lock — i.e. inheritance happened, rather than the waiter simply
///      out-waiting it,
///   3. the holder is back at its base priority afterwards, which is what
///      proves donations and restores balanced.
#[cfg(feature = "pi-smoke")]
mod pi_probe {
    use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};
    use robot_os_sched::{
        task_create_affinity, task_exit, current_task_tid, task_priority,
        wq_block_current, wq_wake_by_tid,
    };
    use robot_os_sync::pi_mutex::PiMutex;

    pub const PROBE_PRIO: u32 = 1;   // runner; blocks immediately, never spins
    // Both must outrank the standing tasks or they never run: rt-motor and
    // flight-ctrl sit at priority 8 in the RT band (not timer-preempted), so
    // anything numerically above 8 is starved on CPU0. The i3 probe picks its
    // bands for the same reason. What matters for this test is only that the
    // waiter outranks the holder.
    const WAITER_PRIO:    u32 = 4;   // contends, higher priority than the holder
    const HOLDER_PRIO:    u32 = 6;   // owns the mutex, deliberately lower
    const CPU0: i8 = 0;              // same hart: the whole point of the test

    /// Enough work that the waiter is certainly contending while we hold the
    /// lock, but bounded so a regression FAILs instead of hanging the board.
    const HOLD_WORK: u32 = 2_000_000;

    static M: PiMutex<u32> = PiMutex::new(0);

    static RUNNER_TID: AtomicU32  = AtomicU32::new(0);
    static HOLDER_TID: AtomicU32  = AtomicU32::new(0);
    static ACQUIRED:   AtomicBool = AtomicBool::new(false);
    /// Set by the waiter immediately before it calls `lock()`. The holder waits
    /// for it before starting its no-yield work, so contention is guaranteed
    /// rather than hoped for. Without this the test is a race against timer
    /// granularity: under load the tick that would schedule the higher-priority
    /// waiter arrives late, the holder finishes uncontended, no donation ever
    /// happens, and a perfectly good implementation reports "no-boost".
    static CONTENDING: AtomicBool = AtomicBool::new(false);
    /// Best (numerically lowest) priority the holder saw while it held the lock.
    static PRIO_WHILE_HELD: AtomicU32 = AtomicU32::new(u32::MAX);
    /// Holder's priority immediately after `release()`, sampled by the holder
    /// itself. Read from the runner instead and you race the reaper: the task
    /// is usually already gone and `task_priority` returns None.
    static PRIO_AFTER_RELEASE: AtomicU32 = AtomicU32::new(u32::MAX);

    fn holder_entry(_: usize) {
        HOLDER_TID.store(current_task_tid(), Ordering::SeqCst);

        let g = M.lock();

        // Spawn the contender only after we own the lock. Doing the ordering
        // this way removes every busy-wait from the probe: no task ever spins
        // waiting for another to reach a phase. The waiter is higher priority,
        // so it preempts us the instant it is created and goes straight into
        // contention.
        let _ = task_create_affinity("pi-waiter", waiter_entry, 0, WAITER_PRIO, CPU0);

        // Setup, not the test: yield until the waiter is actually about to
        // contend. Yielding here is fine — the measured section below is the
        // one that must make progress without us ever giving up the hart.
        while !CONTENDING.load(Ordering::SeqCst) {
            robot_os_sched::task_yield();
        }

        // Deliberately NO yield in here. The waiter must be the one to give up
        // the hart, from inside lock(). With the old spin-to-completion mutex
        // this never happened and the scenario deadlocked.
        // Sample repeatedly and keep the best (numerically lowest) priority
        // seen. A single sample at a fixed point is a race against when the
        // waiter happens to donate — it can easily land before the donation
        // and report "no boost" for a working implementation.
        let me = current_task_tid();
        let mut acc: u32 = 0;
        for i in 0..HOLD_WORK {
            acc = acc.wrapping_add(i);
            if i % 4_096 == 0 {
                if let Some(now) = task_priority(me) {
                    let _ = PRIO_WHILE_HELD.fetch_update(
                        Ordering::SeqCst, Ordering::SeqCst,
                        |best| if now < best { Some(now) } else { None });
                }
            }
        }
        core::hint::black_box(acc);

        drop(g);
        PRIO_AFTER_RELEASE.store(
            task_priority(me).unwrap_or(u32::MAX), Ordering::SeqCst);
        task_exit();
    }

    fn waiter_entry(_: usize) {
        CONTENDING.store(true, Ordering::SeqCst);
        let g = M.lock();                 // must not hang
        ACQUIRED.store(true, Ordering::SeqCst);
        drop(g);
        wq_wake_by_tid(RUNNER_TID.load(Ordering::SeqCst));
        task_exit();
    }

    pub fn runner(_: usize) {
        RUNNER_TID.store(current_task_tid(), Ordering::SeqCst);
        let _ = task_create_affinity("pi-holder", holder_entry, 0, HOLDER_PRIO, CPU0);

        // Block, do not yield: this task is the highest priority in the system,
        // so yielding would just reschedule us and starve the very tasks we are
        // waiting for. Blocking removes us from the ready set entirely.
        while !ACQUIRED.load(Ordering::SeqCst) {
            wq_block_current();
        }

        let held  = PRIO_WHILE_HELD.load(Ordering::SeqCst);
        let after = PRIO_AFTER_RELEASE.load(Ordering::SeqCst);

        // Lower number = higher priority, so the donation landed iff the
        // holder's priority reached at least the waiter's level.
        if held > WAITER_PRIO {
            crate::kprintln!("[PISMOKE] FAIL no-boost held={} want<={}", held, WAITER_PRIO);
        } else if after != HOLDER_PRIO {
            // The donation must be undone exactly once. A value still at the
            // boosted level means a leaked boost; anything else means the
            // counter drifted.
            crate::kprintln!("[PISMOKE] FAIL not-restored after={} want={}",
                             after, HOLDER_PRIO);
        } else {
            crate::kprintln!("[PISMOKE] PASS boosted {}->{}, restored to {}",
                             HOLDER_PRIO, held, after);
        }
        task_exit();
    }
}

#[cfg(feature = "qemu")]
mod i3_probe {
    use core::sync::atomic::{AtomicU32, AtomicU64, AtomicBool, Ordering};
    use robot_os_drivers::wcet::read_cycles;
    use robot_os_sched::{
        task_create_affinity, task_exit, task_yield, current_task_tid,
        tid_for_idx, wq_block_current, wq_wake_by_tid,
    };
    use robot_os_ipc::{shm_create, ShmPerms, lease_grant, lease_accept,
                       lease_return, lease_wait_return};

    // Priority band above all standing tasks (rt-motor/flight = 8).
    pub const PROBE_PRIO:  u32 = 1;   // the runner — must outrun everything
    const LESSOR_PRIO: u32 = 2;       // high-priority waiter
    const SPIN_PRIO:   u32 = 4;       // medium starvers
    const LESSEE_PRIO: u32 = 6;       // low-priority lease holder
    const CPU0: i8 = 0;

    const N_SPINNERS: usize = 4;
    const SPIN_ITERS: u64 = 1_000_000;  // fixed CPU-bound burst per spinner
    const NO_TID: u32 = u32::MAX;

    static PROBE_TID:  AtomicU32 = AtomicU32::new(NO_TID);
    static INVERSION_CYC: AtomicU64 = AtomicU64::new(0);
    static LESSOR_DONE: AtomicBool = AtomicBool::new(false);

    fn lessee_entry(_: usize) {
        let tid = current_task_tid();
        loop {
            if let Some((lid, _shm)) = lease_accept(tid) {
                // Tiny critical section (the work the lessor is waiting on).
                core::hint::black_box(lid);
                // lease_return wakes the lessor internally (WaitQueue).
                //
                // `privileged = false` on purpose (IPC-6). This task *is* the
                // lessee, so it passes the new ownership check on its own
                // merits; asking for the kernel bypass here would make the
                // bench the one call site that never exercises the gate it
                // shares with `SYS_IPC_LEASE_RETURN`.
                let _ = lease_return(lid, tid, false);
                task_exit();
            }
            task_yield();
        }
    }

    fn spinner_entry(_: usize) {
        let mut acc: u64 = 0;
        let mut i: u64 = 0;
        while i < SPIN_ITERS {
            acc = acc.wrapping_add(i ^ 0x9E37_79B9);
            core::hint::black_box(acc);
            i += 1;
        }
        core::hint::black_box(acc);
        task_exit();
    }

    fn lessor_entry(lessee_tid: usize) {
        let lessee = lessee_tid as u32;
        let my_tid = current_task_tid();
        // Own SHM page to lease out (content irrelevant — measuring scheduling).
        let shm = shm_create(my_tid, 1, ShmPerms::ReadWrite).unwrap_or(0);
        // expire_ticks = 0 → no expiry → baseline inversion is unbounded.
        let lid = match lease_grant(shm as usize, my_tid, lessee, 0) {
            Some(l) => l,
            None => { LESSOR_DONE.store(true, Ordering::SeqCst); task_exit(); }
        };

        // Measure the PRODUCTION wait path: lease_wait_return() applies the
        // priority-inheritance policy internally (gated by the const) and blocks
        // until the lessee returns. This probe exercises the real mechanism, not
        // an ad-hoc copy — what we measure is what production gets.
        let t0 = read_cycles();
        lease_wait_return(lid);
        INVERSION_CYC.store(read_cycles().wrapping_sub(t0), Ordering::SeqCst);
        LESSOR_DONE.store(true, Ordering::SeqCst);
        let p = PROBE_TID.load(Ordering::SeqCst);
        if p != NO_TID {
            wq_wake_by_tid(p);
        }
        task_exit();
    }

    pub fn run() {
        let inh = robot_os_limits::LEASE_PRIORITY_INHERITANCE;
        PROBE_TID.store(current_task_tid(), Ordering::SeqCst);

        // Create the lessee first so we can hand its tid to the lessor.
        let lessee_idx = task_create_affinity(
            "i3-lessee", lessee_entry, 0, LESSEE_PRIO, CPU0);
        let lessee_tid = tid_for_idx(lessee_idx).unwrap_or(NO_TID);

        for _ in 0..N_SPINNERS {
            let _ = task_create_affinity(
                "i3-spin", spinner_entry, 0, SPIN_PRIO, CPU0);
        }
        let _ = task_create_affinity(
            "i3-lessor", lessor_entry, lessee_tid as usize, LESSOR_PRIO, CPU0);

        // Block until the lessor reports its measurement (it wakes us). The
        // scenario cannot progress until we yield the CPU here, so the wake
        // always follows this block — no lost-wake in this controlled setup.
        while !LESSOR_DONE.load(Ordering::SeqCst) {
            wq_block_current();
        }

        let cyc = INVERSION_CYC.load(Ordering::SeqCst);
        crate::kprintln!(
            "[I3] inheritance={} spinners={} spin_iters={} lease_inversion_cyc={}",
            if inh { "on" } else { "off" }, N_SPINNERS, SPIN_ITERS, cyc,
        );
    }

    /// Task entry: run the probe once on its (CPU-0, top-priority) host task,
    /// then exit. Runs above the standing tasks so it is scheduled promptly even
    /// in a 1-hart boot (behavior_task is hart-2-affined and never runs then).
    pub fn runner(_: usize) {
        run();
        task_exit();
    }
}

/// K-C5: may the UART bridge carry brain-protocol frames right now?
///
/// The bridge is a full brain-protocol plane — PKT_SENSOR/PKT_CAMERA out,
/// PKT_ACTUATOR (motion commands, including the emergency flag) in — with no
/// envelope and no AEAD in either direction. Under `link-encrypt-enforced`
/// it is therefore down BY POLICY, both directions: accepting unauthenticated
/// actuator frames over a wire would void the whole per-packet gate.
///
/// In a default build `link_policy_denial()` const-folds to `None` and this
/// is a constant `true`. The refusal is announced once, mirroring
/// `auth_envelope::announce_denial`'s rationale: a silently dead bridge is
/// indistinguishable from a cabling fault on the console.
fn bridge_policy_permits() -> bool {
    match robot_os_behavior::auth_envelope::link_policy_denial() {
        None => true,
        Some(why) => {
            use core::sync::atomic::AtomicBool;
            static SAID: AtomicBool = AtomicBool::new(false);
            if !SAID.swap(true, Ordering::Relaxed) {
                kprintln!(
                    "[SECCHAN] FATAL: UART bridge refused — {} — \
                     link-encrypt-enforced is compiled in and the bridge \
                     carries neither envelope nor AEAD. The bridge is DOWN \
                     BY POLICY, both directions.",
                    why.as_str());
            }
            false
        }
    }
}

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
    // RFC-0019 encrypted link: established per TCP connection (fresh ephemeral
    // keys each time → forward secrecy), `None` when plaintext/HMAC-only.
    // `enc_salt` feeds the ephemeral-key + nonce derivation so successive
    // connections/packets don't reuse entropy.
    let mut link: Option<robot_os_behavior::encrypt_link::EncryptLink> = None;
    let mut enc_salt: u64 = 0;

    // Camera frame sending: every CAMERA_SEND_INTERVAL behavior cycles (~2 Hz at 10 Hz loop)
    const CAMERA_SEND_INTERVAL: u32 = 5;
    let mut camera_cycle: u32 = 0;

    // RFC-0027 I1: auto-emit `wcet_report()` + `jitter_report()` every N
    // behavior loop iterations so the bench harness collects per-function
    // WCET data without depending on shell-injection of `wcet\r\n` (which
    // can be dropped by SMP TCG UART IRQ routing).  behavior_task is the
    // most reliably-running task during bench (visible via [BRAIN] log
    // entries) — sys-wdt was the original target but it never reaches its
    // loop body under QEMU TCG.
    //
    // The behavior loop runs at ~10 Hz, so 300 iterations ≈ 30 s — short
    // enough that a 40 s steady scenario sees at least one auto-report.
    // Gated `cfg(feature = "qemu")` for the same reason as the bound
    // zeroing in `crates/drivers/src/wcet.rs` — on real hardware the shell
    // works reliably and the operator can dump on demand.
    // RFC-0027 I1: auto-report uses a real-mtime deadline rather than an
    // iteration count.  Empirical observation 2026-05-29 bench: under QEMU
    // TCG the behavior loop's `task_block(WaitReason::Timer)` is sleeping
    // ~13 s instead of the intended 100 ms, so an iteration counter would
    // either fire too rarely (threshold=300 → never in a 40 s bench) or
    // not fire at all.  An mtime-based deadline is robust to whatever the
    // actual iteration rate turns out to be: when sleep is fixed, the
    // ~10 s cadence keeps the report rate sane; when sleep is broken, it
    // still fires on every iteration that exceeds the deadline.
    #[cfg(feature = "qemu")]
    const WCET_AUTOREPORT_INTERVAL_SEC: u64 = 10;
    #[cfg(feature = "qemu")]
    let mut wcet_autoreport_deadline: u64 = robot_os_drivers::clint::get_time();
    // 2026-05-30: also fire `robot_os_bench::run_all` ONCE shortly after
    // boot to emit a synthetic [BENCH-RES] baseline.  Shell-injected
    // `bench` command via the harness FIFO has proven unreliable end-to-
    // end (auto-report fires but shell-input does not reach the parser
    // under QEMU TCG SMP), so we trigger from a task that demonstrably
    // runs.  100 iterations per microbench keeps the run under 1 s and
    // avoids dominating bench scenario time.
    #[cfg(feature = "qemu")]
    let mut bench_run_all_done: bool = false;
    #[cfg(feature = "qemu")]
    const BENCH_RUN_ALL_ITERS: u64 = 100;

    loop {
        // ── 1. Read sensor state from SENSOR_BUS ─────────────────────────
        // Sensor tasks (imu_task, odom_task, sensor_slow_task) write to
        // the bus at their own rates. We just take a snapshot here.
        let now = robot_os_drivers::clint::get_time();
        let mut state = SensorState::new();
        robot_os_behavior::sensor_bus::SENSOR_BUS.snapshot(&mut state);
        state.timestamp = now;

        // Camera capture (still inline — camera task is future AQ3)
        #[cfg(not(feature = "no-ml"))]
        {
            let tick = TICK_COUNT.load(Ordering::Relaxed);
            let pattern = (tick % 3) as u8;
            let frame = robot_os_camera::cam_capture(pattern);
            let _feat = robot_os_camera::cam_extract_features(&frame);
            state.cam_pixels[..32].copy_from_slice(&frame.pixels);
            state.cam_w = 8;
            state.cam_h = 4;
            state.cam_valid = true;
        }

        // ── 2. Brain Protocol: TCP send/recv ─────────────────────────────
        if robot_os_behavior::remote_is_enabled() {
            // Network polling is handled by the dedicated net-poll task (Phase U1).

            // Connect if not yet connected
            if !tcp_connected {
                let ip   = robot_os_behavior::remote_server_ip();
                let port = robot_os_behavior::remote_server_port();
                if port > 0 {
                    // connect_with_yield resolves ARP first, then sends SYN —
                    // avoids the "first SYN dropped silently due to ARP miss
                    // → close-and-reconnect loop" pattern that previously cost
                    // ~1.5-2 s on every initial connection under SLIRP/QEMU.
                    tcp_fd = robot_os_net::tcp::connect_with_yield(
                        ip, port, 12345, robot_os_sched::task_yield,
                    );
                    if tcp_fd >= 0 {
                        // `tcp::connect` only sends SYN; it returns immediately
                        // with state = SynSent.  We MUST wait for the handshake
                        // to advance to Established before sending the first
                        // StatusPacket — otherwise `send_data` returns -1, the
                        // end-of-iteration `conn_state` check marks the link as
                        // dead, the next iteration calls `connect` again and
                        // we burn TCP_MAX_CONNS slots in a loop without ever
                        // pushing a single byte to the brain.  Yield-poll with
                        // a hard cap so a peer that refuses to ACK doesn't
                        // hang the behavior task forever.
                        //
                        // Cap by WALL-CLOCK time, not yield count.  yield is
                        // cheap (just gives CPU to the next ready task), so a
                        // pure yield count caps quickly without giving the
                        // SYN-ACK time to physically arrive via the virtio-net
                        // interrupt.
                        //
                        // Empirical observation 2026-05-29 bench: ~33% of
                        // handshake attempts under QEMU TCG SMP-4 stall in
                        // SynSent.  Trace: SLIRP NAT under TCG thread-starves
                        // when 4 emulated harts run on one host thread, so an
                        // occasional SYN or SYN-ACK is dropped.  TCP's normal
                        // retransmit at `RTO_INITIAL_MS = 1000` (defined in
                        // `crates/net/src/tcp.rs:135`) would recover, but the
                        // previous 500 ms deadline here was SHORTER than RTO
                        // → no retransmit chance, deterministic failure on any
                        // dropped SYN.  Bumped to TIMER_FREQ * 2 (= 2 s wall)
                        // so the TCP layer gets at least one SYN retransmit
                        // before we give up.  The wait is paid once per
                        // (re)connection, so the worst-case cost is +1.5 s on
                        // the first behavior iteration when there's loss.
                        let handshake_deadline = robot_os_drivers::clint::get_time()
                            + robot_os_drivers::clint::TIMER_FREQ * 2;
                        // SLEEP-poll, never yield-poll. This used to be a
                        // `task_yield()` busy-loop: ~200k yields per 2 s
                        // window at behavior's priority. Under strict
                        // priority dispatch a yield re-enqueues the yielder,
                        // so every same-or-lower-priority task sharing this
                        // hart got NOTHING for the whole 2 s except the
                        // 100 ms retry gap — measured as watchdog storms on
                        // rt-motor's heartbeat path and as the phase-A
                        // throughput floor (~0.8 s/exchange while a brainless
                        // scenario retried forever; the 22-08 audit's
                        // "unidentified remaining bottleneck"). A TCP
                        // handshake is a WAIT, not work: poll the state at
                        // 10 ms — same 2 s deadline, 200 polls, and the hart
                        // belongs to whoever has real work in between.
                        let mut waited = 0u32;
                        while robot_os_drivers::clint::get_time() < handshake_deadline
                            && robot_os_net::tcp::conn_state(tcp_fd as usize)
                               != robot_os_net::tcp::TcpState::Established
                        {
                            let next_poll = robot_os_drivers::clint::get_time()
                                + robot_os_drivers::clint::TIMER_FREQ / 100;
                            robot_os_sched::task_block(
                                robot_os_sched::WaitReason::Timer(next_poll));
                            waited += 1;
                        }
                        let st_after = robot_os_net::tcp::conn_state(tcp_fd as usize);
                        if st_after != robot_os_net::tcp::TcpState::Established {
                            // Handshake didn't complete in 2 s; close the
                            // half-open socket and let the next iteration
                            // retry after the 100 ms loop sleep.
                            kprintln!("[BRAIN] handshake stalled (state={}) after {} polls / 2s",
                                      st_after as u8, waited);
                            robot_os_net::tcp::close(tcp_fd as usize);
                            tcp_fd = -1;
                        } else {
                        tcp_connected = true;
                        robot_os_behavior::remote_set_connected(true);
                        robot_os_behavior::remote_set_socket(tcp_fd);
                        // Deactivate offline mode — brain is back
                        robot_os_behavior::offline::offline_deactivate();
                        kprintln!("[BRAIN] connected fd={} (handshake took {} yields)",
                                  tcp_fd, waited);

                        // ── RFC-0019 encrypted-link handshake ──────────────
                        // If `link_encrypt=1` in CONFIG.INI, run the responder
                        // handshake NOW, before any packet is sent (the brain,
                        // as initiator, speaks first). No silent fallback: if
                        // the flag is set but no LINK.KEY is present, or the
                        // handshake fails, drop the connection rather than send
                        // plaintext.
                        //
                        // K-C5: under `link-encrypt-enforced` the handshake is
                        // unconditional — the `cfg!` is OR'ed here rather than
                        // written into CFG_LINK_ENCRYPT, because that flag is
                        // re-applied by config_apply from a CONFIG.INI that
                        // lives on the USB-exposed FAT volume: a file must not
                        // be able to disarm a compiled-in policy.
                        link = None;
                        if robot_os_config::CFG_LINK_ENCRYPT.load(Ordering::Relaxed)
                            || cfg!(feature = "link-encrypt-enforced")
                        {
                            match robot_os_behavior::auth_envelope::link_key_copy() {
                                Some(psk) => {
                                    enc_salt = enc_salt.wrapping_add(1);
                                    match brain_responder_handshake(
                                        tcp_fd as usize, psk, now ^ enc_salt,
                                    ) {
                                        Some(l) => {
                                            link = Some(l);
                                            kprintln!("[BRAIN] RFC-0019 encrypted link established");
                                            // K-C5: re-arm the one-shot denial
                                            // announcements — a denial hours
                                            // after this handshake must print
                                            // again, not be swallowed by a bit
                                            // set before it.
                                            robot_os_behavior::auth_envelope::reset_denial_announcements();
                                        }
                                        None => {
                                            kprintln!("[BRAIN] RFC-0019 handshake failed — closing");
                                            robot_os_net::tcp::close(tcp_fd as usize);
                                            tcp_fd = -1;
                                            tcp_connected = false;
                                            robot_os_behavior::remote_set_connected(false);
                                        }
                                    }
                                }
                                None => {
                                    kprintln!("[BRAIN] CFG_LINK_ENCRYPT set but no LINK.KEY — \
                                               closing (RFC-0019: no plaintext fallback)");
                                    robot_os_net::tcp::close(tcp_fd as usize);
                                    tcp_fd = -1;
                                    tcp_connected = false;
                                    robot_os_behavior::remote_set_connected(false);
                                }
                            }
                        }

                        // Send StatusPacket immediately on connect (only if the
                        // link is still up — the handshake above may have
                        // dropped it). `send_framed` wraps + optionally encrypts.
                        if tcp_connected {
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
                            let st_sent = send_framed(
                                tcp_fd as usize, &st_frame[..st_len], &mut link, &mut enc_salt,
                            );
                            kprintln!("[BRAIN] status sent: frame={} wire={} enc={}",
                                      st_len, st_sent, link.is_some() as u8);
                        }
                        }  // end of `else` (handshake reached Established)
                    } else {
                        kprintln!("[BRAIN] connect failed rc={}", tcp_fd);
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

                // Frame and send. Wrap the framed packet in the auth envelope
                // (identity passthrough when no LINK.KEY is loaded).
                let mut sp_frame = [0u8; SENSOR_FRAME_SIZE];
                let sp_len = build_packet(PKT_SENSOR, &sp_payload, &mut sp_frame);
                // Wrap + (RFC-0019) encrypt + send-all in one place. #39 fix
                // (loop until the full frame is on the wire) lives inside
                // send_framed via send_all_with_yield.
                // I2 experiment: one-shot head-of-line hold-off probe under the
                // compile-time multi-stream policy. Only when multi-stream is on.
                #[cfg(feature = "qemu")]
                {
                    use core::sync::atomic::AtomicBool;
                    static I2_DONE: AtomicBool = AtomicBool::new(false);
                    if robot_os_config::CFG_MULTI_STREAM.load(Ordering::Relaxed)
                        && !I2_DONE.swap(true, Ordering::Relaxed)
                    {
                        i2_holdoff_probe(tcp_fd as usize);
                    }
                }
                let sent = send_framed(
                    tcp_fd as usize, &sp_frame[..sp_len], &mut link, &mut enc_salt,
                );
                // One-shot diagnostic on first sensor send attempt (qemu only).
                // Localises whether the sensor pump never reaches send (= task
                // stuck up-stream), or sends 0 bytes (= wrap/encrypt/TCP issue).
                #[cfg(feature = "qemu")]
                {
                    use core::sync::atomic::AtomicBool;
                    static SENSOR_LOGGED: AtomicBool = AtomicBool::new(false);
                    if !SENSOR_LOGGED.swap(true, Ordering::Relaxed) {
                        kprintln!("[BRAIN] first sensor send: frame={} wire={} enc={}",
                                  sp_len, sent, link.is_some() as u8);
                    }
                }
                if sent > 0 {
                    robot_os_behavior::remote_inc_sent();
                }

                // Send camera frame periodically (~2 Hz). Skipped on an
                // encrypted link: a camera frame is multi-KiB but the RFC-0019
                // AEAD frame caps at ENC_MAX_PAYLOAD (2048 B), so bulk video
                // would need a chunked/streamed path (future work, RFC-0021
                // multi-stream). Sending it plaintext here would also corrupt
                // the brain's uniform AEAD reader, so we drop it instead.
                camera_cycle += 1;
                // K-C5: `link.is_none()` alone fires precisely when there is
                // no AEAD session — the state the policy forbids. Under
                // `link-encrypt-enforced` this pump would keep pushing raw
                // camera bytes onto the socket while `wrap` refuses everything
                // else. `link_policy_denial()` is `None` always when the
                // policy is off (const-folds away) and never `None` while it
                // is on and unsatisfied.
                //
                // `!is_authenticated()` closes the OTHER reader: with
                // LINK.KEY loaded but no AEAD (HMAC-only mode, today's keyed
                // default) every other frame on this socket travels inside
                // the envelope, and the brain's authenticated reader does
                // `readexactly(ENVELOPE_OVERHEAD)` — a raw camera frame in
                // that stream is parsed as an envelope header, fails the MAC,
                // and desyncs/drops the connection. Same hazard the comment
                // above documents for the AEAD reader, extended to the
                // envelope reader. Raw camera frames are therefore
                // plaintext-mode only (no key at all).
                if camera_cycle >= CAMERA_SEND_INTERVAL
                    && robot_os_drivers::csi::csi_is_ready()
                    && link.is_none()
                    && !robot_os_behavior::auth_envelope::is_authenticated()
                    && robot_os_behavior::auth_envelope::link_policy_denial().is_none()
                    && !robot_os_config::CFG_MULTI_STREAM.load(Ordering::Relaxed)
                {
                    camera_cycle = 0;
                    let (cam_w, cam_h) = robot_os_drivers::csi::csi_resolution();
                    let frame_pixels = cam_w as usize * cam_h as usize;
                    // Camera payload: 5B header + raw pixels (max resolution 320×240).
                    // Buffers live in .bss (static) — putting ~150 KiB on a 16 KiB
                    // task stack page-faults. Single behavior task → no aliasing risk.
                    const CAM_BUF_SIZE: usize = 320 * 240 + CAMERA_HDR_SIZE + FRAME_OVERHEAD;
                    static mut CAM_PAYLOAD: [u8; CAM_BUF_SIZE] = [0u8; CAM_BUF_SIZE];
                    static mut CAM_FRAME:   [u8; CAM_BUF_SIZE] = [0u8; CAM_BUF_SIZE];
                    // SAFETY: only the behavior task references these statics.
                    let cam_payload: &mut [u8; CAM_BUF_SIZE] = unsafe { &mut *core::ptr::addr_of_mut!(CAM_PAYLOAD) };
                    let cam_frame:   &mut [u8; CAM_BUF_SIZE] = unsafe { &mut *core::ptr::addr_of_mut!(CAM_FRAME) };

                    let mut cam_hdr = [0u8; CAMERA_HDR_SIZE];
                    encode_camera_header(&mut cam_hdr, cam_w, cam_h, CAMERA_FMT_GRAY8);
                    cam_payload[..CAMERA_HDR_SIZE].copy_from_slice(&cam_hdr);
                    let captured = robot_os_drivers::csi::csi_capture(
                        &mut cam_payload[CAMERA_HDR_SIZE..CAMERA_HDR_SIZE + frame_pixels]
                    );
                    if captured > 0 {
                        let payload_len = CAMERA_HDR_SIZE + captured;
                        let total_len = payload_len + FRAME_OVERHEAD;
                        let cam_len = build_packet(
                            PKT_CAMERA,
                            &cam_payload[..payload_len],
                            &mut cam_frame[..total_len],
                        );
                        // #39 fix: camera frames are by far the largest packets
                        // (multi-KiB) — guaranteed to span many segments, so
                        // single-shot send_data here would drop ~95% of every
                        // frame.  send_all_with_yield is mandatory here.
                        let _cam_sent = robot_os_net::tcp::send_all_with_yield(
                            tcp_fd as usize,
                            &cam_frame[..cam_len],
                            robot_os_sched::task_yield,
                        );
                    }
                }

                // Check connection state
                let conn_state = robot_os_net::tcp::conn_state(tcp_fd as usize);
                if conn_state != robot_os_net::tcp::TcpState::Established {
                    tcp_connected = false;
                    tcp_fd = -1;
                    // Drop the encrypted channel — a reconnect performs a fresh
                    // RFC-0019 handshake with new ephemeral keys (forward secrecy).
                    link = None;
                    robot_os_behavior::remote_set_connected(false);
                    // Activate offline mode — patrol without brain
                    robot_os_behavior::offline::offline_activate();
                }

                // Receive ActuatorCmd (framed: up to 6 + 3 + 2*8 = 25 bytes).
                // When the brain↔kernel link is authenticated, frames are
                // wrapped in a 26-byte HMAC envelope (auth_envelope), so the
                // raw recv buffer must hold envelope + inner = 26 + 25 = 51 B.
                // When unkeyed, `unwrap` falls back to identity → same size.
                // Round up for headroom.
                // Sized for the deepest nesting: AEAD(46) + envelope(26) +
                // inner(64). When encrypted we decrypt the AEAD frame to the
                // HMAC envelope first, then unwrap that to the inner packet.
                const RECV_INNER_MAX: usize = 64;
                const RECV_ENV_MAX: usize = RECV_INNER_MAX
                    + robot_os_behavior::auth_envelope::ENVELOPE_OVERHEAD;
                const RECV_RAW_MAX: usize = RECV_ENV_MAX
                    + robot_os_behavior::encrypt_link::ENC_OVERHEAD
                    + robot_os_multi_stream::HEADER_LEN + 16;
                let mut raw_buf = [0u8; RECV_RAW_MAX];
                // Sized for several coalesced inner packets, not one: the drain
                // loop below concatenates every envelope it decodes.
                let mut recv_buf = [0u8; RECV_INNER_MAX * 4];
                let n_raw = robot_os_net::tcp::recv(tcp_fd as usize, &mut raw_buf);
                // Returns inner_len on success / identity-fallback (unkeyed);
                // 0 on decrypt failure, HMAC mismatch, replay, or size error.
                let n = if n_raw > 0 {
                    // RFC-0021 demux first (outermost): strip [stream_id][len]
                    // and keep only STREAM_CONTROL payloads for the brain path.
                    // Non-control streams (camera/lidar) are ignored here.
                    let ctrl: &[u8] = if robot_os_config::CFG_MULTI_STREAM.load(Ordering::Relaxed) {
                        match robot_os_multi_stream::unwrap(&raw_buf[..n_raw as usize]) {
                            Some((sid, _, payload))
                                if sid == robot_os_multi_stream::STREAM_CONTROL => payload,
                            _ => &[],
                        }
                    } else {
                        &raw_buf[..n_raw as usize]
                    };
                    if ctrl.is_empty() {
                        0
                    } else {
                        // Drain EVERY coalesced envelope, not just the first.
                        //
                        // K-C3/C4 fixed this for brain-protocol frames sharing
                        // one envelope; the loop below is the same fix one
                        // layer out, for envelopes sharing one recv(). TCP
                        // gives no reason for the brain's send() boundaries to
                        // survive as recv() boundaries, so two commands written
                        // separately routinely arrive together — and the old
                        // code decoded frame 1 and dropped the rest with no
                        // error and no log. An ESTOP behind any other command
                        // was silently lost, which on a robot is the one packet
                        // that must never be.
                        //
                        // Inner packets are concatenated into `recv_buf`; the
                        // existing K-C3/C4 parser below then walks them all,
                        // since the brain protocol is self-delimiting
                        // (MAGIC + len + CRC).
                        let mut off = 0usize;      // cursor into `ctrl`
                        let mut filled = 0usize;   // bytes written to recv_buf
                        let mut env = [0u8; RECV_ENV_MAX];
                        while off < ctrl.len() && filled < recv_buf.len() {
                            let rest = &ctrl[off..];
                            let (inner_len, eaten) = match link.as_ref() {
                                Some(l) => {
                                    // Outer AEAD → inner HMAC envelope.
                                    let (env_len, aead_eaten) =
                                        l.decrypt_consuming(rest, &mut env);
                                    if env_len == 0 { break; }
                                    match robot_os_behavior::auth_envelope::unwrap_consuming(
                                        &env[..env_len], &mut recv_buf[filled..],
                                    ) {
                                        Some((n, _)) => (n, aead_eaten),
                                        None => break,
                                    }
                                }
                                None => match robot_os_behavior::auth_envelope::unwrap_consuming(
                                    rest, &mut recv_buf[filled..],
                                ) {
                                    Some((n, eaten)) => (n, eaten),
                                    None => break,
                                },
                            };
                            // A zero-advance would spin forever on a malformed
                            // frame; treat it as end-of-data rather than
                            // trusting the decoder to always move.
                            if eaten == 0 { break; }
                            filled += inner_len;
                            off += eaten;
                        }
                        filled as i32
                    }
                } else {
                    n_raw
                };
                if n >= 6 {
                    let n = n as usize;
                    let mut cursor = 0usize;
                    // K-C3/C4: a single decoded `recv_buf` can hold several coalesced
                    // brain-protocol frames — the brain writes one frame per command,
                    // but nothing stops two or more (e.g. CONFIG + ACTUATOR + ESTOP)
                    // from landing in the same tick's recv()/decrypt cycle. Parsing
                    // only the frame at offset 0 silently dropped every frame after
                    // it, including an ESTOP. Loop consuming every complete frame,
                    // resyncing on the next MAGIC pair when one fails length/CRC
                    // (corrupt, or torn mid-frame across two separate recv() calls —
                    // reassembling raw bytes across ticks is a larger, envelope-layer
                    // change and is out of scope here).
                    while cursor < n {
                        let (pkt_type, rel_pay_start, pay_len, total) =
                            match parse_packet(&recv_buf[cursor..n]) {
                                Some(f) => f,
                                None => match recv_buf[cursor + 1..n]
                                    .windows(2)
                                    .position(|w| {
                                        w[0] == robot_os_behavior::brain_protocol::MAGIC[0]
                                            && w[1] == robot_os_behavior::brain_protocol::MAGIC[1]
                                    })
                                {
                                    Some(off) => { cursor += 1 + off; continue; }
                                    None => break,
                                },
                            };
                        robot_os_behavior::remote_inc_recv();
                        let pay_start = cursor + rel_pay_start;
                        cursor += total;
                        if pkt_type == PKT_ACTUATOR {
                            let payload = &recv_buf[pay_start..pay_start + pay_len];
                            if let Some(cmd) = decode_actuator_cmd(payload) {
                                // RFC-0035: record command confidence so the motor
                                // envelope can tighten the cap for low-confidence
                                // (e.g. reactive-LLM) commands.
                                robot_os_behavior::safety::cmd_set_low_confidence(
                                    cmd.is_low_confidence());
                                if cmd.is_emergency() {
                                    // Emergency stop — override all layers
                                    robot_os_robot::motor_cmd_publish(0, 0);
                                } else {
                                    // Route the remote command through the L0–L3
                                    // subsumption stack (`arbitrate()` later in
                                    // this loop) rather than publishing direct to
                                    // `CH_MOTOR_CMD`. The previous code did the
                                    // direct publish, so a remote ACTUATOR frame
                                    // could reach the motors that same tick even
                                    // if L1 (obstacle avoid) wanted to veto it.
                                    // ESTOP path above stays direct — that IS
                                    // the override-all-layers semantic.
                                    let (sl, sr) = cmd.diff_drive();
                                    // Feed into remote_action so L2 (vla/remote) picks it up.
                                    // `layer_remote_vla` expects milli-units (-1000..+1000)
                                    // and divides by 10 to get the percent commanded to the
                                    // motors. `diff_drive()` returns percent (-100..+100), so
                                    // scale ×10 here to honor that contract — without this
                                    // the L2 layer saw -10..+10 and the robot ran at 10% of
                                    // the commanded speed.
                                    const REMOTE_PCT_TO_MILLI: i32 = 10;
                                    let mut action = robot_os_behavior::last_action();
                                    action.cmd        = 1; // CMD_MOTOR
                                    action.actions[0] = (sl * REMOTE_PCT_TO_MILLI) as i16;
                                    action.actions[1] = (sr * REMOTE_PCT_TO_MILLI) as i16;
                                    action.received_at = now;
                                    action.valid       = true;
                                    robot_os_behavior::set_last_action(action);
                                    state.remote_action = action;
                                }
                            }
                        } else if pkt_type == PKT_PREDICT {
                            // RFC-0034 speculative actuation — capability v1: the
                            // brain→kernel predictive CHANNEL. Receive + decode +
                            // log the predicted next command (observable proof the
                            // channel works). Acting on it early (through the
                            // Fase-1 envelope, gated by SPECULATIVE_ACTUATION) is
                            // the HW-measured layer, deferred — see RFC-0034.
                            let payload = &recv_buf[pay_start..pay_start + pay_len];
                            if let Some(p) = robot_os_behavior::decode_predict_cmd(payload) {
                                let (pl, pr) = p.cmd.diff_drive();
                                kprintln!("[PREDICT] next l={} r={} conf={}", pl, pr, p.confidence);
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
                                    CFG_KEY_CAMERA => {
                                        if cfg.value == CAMERA_PWR_ON {
                                            robot_os_drivers::csi::csi_power_on();
                                        } else if cfg.value == CAMERA_PWR_OFF {
                                            robot_os_drivers::csi::csi_power_off();
                                        }
                                    },
                                    _ => {} // other config keys handled in future phases
                                }
                            }
                        } else if pkt_type == PKT_PAYLOAD {
                            // E04: payload command (spray / gripper / cam trigger)
                            let payload = &recv_buf[pay_start..pay_start + pay_len];
                            if let Some(cmd) = decode_payload_cmd(payload) {
                                robot_os_behavior::payload::payload_exec(cmd);
                            }
                        } else if pkt_type == PKT_ESTOP {
                            // Remote emergency stop — highest priority
                            robot_os_behavior::safety::estop_activate();
                            robot_os_robot::motor_stop(0);
                            robot_os_robot::motor_stop(1);
                            robot_os_drivers::esc::esc_disarm();
                            kprintln!("[BRAIN] ESTOP received — motors stopped");
                        } else if pkt_type == PKT_DEGRADE {
                            // RFC-0036: brain-triggered degraded mode. reason 0
                            // clears; any non-zero reason arms capability
                            // containment (user-task writes denied at the cap
                            // chokepoint; in-kernel safe-stop unaffected).
                            let payload = &recv_buf[pay_start..pay_start + pay_len];
                            let reason = payload.first().copied().unwrap_or(DEGRADE_CLEAR);
                            if reason == DEGRADE_CLEAR {
                                robot_os_ipc::cap::degraded_set(false);
                                kprintln!("[BRAIN] degraded mode cleared");
                            } else {
                                robot_os_ipc::cap::degraded_set(true);
                                kprintln!("[BRAIN] degraded mode armed — reason {}", reason);
                            }
                        } else if pkt_type == PKT_SEMANTIC_LEVEL {
                            // RFC-0037: graded degrade-level command. 1-byte
                            // payload = level index (0=FULL…3=CONTAINED). Missing
                            // payload → fail-closed (CONTAINED).
                            let payload = &recv_buf[pay_start..pay_start + pay_len];
                            let level = payload.first().copied()
                                .unwrap_or(robot_os_ipc::cap::DEGRADE_LEVEL_CONTAINED);
                            robot_os_ipc::cap::degrade_level_set(level);
                            kprintln!("[BRAIN] semantic level set to {}", level);
                        } else if pkt_type == PKT_MODE {
                            let payload = &recv_buf[pay_start..pay_start + pay_len];
                            if let Some(_mode) = decode_mode_cmd(payload) {
                                // Clear ESTOP on any MODE command (operator reset)
                                if robot_os_behavior::safety::estop_is_active() {
                                    robot_os_behavior::safety::estop_deactivate();
                                    kprintln!("[BRAIN] ESTOP cleared by MODE command");
                                }
                                // RFC-0036: a MODE command also clears degraded mode.
                                if robot_os_ipc::cap::degraded_active() {
                                    robot_os_ipc::cap::degraded_set(false);
                                    kprintln!("[BRAIN] degraded mode cleared by MODE command");
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
        if !tcp_connected && robot_os_drivers::uart_bridge::bridge_is_ready()
            && bridge_policy_permits()
        {
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

            // Send camera frame via UART (JPEG for bandwidth)
            camera_cycle += 1;
            if camera_cycle >= CAMERA_SEND_INTERVAL
                && robot_os_drivers::csi::csi_is_ready()
                && robot_os_drivers::csi::csi_is_powered()
            {
                camera_cycle = 0;
                let (cam_w, cam_h) = robot_os_drivers::csi::csi_resolution();

                // Buffers live in .bss (static) — putting ~58 KiB on a 16 KiB
                // task stack page-faults. Same fix, same reason, as the TCP
                // branch above; this branch was missed because it only runs
                // when `!tcp_connected && bridge_is_ready()`, which CI never
                // reaches, so the fault never showed up in test.
                //
                // The three arrays that used to be locals here are
                // JPEG_MAX_SIZE (19,200 B) + two UART_CAM_BUF_SIZE
                // (19,211 B each) ≈ 57.6 KiB, on a task whose 16 KiB stack
                // has a 4 KiB guard page — roughly 12 KiB usable. With
                // `panic = "abort"`, a guard-page hit is a board reset, i.e.
                // a physical-safety event, not a crash report.
                const UART_CAM_BUF_SIZE: usize =
                    robot_os_drivers::csi::JPEG_MAX_SIZE + CAMERA_HDR_SIZE + FRAME_OVERHEAD;
                static mut UART_JPEG_BUF:    [u8; robot_os_drivers::csi::JPEG_MAX_SIZE] =
                    [0u8; robot_os_drivers::csi::JPEG_MAX_SIZE];
                static mut UART_CAM_PAYLOAD: [u8; UART_CAM_BUF_SIZE] = [0u8; UART_CAM_BUF_SIZE];
                static mut UART_CAM_FRAME:   [u8; UART_CAM_BUF_SIZE] = [0u8; UART_CAM_BUF_SIZE];
                // SAFETY: only the behavior task references these statics, and
                // `behavior_task` is created exactly once (see the single
                // `task_create` for "behavior"), so no two contexts can hold
                // these borrows at the same time. Deliberately separate from
                // the TCP branch's CAM_PAYLOAD/CAM_FRAME: the two branches are
                // mutually exclusive per iteration, but sharing them would
                // couple two code paths for no gain and re-tangle a fix that
                // already landed.
                let jpeg_buf: &mut [u8; robot_os_drivers::csi::JPEG_MAX_SIZE] =
                    unsafe { &mut *core::ptr::addr_of_mut!(UART_JPEG_BUF) };
                let cam_payload: &mut [u8; UART_CAM_BUF_SIZE] =
                    unsafe { &mut *core::ptr::addr_of_mut!(UART_CAM_PAYLOAD) };
                let cam_frame: &mut [u8; UART_CAM_BUF_SIZE] =
                    unsafe { &mut *core::ptr::addr_of_mut!(UART_CAM_FRAME) };

                let jpeg_len = robot_os_drivers::csi::csi_capture_jpeg(&mut jpeg_buf[..]);
                if jpeg_len > 0 {
                    let mut cam_hdr = [0u8; CAMERA_HDR_SIZE];
                    encode_camera_header(&mut cam_hdr, cam_w, cam_h, CAMERA_FMT_JPEG);
                    let payload_len = CAMERA_HDR_SIZE + jpeg_len;
                    let total_len = payload_len + FRAME_OVERHEAD;
                    cam_payload[..CAMERA_HDR_SIZE].copy_from_slice(&cam_hdr);
                    cam_payload[CAMERA_HDR_SIZE..CAMERA_HDR_SIZE + jpeg_len]
                        .copy_from_slice(&jpeg_buf[..jpeg_len]);
                    let cam_len = build_packet(
                        PKT_CAMERA,
                        &cam_payload[..payload_len],
                        &mut cam_frame[..total_len],
                    );
                    robot_os_drivers::uart_bridge::bridge_send(&cam_frame[..cam_len]);
                }
            }

            // Receive ActuatorCmd from UART1
            let mut recv_buf = [0u8; 32];
            let n = robot_os_drivers::uart_bridge::bridge_recv(&mut recv_buf);
            if n >= 6 {
                let n = n as usize;
                let mut cursor = 0usize;
                // K-C3/C4: same coalesced-frame issue as the TCP path above — a
                // single bridge_recv() can return several concatenated frames
                // (e.g. CONFIG + ACTUATOR + ESTOP); consume every complete frame,
                // resyncing on the next MAGIC pair when one fails length/CRC.
                while cursor < n {
                    let (pkt_type, rel_pay_start, pay_len, total) =
                        match parse_packet(&recv_buf[cursor..n]) {
                            Some(f) => f,
                            None => match recv_buf[cursor + 1..n]
                                .windows(2)
                                .position(|w| {
                                    w[0] == robot_os_behavior::brain_protocol::MAGIC[0]
                                        && w[1] == robot_os_behavior::brain_protocol::MAGIC[1]
                                })
                            {
                                Some(off) => { cursor += 1 + off; continue; }
                                None => break,
                            },
                        };
                    robot_os_behavior::remote_inc_recv();
                    let pay_start = cursor + rel_pay_start;
                    cursor += total;
                    if pkt_type == PKT_ACTUATOR {
                        let payload = &recv_buf[pay_start..pay_start + pay_len];
                        if let Some(cmd) = decode_actuator_cmd(payload) {
                            // RFC-0035: record command confidence (UART path).
                            robot_os_behavior::safety::cmd_set_low_confidence(
                                cmd.is_low_confidence());
                            if cmd.is_emergency() {
                                robot_os_robot::motor_cmd_publish(0, 0);
                            } else {
                                // Match the TCP path: feed `remote_action` only,
                                // letting `arbitrate()` (L0–L3) be the sole writer
                                // to `CH_MOTOR_CMD`. The previous direct publish
                                // here let UART traffic bypass L0/L1 safety —
                                // exactly the bug the TCP path was just fixed for.
                                // ESTOP above stays direct (override-all semantic).
                                let (sl, sr) = cmd.diff_drive();
                                // Scale percent → milli-units to honour the
                                // `layer_remote_vla` contract (it /10's actions[]
                                // back to percent).
                                const REMOTE_PCT_TO_MILLI: i32 = 10;
                                let mut action = robot_os_behavior::last_action();
                                action.cmd        = 1;
                                action.actions[0] = (sl * REMOTE_PCT_TO_MILLI) as i16;
                                action.actions[1] = (sr * REMOTE_PCT_TO_MILLI) as i16;
                                action.received_at = now;
                                action.valid       = true;
                                robot_os_behavior::set_last_action(action);
                                state.remote_action = action;
                            }
                        }
                    } else if pkt_type == PKT_PREDICT {
                        // RFC-0034 speculative actuation — capability v1 (UART
                        // path): receive + decode + log the predictive channel.
                        // Early-apply is HW-deferred (see RFC-0034).
                        let payload = &recv_buf[pay_start..pay_start + pay_len];
                        if let Some(p) = robot_os_behavior::decode_predict_cmd(payload) {
                            let (pl, pr) = p.cmd.diff_drive();
                            kprintln!("[PREDICT] next l={} r={} conf={}", pl, pr, p.confidence);
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
                                CFG_KEY_CAMERA => {
                                    if cfg.value == CAMERA_PWR_ON {
                                        robot_os_drivers::csi::csi_power_on();
                                    } else if cfg.value == CAMERA_PWR_OFF {
                                        robot_os_drivers::csi::csi_power_off();
                                    }
                                },
                                _ => {}
                            }
                        }
                    } else if pkt_type == PKT_PAYLOAD {
                        // E04: payload command via UART bridge
                        let payload = &recv_buf[pay_start..pay_start + pay_len];
                        if let Some(cmd) = decode_payload_cmd(payload) {
                            robot_os_behavior::payload::payload_exec(cmd);
                        }
                    } else if pkt_type == PKT_ESTOP {
                        robot_os_behavior::safety::estop_activate();
                        robot_os_robot::motor_stop(0);
                        robot_os_robot::motor_stop(1);
                        robot_os_drivers::esc::esc_disarm();
                        kprintln!("[BRAIN] ESTOP received (UART) — motors stopped");
                    } else if pkt_type == PKT_DEGRADE {
                        // RFC-0036: brain-triggered degraded mode (UART bridge).
                        let payload = &recv_buf[pay_start..pay_start + pay_len];
                        let reason = payload.first().copied().unwrap_or(DEGRADE_CLEAR);
                        if reason == DEGRADE_CLEAR {
                            robot_os_ipc::cap::degraded_set(false);
                            kprintln!("[BRAIN] degraded mode cleared (UART)");
                        } else {
                            robot_os_ipc::cap::degraded_set(true);
                            kprintln!("[BRAIN] degraded mode armed (UART) — reason {}", reason);
                        }
                    } else if pkt_type == PKT_SEMANTIC_LEVEL {
                        // RFC-0037: graded degrade-level command (UART bridge).
                        // 1-byte payload = level index (0=FULL…3=CONTAINED).
                        // Missing payload → fail-closed (CONTAINED).
                        let payload = &recv_buf[pay_start..pay_start + pay_len];
                        let level = payload.first().copied()
                            .unwrap_or(robot_os_ipc::cap::DEGRADE_LEVEL_CONTAINED);
                        robot_os_ipc::cap::degrade_level_set(level);
                        kprintln!("[BRAIN] semantic level set to {} (UART)", level);
                    } else if pkt_type == PKT_MODE {
                        let payload = &recv_buf[pay_start..pay_start + pay_len];
                        if let Some(_mode) = decode_mode_cmd(payload) {
                            if robot_os_behavior::safety::estop_is_active() {
                                robot_os_behavior::safety::estop_deactivate();
                                kprintln!("[BRAIN] ESTOP cleared by MODE command (UART)");
                            }
                            // RFC-0036: MODE also clears degraded mode.
                            if robot_os_ipc::cap::degraded_active() {
                                robot_os_ipc::cap::degraded_set(false);
                                kprintln!("[BRAIN] degraded mode cleared by MODE command (UART)");
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
                                        state.odom_dist_mm, state.odom_heading_cdeg);
        }

        // RFC-0027 I1: periodic WCET auto-report (see WCET_AUTOREPORT_INTERVAL_SEC
        // declaration above for rationale).
        #[cfg(feature = "qemu")]
        {
            let now_t = robot_os_drivers::clint::get_time();
            if now_t >= wcet_autoreport_deadline {
                wcet_autoreport_deadline = now_t
                    + WCET_AUTOREPORT_INTERVAL_SEC * robot_os_drivers::clint::TIMER_FREQ;
                robot_os_drivers::wcet::wcet_report();
                robot_os_drivers::wcet::jitter_report();
            }
            // One-shot synthetic bench run.  See bench_run_all_done
            // declaration for rationale.
            if !bench_run_all_done {
                bench_run_all_done = true;
                robot_os_bench::run_all(BENCH_RUN_ALL_ITERS);
                // K-C5: the auth bench calls `wrap` unkeyed, and under
                // `link-encrypt-enforced` that burns the one-shot (tx, NoKey)
                // announcement slot — the first REAL denial would then print
                // nothing. Re-arm after the synthetic sweep.
                robot_os_behavior::auth_envelope::reset_denial_announcements();
            }
        }

        // ── Sleep 100ms using IO-wait (not busy yield) ─────────────────
        let sleep_deadline = robot_os_drivers::clint::get_time()
            + robot_os_drivers::clint::TIMER_FREQ / 10;
        robot_os_sched::task_block(robot_os_sched::WaitReason::Timer(sleep_deadline));
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
            (target.yaw_rate_mdps as i64 - imu.gyro_mdps[2] as i64)
                .clamp(i32::MIN as i64, i32::MAX as i64) as i32,
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
// ── I-13: transactional control ticks (RFC-0029) ─────────────────────────────
// A recoverable fault inside an *armed* control tick is rolled back — the
// trap handler restarts `rt_motor_task` at its entry (a known PC + a valid
// saved SP) after a motor safe-stop, instead of taking the fatal path that
// halts the kernel. Restarting at a function ENTRY (not mid-tick) is safe
// without hand-rolled setjmp/longjmp: the prologue rebuilds the frame and the
// task never returns. `is_recoverable` is a conservative whitelist — it does
// NOT include ecall or page-fault causes (those keep their existing handlers).
const MAX_TXN_HARTS: usize = 8;
struct TxnSlot {
    armed: core::sync::atomic::AtomicBool,
    sp:    core::sync::atomic::AtomicUsize,
    pc:    core::sync::atomic::AtomicUsize,
}
impl TxnSlot {
    const fn new() -> Self {
        Self {
            armed: core::sync::atomic::AtomicBool::new(false),
            sp:    core::sync::atomic::AtomicUsize::new(0),
            pc:    core::sync::atomic::AtomicUsize::new(0),
        }
    }
}
static TXN: [TxnSlot; MAX_TXN_HARTS] = [const { TxnSlot::new() }; MAX_TXN_HARTS];
/// Lifetime count of transactional rollbacks (observability).
static TXN_ABORTS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Consecutive transactional restarts with no completed control tick in
/// between. A *deterministic* recoverable fault would otherwise restart the
/// control task forever — the task re-arms at its entry before the fault
/// recurs, so disarming on rollback does NOT break the loop. This counter does:
/// once it reaches `MAX_TXN_RESTARTS`, `txn_try_rollback` declines and the
/// fault falls through to the fatal path, surfacing the real bug instead of
/// silently looping. A completed tick clears it (`txn_note_tick_complete`), so
/// genuine one-off transients never accumulate toward the budget.
static TXN_RESTART_STREAK: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
/// Max consecutive restarts (no tick completed) before escalating to fatal.
const MAX_TXN_RESTARTS: u32 = 8;

/// Arm the per-hart transactional checkpoint: a recoverable fault until the
/// next disarm restarts at `(sp, pc)`.
fn txn_arm(sp: usize, pc: usize) {
    if !robot_os_limits::CONTROL_TXN_TICKS {
        return;
    }
    let h = robot_os_arch::cpu::hart_id() as usize;
    if h < MAX_TXN_HARTS {
        TXN[h].sp.store(sp, Ordering::Relaxed);
        TXN[h].pc.store(pc, Ordering::Relaxed);
        TXN[h].armed.store(true, Ordering::Relaxed);
    }
}

/// Conservative whitelist of faults a transactional tick may roll back:
/// **misaligned load/store only** — a plausibly transient "bad external data
/// alignment in the control loop" (e.g. an unaligned field from a sensor
/// frame). Illegal-instruction is deliberately **excluded**: it is almost
/// always a genuine defect (corrupt code, a bad function pointer), not a
/// recoverable transient, so it must surface via the fatal path rather than be
/// silently rolled back (RFC-0029 kill criterion: "false rollbacks mask real
/// bugs"). Page faults (demand paging) and ecall keep their own handlers.
fn txn_is_recoverable(cause: usize) -> bool {
    matches!(
        cause,
        trap::TRAP_LOAD_MISALIGNED | trap::TRAP_STORE_MISALIGNED
    )
}

/// Trap-handler hook: if a recoverable fault hit an armed tick on this hart,
/// safe-stop the motors and rewrite the trap frame to restart the control
/// task at its entry. Returns true if it handled (rolled back) the fault.
///
/// LOAD-BEARING PRECONDITION (RFC-0017 cert review): the restart resets SP to
/// the stack top and re-enters at the task entry, so **the abandoned stack's
/// destructors never run** — any RAII guard / blocking lock held at fault time
/// leaks. The control tick *does* hold SpinLocks (`motor_pid::TICK_STATE`,
/// `PID_CONTROLLERS`) across its computation, so a whitelisted fault taken
/// inside such a region would deadlock the next acquire. This is safe **only**
/// because the whitelist is misaligned-load/store-only AND the tick body
/// touches exclusively aligned, typed data (no raw unaligned access) — so no
/// whitelisted fault is reachable inside a locked region. Broadening the
/// whitelist (illegal-instr, page faults) OR adding a raw unaligned access to
/// the tick re-opens this; both require re-auditing the lock discipline here.
fn txn_try_rollback(frame: &mut TrapFrame, cause: usize) -> bool {
    if !robot_os_limits::CONTROL_TXN_TICKS {
        return false; // const-eliminated when off → trap handler unchanged
    }
    if !txn_is_recoverable(cause) {
        return false;
    }
    let h = robot_os_arch::cpu::hart_id() as usize;
    if h >= MAX_TXN_HARTS || !TXN[h].armed.load(Ordering::Relaxed) {
        return false;
    }
    // Abort budget: a deterministic fault re-arms at entry and recurs, so the
    // disarm below cannot break the loop on its own. If we have restarted
    // MAX_TXN_RESTARTS times without a single completed tick, treat the fault
    // as a real (non-transient) bug and decline — the caller falls through to
    // the fatal path, surfacing it instead of looping forever.
    if TXN_RESTART_STREAK.load(Ordering::Relaxed) >= MAX_TXN_RESTARTS {
        return false;
    }
    TXN[h].armed.store(false, Ordering::Relaxed);
    TXN_RESTART_STREAK.fetch_add(1, Ordering::Relaxed);
    TXN_ABORTS.fetch_add(1, Ordering::Relaxed);
    // Safe-stop so no half-applied motor command survives the rollback.
    // (Channel is a SeqLock — re-entrant publish from trap context never
    // blocks; worst case a concurrent reader retries one snapshot.)
    robot_os_robot::motor_cmd_publish(0, 0);
    // Restart the control task at its entry with a CLEAN stack top (reset SP),
    // NOT the mid-function SP that was live at fault time. The entry prologue
    // then runs exactly once per restart, so the stack does not descend a
    // frame on every rollback (the bug a post-prologue capture introduced).
    frame.regs[2] = TXN[h].sp.load(Ordering::Relaxed) as _;
    frame.sepc    = TXN[h].pc.load(Ordering::Relaxed) as _;
    true
}

/// Clear the transactional restart streak — called at the end of every
/// completed control tick. A completed tick proves forward progress, so the
/// abort budget (`MAX_TXN_RESTARTS`) only ever counts restarts that made *no*
/// progress (a deterministic fault), never lifetime one-off transients.
#[inline]
fn txn_note_tick_complete() {
    if !robot_os_limits::CONTROL_TXN_TICKS {
        return;
    }
    // Read-mostly: the no-fault steady state has streak == 0, so the common
    // path is a single relaxed *load* with no store — the hot tick is left
    // effectively unchanged (no cache-line dirtying). Only a tick that follows
    // a rollback writes.
    if TXN_RESTART_STREAK.load(Ordering::Relaxed) != 0 {
        TXN_RESTART_STREAK.store(0, Ordering::Relaxed);
    }
}

fn rt_motor_task(_: usize) {
    // I-13: arm the transactional checkpoint so a recoverable (misaligned)
    // fault mid-tick restarts here — safe-stop + continue — instead of halting
    // the kernel. The reset SP is the task's CLEAN stack top (pre-prologue),
    // not a mid-function SP, so each restart runs the entry prologue exactly
    // once and the stack does not leak a frame per rollback. Re-runs on every
    // (re)entry; the abort budget (MAX_TXN_RESTARTS) bounds deterministic faults.
    txn_arm(
        robot_os_sched::current_task_stack_top(),
        rt_motor_task as fn(usize) as usize,
    );
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
            let mut cmd = robot_os_robot::motor_cmd_read();
            if robot_os_robot::CH_MOTOR_CMD.is_valid() {
                // RFC-0033: bounded runtime safety monitor — the LAST line of
                // defence at the single MotorCmd→PWM chokepoint. Enforces hard
                // ESTOP + per-robot-type speed cap on the command MAGNITUDE
                // (the sensor-reactive L0 upstream does not). Structurally
                // unbypassable: every command source funnels through here.
                let (env_l, env_r) = robot_os_behavior::safety::motor_envelope(
                    cmd.speed_l, cmd.speed_r);
                cmd.speed_l = env_l;
                cmd.speed_r = env_r;

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

        // K-A1: a completed iteration proves the control task is alive; the
        // timer ISR feeds the hardware WDT only while this advances.
        CONTROL_HEARTBEAT.fetch_add(1, Ordering::Relaxed);

        // I-13: a completed tick proves forward progress — clear the restart
        // streak so the abort budget only counts consecutive no-progress
        // restarts (a deterministic fault), not lifetime one-off transients.
        txn_note_tick_complete();
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
    let mut boot_good_marked = false;
    let boot_start_tick = TICK_COUNT.load(Ordering::Relaxed);

    // RFC-0027 I1: periodic auto-report of WCET stats so the bench harness
    // can collect per-function samples even when shell-injection of the
    // `wcet` command fails (UART IRQ on a different hart under SMP TCG).
    // Only enabled under `feature = "qemu"` — on real hardware the shell
    // works reliably and the operator can dump on demand.
    //
    // WDT iterates every 500 ms (see WDT_CHECK_INTERVAL_DIV).  Report every
    // 60 iterations ≈ 30 s — long enough not to spam UART, short enough
    // that a 40 s bench scenario sees at least one report.
    // RFC-0027 I1: auto-report originally lived here in `system_wdt_task`, but
    // empirical observation (2026-05-29 bench) showed sys-wdt never reaches its
    // loop body under QEMU TCG — only the early-boot "[WDT] crash counter reset"
    // line emits, then nothing.  Moved the auto-report to `behavior_task` which
    // demonstrably runs (visible via [BRAIN] log entries throughout bench).
    // sys-wdt liveness is a separate preexisting issue, tracked separately.

    loop {
        // Spin-yield until ~500 ms of *real* time has elapsed, measured via the
        // mtime counter (which keeps advancing even if the timer *interrupt* is
        // frozen — exactly the failure this watchdog must still detect, so we
        // must NOT block on a Timer deadline here). The old loop did
        // `for _ in 0..sched_hz/2 { yield }`, but yields are not time delays —
        // they return near-instantly, so the liveness check below ran thousands
        // of times per second and mis-read the tickless timer's sparse
        // TICK_COUNT advances as false "stalls" (flooding the log). Gating on a
        // real mtime interval makes a non-advancing TICK_COUNT mean what it
        // should: the timer ISR genuinely stopped firing.
        const WDT_CHECK_INTERVAL_DIV: u64 = 2; // TIMER_FREQ / 2 = 500 ms
        let check_interval = robot_os_drivers::clint::TIMER_FREQ / WDT_CHECK_INTERVAL_DIV;
        let check_start = robot_os_drivers::clint::get_time();
        while robot_os_drivers::clint::get_time().wrapping_sub(check_start) < check_interval {
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
        // Threshold: 3 stalls on hardware (where 1.5 s of frozen timer is
        // unambiguously catastrophic) but much higher on QEMU TCG — the
        // 4-SMP TCG translator can wall-stall multiple consecutive
        // yields without any real timer fault (the E2E wheeled run was
        // tripping SAFE STOP every other cycle on a healthy kernel).
        // The SAFE STOP itself is also a no-op on QEMU (no real motors)
        // so the only effect of a false positive is log noise + motor
        // calls that go nowhere — but the cascade hides the real boot
        // log, which IS the problem we hit.
        #[cfg(feature = "qemu")]
        const WDT_FROZEN_THRESHOLD: u32 = 200;
        #[cfg(not(feature = "qemu"))]
        const WDT_FROZEN_THRESHOLD: u32 = 3;

        let now_tick = TICK_COUNT.load(Ordering::Relaxed);
        if now_tick == last_tick {
            frozen_count += 1;
            // Only log the first stall and the SAFE STOP — flooding the
            // UART with one line per polled-but-not-advanced check costs
            // ~milliseconds per line and *worsens* the apparent stall.
            if frozen_count == 1 {
                kprintln!("[WDT] Timer stall starting (tick_count={})", now_tick);
            }
            if frozen_count == WDT_FROZEN_THRESHOLD {
                kprintln!("[WDT] Timer FROZEN after {} stalls — SAFE STOP", frozen_count);
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

        // ── 3. GPIO kill-switch poll ───────────────────────────────────────
        let estop_pin = robot_os_config::CFG_ESTOP_GPIO_PIN.load(Ordering::Relaxed);
        if estop_pin < 64 && robot_os_drivers::gpio::gpio_read(estop_pin) == 0 {
            // Active-low: pin grounded = ESTOP triggered
            kprintln!("[WDT] GPIO ESTOP (pin {}) — motors stopped", estop_pin);
            robot_os_robot::motor_stop(0);
            robot_os_robot::motor_stop(1);
            robot_os_drivers::esc::esc_disarm();
        }

        // ── 4. Driver health check (AQ2) ────────────────────────────────
        // Detect stalled drivers (no heartbeat) and trigger auto-restart.
        // `driver_check_health` compares against `last_heartbeat` which is
        // stored in *milliseconds* (set by `SYS_DRV_HEARTBEAT`), so convert
        // the raw mtime counter to ms before passing. Was previously called
        // with raw ticks — latent today (no heartbeating drivers) but would
        // have crash-looped every registered driver the moment one shipped.
        let now_ms = robot_os_drivers::clint::get_time()
            / (robot_os_drivers::clint::TIMER_FREQ / 1000);
        robot_os_sched::driver_check_health(now_ms);

        // ── 5. OTA boot-good mark ────────────────────────────────────────
        // After OTA_BOOT_GOOD_DELAY_S of successful uptime, mark boot as good.
        if !boot_good_marked {
            let elapsed_ticks = now_tick.wrapping_sub(boot_start_tick) as u64;
            let sched_hz = robot_os_drivers::clint::sched_hz_get();
            let elapsed_s = if sched_hz > 0 { elapsed_ticks / sched_hz } else { 0 };
            if elapsed_s >= robot_os_ota::OTA_BOOT_GOOD_DELAY_S as u64 {
                robot_os_ota::ota_mark_boot_good();
                boot_good_marked = true;
            }
        }

    }
}

/// Post-mortem line naming the null guard when the faulting VA is inside it.
///
/// Only ever called from a fault that is already fatal for the task, so the
/// UART cost is irrelevant here. It exists because "Store page fault at 0x8"
/// on its own does not tell the reader that the kernel *refused* to fix it:
/// without this line the guard looks like an ordinary unmapped page, and the
/// next person to debug a null dereference re-derives why demand paging did
/// not kick in. See `robot_os_mm::vmm::USER_GUARD_LIMIT`.
#[inline]
fn page_fault_note_guard(stval: usize) {
    if robot_os_mm::vmm::in_null_guard(stval) {
        kprintln!("  null guard: VA < {:#x} is never mapped (null pointer dereference)",
            robot_os_mm::vmm::USER_GUARD_LIMIT);
    }
}

/// Post-mortem line with the faults this system resolved *silently* before
/// this one. This is where the counters bumped on the fast path are consulted
/// — the page-fault arm no longer prints anything for a fault it fixes, so
/// this line is what preserves the "how much COW traffic was there" signal
/// that the old per-fault banner used to carry, at zero cost per fault.
#[inline]
fn page_fault_note_resolved() {
    let (cow, demand) = robot_os_mm::vmm::faults_resolved();
    kprintln!("  resolved so far: {} COW, {} demand", cow, demand);
}

/// Trap handler called from trap_entry.S.
///
/// Returns the SATP value to load before SRET (0 = no page-table switch).
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
            // F16.2: measure timer ISR handler latency
            let _isr_start = robot_os_drivers::wcet::wcet_begin();
            // F16.4: record jitter between successive timer ISR fires
            robot_os_drivers::wcet::jitter_record(robot_os_drivers::wcet::JITTER_TIMER_ISR);

            // E2E-WCET diagnostic (qemu only): probe which sub-step
            // of the ISR is responsible for the multi-hundred-ms
            // ISR wall times we see under E2E load. Wrapped in
            // `#[cfg(feature = "qemu")]` so production builds pay
            // nothing.
            #[cfg(feature = "qemu")]
            macro_rules! probe {
                ($label:literal, $body:expr) => {{
                    let t0 = robot_os_drivers::clint::get_time();
                    let r = $body;
                    let elapsed_us = (robot_os_drivers::clint::get_time()
                        .wrapping_sub(t0))
                        * 1_000_000 / robot_os_drivers::clint::TIMER_FREQ;
                    if elapsed_us > 1_000 {
                        // Only print >1 ms so we don't flood the log.
                        kprintln!(
                            "[ISR-WCET] {} took {} µs",
                            $label, elapsed_us
                        );
                    }
                    r
                }};
            }
            #[cfg(not(feature = "qemu"))]
            macro_rules! probe {
                ($label:literal, $body:expr) => {{ let _ = $label; $body }};
            }

            let _ticks = TICK_COUNT.fetch_add(1, Ordering::Relaxed) as u64 + 1;
            #[cfg(not(feature = "no-mmu"))]
            let ticks = _ticks;

            // If any hart has panicked, halt this one: bring our actuators to a
            // safe state and wfi-loop without kicking the WDT or scheduling, so
            // the board resets cleanly instead of limping on in a bad state.
            if robot_os_common::is_panicked() {
                robot_os_robot::motor_stop_panic(0);
                robot_os_robot::motor_stop_panic(1);
                robot_os_drivers::esc::esc_disarm_panic();
                loop { robot_os_arch::cpu::wfi(); }
            }

            // K-A1: feed the hardware WDT from hart 0 only, and only while the
            // RT control task is advancing. Kicking unconditionally would keep
            // the board alive even if the control task hung with the motors at
            // their last command; gating on its heartbeat lets the HW WDT reset
            // instead. `hb == 0` keeps feeding until the task first runs (no
            // boot-time false reset); after that a stall beyond the grace window
            // stops the kicks. (WDT is a no-op on QEMU; real effect on VF2/K1.)
            if robot_os_arch::cpu::hart_id() == 0 {
                let hb = CONTROL_HEARTBEAT.load(Ordering::Relaxed);
                if hb != WDT_LAST_HEARTBEAT.load(Ordering::Relaxed) {
                    WDT_LAST_HEARTBEAT.store(hb, Ordering::Relaxed);
                    WDT_STALL_TICKS.store(0, Ordering::Relaxed);
                    robot_os_drivers::wdt::wdt_kick();
                } else if hb == 0
                    || WDT_STALL_TICKS.fetch_add(1, Ordering::Relaxed) + 1 <= WDT_CONTROL_STALL_LIMIT
                {
                    robot_os_drivers::wdt::wdt_kick();
                }
                // else: control task stalled after running — stop feeding the WDT.
            }

            // AQ0: Wake tasks whose timer deadline has expired.
            let now = robot_os_drivers::clint::get_time();

            // M01: Update vDSO timing page (no ecall needed from user-space).
            #[cfg(not(feature = "no-mmu"))]
            {
                let uptime_ms = now / (robot_os_drivers::clint::TIMER_FREQ / 1000);
                probe!("vdso_update", robot_os_mm::vdso::vdso_update(ticks, uptime_ms));
            }
            probe!("wake_expired_timers", robot_os_sched::wake_expired_timers(now));
            // K-C25: deliver wakes stamped onto a task that then parked
            // (Blocked + WAKE_STAMP + context saved) — one-shot wakes have no
            // second chance, so the tick is their consumer of last resort.
            // See `sched_word::reap_orphaned_stamp` for the measured wedge.
            probe!("reap_stamped_sleepers", robot_os_sched::reap_stamped_sleepers());

            // M04: Expire leases whose deadline has passed.
            //
            // `iter().take(n)`, NOT `&expired[..n]`: `n` crosses an opaque
            // crate boundary (`lto = false`), so LLVM cannot discharge the
            // slice-range check and would emit a panic path — inside the
            // timer ISR, under `panic = "abort"`.
            probe!("lease_tick", {
                let mut expired = [0u32; robot_os_ipc::MAX_LEASES];
                let n = robot_os_ipc::lease_tick(now, &mut expired);
                for &lessor_tid in expired.iter().take(n) {
                    // Wake a lessor blocked in lease_wait_return (WaitQueue)
                    // so it observes the Expired state and undoes any
                    // inherited priority boost (RFC-0031 — no lost restore on
                    // expiry). Also wake the legacy FastIpcServer waiters.
                    robot_os_sched::wq_wake_by_tid(lessor_tid);
                    robot_os_sched::wake_fast_ipc_server(lessor_tid);
                }
            });

            // M03: Schedule next timer at the nearest deadline (tickless).
            // Falls back to periodic tick if no tasks are sleeping on a timer.
            let hart = robot_os_arch::cpu::hart_id();
            robot_os_drivers::clint::set_next_tick_smart(
                hart as u32,
                robot_os_sched::nearest_timer_deadline(),
            );

            // AQ8: Trace timer tick.
            robot_os_ipc::trace_event(robot_os_ipc::TRACE_IRQ,
                cause as u32, hart as u32, 0, 0);

            // F16.1: record timer ISR execution time BEFORE schedule().
            // schedule() may context-switch us out and back later — measuring
            // after it includes wall time of unrelated tasks and reports
            // bogus "1.2 second ISRs". The ISR's own work is the only
            // meaningful WCET data point here.
            robot_os_drivers::wcet::wcet_end(
                robot_os_drivers::wcet::WCET_TIMER_ISR, _isr_start);

            // Let the scheduler preempt if the current task's time slice expired.
            robot_os_sched::schedule();
        }
        INT_EXTERNAL_S => {
            {
                let hart = robot_os_arch::cpu::hart_id();
                let irq = robot_os_drivers::plic::claim(hart as u32);
                if irq != 0 {
                    // AQ8: Trace external IRQ.
                    robot_os_ipc::trace_event(robot_os_ipc::TRACE_IRQ,
                        irq, hart as u32, 0, 0);

                    if irq == robot_os_drivers::uart::UART_IRQ {
                        robot_os_drivers::uart::irq_handler();
                    }

                    // AQ0: Wake tasks blocked on this IRQ.
                    robot_os_sched::wake_by_irq(irq);

                    // F00.3: Dispatch to userspace IRQ bindings (ports, etc.)
                    robot_os_ipc::irq_dispatch(irq);

                    robot_os_drivers::plic::complete(hart as u32, irq);
                }
            }
        }
        INT_SOFTWARE_S => {
            // IPI received. Today the only sender is (K-C15)
            // `cpu_enqueue_locked`, ringing a hart that has just been given a
            // ready task.
            //
            // **There is no TLB shootdown sender in this tree.** This arm used
            // to run `csr::sfence_vma()` unconditionally, for a shootdown IPI
            // that nothing ever sent — `send_ipi` had zero callers anywhere
            // until K-C15 added the wake doorbell below, so the flush was dead
            // code guarding a message that never arrived.
            //
            // Leaving it in place once the doorbell exists is not conservative,
            // it is expensive: it puts a full `sfence.vma zero, zero` on the
            // path of *every* cross-CPU wake. Measured under QEMU TCG — where a
            // global flush discards the whole softMMU TLB — it dominated the
            // very latency the doorbell was added to remove.
            //
            // Whoever adds a real shootdown must NOT restore the flush here.
            // Give the IPI a per-hart reason flag and flush only when the
            // shootdown bit is set; otherwise every wake pays for it again.
            // Clear S-mode software interrupt pending (SIP.SSIP = bit 1)
            // BEFORE rescheduling: schedule() may context-switch away and not
            // return here for a long time, and leaving SSIP set would re-enter
            // this arm immediately on the way out.
            csr::clear_sip_ssip();
            // K-C15: the whole point of the doorbell. Without this the hart
            // wakes from `wfi()`, finds nothing has asked it to do anything,
            // and goes straight back to sleep with a runnable task sitting in
            // its own queue.
            robot_os_sched::schedule();
        }
        _ => {
            // Avoid kprintln from ISR — it acquires the UART spinlock and
            // can block the ISR for ms when worker tasks hold the lock.
            // Record to trace ring; userspace dumps it later.
            robot_os_ipc::trace_event(robot_os_ipc::TRACE_IRQ,
                cause as u32, u32::MAX, 0, 0);
        }
    }
}

/// Handle synchronous exceptions.
///
/// Returns the SATP to switch to on SRET (0 = keep current page table).
fn handle_exception(frame: &mut TrapFrame, cause: usize) -> usize {
    // I-13 (RFC-0029): transactional control-tick rollback. A recoverable
    // fault inside an armed tick restarts the control task at a safe-stop
    // instead of the fatal path below. Excludes ecall + page-fault causes
    // (txn_is_recoverable whitelist), so their handlers run unchanged.
    if txn_try_rollback(frame, cause) {
        return 0;
    }
    match cause {
        // ── System calls (ecall from U-mode or S-mode) ────────────────────
        TRAP_ECALL_FROM_U | TRAP_ECALL_FROM_S => {
            let num = frame.regs[17]; // a7 = syscall number
            // Snapshot before dispatch: `frame.regs[10]` is overwritten with the
            // return value below, and a forked child must inherit the register
            // file as it was at the `ecall`.
            let reg_snapshot: [u64; 32] = core::array::from_fn(|i| frame.regs[i] as u64);

            // AQ8: Trace syscall entry.
            robot_os_ipc::trace_syscall(num as u32, 0);

            // K-A15: sepc/user_sp passed straight through as call parameters
            // (this trap frame's own values, hart-local) instead of via the
            // shared-global `set_ecall_context` this replaced — see the doc
            // on `syscall_dispatch` for why that mattered for SYS_FORK.
            // Extra return registers (fast-IPC payload delivery).
            //
            // **WHY an out-parameter and not `&mut frame.regs`.** `reg_snapshot`
            // above is a *copy* — it has to be, because `frame.regs[10]` is
            // overwritten with the return value below and a forked child must
            // inherit the register file as it was at the `ecall`. So an arm
            // cannot reach this frame through it. Arms that return more than
            // `a0` fill `out`, and the copy back happens here.
            let mut out = robot_os_syscall::SyscallOut::new();
            let result = robot_os_syscall::syscall_dispatch_out(
                num as u64,
                frame.regs[10] as u64, frame.regs[11] as u64, frame.regs[12] as u64,
                frame.regs[13] as u64, frame.regs[14] as u64, frame.regs[15] as u64,
                frame.sepc as u64, frame.regs[2] as u64,
                // K-C11: the parent's whole user register file. SYS_FORK is the
                // only consumer — the child has to resume the parent's code with
                // its callee-saved registers intact, not with whatever the kernel
                // task that dispatched it happened to leave behind.
                &reg_snapshot,
                &mut out,
            );
            frame.regs[10] = result as _; // return value in a0
            // **WHY `written` is not decoration.** `a1..a5` are *argument*
            // registers, and every `libsys` wrapper passes them as `in("aN")` —
            // operands rustc is entitled to assume survive the call. Writing
            // them unconditionally would be UB in ring 3 across the whole tree,
            // so only the arms that opt in get copied back.
            //
            // Unrolled on purpose: `for i in 0..SYSCALL_OUT_REGS` trips
            // `needless_range_loop`, and warnings are failures in this project.
            if out.written {
                frame.regs[11] = out.regs[0] as _; // a1 = caller TID
                frame.regs[12] = out.regs[1] as _; // a2..a5 = request words
                frame.regs[13] = out.regs[2] as _;
                frame.regs[14] = out.regs[3] as _;
                frame.regs[15] = out.regs[4] as _;
            }
            frame.sepc += 4;              // skip ecall instruction

            // K-C21: if THIS task ran exec_user() inside this ecall, consume
            // its own hand-off and switch to U-mode. Per-task, not the old
            // global slot — another hart finishing an unrelated syscall in
            // this window can no longer steal the context and SRET into an
            // address space that was never its own. The taker has already
            // installed the new satp and destroyed the replaced address
            // space (K-C22); the value returned here makes the SRET path
            // re-write the satp already in force, which is harmless.
            #[cfg(not(feature = "no-mmu"))]
            if let Some(ctx) = robot_os_sched::take_current_task_exec_ctx() {
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

            // AQ8: Trace page fault (critical for post-mortem debugging).
            // AQ8 trace stays on EVERY fault, resolved or not: it is a ring
            // buffer write with no UART lock, and it is what `trace_dump`
            // replays on the fatal path below.
            robot_os_ipc::trace_fault(
                frame.stval as u32, cause as u32,
                robot_os_sched::current_task_tid(),
            );

            // The banner used to be printed HERE, before COW and demand paging
            // were even attempted. Nearly every fault this kernel takes is a
            // COW break from `fork()` that resolves fine, so the log filled up
            // with blocks that read exactly like a fatal crash and were not:
            // in one 30 s `ipctest` run, 3 of 3 `[PAGE FAULT]` blocks were
            // successful COW breaks, and they cost two humans (and one agent)
            // a wrong diagnosis. It is also ~160 µs of UART-lock time per
            // 64 bytes under QEMU, paid on the hot fork path.
            //
            // So: resolve first, print only what could NOT be resolved. Do not
            // move these prints back up — the unresolved path below still
            // emits every field the old banner did, plus the resolved-fault
            // counters, so nothing is lost where it actually matters.
            if from_user {
                // AQ9: Try COW fault resolution first (store page fault only).
                #[cfg(not(feature = "no-mmu"))]
                if cause == TRAP_STORE_PAGE_FAULT {
                    let user_pt = robot_os_sched::current_user_pt();
                    if user_pt != 0 {
                        if robot_os_mm::vmm::handle_cow_fault(user_pt, frame.stval as usize).is_ok() {
                            robot_os_mm::vmm::note_cow_resolved();
                            return 0; // COW resolved, resume task — silently
                        }
                    }
                }

                // AQ10: Try demand paging (load/store/instr fault on a demand-mapped page).
                // Both handlers refuse any VA under `vmm::USER_GUARD_LIMIT`,
                // so a null dereference can never be "resolved" into a mapped
                // zero page — it falls through to the kill below.
                #[cfg(not(feature = "no-mmu"))]
                {
                    let user_pt = robot_os_sched::current_user_pt();
                    if user_pt != 0 {
                        if robot_os_mm::vmm::handle_demand_fault(user_pt, frame.stval as usize).is_ok() {
                            robot_os_mm::vmm::note_demand_resolved();
                            return 0; // Page allocated on demand, resume — silently
                        }
                    }
                }

                // Neither COW nor demand paging — this one is real. Full
                // post-mortem, then kill the offending task.
                kprintln!();
                kprintln!("[PAGE FAULT] CPU {} — {} at {:#x}",
                    hart, trap::cause_str(cause), frame.stval);
                kprintln!("  sepc: {:#x}  task: {}", frame.sepc,
                    robot_os_sched::current_task_name());
                page_fault_note_guard(frame.stval as usize);
                page_fault_note_resolved();
                kprintln!("[PAGE FAULT] Killing user task");
                robot_os_sched::task_exit();
                // task_exit() never returns — context_switch abandons this frame.
            } else {
                // S-mode (kernel) fault: this is a kernel bug. Nothing here is
                // recoverable, so this branch prints everything unconditionally.
                kprintln!();
                kprintln!("[PAGE FAULT] CPU {} — {} at {:#x}",
                    hart, trap::cause_str(cause), frame.stval);
                kprintln!("  sepc: {:#x}  task: {}", frame.sepc,
                    robot_os_sched::current_task_name());
                page_fault_note_guard(frame.stval as usize);
                page_fault_note_resolved();
                // Stop all motors, log diagnostics, shutdown system.
                kprintln!("[FATAL] Kernel page fault on CPU {} — initiating shutdown", hart);
                kprintln!("  regs[1] (ra):  {:#x}", frame.regs[1]);
                kprintln!("  regs[2] (sp):  {:#x}", frame.regs[2]);
                kprintln!("  regs[8] (s0):  {:#x}", frame.regs[8]);
                // AQ8: Dump trace buffer before dying — last chance for debugging.
                robot_os_ipc::trace_dump(20);
                // Emergency motor stop to prevent runaway
                robot_os_robot::motor_cmd_publish(0, 0);
                robot_os_arch::sbi::shutdown();
            }
        }

        // ── All other exceptions: fatal only if they came from S-mode ─────
        //
        // Illegal instruction (cause 2), `ebreak`, misaligned load/store and
        // the rest land here. This arm used to shut the board down
        // unconditionally, which made it strictly harsher than the page-fault
        // arm directly above for no defensible reason: a ring-3 task that
        // dereferences a null pointer merely dies, but a ring-3 task that
        // executes one bad opcode killed the whole robot.
        //
        // That is not a theoretical gap. An ELF whose entry point lands on
        // garbage inside a mapped RX page raises cause 2, NOT a page fault —
        // the page is mapped and executable, the bytes are just not
        // instructions — so it never reached the "kill the offending task"
        // path next door. On the autorun path that ELF is read from the FAT32
        // volume `msc_gadget.rs` also exports over USB mass storage, so a file
        // truncated by a yanked cable produced a guaranteed shutdown loop:
        // boot, exec, illegal instruction, shutdown, repeat — with no shell
        // ever reaching a prompt to replace the bad file.
        //
        // The privilege split is the fix: WHERE the trap came from is what
        // decides whether this is a kernel bug (unrecoverable) or just a bad
        // program (kill it and carry on).
        _ => {
            let hart = robot_os_arch::cpu::hart_id();
            // SPP bit: 0 = came from U-mode, 1 = came from S-mode. Exactly the
            // mechanism the page-fault arm uses — once we are inside the
            // handler, sstatus.SPP is the only trustworthy record of the
            // privilege level the trap interrupted.
            let from_user = (frame.sstatus as usize) & csr::SSTATUS_SPP == 0;

            kprintln!();
            kprintln!("[EXCEPTION] CPU {} — {}", hart, trap::cause_str(cause));
            kprintln!("  sepc:   {:#x}", frame.sepc);
            kprintln!("  stval:  {:#x}", frame.stval);
            kprintln!("  scause: {:#x}", frame.scause);
            kprintln!("  regs[1] (ra):  {:#x}", frame.regs[1]);
            kprintln!("  regs[2] (sp):  {:#x}", frame.regs[2]);

            if from_user {
                // Ring-3 executed something it had no business executing.
                // Kill just that task, like the page-fault arm does.
                //
                // Deliberately NO `motor_cmd_publish(0, 0)` on this path, and
                // the page-fault arm makes the same choice: the dead task is
                // one of many, the control loop and the reflex daemon are
                // still running, and slamming the motors to zero from a trap
                // handler would inject a stop command that the surviving
                // control stack neither requested nor knows about — worse
                // than useless on a robot mid-motion. Emergency stop belongs
                // to the S-mode branch, where nothing is left to steer.
                kprintln!("[EXCEPTION] Killing user task '{}' (tid {})",
                    robot_os_sched::current_task_name(),
                    robot_os_sched::current_task_tid());
                robot_os_sched::task_exit();
                // task_exit() never returns — context_switch abandons this frame.
            } else {
                // S-mode: the kernel itself hit a bad instruction or a
                // misaligned access. Nothing here is recoverable — there is no
                // smaller unit than "the kernel" left to kill — so stop the
                // motors to prevent a runaway and go down.
                robot_os_robot::motor_cmd_publish(0, 0);
                kprintln!("[FATAL] Unhandled exception on CPU {} — shutdown", hart);
                robot_os_arch::sbi::shutdown();
            }
        }
    }
}

