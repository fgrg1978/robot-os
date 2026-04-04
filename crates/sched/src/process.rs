// Process management: ELF loader, exec, SRET to U-mode.
// Phase 7 — enables kernel to launch RISC-V 64-bit ELF user programs.

use robot_os_arch::mmu::{PteFlags, PAGE_SIZE, make_satp};
use robot_os_mm::{pmm, vmm};
use robot_os_sync::SpinLock;
use core::sync::atomic::{AtomicU64, Ordering};

// ── User-space memory layout ──────────────────────────────────────────────────

/// Top of user virtual stack (2 GiB mark, valid in Sv39 user space).
/// Sv39 user space: VA[38:0] with bits[63:39]=0, max = 0x3F_FFFF_FFFF.
/// 2 GiB = 0x8000_0000 is well within that range.
const USER_STACK_TOP:  usize = 0x0000_0000_8000_0000; // 2 GiB
const USER_STACK_SIZE: usize = 4 * PAGE_SIZE; // 16 KiB

// ── ELF constants ─────────────────────────────────────────────────────────────

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const PT_LOAD: u32 = 1;
const PF_W: u32 = 2;

// ── ExecContext ────────────────────────────────────────────────────────────────

/// Pending exec context: set by exec_user(), consumed by trap_handler.
pub struct ExecContext {
    pub satp:    u64, // new user SATP
    pub entry:   u64, // ELF entry point virtual address
    pub user_sp: u64, // user stack pointer (aligned)
    pub sstatus: u64, // SSTATUS to restore: SPP=0, SPIE=1
    pub user_pt: u64, // physical address of user page table
    pub brk:     u64, // initial brk (= end of last loaded segment, page-aligned)
}

static PENDING_EXEC: SpinLock<Option<ExecContext>> = SpinLock::new(None);

// ── Ecall context for fork ───────────────────────────────────────────────────
// Set by trap handler before syscall_dispatch so sys_fork_impl can capture
// the parent's sepc and user_sp for the fork child.

static ECALL_SEPC:    AtomicU64 = AtomicU64::new(0);
static ECALL_USER_SP: AtomicU64 = AtomicU64::new(0);

/// Called from trap handler before ecall dispatch.
pub fn set_ecall_context(sepc: u64, user_sp: u64) {
    ECALL_SEPC.store(sepc, Ordering::Release);
    ECALL_USER_SP.store(user_sp, Ordering::Release);
}

// Fork child context: saved by sys_fork_impl, consumed by fork_child_entry.
static FORK_CHILD_CTX: SpinLock<Option<ForkChildCtx>> = SpinLock::new(None);

struct ForkChildCtx {
    entry:   u64,   // sepc + 4 (instruction after ecall)
    user_sp: u64,   // parent's user stack pointer
    satp:    u64,   // child's SATP value
}

/// Store a pending exec context (consumed once by trap_handler).
pub fn set_pending_exec(ctx: ExecContext) {
    *PENDING_EXEC.lock() = Some(ctx);
}

/// Take (and clear) the pending exec context.
pub fn take_pending_exec() -> Option<ExecContext> {
    PENDING_EXEC.lock().take()
}

// ── ELF loader ────────────────────────────────────────────────────────────────

/// Load a RISC-V ELF64 binary into a new Sv39 user address space.
///
/// On success: stores ExecContext via `set_pending_exec()` and returns 0.
/// On failure: returns -1.
pub fn exec_user(elf: &[u8]) -> i64 {
    match load_elf(elf) {
        Some(ctx) => {
            // Store user PT info into the current task so context_switch.S can
            // write the correct SATP on every subsequent context switch.
            crate::scheduler::set_current_user_info(ctx.satp, ctx.user_pt, ctx.brk);
            set_pending_exec(ctx);
            0
        }
        None => -1,
    }
}

