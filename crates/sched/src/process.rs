// Process management: ELF loader, exec, SRET to U-mode.
// Phase 7 — enables kernel to launch RISC-V 64-bit ELF user programs.

use robot_os_arch::mmu::{PteFlags, PAGE_SIZE, make_satp};
use robot_os_mm::{pmm, vmm, vdso};
use robot_os_common::error::KernelError;
use core::sync::atomic::{AtomicU64, Ordering};

// ── User-space memory layout ──────────────────────────────────────────────────

use robot_os_limits::USER_STACK_SIZE_BYTES as USER_STACK_SIZE;

/// Top of user virtual stack (2 GiB mark, valid in Sv39 user space).
/// Sv39 user space: VA[38:0] with bits[63:39]=0, max = 0x3F_FFFF_FFFF.
/// 2 GiB = 0x8000_0000 is well within that range.
const USER_STACK_TOP: usize = 0x0000_0000_8000_0000; // 2 GiB

// ── ELF constants ─────────────────────────────────────────────────────────────

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// Ceiling for everything userspace places in the low VA region: the loaded
/// image and the `brk` heap.
///
/// Every user page table carries the kernel's own mappings, merged in by
/// [`vmm::copy_kernel_entries_to_user`] at VPN[1] granularity for VPN[2]=0.
/// The lowest address the kernel identity-maps on the RV64 boards this OS
/// targets is the CLINT at `0x0200_0000`; PLIC (`0x0C00_0000`), UART
/// (`0x1000_0000`) and the rest sit above it, and RAM is at `0x8000_0000`.
/// Keeping the image and the heap strictly below the CLINT means user VPN[1]
/// slots (0..=15) and kernel VPN[1] slots (16, 96, 128, …) never collide, so
/// the merge never has to choose between a user mapping and a kernel one.
///
/// It also keeps the image clear of the two other things that live in a user
/// address space — the vDSO at `0x5000_0000` and the stack just below
/// `USER_STACK_TOP` — whose overlap was previously swallowed by an ignored
/// `vmm::map` result.
///
/// [`vmm::kernel_entry_collision`] is the platform-independent backstop for
/// this constant: if some board maps something lower, exec fails loudly
/// instead of silently losing a kernel mapping.
const USER_LOW_MAX: usize = 0x0200_0000; // 32 MiB — CLINT base

/// Pure `PT_LOAD` bounds checks, in their own file only so the host test
/// runner (`crates/sched-wake-tests`) can compile them — the rest of this
/// module cannot leave the target. Declared here rather than in `lib.rs` to
/// keep it plainly a part of the loader.
#[path = "elf_bounds.rs"]
pub mod elf_bounds;

/// The address limits `elf_bounds` enforces, taken from their real
/// definitions. This is the only place they are named together; `elf_bounds`
/// declares none of them itself, so there is nothing to drift.
#[inline]
const fn seg_limits() -> elf_bounds::SegLimits {
    elf_bounds::SegLimits {
        guard_limit: vmm::USER_GUARD_LIMIT,
        low_max: USER_LOW_MAX,
        page_size: PAGE_SIZE,
    }
}

// ── ExecContext ────────────────────────────────────────────────────────────────

/// Loader output: everything `exec_user` needs to install the new address
/// space on the current task. Internal to the loader — the hand-off to the
/// consumption sites travels on the task's own `exec_*` fields (K-C21) and
/// comes back out as an [`ExecHandoff`].
pub struct ExecContext {
    pub satp:    u64, // new user SATP
    pub entry:   u64, // ELF entry point virtual address
    pub user_sp: u64, // user stack pointer (aligned)
    pub sstatus: u64, // SSTATUS to restore: SPP=0, SPIE=1
    pub user_pt: u64, // physical address of user page table
    pub brk:     u64, // initial brk (= end of last loaded segment, page-aligned)
}

/// What [`take_current_task_exec_ctx`] hands the consumption sites: the
/// register-visible half of the exec (`user_pt`/`brk` were already applied to
/// the task by `exec_user`, and the old address space is already gone by the
/// time this struct exists).
pub struct ExecHandoff {
    pub entry:   u64,
    pub user_sp: u64,
    pub sstatus: u64,
    pub satp:    u64,
}

// K-A15: the parent's ecall sepc/user_sp used to be captured into a global
// (`ECALL_SEPC`/`ECALL_USER_SP`, set by the trap handler before every
// syscall dispatch) and the fork hand-off into another global
// (`FORK_CHILD_CTX`) — both raced against concurrent syscalls/forks on
// other harts (see KERNEL_REVIEW_NOTES / audit finding K-A15). sepc/user_sp
// are now threaded through as plain function parameters (hart-local, no
// shared state, no race possible) all the way from the trap handler down to
// `sys_fork_impl`; the fork hand-off itself now lives on the child's own
// Task struct (`fork_entry`/`fork_user_sp`/`fork_satp`/`fork_ctx_ready` —
// see their doc in `crates/sched/src/task.rs`) instead of a single shared
// slot, via `scheduler::set_task_fork_ctx`/`take_current_task_fork_ctx`.
//
// K-C21: the exec hand-off (`PENDING_EXEC`, a global
// `SpinLock<Option<ExecContext>>` drained at the end of every U-mode ecall
// on every hart) was the last survivor of that same class, and it raced the
// same way: another hart finishing any syscall in the window SRET'd into
// the exec'er's fresh address space while the exec'er resumed its old sepc
// under the new page table. It now lives on the exec'ing task's own slot
// (`exec_entry`/`exec_user_sp`/`exec_sstatus`/`exec_satp`/`exec_old_pt`/
// `exec_ctx_ready` — see their doc in `crates/sched/src/task.rs`).

// ── ELF loader ────────────────────────────────────────────────────────────────

/// Load a RISC-V ELF64 binary into a new Sv39 user address space.
///
/// On success: publishes the hand-off on the CURRENT task (consumed by the
/// same task via [`take_current_task_exec_ctx`]) and returns 0.
/// On failure: returns -1.
pub fn exec_user(elf: &[u8]) -> i64 {
    match load_elf(elf) {
        Some(ctx) => {
            // K-C22(A): capture the address space this task is abandoning
            // BEFORE `set_current_user_info` overwrites `user_pt` with the
            // new one — this is the only moment the old root is still
            // reachable. It rides in the hand-off because it must be
            // destroyed by the CONSUMER, after satp points at the new page
            // table: right now this hart is still fetching kernel code
            // through the old PT's kernel entries.
            let old_pt = crate::scheduler::current_user_pt() as u64;
            // Store user PT info into the current task so context_switch.S can
            // write the correct SATP on every subsequent context switch.
            crate::scheduler::set_current_user_info(ctx.satp, ctx.user_pt, ctx.brk);
            crate::scheduler::set_current_task_exec_slots(
                ctx.entry, ctx.user_sp, ctx.sstatus, ctx.satp, old_pt,
            );
            0
        }
        None => -1,
    }
}

