//! Syscall ABI conformance test for a ring-3 process.
//!
//! WHY THIS EXISTS
//!
//! On 2026-08-21 an audit found `crates/libsys` and the kernel handlers
//! disagreeing on the argument shape of a dozen syscalls. `exec` declared
//! `(entry_addr, stack_addr)` where `sys_exec` reads `(elf_ptr, elf_len)`.
//! `drv_mmap` passed `(phys, size)` where the dispatcher indexes
//! `(drv_id, mmio_idx)`. `disk_read` passed `(sector, buf, len)` where the
//! kernel reads `(sector, count, buf)`. `pipe` handed the kernel a
//! `[u64; 2]` for an `int[2]`. `trace_dump` and `drv_heartbeat` reached
//! syscalls that read `a0` through a `syscall0` that never writes it.
//!
//! Not one of those was caught by a test, a build, or a boot — because
//! **no userspace program called any of them.** They compiled, they were
//! documented, and they had never once been executed. That is the recurring
//! shape in this tree, and a wrapper nothing calls is a wrapper nothing
//! checks.
//!
//! This binary is the check. It calls the wrappers whose contract was
//! verified against `crates/syscall/src/dispatch.rs` and
//! `crates/syscall/src/handlers.rs`, and asserts the kernel returns what the
//! libsys doc comment promises. If either side drifts again, this fails on
//! the next `qemu-abitest` run instead of in three months on hardware.
//!
//! WHAT IT WILL NOT DO
//!
//!   * No destructive or one-way call. `seccomp` is irreversible and would
//!     break every later assertion; `shutdown`/`reboot` end the run;
//!     `disk_write`/`unlink` mutate the FAT32 image the other scenarios
//!     grep; `kill` targets a live task. None are called.
//!   * No call that succeeds by never returning. A successful `exec`
//!     replaces the address space — so `exec` and `execpath` are asserted
//!     only through their deterministic *failure* paths, which exercise the
//!     same argument decode.
//!   * No dependence on a disk or a NIC. `make qemu-smp` carries neither,
//!     so every assertion here holds with or without them.
//!
//! WHAT A FAILURE MEANS
//!
//! Each line prints the raw `rc`. Two different "denied" codes exist in this
//! kernel — `-99` from a handler-side `cap_check`, `-1` from a dispatch-side
//! one — so assertions test for *negativity* and print the code rather than
//! pinning a value that is not uniform. Pinning it would make the test
//! brittle; printing it makes the split visible.

#![no_std]
#![no_main]

use robot_os_libsys as sys;

static mut FAILURES: u32 = 0;
static mut CHECKS: u32 = 0;

fn report(name: &[u8], ok: bool, rc: isize) {
    sys::print(if ok { b"[ABITEST]   ok   " } else { b"[ABITEST]  FAIL  " });
    sys::print(name);
    sys::print(b" rc=");
    print_i(rc);
    sys::print(b"\n");
    // `overflow-checks = true` and `panic = "abort"`: a wrapping increment
    // here would reset the board. Saturating cannot.
    unsafe {
        CHECKS = CHECKS.saturating_add(1);
        if !ok {
            FAILURES = FAILURES.saturating_add(1);
        }
    }
}

/// Minimal signed-decimal print — no `core::fmt` in a no_std ring-3 binary.
/// Same shape as `userspace/captest`, including the `i64` widening that
/// keeps `isize::MIN` from overflowing on negation.
fn print_i(v: isize) {
    if v < 0 {
        sys::print(b"-");
    }
    let mut n = if v < 0 { (v as i64).unsigned_abs() } else { v as u64 };
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if n == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys::print(&buf[i..]);
}

/// The kernel must reject this call.
fn expect_err(name: &[u8], rc: isize) {
    report(name, rc < 0, rc);
}

/// The kernel must return exactly `want`.
fn expect_eq(name: &[u8], rc: isize, want: isize) {
    report(name, rc == want, rc);
}

/// The kernel must return something strictly positive.
fn expect_pos(name: &[u8], rc: isize) {
    report(name, rc > 0, rc);
}