fn load_elf(elf: &[u8]) -> Option<ExecContext> {
    if elf.len() < 64               { return None; }
    if &elf[0..4] != ELF_MAGIC      { return None; }
    if elf[4] != 2                  { return None; } // ELFCLASS64
    if elf[5] != 1                  { return None; } // ELFDATA2LSB
    if r16(elf, 18) != 0xf3        { return None; } // EM_RISCV

    let e_entry     = r64(elf, 24);
    let e_phoff     = r64(elf, 32) as usize;
    let e_phentsize = r16(elf, 54) as usize;
    let e_phnum     = r16(elf, 56) as usize;

    if e_phentsize < 56 || e_phnum == 0 { return None; }

    let user_pt = vmm::create_pagetable().ok()?;

    // Copy kernel L2/L1 entries into the user PT so that the trap handler
    // (trap_vector at ~0x80200000) and MMIO (UART, CLINT, etc.) are
    // reachable in S-mode when an ecall fires with the user PT active.
    // Kernel pages have no USER bit — U-mode cannot access them directly.
    vmm::copy_kernel_entries_to_user(user_pt);

    // Track the highest mapped virtual address for initial brk.
    let mut brk_va: usize = 0;

    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        if ph + 56 > elf.len() { break; }

        if r32(elf, ph) != PT_LOAD { continue; }

        let p_flags  = r32(elf, ph + 4);
        let p_offset = r64(elf, ph + 8)  as usize;
        let p_vaddr  = r64(elf, ph + 16) as usize;
        let p_filesz = r64(elf, ph + 32) as usize;
        let p_memsz  = r64(elf, ph + 40) as usize;

        if p_memsz == 0 { continue; }

        let flags = if p_flags & PF_W != 0 {
            PteFlags::USER_RW | PteFlags::ACCESSED | PteFlags::DIRTY
        } else {
            PteFlags::USER_RX | PteFlags::ACCESSED
        };

        let va_start = p_vaddr & !(PAGE_SIZE - 1);
        let va_end   = page_up(p_vaddr + p_memsz);

        if va_end > brk_va { brk_va = va_end; }

        let mut va = va_start;
        while va < va_end {
            let page = pmm::alloc_page().ok()?;
            let phys = page.as_usize();

            let seg_off = va.saturating_sub(p_vaddr);
            if seg_off < p_filesz {
                let src_off = p_offset + seg_off;
                let copy_n  = p_filesz.saturating_sub(seg_off).min(PAGE_SIZE);
                if src_off + copy_n <= elf.len() {
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            elf.as_ptr().add(src_off),
                            phys as *mut u8,
                            copy_n,
                        );
                    }
                }
            }
            let _ = vmm::map(user_pt, va, phys, flags);
            va += PAGE_SIZE;
        }
    }

    // User stack
    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    let mut va = stack_bottom;
    while va < USER_STACK_TOP {
        let page = pmm::alloc_page().ok()?;
        let _ = vmm::map(
            user_pt, va, page.as_usize(),
            PteFlags::USER_RW | PteFlags::ACCESSED | PteFlags::DIRTY,
        );
        va += PAGE_SIZE;
    }

    let user_satp = make_satp(user_pt, crate::alloc_asid()) as u64;
    let user_sp   = (USER_STACK_TOP - 16) as u64;
    // sstatus: SPP=0 (U-mode), SPIE=1 (enable interrupts after SRET), SIE=0
    let sstatus   = 1u64 << 5; // SPIE

    Some(ExecContext {
        satp:    user_satp,
        entry:   e_entry,
        user_sp,
        sstatus,
        user_pt: user_pt as u64,
        brk:     brk_va as u64,
    })
}

// ── SRET to user mode ─────────────────────────────────────────────────────────

/// Switch from kernel S-mode to user U-mode.  Never returns.
///
/// Sets sscratch = current kernel SP (so the next U-mode trap can find the
/// kernel stack), switches to the user page table, then SRETs to `entry`.
///
/// # Safety
/// Caller must guarantee `entry` and `user_sp` are valid user-space addresses.
pub unsafe fn sret_to_user(entry: usize, user_sp: usize, satp: usize) -> ! {
    let sspie: usize = 1 << 5; // SPIE bit — interrupts enabled in U-mode
    core::arch::asm!(
        "csrw  sepc, {entry}",      // sepc = user entry point
        "csrw  sstatus, {sspie}",   // SPP=0 (U-mode), SPIE=1, SIE=0
        "csrw  sscratch, sp",       // sscratch = kernel SP (for re-entry)
        "csrw  satp, {satp}",       // switch page table
        "sfence.vma zero, zero",
        "mv    sp, {user_sp}",      // switch to user stack
        "li a0,0","li a1,0","li a2,0","li a3,0",
        "li a4,0","li a5,0","li a6,0","li a7,0",
        "sret",
        entry   = in(reg) entry,
        sspie   = in(reg) sspie,
        satp    = in(reg) satp,
        user_sp = in(reg) user_sp,
        options(noreturn),
    )
}

// ── User-space memory access ──────────────────────────────────────────────────