/// K-C21/K-C22: consume the exec hand-off published by [`exec_user`] on the
/// CURRENT task, install the new address space, and destroy the old one.
///
/// Every consumption site (the tail of the U-mode ecall arm in the kernel's
/// trap handler; the shell and autorun kernel tasks just before their
/// `sret_to_user`) must go through this function — the ordering inside it is
/// the entire K-C22(A) fix:
///
///  1. `csrw satp` to the NEW page table (with full `sfence.vma`). Safe at
///     any of the call sites: `load_elf_into` finished with
///     [`vmm::copy_kernel_entries_to_user`], so kernel text, stacks and MMIO
///     are mapped in the new PT and this function keeps executing across the
///     switch — the same property every trap from U-mode already relies on.
///  2. Only THEN destroy the old address space. Destroying before the switch
///     would free frames — including live page-table frames — that this
///     hart's satp/TLB still translates through; another hart reallocating
///     them mid-walk turns that into silent corruption. After the switch the
///     old root is referenced by nothing: `task_satp`/`user_pt` already point
///     at the new PT (step done in `exec_user`), and any OTHER hart that ever
///     ran this PT flushed its TLB when `context_switch.S` moved it off
///     (full `sfence.vma` on every satp change).
///
/// The trap-handler site still returns `satp` for the SRET path's own
/// `csrw satp`; that re-write of the value already installed here is
/// harmless, as is the one inside `sret_to_user` for the kernel-task sites.
///
/// Same-task-only, like the slot it drains: no TID check is needed (contrast
/// `set_task_fork_ctx`) because publisher and consumer are one task in one
/// syscall — the slot cannot be freed, reused, or observed by another hart
/// in between.
pub fn take_current_task_exec_ctx() -> Option<ExecHandoff> {
    let (entry, user_sp, sstatus, satp, old_pt) =
        crate::scheduler::take_current_task_exec_slots()?;
    robot_os_arch::csr::write_satp(satp as usize); // includes sfence.vma
    if old_pt != 0 {
        // A ring-3 task looping SYS_EXEC used to drain the PMM through the
        // success path — nothing ever freed the replaced address space.
        destroy_user_address_space(old_pt);
    }
    Some(ExecHandoff { entry, user_sp, sstatus, satp })
}

/// K-C22: tear down an address space that no hart can still be running on.
///
/// This is the teardown for the *post-construction* lifetime (exec replaced
/// it, or its task exited and the pool slot is being reused) — as opposed to
/// [`vmm::destroy_user_pagetable`], which the load/fork failure paths call on
/// page tables that never left their builder. The difference: a live process
/// may have had shm ([`shm_map_user`]) and MMIO ([`mmio_map_user`]) frames
/// mapped USER into its PT, and those frames are NOT owned by the address
/// space — shm pages belong to the shm registry (other processes may map
/// them; freeing here is a cross-process use-after-free), MMIO frames belong
/// to the hardware. Both only ever live in the [`USER_MMIO_BASE`,
/// [`USER_MMIO_LIMIT`]) VA window ([`reserve_mmio_va`] is the single
/// allocator), so the teardown spares every leaf frame in that window while
/// still freeing the window's L0/L1 tables — those ARE this PT's own.
pub fn destroy_user_address_space(user_pt: u64) {
    vmm::destroy_user_pagetable_skip_range(user_pt as usize, USER_MMIO_BASE, USER_MMIO_LIMIT);
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

    // Every failure inside `load_elf_into` leaves behind a root page table,
    // the L1/L0 tables built for it, and every data page copied so far.
    // `exec` is reachable from ring 3, so a loop of malformed images used to
    // drain the PMM one image at a time until the kernel could no longer
    // allocate anything — a slow, silent death rather than a rejected exec.
    // Funnelling the whole build through one call gives a single teardown
    // point that covers all of them.
    match load_elf_into(elf, user_pt, e_entry, e_phoff, e_phentsize, e_phnum) {
        Some(ctx) => Some(ctx),
        None => {
            vmm::destroy_user_pagetable(user_pt);
            None
        }
    }
}