/// A plain boolean assertion with no syscall return to show.
fn expect_true(name: &[u8], ok: bool) {
    report(name, ok, if ok { 0 } else { -1 });
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::println(b"[ABITEST] Starting - libsys/kernel ABI conformance");

    check_process();
    check_console_io();
    check_paths_and_nul();
    check_exec();
    check_pipe();
    check_disk_wrapper_bounds();
    check_trace_and_heartbeat();
    check_vdso();
    check_kernel_stubs();
    check_missing_dispatch_arms();

    let failed = unsafe { FAILURES };
    let total = unsafe { CHECKS };
    sys::print(b"[ABITEST] ");
    print_i(total as isize);
    sys::println(b" check(s) run");
    if failed == 0 {
        sys::println(b"[ABITEST] ALL PASSED");
        sys::exit(0);
    } else {
        sys::print(b"[ABITEST] FAILED: ");
        print_i(failed as isize);
        sys::println(b" check(s)");
        sys::exit(1);
    }
}

// ── Process identity and memory ─────────────────────────────────────────────

fn check_process() {
    // A ring-3 task always has a real tid. If this is <= 0 the process is not
    // where the rest of the test assumes it is.
    expect_pos(b"getpid() > 0", sys::getpid());

    // `sys_meminfo` returns FREE PAGES, not bytes. The old doc said "total
    // memory in bytes"; a page count on any bootable configuration is > 0
    // and far below what a byte count would be.
    expect_pos(b"meminfo() > 0 (free PAGES, not bytes)", sys::meminfo());

    // `brk(0)` queries the current break without moving it. A user task
    // always has one.
    expect_pos(b"brk(0) query > 0", sys::brk(0));

    // `sys_uptime` is the raw CLINT mtime counter, NOT milliseconds. The
    // only portable assertion is monotonicity.
    let t0 = sys::uptime();
    sys::yield_now();
    let t1 = sys::uptime();
    expect_true(b"uptime() monotonic (raw CLINT ticks)", t1 >= t0 && t0 > 0);

    // Yield must return cleanly to ring 3.
    expect_eq(b"yield_now() returns", { sys::yield_now(); 0 }, 0);

    // `sys_wait` is `-1 // Phase 8+`. Documented as unimplemented; assert it
    // so the doc stops being true silently if someone implements it.
    expect_err(b"wait() unimplemented -> negative", sys::wait());
}

// ── Console I/O byte counts ─────────────────────────────────────────────────

fn check_console_io() {
    // `sys_write` on fd 1/2 returns the count it was GIVEN (`return chunk as
    // i64` after `write_str_translated`), not the number of bytes the UART
    // emitted — LF-to-CRLF translation does not inflate the result. Since
    // MSG is well under the 4 KiB clamp, the answer must equal MSG.len().
    // This is the contract every other line of output in this file leans on.
    const MSG: &[u8] = b"[ABITEST] (write returns its input byte count)\n";
    let n = sys::write(sys::STDOUT, MSG);
    expect_eq(b"write(STDOUT) returns MSG.len()", n, MSG.len() as isize);

    // `sys_getchar` is NON-blocking despite the old libsys doc: it tests
    // `uart::can_read()` and returns -1 on an empty FIFO rather than
    // waiting. Reaching the next line at all is the substance of the check
    // — a genuinely blocking implementation would hang here forever with
    // idle CI stdin, and the scenario would time out instead of failing.
    // The value assertion pins the return domain: -1, or one byte.
    let c = sys::getchar();
    expect_true(
        b"getchar() returned (non-blocking) with -1 or a byte",
        c == -1 || (0..=255).contains(&c),
    );
}

// ── NUL-terminated path contract ────────────────────────────────────────────