/// Copy `len` bytes FROM user virtual address `user_src` INTO kernel buffer `kernel_dst`.
///
/// When the current task has a user PT (user_pt != 0), each source page is
/// translated via the page table and copied byte-by-byte from the physical
/// address.  For kernel tasks (user_pt == 0) the pointer is used directly.
///
/// Returns `true` on success, `false` if any page is unmapped.
pub fn copy_from_user(kernel_dst: *mut u8, user_src: usize, len: usize) -> bool {
    let user_pt = crate::scheduler::current_user_pt();
    if user_pt == 0 {
        // Kernel task — identity-mapped; pointer is valid.
        unsafe { core::ptr::copy_nonoverlapping(user_src as *const u8, kernel_dst, len); }
        return true;
    }
    let mut done = 0usize;
    while done < len {
        let va   = user_src + done;
        let Some(pa) = vmm::translate(user_pt, va) else { return false; };
        let chunk = (PAGE_SIZE - (va & (PAGE_SIZE - 1))).min(len - done);
        unsafe {
            core::ptr::copy_nonoverlapping(pa as *const u8, kernel_dst.add(done), chunk);
        }
        done += chunk;
    }
    true
}

/// Copy `len` bytes FROM kernel buffer `kernel_src` INTO user virtual address `user_dst`.
///
/// Returns `true` on success, `false` if any page is unmapped.
pub fn copy_to_user(user_dst: usize, kernel_src: *const u8, len: usize) -> bool {
    let user_pt = crate::scheduler::current_user_pt();
    if user_pt == 0 {
        unsafe { core::ptr::copy_nonoverlapping(kernel_src, user_dst as *mut u8, len); }
        return true;
    }
    let mut done = 0usize;
    while done < len {
        let va   = user_dst + done;
        let Some(pa) = vmm::translate(user_pt, va) else { return false; };
        let chunk = (PAGE_SIZE - (va & (PAGE_SIZE - 1))).min(len - done);
        unsafe {
            core::ptr::copy_nonoverlapping(kernel_src.add(done), pa as *mut u8, chunk);
        }
        done += chunk;
    }
    true
}

/// Copy a NUL-terminated C string from user space into `dst` (max `max_len` bytes incl. NUL).
/// Returns the number of bytes copied (excluding NUL) on success, or `None` on fault.
pub fn copy_cstr_from_user(dst: &mut [u8], user_ptr: usize) -> Option<usize> {
    let user_pt = crate::scheduler::current_user_pt();
    let mut len = 0usize;
    loop {
        if len >= dst.len() { return None; } // buffer overflow
        let va = user_ptr + len;
        let b: u8 = if user_pt == 0 {
            unsafe { *(va as *const u8) }
        } else {
            let pa = vmm::translate(user_pt, va)?;
            unsafe { *(pa as *const u8) }
        };
        dst[len] = b;
        if b == 0 { return Some(len); }
        len += 1;
    }
}

// ── sys_brk ───────────────────────────────────────────────────────────────────

/// Implement the brk(2) syscall: extend or query the user heap.
///
/// - `addr == 0`: return current brk
/// - `addr > current_brk`: allocate new pages, advance brk, return new brk
/// - `addr < current_brk` (shrink): unsupported in Phase 7, return current brk
pub fn sys_brk_impl(addr: u64) -> i64 {
    let user_pt = crate::scheduler::current_user_pt();
    if user_pt == 0 { return -1; } // kernel task

    let cur_brk = crate::scheduler::update_user_brk(0); // query
    if addr == 0 || addr == cur_brk { return cur_brk as i64; }
    if addr < cur_brk { return cur_brk as i64; } // shrink not supported

    // Allocate pages from cur_brk to addr.
    let new_brk = page_up(addr as usize) as u64;
    let mut va = page_up(cur_brk as usize);
    while (va as u64) < new_brk {
        match pmm::alloc_page() {
            Ok(page) => {
                let _ = vmm::map(
                    user_pt, va, page.as_usize(),
                    PteFlags::USER_RW | PteFlags::ACCESSED | PteFlags::DIRTY,
                );
            }
            Err(_) => return cur_brk as i64, // OOM — return old brk
        }
        va += PAGE_SIZE;
    }
    crate::scheduler::update_user_brk(new_brk) as i64
}

// ── fork() ──────────────────────────────────────────────────────────────────

