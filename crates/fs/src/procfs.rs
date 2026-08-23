//! Procfs + Sysfs — virtual read-only filesystems (F21).
//!
//! Exposes kernel internals as synthetic text files, following the Linux
//! `/proc` and `/sys` conventions.  Files are generated on-the-fly by
//! registered providers; no persistent storage is needed.
//!
//! ## `/proc` entries
//!
//! | Path            | Content                                          |
//! |-----------------|--------------------------------------------------|
//! | `/proc/uptime`  | Seconds.milliseconds since boot                 |
//! | `/proc/meminfo` | PMM total/free/used pages in kB                 |
//! | `/proc/fs`      | TmpFS file count, used/max bytes                |
//!
//! ## `/sys` entries
//!
//! | Path             | Content                                         |
//! |------------------|-------------------------------------------------|
//! | `/sys/version`   | Kernel version string + platform name           |
//! | `/sys/platform`  | Short platform name (QEMU / VF2 / K1)          |
//!
//! ## Usage
//! ```rust
//! procfs_init();                       // register built-in providers
//! let mut buf = [0u8; 512];
//! let n = procfs_read(b"/proc/uptime", &mut buf);
//! ```

extern crate alloc;

use alloc::string::String;
use robot_os_sync::SpinLock;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of registered virtual files (procfs + sysfs combined).
pub const PROCFS_MAX_ENTRIES: usize = 32;
/// Maximum path length for a virtual file path component (no leading slash).
pub const PROCFS_PATH_LEN:    usize = 48;

// ── Entry type ────────────────────────────────────────────────────────────────

/// Virtual-filesystem namespace.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProcNs {
    Proc,
    Sys,
}

struct ProcEntry {
    path:     [u8; PROCFS_PATH_LEN],
    path_len: u8,
    ns:       ProcNs,
    gen:      fn(&mut [u8]) -> usize,
    active:   bool,
}

const EMPTY_ENTRY: ProcEntry = ProcEntry {
    path: [0; PROCFS_PATH_LEN],
    path_len: 0,
    ns: ProcNs::Proc,
    gen: |_| 0,
    active: false,
};

// ── Global table ──────────────────────────────────────────────────────────────

struct ProcfsState {
    entries: [ProcEntry; PROCFS_MAX_ENTRIES],
    count:   usize,
}

impl ProcfsState {
    const fn new() -> Self {
        ProcfsState { entries: [EMPTY_ENTRY; PROCFS_MAX_ENTRIES], count: 0 }
    }
}

static PROCFS: SpinLock<ProcfsState> = SpinLock::new(ProcfsState::new());

// ── Public API ────────────────────────────────────────────────────────────────

/// Register a virtual file.
///
/// `path` is relative to the namespace root (e.g. `b"uptime"` for `/proc/uptime`).
/// `gen` is called each time the file is read; it must fill `buf` and return bytes written.
/// Returns `false` if the table is full or `path` is too long.
pub fn procfs_register(ns: ProcNs, path: &[u8], gen: fn(&mut [u8]) -> usize) -> bool {
    if path.len() >= PROCFS_PATH_LEN { return false; }
    let mut state = PROCFS.lock();
    if state.count >= PROCFS_MAX_ENTRIES { return false; }
    let slot = match state.entries.iter().position(|e| !e.active) {
        Some(s) => s,
        None    => return false,
    };
    let e = &mut state.entries[slot];
    e.path[..path.len()].copy_from_slice(path);
    e.path_len = path.len() as u8;
    e.ns       = ns;
    e.gen      = gen;
    e.active   = true;
    state.count += 1;
    true
}

/// Read a virtual file into `buf`.
///
/// `full_path` must include the namespace prefix (`/proc/` or `/sys/`).
/// Returns bytes written to `buf`, or 0 if the path is not found.
pub fn procfs_read(full_path: &[u8], buf: &mut [u8]) -> usize {
    let (ns, rel) = if full_path.starts_with(b"/proc/") {
        (ProcNs::Proc, &full_path[6..])
    } else if full_path.starts_with(b"/sys/") {
        (ProcNs::Sys, &full_path[5..])
    } else {
        return 0;
    };

    // Look up the generator without holding the lock during generation.
    let gen_fn = {
        let state = PROCFS.lock();
        let mut found = None;
        for e in state.entries.iter() {
            if !e.active || e.ns != ns { continue; }
            let plen = e.path_len as usize;
            if &e.path[..plen] == rel { found = Some(e.gen); break; }
        }
        found
    };

    match gen_fn {
        Some(f) => f(buf),
        None    => 0,
    }
}