/// Build the address space for `user_pt`. Returns `None` on any rejection;
/// the caller owns the teardown.
///
/// Ordering is load-bearing (see [`vmm::copy_kernel_entries_to_user`]): all
/// user mappings go in first, and the kernel entries are merged in as the
/// very last step. Consequently **there is no failure path after the merge** —
/// every `return None` below happens while `user_pt` still contains nothing
/// but tables this function allocated, which is exactly what
/// [`vmm::destroy_user_pagetable`] needs in order to free them safely.
fn load_elf_into(
    elf: &[u8],
    user_pt: usize,
    e_entry: u64,
    e_phoff: usize,
    e_phentsize: usize,
    e_phnum: usize,
) -> Option<ExecContext> {
    // Track the highest mapped virtual address for initial brk.
    let mut brk_va: usize = 0;

    // Does `e_entry` land in a mapped, file-backed, executable segment?
    // Nothing validated it before, and it is handed straight to `sret_to_user`
    // as sepc: an ELF could name any address at all — an unmapped page, the
    // stack, the vDSO — and the SRET would fault immediately in U-mode with
    // the kernel treating it as a fatal user trap.
    let mut entry_ok = false;

    // End (`p_vaddr + p_memsz`, unaligned) of the last accepted segment. The
    // page-reuse branch below *documents* that segments arrive in ascending
    // vaddr order, and then relies on it; nothing checked it. See
    // `elf_bounds::check_pt_load`.
    let mut prev_seg_end: usize = 0;

    for i in 0..e_phnum {
        // Bounded ph offset — `i * e_phentsize` must not overflow, and
        // the program header itself (56 bytes) must fit entirely in the
        // ELF blob. Even if e_phentsize > 56, we only read 56 bytes.
        //
        // These reject the image instead of `break`ing out of the loop:
        // breaking meant a truncated or bogus `e_phoff`/`e_phentsize` loaded
        // however many segments happened to fit and then SRET'd into a
        // half-built address space, which is a far worse outcome than a
        // failed exec.
        let ph = match e_phoff.checked_add(i.checked_mul(e_phentsize)?) {
            Some(p) => p,
            None    => return None,
        };
        if ph.checked_add(56).map_or(true, |end| end > elf.len()) { return None; }

        if r32(elf, ph) != PT_LOAD { continue; }

        let p_flags  = r32(elf, ph + 4);
        let p_offset = r64(elf, ph + 8)  as usize;
        let p_vaddr  = r64(elf, ph + 16) as usize;
        let p_filesz = r64(elf, ph + 32) as usize;
        let p_memsz  = r64(elf, ph + 40) as usize;

        // Sanity bounds on user-supplied ELF fields. Without these a
        // malicious ELF can:
        //   - set p_vaddr = 0 → the null-guard page mapped for real, which
        //     un-does `handle_demand_fault`/`handle_cow_fault`'s refusal to
        //     resolve anything below `vmm::USER_GUARD_LIMIT` (a task that
        //     jumps through a null pointer goes back to executing zeros)
        //   - set p_vaddr over kernel MMIO or over the stack/vDSO (see
        //     USER_LOW_MAX) → S-mode mappings clobbered in the user PT
        //   - set p_memsz huge → infinite alloc loop / OOM kernel
        //   - set p_filesz > p_memsz → unspecified by ELF spec, refuse
        //   - set p_offset+p_filesz > elf.len() → OOB read
        //   - emit segments out of vaddr order → a later segment's file bytes
        //     rewritten over an earlier segment's already-mapped page
        // All of them live in `elf_bounds` so they can actually be tested;
        // nothing about them is duplicated here.
        let (va_start, va_end) = match elf_bounds::check_pt_load(
            p_offset, p_vaddr, p_filesz, p_memsz,
            elf.len(), prev_seg_end, seg_limits(),
        ) {
            elf_bounds::SegCheck::Empty     => continue,
            elf_bounds::SegCheck::Reject(_) => return None,
            elf_bounds::SegCheck::Load(r)   => {
                prev_seg_end = r.seg_end;
                (r.va_start, r.va_end)
            }
        };

        // Entry point must sit in the *file-backed* part of a segment this
        // loader actually maps executable.
        //
        // `p_memsz` would be too loose: its tail is the zero-filled BSS, and
        // an entry there executes zeros (illegal instruction). And `PF_X`
        // alone would be too loose in the other direction — the flag
        // derivation below maps any writable segment as USER_RW with no EXEC
        // bit, so an `RWX` segment (p_flags = 7) advertises X in the header
        // but faults on instruction fetch. The check has to agree with the
        // mapper, not with the ELF header.
        if p_flags & PF_X != 0 && p_flags & PF_W == 0 {
            let e = e_entry as usize;
            if e >= p_vaddr && e < p_vaddr.saturating_add(p_filesz) {
                entry_ok = true;
            }
        }

        // W^X, in both directions.
        //
        // This used to be a two-way split: writable → RW, everything else →
        // **RX**. So a plain read-only segment (`p_flags = PF_R`, no PF_X) was
        // mapped executable, and that was not merely loose — it was an RWX
        // hole reachable from ring 3. A crafted image with a writable segment
        // followed by an `R`-only segment sharing its page hit the reuse
        // branch below with `add = USER_RX`; `add_user_leaf_perms` only
        // refuses WRITE-onto-EXEC, so EXEC-onto-WRITE went through and the
        // page ended up R+W+X. Ring 3 could then write instructions into it
        // and jump there, with the entry-point check satisfied by a separate,
        // well-formed PF_X segment.
        //
        // Three-way now, and `.rodata` loses the X bit it never needed:
        // `userspace/*/user.ld` puts all code in `.text` and page-aligns the
        // first writable byte, so the only real-image page sharing is
        // `.rodata` on the `.text` page — which still resolves to "already
        // sufficient" in `add_user_leaf_perms` and is left RX.
        let flags = if p_flags & PF_W != 0 {
            PteFlags::USER_RW | PteFlags::ACCESSED | PteFlags::DIRTY
        } else if p_flags & PF_X != 0 {
            PteFlags::USER_RX | PteFlags::ACCESSED
        } else {
            PteFlags::USER_RO | PteFlags::ACCESSED
        };

        if va_end > brk_va { brk_va = va_end; }

        let mut va = va_start;
        while va < va_end {
            // A previous PT_LOAD segment may already have mapped this page:
            // Rust ELFs emit separate RX (.text) / R (.rodata) / RW (.data)
            // LOAD segments that frequently share a boundary page. Mapping a
            // fresh page per segment let the later segment clobber the earlier
            // one — e.g. an R-only .rodata page overwriting the RX .text page,
            // silently dropping the X bit so the very first user instruction
            // faulted. Reuse the existing physical page instead; segments are
            // ordered by ascending vaddr so the executable text maps first and
            // its RX flags (which already permit reads) cover the shared page.
            //
            // The reuse lookup uses `translate_user`, not the permission-blind
            // `translate`: the latter resolves *any* valid PTE, including the
            // kernel's identity-mapped MMIO. An ELF with `p_vaddr =
            // 0x0200_4000` therefore got the CLINT's physical address handed
            // back and the memcpy below wrote attacker-chosen file bytes into
            // CLINT registers in S-mode. `translate_user` requires
            // VALID + USER + READ at every leaf level, so a kernel/MMIO leaf
            // resolves to `None` and never becomes a memcpy destination.
            //
            // `write = false` is deliberate. The page being reused is
            // typically the RX `.text` page a following `.rodata` segment
            // shares; asking for WRITE would reject it and defeat the whole
            // reuse branch. Write permission is not the question here — these
            // are pages this function allocated moments ago and still owns,
            // and their final PTE flags are what user mode will be held to.
            let phys = match vmm::translate_user(user_pt, va, false) {
                Some(existing) => {
                    // The page is already mapped by an earlier segment. Its
                    // flags were fixed then and never revisited, which is how
                    // a `.rodata` segment ending mid-page could leave the
                    // following `.data`/`.bss` segment read-only: the first
                    // store to a `static mut` there faulted. `abitest` hit
                    // exactly that; `captest` shares the layout and survived
                    // only because it never writes its counter.
                    //
                    // Widen to whatever THIS segment needs. W^X is preserved
                    // inside `add_user_leaf_perms`: an `.rodata`/`.data`
                    // overlap is repairable, a `.text`/`.data` overlap is
                    // refused rather than silently made writable-executable.
                    //
                    // The other half of W^X is ours: `add_user_leaf_perms`
                    // refuses WRITE onto an EXEC leaf, but nothing there
                    // refuses EXEC onto a WRITE leaf. An executable segment
                    // therefore never gets to reuse a page some earlier
                    // segment already mapped — it must own its first page
                    // outright. Real images satisfy this trivially (the RX
                    // segment is the first one and starts page-aligned at
                    // 0x10000 in all 12 binaries under `build/`); a crafted
                    // one that puts a writable segment first fails closed.
                    if flags.contains(PteFlags::EXEC) {
                        return None;
                    }
                    if vmm::add_user_leaf_perms(user_pt, va, flags).is_err() {
                        return None;
                    }
                    existing
                }
                None => {
                    let page = pmm::alloc_page().ok()?;
                    let p = page.as_usize();
                    // A map failure here means the VA is already occupied by
                    // something we cannot write through (only reachable if the
                    // ordering invariant above is ever broken). Ignoring it
                    // used to leak `page` *and* leave the copy below writing
                    // into a frame that is in no page table at all.
                    if vmm::map(user_pt, va, p, flags).is_err() {
                        let _ = pmm::free_page(page);
                        return None;
                    }
                    p
                }
            };

            // Copy the intersection of this page [va, va+PAGE) with the
            // segment's file-backed range [p_vaddr, p_vaddr+p_filesz) into the
            // page at the correct *destination offset*. When a segment starts
            // mid-page (p_vaddr > va, e.g. .rodata sharing the .text page), its
            // bytes must land at `p_vaddr - va` within the page — not at offset
            // 0, which previously clobbered the preceding segment's code.
            let page_end     = va.saturating_add(PAGE_SIZE);
            let seg_file_end = p_vaddr.saturating_add(p_filesz);
            let copy_start   = va.max(p_vaddr);
            let copy_end     = page_end.min(seg_file_end);
            if copy_start < copy_end {
                let dest_off = copy_start - va;        // offset within the page
                let seg_off  = copy_start - p_vaddr;   // offset within the segment
                let src_off  = p_offset.saturating_add(seg_off);
                let copy_n   = copy_end - copy_start;
                if let (Some(src_end), Some(dst_end)) =
                    (src_off.checked_add(copy_n), dest_off.checked_add(copy_n))
                {
                    if src_end <= elf.len() && dst_end <= PAGE_SIZE {
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                elf.as_ptr().add(src_off),
                                (phys + dest_off) as *mut u8,
                                copy_n,
                            );
                        }
                    }
                }
            }
            va += PAGE_SIZE;
        }
    }

    // Reject an entry point we cannot vouch for. RISC-V fetches on 2-byte
    // boundaries (compressed instructions), so an odd address is malformed by
    // construction.
    //
    // `e_entry` needs no bound of its own, above or below: `entry_ok` is only
    // ever set from inside a segment that already passed
    // `elf_bounds::check_pt_load`, so it is transitively confined to
    // `USER_GUARD_LIMIT..USER_LOW_MAX`. That is worth stating because it is
    // the only ELF-supplied address here that is *not* checked directly —
    // it is handed straight to `sret_to_user` as sepc.
    if !entry_ok || (e_entry & 1) != 0 {
        return None;
    }

    // User stack
    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    let mut va = stack_bottom;
    while va < USER_STACK_TOP {
        let page = pmm::alloc_page().ok()?;
        if vmm::map(
            user_pt, va, page.as_usize(),
            PteFlags::USER_RW | PteFlags::ACCESSED | PteFlags::DIRTY,
        ).is_err() {
            let _ = pmm::free_page(page);
            return None;
        }
        va += PAGE_SIZE;
    }

    // Map vDSO page (read-only) so user-space can read kernel time data
    // without issuing an ecall.
    let vdso_phys = vdso::vdso_phys();
    if vdso_phys != 0 {
        // A failure here is not fatal — the vDSO is an optimisation and
        // userspace falls back to the ecall path. It cannot collide with the
        // image either (USER_LOW_MAX) or the stack (different VPN[1]).
        let _ = vmm::map(
            user_pt,
            vdso::VDSO_USER_BASE,
            vdso_phys,
            PteFlags::USER_RO | PteFlags::ACCESSED,
        );
    }

    // ── Kernel entries — LAST, and after this point nothing may fail ────────
    //
    // Refuse the image if merging the kernel's mappings would have to drop one
    // of them because a user mapping already owns the slot. Losing, say, the
    // CLINT in this address space is not a load-time error: the process starts
    // and then the first timer interrupt taken under its SATP faults in S-mode
    // on an address the kernel believes is identity-mapped. Checking before
    // the copy also keeps the teardown above valid — it must never see a page
    // table holding pointers into the kernel's own tables.
    if let Some((vpn2, vpn1)) = vmm::kernel_entry_collision(user_pt) {
        robot_os_drivers::kprintln!(
            "[EXEC] refused: image occupies kernel slot VPN2={} VPN1={}",
            vpn2, vpn1,
        );
        return None;
    }

    // Copy kernel L2/L1 entries into the user PT so that the trap handler
    // (trap_vector at ~0x80200000) and MMIO (UART, CLINT, etc.) are
    // reachable in S-mode when an ecall fires with the user PT active.
    // Kernel pages have no USER bit — U-mode cannot access them directly.
    vmm::copy_kernel_entries_to_user(user_pt);

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
        // K-C18: zero EVERY general-purpose register, not just a0-a7.
        //
        // **WHY the old version was a live kernel-pointer leak.** Only the
        // argument registers were cleared, so a fresh process entered its ELF
        // entry point with `ra`, `gp`, `t0..t6` and `s0..s11` still holding
        // whatever the kernel task that ran the loader had left in them.
        // Observed in a real run: `regs[1] (ra) = 0x8020198a`, a kernel text
        // address, handed to ring 3 at every exec. That defeats any layout
        // randomisation and gives an unprivileged task a free oracle.
        //
        // It was also a crash. Nothing in the RISC-V ELF entry ABI defines
        // these registers, so a program is entitled to `ret` from a leaf that
        // never set `ra` — and that jumps straight into kernel text. Measured:
        // `[PAGE FAULT] Instruction page fault at 0x80244110` (inside
        // `_start`..`_text_end`), task killed. The guard held, so this was
        // never an escalation, but the pointer had no business being there.
        //
        // **And K-C11 made it inheritable.** Now that fork faithfully copies
        // the parent's whole register file to the child, a parent that never
        // overwrote its garbage `ra` passes it on. Fixing the loader is what
        // stops the garbage existing in the first place; the fork path is
        // correct to copy whatever it finds.
        //
        // `sp` is set above and `tp` is deliberately left holding the kernel's
        // hart id — see `sret_to_user_forked` for why touching it is the one
        // change that would reintroduce K-A11.
        "li ra,0", "li gp,0",
        "li t0,0","li t1,0","li t2,0","li t3,0","li t4,0","li t5,0","li t6,0",
        "li s0,0","li s1,0","li s2,0","li s3,0","li s4,0","li s5,0",
        "li s6,0","li s7,0","li s8,0","li s9,0","li s10,0","li s11,0",
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