fn check_paths_and_nul() {
    // The kernel reads paths with `copy_cstr_from_user`, which scans for a
    // NUL and never sees the slice length. This is the real bug found on
    // 2026-08-21 in brain_client.

    // A path with no terminator must be rejected BY THE WRAPPER, before the
    // ecall, so the kernel never scans past the literal.
    expect_eq(
        b"open(unterminated) -> E_INVAL from libsys",
        sys::open(b"/fat/NOT_TERMINATED", 0),
        sys::E_INVAL,
    );
    expect_eq(
        b"mkdir(unterminated) -> E_INVAL from libsys",
        sys::mkdir(b"/nope"),
        sys::E_INVAL,
    );
    expect_eq(
        b"service_discover(unterminated) -> E_INVAL",
        sys::service_discover(b"svc"),
        sys::E_INVAL,
    );

    // A properly terminated path reaches the kernel and gets a kernel answer
    // (not E_INVAL). Asserting a *missing* file keeps this independent of
    // whether a FAT32 image is attached — `qemu-smp` has no disk.
    let rc = sys::open(sys::cstr!(b"/definitely/not/here"), 0);
    expect_true(
        b"open(cstr!, missing) reaches kernel, fails there",
        rc < 0 && rc != sys::E_INVAL,
    );

    // `cstr!` must append exactly one NUL and keep the body intact. Checking
    // the macro's own output is what makes it trustworthy at every call site.
    const P: &[u8] = sys::cstr!(b"/fat/X");
    expect_true(
        b"cstr! appends exactly one NUL",
        P.len() == 7 && P[6] == 0 && P[0] == b'/' && P[5] == b'X',
    );
    expect_true(b"has_nul() agrees with cstr! output", sys::has_nul(P));
    expect_true(b"has_nul() rejects a bare literal", !sys::has_nul(b"/fat/X"));

    // The kernel's predicate is "contains a NUL", not "ends with one" — a
    // scratch buffer with trailing slack is legal and must not be rejected.
    let mut padded = [0u8; 32];
    padded[0] = b'/';
    padded[1] = b'x';
    expect_true(b"has_nul() accepts NUL + trailing slack", sys::has_nul(&padded));

    // `chdir`/`getcwd` have wrappers and syscall numbers but NO dispatch arm
    // — see check_missing_dispatch_arms(). The NUL guard still runs first.
    expect_eq(
        b"chdir(unterminated) -> E_INVAL before ecall",
        sys::chdir(b"/tmp"),
        sys::E_INVAL,
    );
}

// ── SYS_EXEC: (elf_ptr, elf_len), not (entry, stack) ────────────────────────

fn check_exec() {
    // `sys_exec` takes a POINTER TO AN ELF IMAGE and a LENGTH, bounces the
    // range through `copy_from_user` into a 128 KiB static, and execs it.
    // The old wrapper passed an entry address where a1 is a length.
    //
    // Success never returns (the trap handler enters the new image on sret),
    // so the contract is asserted through failure paths that each exercise a
    // different branch of the kernel's argument decode. All must return -1.

    // a0 == 0 -> rejected before any copy.
    expect_err(b"exec(empty slice) -> ptr/len rejected", sys::exec(&[]));

    // A non-ELF buffer of honest length: reaches copy_from_user, is copied
    // into EXEC_BOUNCE, and fails in the parse. This is the branch that
    // proves a1 is a LENGTH — under the old (entry, stack) reading the
    // kernel would have taken this buffer's address for an entry point.
    //
    // Twelve bytes is deliberately short and it is SAFE to be: `load_elf`
    // (crates/sched/src/process.rs:104) opens with
    // `if elf.len() < 64 { return None; }`, before it reads e_phoff or
    // e_phnum. Without that guard a truncated header would index past the
    // slice, and `panic = "abort"` would turn this assertion into a board
    // reset — a conformance test must not brick what it is testing.
    let not_an_elf = [0x7fu8, b'N', b'O', b'T', 0, 0, 0, 0, 0, 0, 0, 0];
    expect_err(b"exec(non-ELF, valid len) -> bad image", sys::exec(&not_an_elf));

    // Over EXEC_MAX_BYTES (128 KiB): rejected on the length check BEFORE the
    // copy, so no valid buffer is needed. Building the oversize slice from a
    // valid pointer with a bogus length would be UB, so this asserts the
    // constant the kernel enforces instead.
    expect_true(
        b"EXEC_MAX_BYTES mirrors kernel cap (128 KiB)",
        sys::EXEC_MAX_BYTES == 128 * 1024,
    );

    // execpath takes a NUL-terminated path, not an fd or a length.
    expect_err(
        b"execpath(missing path) -> negative",
        sys::execpath(sys::cstr!(b"/no/such/binary")),
    );
    expect_eq(
        b"execpath(unterminated) -> E_INVAL",
        sys::execpath(b"/no/such/binary"),
        sys::E_INVAL,
    );
}