/// List all registered paths by calling `cb(full_path_str)` for each entry.
pub fn procfs_ls(mut cb: impl FnMut(&str)) {
    let state = PROCFS.lock();
    for e in state.entries.iter() {
        if !e.active { continue; }
        let plen = e.path_len as usize;
        let prefix = match e.ns { ProcNs::Proc => "/proc/", ProcNs::Sys => "/sys/" };
        let mut s = String::from(prefix);
        if let Ok(r) = core::str::from_utf8(&e.path[..plen]) {
            s.push_str(r);
            cb(&s);
        }
    }
}

/// Number of registered virtual-file entries.
pub fn procfs_count() -> usize { PROCFS.lock().count }

// ── Built-in provider registration ───────────────────────────────────────────

/// Register all built-in `/proc` and `/sys` providers.
/// Call once from `kernel_main` after all subsystems are initialized.
pub fn procfs_init() {
    procfs_register(ProcNs::Proc, b"uptime",  gen_uptime);
    procfs_register(ProcNs::Proc, b"meminfo", gen_meminfo);
    procfs_register(ProcNs::Proc, b"fs",      gen_fs);
    procfs_register(ProcNs::Sys,  b"version", gen_version);
    procfs_register(ProcNs::Sys,  b"platform",gen_platform);
}

// ── Generator functions ───────────────────────────────────────────────────────

fn write_str(buf: &mut [u8], s: &str) -> usize {
    let b = s.as_bytes();
    let n = b.len().min(buf.len());
    buf[..n].copy_from_slice(&b[..n]);
    n
}

fn gen_uptime(buf: &mut [u8]) -> usize {
    // Read RISC-V `time` CSR (rdtime pseudo-instruction, S-mode readable).
    // Non-riscv64 builds (host unit tests) have no rdtime; substitute 0.
    #[cfg(target_arch = "riscv64")]
    let ticks: u64 = { let t: u64; unsafe { core::arch::asm!("rdtime {}", out(reg) t); } t };
    #[cfg(not(target_arch = "riscv64"))]
    let ticks: u64 = 0;

    let freq = robot_os_drivers::platform::hw::TIMER_FREQ;
    let secs  = ticks / freq;
    let msecs = (ticks % freq) * 1000 / freq;
    let s = alloc::format!("{}.{:03}\n", secs, msecs);
    write_str(buf, &s)
}

fn gen_meminfo(buf: &mut [u8]) -> usize {
    const PAGE_KIB: usize = 4; // PAGE_SIZE = 4096 bytes = 4 KiB
    let total_kib = robot_os_mm::pmm::total_pages() * PAGE_KIB;
    let free_kib  = robot_os_mm::pmm::free_pages()  * PAGE_KIB;
    let used_kib  = robot_os_mm::pmm::used_pages()  * PAGE_KIB;
    let s = alloc::format!(
        "MemTotal: {} kB\nMemFree:  {} kB\nMemUsed:  {} kB\n",
        total_kib, free_kib, used_kib
    );
    write_str(buf, &s)
}

fn gen_fs(buf: &mut [u8]) -> usize {
    let (files, used, max) = crate::tmpfs::tmpfs_stats();
    let s = alloc::format!(
        "tmpfs_files: {}\ntmpfs_used:  {} B\ntmpfs_max:   {} B\n",
        files, used, max
    );
    write_str(buf, &s)
}

fn gen_version(buf: &mut [u8]) -> usize {
    let s = alloc::format!(
        "Robot OS 0.1.0 ({})\n",
        robot_os_drivers::platform::hw::PLATFORM_NAME
    );
    write_str(buf, &s)
}

fn gen_platform(buf: &mut [u8]) -> usize {
    let s = alloc::format!("{}\n", robot_os_drivers::platform::hw::PLATFORM_NAME);
    write_str(buf, &s)
}