/// SRET into user mode restoring a forked child's **complete** register file
/// (K-C11). Fork-only; a fresh `exec` still uses [`sret_to_user`].
///
/// `regs` is the parent's trap frame in `x0..x31` order. It must point at
/// memory reachable *after* `satp` is switched — in practice the caller's
/// kernel stack; see `scheduler::take_current_task_fork_ctx`.
///
/// ## What is deliberately NOT restored
///
/// **`tp` (x4).** In this kernel `tp` carries the hart id: `current_cpu_id()`
/// reads it, and `trap_entry.S` saves whatever `tp` user mode had and never
/// re-establishes a kernel value — so after any trap from U-mode the kernel
/// runs on the user's `tp`. A child that inherited its *parent's* `tp` and then
/// got dispatched on a different hart would make the kernel address another
/// CPU's `PER_CPU` state on its very first syscall — the K-A11 corruption,
/// reintroduced by the fix. Leaving the kernel `tp` this entry already runs
/// with is both correct for the hart the child is on and identical to the
/// behaviour before this change. There is no TLS in this tree for user `tp` to
/// mean anything else.
///
/// (That `trap_entry.S` restores a user-controlled `tp` into kernel context at
/// all is a separate finding, not created here and not fixed here: ring 3 can
/// write `tp` and make the kernel misidentify its own hart. See the audit.)
///
/// **`x0`** is hardwired zero, and **`a0`** is forced to 0 last: `fork()`
/// returns 0 in the child, and that must survive the restore.
///
/// # Safety
/// Switches `satp` and never returns. `entry`, `satp` and `regs` must all
/// describe the same, fully-published child context.
pub unsafe fn sret_to_user_forked(entry: usize, satp: usize, regs: &[u64; 32]) -> ! {
    let sspie: usize = 1 << 5; // SPIE — interrupts enabled in U-mode, SPP=0
    let base = regs.as_ptr();
    core::arch::asm!(
        // CSRs first: every asm input is consumed before any GPR is clobbered.
        "csrw  sepc, {entry}",
        "csrw  sstatus, {sspie}",
        "csrw  sscratch, sp",        // kernel SP, for the next trap's re-entry
        "csrw  satp, {satp}",
        "sfence.vma zero, zero",
        // t0 is the base pointer from here on; it is reloaded from its own
        // slot as the very last GPR so the block stays addressable throughout.
        "mv    t0, {base}",
        "ld    x1,   8(t0)",         // ra
        "ld    x2,  16(t0)",         // sp  — user stack, from the frame itself
        "ld    x3,  24(t0)",         // gp
        // x4 (tp) deliberately skipped — see the doc above.
        "ld    x6,  48(t0)",         // t1
        "ld    x7,  56(t0)",         // t2
        "ld    x8,  64(t0)",         // s0/fp
        "ld    x9,  72(t0)",         // s1
        "ld    x10, 80(t0)",         // a0 (overwritten with 0 below)
        "ld    x11, 88(t0)",         // a1
        "ld    x12, 96(t0)",         // a2
        "ld    x13, 104(t0)",        // a3
        "ld    x14, 112(t0)",        // a4
        "ld    x15, 120(t0)",        // a5
        "ld    x16, 128(t0)",        // a6
        "ld    x17, 136(t0)",        // a7
        "ld    x18, 144(t0)",        // s2
        "ld    x19, 152(t0)",        // s3
        "ld    x20, 160(t0)",        // s4
        "ld    x21, 168(t0)",        // s5
        "ld    x22, 176(t0)",        // s6
        "ld    x23, 184(t0)",        // s7
        "ld    x24, 192(t0)",        // s8
        "ld    x25, 200(t0)",        // s9
        "ld    x26, 208(t0)",        // s10
        "ld    x27, 216(t0)",        // s11
        "ld    x28, 224(t0)",        // t3
        "ld    x29, 232(t0)",        // t4
        "ld    x30, 240(t0)",        // t5
        "ld    x31, 248(t0)",        // t6
        "ld    x5,  40(t0)",         // t0 — base register, restored last
        "li    a0, 0",               // fork() returns 0 in the child
        "sret",
        entry = in(reg) entry,
        sspie = in(reg) sspie,
        satp  = in(reg) satp,
        base  = in(reg) base,
        options(noreturn),
    )
}