// ── SYS_PIPE: the kernel writes int[2], i.e. [u32; 2] ───────────────────────

fn check_pipe() {
    // `sys_pipe` builds `let fds: [u32; 2]` and copies out
    // `size_of_val(&fds)` = **8 bytes**. The old wrapper passed
    // `&mut [u64; 2]` (16 bytes): the kernel filled only the first 8, so the
    // caller read fds[0] as `(write << 32) | read` and fds[1] as a stale
    // zero — two wrong values from a call that reported success.
    //
    // The sentinel is what catches that. Both u32 slots must come back
    // written; if the kernel wrote only 4 bytes (or the wrapper handed it a
    // wider destination) the second slot still holds u32::MAX.
    //
    // NOTE, and it is not what a POSIX reader expects: `pipe_create`
    // (crates/ipc/src/pipe.rs:104) ends in `Some((idx, idx))` — **the same
    // slot index for both ends**, with the source comment "read/write ends
    // distinguished by caller". So the two values are EQUAL by design, and
    // asserting they differ would fail against a correct kernel. Pinned as
    // equality so that separating the ends later shows up here.
    let mut fds = [u32::MAX, u32::MAX];
    let rc = sys::pipe(&mut fds);
    expect_eq(b"pipe() -> 0", rc, 0);
    if rc == 0 {
        expect_true(
            b"pipe() wrote BOTH u32 slots (8 bytes, not one u64)",
            fds[0] != u32::MAX && fds[1] != u32::MAX,
        );
        expect_true(
            b"pipe() ends share one slot index (kernel design)",
            fds[0] == fds[1],
        );
        sys::print(b"[ABITEST]        pipe fds = ");
        print_i(fds[0] as isize);
        sys::print(b", ");
        print_i(fds[1] as isize);
        sys::print(b"\n");
    }
    // Deliberately NOT closed. These are pipe-pool indices, not VFS fds:
    // `sys_close` runs `vfs_close` against the kernel FD table, which knows
    // nothing about pipes ("NO_IDX = no inode (or pipe — future)",
    // crates/fs/src/vfs.rs:107). There is no pipe read/write/close syscall
    // at all — SYS_PIPE is the only member of the family with a dispatch
    // arm. Calling close() here would operate on an unrelated fd number.
    // One pool slot is leaked per run; see the ABI audit report.
}

// ── SYS_DISK_READ argument order, checked without touching a disk ───────────

fn check_disk_wrapper_bounds() {
    // The kernel reads (sector, COUNT, buf); the old wrapper sent
    // (sector, buf_ptr, buf_len), so the buffer address landed in `count`
    // and the kernel copied count*512 bytes to the address that had been the
    // length. The wrapper now derives `count` from `buf.len()`, because the
    // kernel is never told how large the destination is — a caller-supplied
    // count is a ring-3 overflow behind an honest signature.
    //
    // These assertions exercise that guard WITHOUT issuing a disk ecall, so
    // they hold on `qemu-smp`, which has no block device attached.

    // Sub-sector buffer: refused by the wrapper, no ecall.
    let mut tiny = [0u8; 16];
    expect_eq(
        b"disk_read(<1 sector) -> E_INVAL, no ecall",
        sys::disk_read(0, &mut tiny),
        sys::E_INVAL,
    );

    // Partial trailing sector: refused rather than silently truncated.
    let mut ragged = [0u8; 600];
    expect_eq(
        b"disk_read(1.17 sectors) -> E_INVAL",
        sys::disk_read(0, &mut ragged),
        sys::E_INVAL,
    );

    // Empty write buffer: same guard on the write path.
    expect_eq(
        b"disk_write(empty) -> E_INVAL, no ecall",
        sys::disk_write(0, &[]),
        sys::E_INVAL,
    );

    // The constants must keep mirroring the kernel's own caps.
    expect_true(
        b"DISK_SECTOR_BYTES/MAX_SECTORS mirror kernel",
        sys::DISK_SECTOR_BYTES == 512 && sys::DISK_MAX_SECTORS == 128,
    );
}