/// Implement fork(): create a child process that is a copy of the parent.
///
/// Returns child TID to the parent (>0), 0 to the child, or -1 on error.
///
/// Implementation:
///  1. Duplicate the parent's user page table (deep copy: new physical pages).
///  2. Create a new kernel task with name "forked".
///  3. The child's a0 register (return value) is set to 0.
///  4. The parent receives the child's TID.
///
/// Limitations:
///  - Only works for user-mode tasks (user_pt != 0).
///  - No copy-on-write optimization (full copy).
///  - Kernel file descriptors are NOT duplicated.
pub fn sys_fork_impl() -> i64 {
    let parent_pt = crate::scheduler::current_user_pt();
    if parent_pt == 0 { return -1; } // kernel task can't fork

    // Duplicate the page table: walk all valid PTEs, allocate new pages, copy contents.
    let child_pt = match vmm::create_pagetable() {
        Ok(pt) => pt,
        Err(_) => return -1,
    };

    // Copy kernel entries so traps work in the child.
    vmm::copy_kernel_entries_to_user(child_pt);

    // Walk the parent's user page table and copy all user pages.
    // We iterate over the L2 (mega-page) and L1 (page) levels.
    // For simplicity, we copy the address range 0x10000..USER_STACK_TOP.
    let copy_start = 0x10000usize; // typical user VA start
    let copy_end   = USER_STACK_TOP;
    let mut va = copy_start;
    while va < copy_end {
        if let Some(pa) = vmm::translate(parent_pt, va) {
            // This VA is mapped in the parent — allocate a page and copy.
            match pmm::alloc_page() {
                Ok(page) => {
                    let new_pa = page.as_usize();
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            pa as *const u8,
                            new_pa as *mut u8,
                            PAGE_SIZE,
                        );
                    }
                    let flags = PteFlags::USER_RW | PteFlags::ACCESSED | PteFlags::DIRTY;
                    let _ = vmm::map(child_pt, va, new_pa, flags);
                }
                Err(_) => return -1, // OOM
            }
        }
        va += PAGE_SIZE;
    }

    // Get parent's brk to set in child.
    let parent_brk = crate::scheduler::update_user_brk(0);

    // Create child task. We use a trampoline that just yields forever — the real
    // entry will be set via the pending exec mechanism when we SRET.
    let child_idx = crate::task_create("forked", fork_child_entry, 0, crate::DEFAULT_PRIORITY);
    if child_idx == 0 { return -1; }

    // Apply the child's user page table so context_switch.S writes the
    // correct SATP when the child is scheduled.
    let child_satp = make_satp(child_pt, crate::alloc_asid()) as u64;
    crate::scheduler::set_task_user_info(child_idx, child_satp, child_pt as u64, parent_brk);

    // AQ11: Inherit parent's syscall filter — child cannot be less restricted.
    let parent_filter = crate::scheduler::current_syscall_filter();
    crate::scheduler::set_task_syscall_filter(child_idx, parent_filter);

    // Capture ecall context: sepc (ecall addr) + user SP.
    // The trap handler stores these before dispatching the syscall.
    let ecall_sepc = ECALL_SEPC.load(Ordering::Acquire);
    let ecall_usp  = ECALL_USER_SP.load(Ordering::Acquire);

    // Save context for fork_child_entry.
    *FORK_CHILD_CTX.lock() = Some(ForkChildCtx {
        entry:   ecall_sepc + 4,  // skip the ecall instruction
        user_sp: ecall_usp,
        satp:    child_satp,
    });

    child_idx as i64
}

/// Fork child entry point.  Reads the saved context and SRETs to user
/// mode with a0=0 (the fork return value for the child process).
fn fork_child_entry(_arg: usize) {
    let ctx = FORK_CHILD_CTX.lock().take();
    if let Some(ctx) = ctx {
        // SRET to user mode: entry = instruction after ecall, a0 = 0
        unsafe { sret_to_user(ctx.entry as usize, ctx.user_sp as usize, ctx.satp as usize); }
    }
    // Fallback: if no context, just exit (shouldn't happen).
}

// ── Little-endian helpers ─────────────────────────────────────────────────────

#[inline] fn r16(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off+1]])
}
#[inline] fn r32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([d[off], d[off+1], d[off+2], d[off+3]])
}
#[inline] fn r64(d: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        d[off],   d[off+1], d[off+2], d[off+3],
        d[off+4], d[off+5], d[off+6], d[off+7],
    ])
}
#[inline] fn page_up(a: usize) -> usize {
    (a + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}