// ── User-space memory access ──────────────────────────────────────────────────

/// Copy `len` bytes FROM user virtual address `user_src` INTO kernel buffer `kernel_dst`.
///
/// When the current task has a user PT (user_pt != 0), each source page is
/// translated via [`vmm::translate_user`] — which enforces `VALID + USER +
/// READ`, rejecting kernel/MMIO addresses — and copied from the physical
/// address a page at a time.  For kernel tasks (user_pt == 0) the pointer is
/// trusted and used directly; that path is unreachable from U-mode (a task
/// that ran an ELF via `exec_user`, or a forked child, always has
/// `user_pt != 0` set before it can issue a syscall).
///
/// Hostile-input handling (must never panic under `overflow-checks = true`):
///   - `len == 0` → success, no walk.
///   - `user_src + len` wrapping the address space → reject.
///   - a range that starts in valid user memory and crosses into an unmapped
///     or non-USER page → reject at that page (whole-range validation).
///   - NULL / near-NULL → the zero page is not USER-mapped → reject.
///
/// Returns `true` on success, `false` on any unmapped/forbidden page.
pub fn copy_from_user(kernel_dst: *mut u8, user_src: usize, len: usize) -> bool {
    if len == 0 { return true; }
    // Reject a length that wraps `user_src` past the end of the address space.
    // Once this holds, every `user_src + done` below (done < len) is provably
    // non-overflowing.
    if user_src.checked_add(len).is_none() { return false; }

    let user_pt = crate::scheduler::current_user_pt();
    if user_pt == 0 {
        // Kernel task — identity-mapped; pointer is trusted.
        unsafe { core::ptr::copy_nonoverlapping(user_src as *const u8, kernel_dst, len); }
        return true;
    }
    let mut done = 0usize;
    while done < len {
        let va   = user_src + done;
        let Some(pa) = vmm::translate_user(user_pt, va, false) else { return false; };
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
/// Each destination page is validated via [`vmm::translate_user`] with
/// `write = true`, which enforces `VALID + USER + WRITE` and rejects
/// kernel/MMIO addresses. A copy-on-write page (shared read-only after
/// `fork`) is broken into a private copy before the write, so a legitimate
/// post-fork write lands in the caller's own page instead of the previously
/// unchecked path silently writing the page still shared with the parent.
///
/// Same hostile-input handling as [`copy_from_user`]: `len == 0` → success,
/// wrapping range → reject, cross into non-USER/unmapped page → reject,
/// NULL → reject. Never panics.
///
/// Returns `true` on success, `false` on any forbidden/unmapped page.
pub fn copy_to_user(user_dst: usize, kernel_src: *const u8, len: usize) -> bool {
    if len == 0 { return true; }
    if user_dst.checked_add(len).is_none() { return false; }

    let user_pt = crate::scheduler::current_user_pt();
    if user_pt == 0 {
        unsafe { core::ptr::copy_nonoverlapping(kernel_src, user_dst as *mut u8, len); }
        return true;
    }
    let mut done = 0usize;
    while done < len {
        let va   = user_dst + done;
        let Some(pa) = vmm::translate_user(user_pt, va, true) else { return false; };
        let chunk = (PAGE_SIZE - (va & (PAGE_SIZE - 1))).min(len - done);
        unsafe {
            core::ptr::copy_nonoverlapping(kernel_src.add(done), pa as *mut u8, chunk);
        }
        done += chunk;
    }
    true
}

/// Copy a NUL-terminated C string from user space into `dst` (at most
/// `dst.len()` bytes including the NUL). Returns the number of bytes copied
/// (excluding the NUL) on success, or `None` on fault / missing terminator /
/// forbidden address.
///
/// For user tasks this walks the page table **once per page** (not once per
/// byte): a page is resolved via [`vmm::translate_user`] (enforcing
/// `VALID + USER + READ`, so kernel/MMIO source addresses are rejected), then
/// scanned for the NUL up to the page boundary, re-walking only when the
/// scan crosses into the next page. `user_ptr + len` is guarded with
/// `checked_add` so a near-`usize::MAX` pointer rejects instead of panicking
/// under `overflow-checks = true`.
pub fn copy_cstr_from_user(dst: &mut [u8], user_ptr: usize) -> Option<usize> {
    let user_pt = crate::scheduler::current_user_pt();
    let mut len = 0usize;

    if user_pt == 0 {
        // Kernel task — trusted, identity-mapped. Bounded by `dst`.
        loop {
            if len >= dst.len() { return None; }
            let va = user_ptr.checked_add(len)?;
            let b = unsafe { *(va as *const u8) };
            dst[len] = b;
            if b == 0 { return Some(len); }
            len += 1;
        }
    }

    // User task — resolve and scan one page at a time.
    loop {
        if len >= dst.len() { return None; }
        let va = user_ptr.checked_add(len)?;
        let pa = vmm::translate_user(user_pt, va, false)?;
        // Bytes remaining in this physical page starting at `va`.
        let page_remaining = PAGE_SIZE - (va & (PAGE_SIZE - 1));
        let mut off = 0usize;
        while off < page_remaining {
            if len >= dst.len() { return None; }
            let b = unsafe { *((pa + off) as *const u8) };
            dst[len] = b;
            if b == 0 { return Some(len); }
            len += 1;
            off += 1;
        }
        // Crossed the page boundary; next iteration re-walks the next page.
    }
}

// ── sys_brk ───────────────────────────────────────────────────────────────────

/// Implement the brk(2) syscall: extend or query the user heap.
///
/// - `addr == 0`: return current brk
/// - `addr > current_brk`: allocate new pages, advance brk, return new brk
/// - `addr < current_brk` (shrink): unsupported in Phase 7, return current brk
///
/// Hostile-input handling — `addr` comes straight from a ring-3 register:
///   - **No arithmetic may overflow.** With `panic = "abort"` +
///     `overflow-checks = true` an overflow is not a wrong answer, it is a
///     board reset: on a robot, a physical-safety event. `brk(u64::MAX)`
///     cleared every guard above and then overflowed the page-round-up.
///     [`page_up`] now saturates.
///   - **The heap is bounded** by [`USER_LOW_MAX`]. Unbounded, a single
///     `brk(0x7FFF_FFFF)` walks ~500K pages: it drains the PMM, and on the way
///     it maps over the kernel's MMIO slots and (further up) the user stack.
///   - **Mapping failures are not swallowed.** The old `let _ = vmm::map(..)`
///     dropped `AlreadyMapped` on the floor, leaking the page it had just
///     allocated on every repeated call over the same range.
pub fn sys_brk_impl(addr: u64) -> i64 {
    let user_pt = crate::scheduler::current_user_pt();
    if user_pt == 0 { return -1; } // kernel task

    let cur_brk = crate::scheduler::update_user_brk(0); // query
    if addr == 0 || addr == cur_brk { return cur_brk as i64; }
    if addr < cur_brk { return cur_brk as i64; } // shrink not supported

    // Saturating round-up: `u64::MAX` lands on `0xFFFF_FFFF_FFFF_F000`, which
    // the ceiling below then rejects — no wrap, no panic, no allocation.
    let new_brk = page_up(addr as usize) as u64;
    if new_brk > USER_LOW_MAX as u64 { return cur_brk as i64; }

    // Commit whatever we managed to map, so a partial extension is reported
    // honestly instead of handing back pages the caller cannot see.
    let commit = |va: usize| -> i64 {
        if va as u64 > cur_brk {
            crate::scheduler::update_user_brk(va as u64) as i64
        } else {
            cur_brk as i64
        }
    };

    // Allocate pages from cur_brk to addr. `va < new_brk <= USER_LOW_MAX`, so
    // the `va += PAGE_SIZE` below cannot overflow either.
    let mut va = page_up(cur_brk as usize);
    while (va as u64) < new_brk {
        let page = match pmm::alloc_page() {
            Ok(p) => p,
            Err(_) => return commit(va), // OOM — keep what we mapped
        };
        match vmm::map(
            user_pt, va, page.as_usize(),
            PteFlags::USER_RW | PteFlags::ACCESSED | PteFlags::DIRTY,
        ) {
            Ok(()) => {}
            Err(KernelError::AlreadyMapped) => {
                // Already backed (an image page overlapping the first heap
                // page, or a re-issued brk over the same range). Hand the
                // fresh frame straight back — the old code leaked it.
                let _ = pmm::free_page(page);
            }
            Err(_) => {
                let _ = pmm::free_page(page);
                return commit(va);
            }
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
///
/// K-A15: `sepc`/`user_sp` are the trap-time values from the parent's own
/// ecall — passed as plain parameters (not read back from shared state) so
/// they're inherently hart-local: the value this specific `sys_fork_impl`
/// call sees can never be another hart's concurrent syscall's sepc/user_sp.
pub fn sys_fork_impl(sepc: u64, user_sp: u64, regs: &[u64; 32]) -> i64 {
    let parent_pt = crate::scheduler::current_user_pt();
    if parent_pt == 0 { return -1; } // kernel task can't fork

    // AQ9: Copy-on-Write fork — share all user pages read-only instead of
    // copying them eagerly.  The COW fault handler allocates new pages on write.
    let child_pt = match vmm::fork_cow(parent_pt) {
        Ok(pt) => pt,
        Err(_) => return -1,
    };

    // Copy kernel entries so traps work in the child.
    //
    // Ordering here is already the correct one and must stay that way:
    // `fork_cow` has populated the child's own L1/L0 tables for every user
    // page, so this merges into a PT that owns its tables (see the ordering
    // invariant on `vmm::copy_kernel_entries_to_user` and the matching tail of
    // `load_elf_into`). The collision check is a formality — the child's user
    // layout is a copy of the parent's, which passed the same check at exec —
    // but a dropped kernel entry would be just as fatal here, and refusing the
    // fork is recoverable where a child with no CLINT is not.
    if let Some((vpn2, vpn1)) = vmm::kernel_entry_collision(child_pt) {
        robot_os_drivers::kprintln!(
            "[FORK] refused: child occupies kernel slot VPN2={} VPN1={}",
            vpn2, vpn1,
        );
        vmm::destroy_user_pagetable(child_pt);
        return -1;
    }
    vmm::copy_kernel_entries_to_user(child_pt);

    // Get parent's brk to set in child.
    let parent_brk = crate::scheduler::update_user_brk(0);

    // Create child task. We use a trampoline that just yields forever — the real
    // entry will be set via the pending exec mechanism when we SRET.
    //
    // K-A13: fork() is reachable from unprivileged userspace in an unbounded
    // loop (fork-bomb). `task_create` panics — a full board reset under this
    // profile's `panic = "abort"` — when the task pool is exhausted, so this
    // MUST use the fallible variant and report -1 (matches the existing
    // fork-failure convention just above), not let the kernel abort.
    //
    // Both failure exits below still own `child_pt` outright (nothing
    // references it until `set_task_user_info` publishes it), so they must
    // release it — otherwise a fork-bomb that exhausts the task pool leaks a
    // full COW page table per attempt and turns a bounded, recoverable denial
    // into permanent PMM exhaustion.
    let child_idx = match crate::try_task_create_affinity(
        "forked", fork_child_entry, 0, crate::DEFAULT_PRIORITY, -1,
    ) {
        Some(idx) => idx,
        None => {
            vmm::destroy_user_pagetable(child_pt);
            return -1;
        }
    };

    // Capture the child's TID immediately. Safe against slot reuse: the child
    // cannot have exited yet — `fork_child_entry` never exits before consuming
    // the fork context, which is only published below — so the slot still
    // belongs to it. The TID is what identifies the child from here on
    // (`set_task_fork_ctx` re-checks it under POOL_LOCK) and is also the
    // correct return value: the pool INDEX (returned previously) can
    // legitimately be 0 for a reused slot 0, which the parent would
    // misinterpret as "I am the child" — and it also disagreed with
    // `sys_getpid`, which reports TIDs.
    let child_tid = match crate::tid_for_idx(child_idx) {
        Some(tid) => tid,
        None => {
            // Unreachable: the slot was just allocated. Still release the page
            // table — it is not yet published on any task.
            vmm::destroy_user_pagetable(child_pt);
            return -1;
        }
    };

    // Apply the child's user page table so context_switch.S writes the
    // correct SATP when the child is scheduled.
    let child_satp = make_satp(child_pt, crate::alloc_asid()) as u64;
    crate::scheduler::set_task_user_info(child_idx, child_satp, child_pt as u64, parent_brk);

    // AQ11: Inherit parent's syscall filter — child cannot be less restricted.
    let parent_filter = crate::scheduler::current_syscall_filter();
    crate::scheduler::set_task_syscall_filter(child_idx, parent_filter);

    // Publish the hand-off on the child's OWN task slot — sepc+4 (skip the
    // ecall instruction) as its entry PC, the parent's user SP, and the
    // child's own SATP. See the K-A15 doc above and on `Task::fork_ctx_ready`.
    // Identity-checked against `child_tid` under POOL_LOCK; failure means the
    // slot no longer belongs to our child, which cannot happen while the child
    // is waiting (it never exits before consuming) — defensive only.
    if !crate::scheduler::set_task_fork_ctx(
        child_idx, child_tid, sepc + 4, user_sp, child_satp, regs,
    ) {
        // Reachable only if the child's slot stopped being the child's
        // (defensive — see above). `child_pt` is NOT destroyed here, unlike
        // the earlier failure exits, because at this point it has already
        // been PUBLISHED on the slot by `set_task_user_info`, and the
        // identity check failing means the slot moved on without us — in one
        // of two states we cannot tell apart:
        //  (a) already reused: the claim in `try_task_create_affinity`
        //      captured that `user_pt` and destroyed it (K-C22(B)) —
        //      destroying again here would be a double-free;
        //  (b) freed but not yet reused: `user_pt` still holds `child_pt` on
        //      the dead slot, and the next claim of that slot destroys it.
        // Either way the reuse-time reclaim owns the teardown; abandoning
        // the PT here leaks nothing.
        return -1;
    }

    child_tid as i64
}

/// Number of `fork_child_entry` wait iterations after which a diagnostic is
/// printed. The parent's remaining work after `try_task_create_affinity`
/// returns (a handful of plain field writes) is O(1) and non-blocking, so the
/// publish normally lands within a few yields — but a child dispatched onto an
/// idle hart burns yield iterations in microseconds, so a parent held off for
/// even a few ms (preempted by RT/deadline work, spinning on a contended lock)
/// can exceed any small bound. This is therefore a log threshold, NOT an exit
/// bound: exiting here would leak the child's COW page table and silently
/// break fork's contract (the parent has already been promised a child TID).
const FORK_CTX_WAIT_ITERS: u32 = 1000;

/// Fork child entry point.  Reads the saved context and SRETs to user
/// mode with a0=0 (the fork return value for the child process).
fn fork_child_entry(_arg: usize) {
    // K-A15: the parent may not have finished publishing our fork context
    // yet if we were dispatched on another (idle) hart immediately after
    // try_task_create_affinity() enqueued us — yield until it lands. This
    // wait terminates: the parent's path from task creation to
    // `set_task_fork_ctx` is straight-line, non-blocking code with no early
    // return, and while we wait our slot stays valid with our TID, so the
    // identity-checked publish is guaranteed to reach us. Yielding keeps the
    // hart available to other work in the meantime.
    let mut waited: u32 = 0;
    loop {
        if let Some((entry, _user_sp, satp, regs)) =
            crate::scheduler::take_current_task_fork_ctx()
        {
            // K-C11: SRET into user mode restoring the parent's *whole*
            // register file, not just pc/sp. `regs` is a by-value copy living
            // on this function's kernel stack — see
            // `take_current_task_fork_ctx` for why it must not be a pointer
            // into `TASKS`.
            //
            // `user_sp` is deliberately unused: the stack pointer is `regs[2]`
            // and comes back with everything else. Restoring it from a second,
            // independent source is how the two quietly drift apart.
            unsafe { sret_to_user_forked(entry as usize, satp as usize, &regs); }
        }
        if waited == FORK_CTX_WAIT_ITERS {
            robot_os_drivers::kprintln!(
                "[FORK] child tid {} still waiting for fork ctx (parent hart stalled?)",
                crate::current_task_tid(),
            );
        }
        waited = waited.saturating_add(1);
        crate::task_yield();
    }
}

// ── MMIO mapping for userspace drivers (F00.2) ──────────────────────────────

/// Base virtual address for user-space MMIO mappings.
/// Placed at 1.5 GiB, below the stack at 2 GiB, above typical code/heap.
const USER_MMIO_BASE: usize = 0x0000_0000_6000_0000; // 1.5 GiB

/// Maximum size of a single MMIO mapping (1 MiB).
const USER_MMIO_MAX_SIZE: usize = 1024 * 1024;

/// Hard ceiling for the MMIO/shm VA window: the bottom of the user stack.
///
/// [`MMIO_NEXT_VA`] only ever grows and is shared by *every* process, so the
/// window is consumed system-wide and never reclaimed. Unbounded, roughly 512
/// cumulative 1 MiB mappings walk it past the stack and then past
/// `0x8000_0000` — the VPN[2]=2 slot that
/// [`vmm::copy_kernel_entries_to_user`] grafts in wholesale, meaning the L2
/// entry there points at the *kernel's own* L1 table. A `vmm::map` at such a
/// VA would allocate an L0 table inside the kernel page table and publish
/// USER leaves in it: the same address-space corruption the loader ordering
/// fix removes, arriving through a different door and accumulating across the
/// lifetime of the board rather than per process.
const USER_MMIO_LIMIT: usize = USER_STACK_TOP - USER_STACK_SIZE;

/// Next free MMIO virtual address (grows upward).
static MMIO_NEXT_VA: AtomicU64 = AtomicU64::new(USER_MMIO_BASE as u64);

/// Reserve `pages` consecutive pages from the shared MMIO/shm VA window.
///
/// Returns `None` once the window is exhausted. The reservation is consumed
/// even on refusal — the window is monotonic by design and there is nothing to
/// roll back to — so exhaustion is permanent and every later caller is denied
/// too. That is the intended outcome: handing out VAs past
/// [`USER_MMIO_LIMIT`] corrupts the kernel page table (see there), while
/// denying them merely fails a driver mapping.
fn reserve_mmio_va(pages: usize) -> Option<usize> {
    let span = pages.checked_mul(PAGE_SIZE)?;
    let base = MMIO_NEXT_VA.fetch_add(span as u64, Ordering::Relaxed) as usize;
    let end  = base.checked_add(span)?;
    if end > USER_MMIO_LIMIT { return None; }
    Some(base)
}

/// Map a shared memory region (F00.4) into the current task's user page table.
///
/// Takes the physical pages directly from the caller to avoid a dependency on
/// `robot_os_ipc` (which already depends on `robot_os_sched` — would be circular).
///
/// - `phys_pages`: slice of physical page addresses to map contiguously.
/// - `rw`: true = read-write, false = read-only.
///
/// Returns the virtual base address, or None on failure.
pub fn shm_map_user(phys_pages: &[usize], rw: bool) -> Option<usize> {
    let user_pt = crate::scheduler::current_user_pt();
    if user_pt == 0 {
        return None; // kernel task
    }
    let page_count = phys_pages.len();
    if page_count == 0 {
        return None;
    }
    let va_base = reserve_mmio_va(page_count)?;
    let flags = if rw {
        PteFlags::USER_RW | PteFlags::ACCESSED | PteFlags::DIRTY
    } else {
        PteFlags::USER_RO | PteFlags::ACCESSED | PteFlags::DIRTY
    };
    for (i, &phys) in phys_pages.iter().enumerate() {
        let va = va_base + i * PAGE_SIZE;
        if vmm::map(user_pt, va, phys, flags).is_err() {
            return None; // partial mapping; caller responsible for shm_release
        }
    }
    Some(va_base)
}

/// Map a physical MMIO region into the current task's user page table.
/// Returns the virtual address in user space, or None on failure.
///
/// The mapping uses USER_RW flags (readable, writable, user-accessible, no exec).
/// A+D bits are pre-set to avoid software-managed A/D faults on MMIO.
pub fn mmio_map_user(phys_base: usize, size: usize) -> Option<usize> {
    if size == 0 || size > USER_MMIO_MAX_SIZE {
        return None;
    }
    let user_pt = crate::scheduler::current_user_pt();
    if user_pt == 0 {
        return None; // kernel task — no user page table
    }

    // Round size up to page boundary
    let size_pages = page_up(size) / PAGE_SIZE;

    // Allocate contiguous VA range
    let va_base = reserve_mmio_va(size_pages)?;

    // Map each page: physical MMIO directly into user PT with U+R+W+A+D flags
    let flags = PteFlags::USER_RW
        | PteFlags::ACCESSED
        | PteFlags::DIRTY;

    for i in 0..size_pages {
        let va = va_base + i * PAGE_SIZE;
        let pa = phys_base + i * PAGE_SIZE;
        if vmm::map(user_pt, va, pa, flags).is_err() {
            // Roll back and fail. The old `break` returned `Some(va_base)` for
            // a range that was only partially mapped, so the driver got a
            // pointer that faults somewhere in the middle of the region it was
            // told it owned. Unmapping is safe here: these are device frames
            // the PMM never owned, so nothing is freed.
            for j in 0..i {
                vmm::unmap(user_pt, va_base + j * PAGE_SIZE);
            }
            return None;
        }
    }

    Some(va_base)
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
/// Round `a` up to the next page boundary, saturating instead of wrapping.
///
/// `a + PAGE_SIZE - 1` overflows for any `a` within a page of `usize::MAX`,
/// and under this build profile (`overflow-checks = true`, `panic = "abort"`)
/// an overflow reboots the board. `sys_brk_impl` passes a raw ring-3 register
/// here, so that was a one-instruction reset available to unprivileged code.
/// Saturating yields `0xFFFF_FFFF_FFFF_F000`, which every caller's range check
/// rejects.
#[inline] fn page_up(a: usize) -> usize {
    a.saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}