// ── Syscalls that read a0 and used to be reached through syscall0 ───────────

fn check_trace_and_heartbeat() {
    // `syscall0` declares a0 as `lateout` only — nothing writes the register
    // before the ecall. Any syscall whose dispatch arm READS a0 therefore
    // received leftover garbage. Two did: SYS_TRACE_DUMP (entry count) and
    // SYS_DRV_HEARTBEAT (driver id).

    // trace_dump(n) dumps n entries; the arm returns 0 unconditionally. A
    // count of 1 keeps the console output to one line so the CI greps that
    // scan this scenario's log are not swamped.
    expect_eq(b"trace_dump(1) -> 0 (a0 is the count)", sys::trace_dump(1), 0);
    expect_true(
        b"TRACE_DUMP_DEFAULT_COUNT mirrors kernel (50)",
        sys::TRACE_DUMP_DEFAULT_COUNT == 50,
    );

    // drv_heartbeat now takes the drv_id explicitly. The arm returns 0 even
    // for an unregistered id (it is a fire-and-forget watchdog kick), so the
    // assertion is that it is reachable and well-formed, not that the id
    // exists. Using u64::MAX guarantees no real driver's watchdog is
    // refreshed by this test.
    expect_eq(
        b"drv_heartbeat(bogus id) -> 0, a0 now explicit",
        sys::drv_heartbeat(u64::MAX),
        0,
    );
}

// ── vDSO: the zero-ecall path, and the offset bug it once hid ───────────────

fn check_vdso() {
    // `vdso_uptime_ticks` read byte offset 8 — the seqlock counter — instead
    // of 16 for a long time. `seq` advances by 2 per publish, so it still
    // looked like a plausible monotonic counter, which is exactly why the
    // bug survived. Monotonicity alone does NOT catch it.
    //
    // What the two fields actually are (kernel/src/main.rs:4714-4756):
    //   uptime_ticks (offset 16) = TICK_COUNT, one per timer INTERRUPT.
    //   uptime_ms    (offset 24) = clint_now / (TIMER_FREQ / 1000).
    // They run at different rates, and which is larger depends on the
    // scheduler frequency — so an ordering assertion between them would be
    // a guess. The rate check below targets `uptime_ms` alone, where the
    // expected rate IS known: one unit per millisecond.

    let ms0 = sys::vdso_uptime_ms();
    let t0 = sys::vdso_uptime_ticks();
    // Nonzero implies the magic matched — `vdso_read_u64` returns 0 outright
    // on a bad or unmapped page.
    expect_true(b"vdso page mapped + magic ok (ms nonzero)", ms0 != 0);
    expect_true(b"vdso uptime_ticks nonzero", t0 != 0);
    expect_true(b"vdso kernel_version nonzero", sys::vdso_kernel_version() != 0);

    // Burn a known wall-clock interval, then check `uptime_ms` moved by
    // about that many units. This is the assertion the offset bug fails:
    // the seqlock counter advances a couple of units per timer interrupt,
    // nowhere near one per millisecond. The window is deliberately wide
    // (>= 20, <= 5000 for a 50 ms sleep) — it is checking the field's
    // *units*, not the host's timekeeping accuracy.
    const SLEEP_MS: u64 = 50;
    sys::sleep(SLEEP_MS);
    let ms1 = sys::vdso_uptime_ms();
    let t1 = sys::vdso_uptime_ticks();

    expect_true(b"vdso ms monotonic", ms1 >= ms0);
    expect_true(b"vdso ticks monotonic", t1 >= t0);

    // `saturating_sub`, not `-`: the assertions above REPORT non-monotonicity,
    // they do not prevent it. Under `overflow-checks = true` a plain
    // subtraction on a failing kernel would underflow, and `panic = "abort"`
    // turns that into a board reset — a diagnostic tool must not brick the
    // board it is diagnosing.
    let d_ms = ms1.saturating_sub(ms0);
    let d_ticks = t1.saturating_sub(t0);
    expect_true(
        b"vdso uptime_ms really counts MILLISECONDS",
        d_ms >= SLEEP_MS / 2 && d_ms <= SLEEP_MS * 100,
    );
    sys::print(b"[ABITEST]        vdso d_ms over 50ms sleep = ");
    print_i(d_ms as isize);
    sys::print(b", d_ticks = ");
    print_i(d_ticks as isize);
    sys::print(b"\n");

    // The tick field must be a live counter, not a frozen value: the timer
    // interrupt fires many times during a 50 ms sleep.
    expect_true(b"vdso uptime_ticks advanced during sleep", t1 > t0);

    // SYS_UPTIME is the raw CLINT mtime counter (~10 MHz) while the vDSO
    // tick field counts interrupts, so the ecall value is necessarily the
    // larger of the two. This pins the fact that they are DIFFERENT clocks —
    // a caller cannot substitute one for the other.
    let ecall = sys::uptime();
    expect_true(
        b"uptime() ecall is CLINT ticks, not vdso interrupt count",
        ecall > 0 && (ecall as u64) > t1,
    );
}

// ── Syscalls the kernel answers with sys_stub() ─────────────────────────────

fn check_kernel_stubs() {
    // `dispatch.rs:771` collapses SYS_ROBOT_INIT..=SYS_SENSOR_ADD into
    // `sys_stub()`, which is `-1`. libsys documents each of these as a stub;
    // these assertions make the docs falsifiable. If someone implements one,
    // this test fails and the doc gets updated — which is the point.
    //
    // robot_estop() is the one that matters: it is named like a safety path
    // and does nothing at all.
    expect_err(b"robot_init() stub -> negative", sys::robot_init());
    expect_err(b"robot_estop() stub -> negative (NOT a safety path)", sys::robot_estop());
    expect_err(b"sensor_info() stub -> negative", sys::sensor_info());
    expect_err(b"platform_type() stub -> negative", sys::platform_type());

    // sys_stat/sys_mount/sys_umount are stubs in handlers.rs, not dispatch.
    let mut statbuf = [0u8; 64];
    expect_err(
        b"stat() stub -> negative",
        sys::stat(sys::cstr!(b"/"), &mut statbuf),
    );
    expect_err(b"umount() stub -> negative", sys::umount(sys::cstr!(b"/fat")));

    // sys_sync is a real (if trivial) implementation: `{ 0 }`. Asserting it
    // separates "stub returning -1" from "implemented, nothing to do".
    expect_eq(b"sync() implemented -> 0", sys::sync(), 0);

    // sys_taskinfo is `{ 0 }` too — it reports nothing but is not an error.
    expect_eq(b"taskinfo() -> 0 (reports nothing)", sys::taskinfo(), 0);
}

// ── Syscall numbers with a wrapper but no dispatch arm ──────────────────────

fn check_missing_dispatch_arms() {
    // SYS_CHDIR (254) and SYS_GETCWD (255) are declared in
    // crates/syscall/src/numbers.rs and wrapped here, but `syscall_dispatch`
    // has no arm for either — they fall through to `_ => -1`. That is a
    // kernel-side gap, recorded rather than papered over: these assertions
    // pin the current behaviour so implementing them is a deliberate,
    // visible change rather than a silent one.
    expect_err(
        b"chdir() has no dispatch arm -> -1",
        sys::chdir(sys::cstr!(b"/")),
    );
    let mut cwd = [0u8; 64];
    expect_err(b"getcwd() has no dispatch arm -> -1", sys::getcwd(&mut cwd));

    // An unclaimed syscall number must also fall through, which is what
    // makes the two assertions above meaningful rather than tautological:
    // it shows -1 here is the default arm, and that the ABI has a defined
    // answer for a number it does not know.
    expect_err(b"unknown syscall number -> -1", unknown_syscall());
}

/// Issue a syscall number no arm claims (999 is above every number in
/// `crates/syscall/src/numbers.rs`). Kept local because libsys deliberately
/// exposes no raw-ecall escape hatch.
fn unknown_syscall() -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 999u64,
            lateout("a0") ret,
            options(nostack),
        );
    }
    ret
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // `panic = "abort"` + `overflow-checks = true`: any panic is a board
    // reset, so this must not itself allocate, format, or arithmetic.
    sys::println(b"[ABITEST] PANIC");
    sys::exit(2);
}
