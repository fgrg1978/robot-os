//! Ring-3 IPC probe.
//!
//! WHY THIS EXISTS
//!
//! `SYS_IPC_FAST_CALL` / `_ACCEPT` / `_REPLY` have existed for months, are
//! documented, and — until this binary — **had never been executed from ring
//! 3**. The same was true of the ownership gates added to `shm`, `port` and
//! `io_ring`, and of the whole `Cap<T>` typed family. Five serious fast-IPC
//! defects survived in a tree whose CI was green, because green meant "it
//! compiles", not "it runs".
//!
//! This binary runs them. It is the difference between work done and work
//! verified.
//!
//! WHAT IT ASSERTS, AND WHY EACH HALF MATTERS
//!
//!   A. **Fast-IPC round trip: 8 clients x 200 calls, under `-smp 4`.** The
//!      parent is the server and answers in a loop with NO delay between
//!      accept and reply; eight `fork()`ed children hammer it. Each client
//!      asserts the word it collects is the *reply*, not its own *request* —
//!      that is IPC-2, where `fast_ipc_accept` marked a claimed slot
//!      `Replied` while `words` still held the request, so any wake between
//!      accept and reply made the client collect its own question as the
//!      answer.
//!
//!      1600 calls, not one. `SYS_IPC_FAST_CALL` allocates the slot, wakes
//!      the server, and only *then* blocks; on four harts the server can
//!      reply inside that window and the wake is lost, leaving the client
//!      asleep for good. A one-shot test passes over that race roughly
//!      always, and a `sleep` between accept and reply hides it outright.
//!      Progress is printed every 32 iterations so a hang reads as "child 3
//!      wedged at seq=137", not as "never started".
//!
//!      **This phase deadlocks today, and that is the finding.** The client
//!      side of K-C10 is closed (`SYS_IPC_FAST_REPLY` now wakes by TID with a
//!      `wake_pending` stamp, and `SYS_IPC_FAST_CALL` retries spurious
//!      wakes). The **server** side is not: `SYS_IPC_FAST_ACCEPT`'s miss path
//!      is still a bare `task_block(WaitReason::FastIpcServer(tid))` with no
//!      stamp consumption, and `SYS_IPC_FAST_CALL` wakes it through
//!      `wake_fast_ipc_server`, the predicate *sweep*
//!      (`crates/sched/src/wait.rs`), not the TID-directed stamped variant.
//!      So: server polls and finds nothing; client on another hart allocates
//!      the slot and sweeps for a task blocked on `FastIpcServer` — the
//!      server is still Running, nothing matches, no stamp is left; the
//!      server then blocks forever, and every client blocks forever waiting
//!      for a reply. Observed exactly that: eight clients, all stuck at
//!      `seq=0`, server never reaching its first milestone.
//!
//!      Left red on purpose. See the scenario at the end of
//!      `crates/sched-wake-tests/src/lib.rs`.
//!
//!   B. **Server impersonation.** `slot_idx` is a small integer the caller
//!      chooses, so a task that is not the server can reply into someone
//!      else's exchange. Three tasks: a server, a client, and the parent as
//!      the impostor sweeping all 64 slots with a poison word. BOTH halves
//!      are asserted — that every impostor reply is *refused*, and that the
//!      legitimate client still wakes with the *legitimate* word.
//!
//!      The second half is the one this project keeps skipping. The reflex
//!      loop was declared verified off a log line saying it had decided to
//!      back up, while the motors were driving forward into the obstacle at
//!      full speed. A rejected return code proves a decision; only the
//!      client's collected payload proves the actuation.
//!
//!   C. **Ownership gates nobody executed.** A full shm map/write/unshare
//!      cycle in one task, then a *different* task sweeping every shm id,
//!      port id and io_ring id looking for one it does not own. Both halves
//!      again: the owner must succeed where the stranger is refused, or the
//!      test would also pass against a kernel that denies everyone.
//!
//!   D. **Typed capability past task 64.** `MAX_TASKS` is 64 and TIDs are
//!      monotone, so a TID-indexed cap table went permanently dead on a
//!      long-lived board once it had created its 64th task — the exact
//!      reason the tables were re-indexed by pool slot. This forks and
//!      reaps enough short-lived tasks to push the TID counter past 64,
//!      then mints and uses typed caps from a task whose TID is above it.
//!
//!   E. **Fast IPC actually carries the request.** Phases A and B prove an
//!      exchange completes and that an impostor cannot answer it; neither
//!      proves the server was told *what* was asked, because both reply a
//!      constant. `SYS_IPC_FAST_ACCEPT` returned only the slot index and
//!      threw away the caller TID and the four request words, so a ring-3
//!      server could answer but not answer *anything in particular* — a
//!      wake primitive wearing an RPC's documentation. This phase forks a
//!      server and a client, the client derives its request from its own
//!      TID, the server replies a function of all four received words, and
//!      the parent recomputes that function independently. A kernel that
//!      delivers nothing, zeros, or a stale payload fails it.
//!
//! WHAT IT DOES NOT DO
//!
//!   * No timing. `latbench` owns that, and a wall-clock threshold in a gate
//!     measures the host's load rather than the kernel.
//!   * No `wait()`. `sys_wait` is `-1  // Phase 8+`, so children report
//!     their verdicts to the parent over a kernel channel instead. The
//!     channel is created by the parent, and `channel_recv` is owner-gated,
//!     so only the parent can drain it.
//!   * No assertion on a specific errno. Denials in this kernel are `-1`
//!     from dispatch, `-99` from a handler, and `-Errno` from the typed
//!     path; assertions test for the sign and print the code.

#![no_std]
#![no_main]

use robot_os_libsys as sys;

// ── Test bookkeeping ────────────────────────────────────────────────────────

static mut FAILURES: u32 = 0;
static mut CHECKS: u32 = 0;

/// One log line, assembled in a stack buffer and emitted with a **single**
/// `write`.
///
/// The first version of this file printed each line as five or six separate
/// `sys::print` calls. With nine tasks running on four harts the kernel
/// interleaved them mid-line and the log came out as
/// `A/forked all clients rc=[IPCTEST] child=8` — two different tasks spliced
/// into one line. Progress output whose only job is to say *which* child
/// wedged *where* is worthless if the child index and the sequence number can
/// come from different tasks. One syscall per line makes each line atomic at
/// the UART.
struct Line {
    buf: [u8; 128],
    n: usize,
}

impl Line {
    fn new() -> Self {
        Line { buf: [0u8; 128], n: 0 }
    }

    fn s(&mut self, b: &[u8]) -> &mut Self {
        let room = self.buf.len() - self.n;
        let take = if b.len() < room { b.len() } else { room };
        self.buf[self.n..self.n + take].copy_from_slice(&b[..take]);
        self.n += take;
        self
    }

    /// Signed decimal. The `i64` widening keeps `isize::MIN` from overflowing
    /// on negation — `overflow-checks = true` plus `panic = "abort"` makes
    /// that a board reset, not a wrong number.
    fn i(&mut self, v: isize) -> &mut Self {
        if v < 0 {
            self.s(b"-");
        }
        let mut n = if v < 0 { (v as i64).unsigned_abs() } else { v as u64 };
        let mut tmp = [0u8; 20];
        let mut i = tmp.len();
        if n == 0 {
            i -= 1;
            tmp[i] = b'0';
        }
        while n > 0 {
            i -= 1;
            tmp[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }
        self.s(&tmp[i..])
    }

    fn flush(&mut self) {
        self.s(b"\n");
        sys::print(&self.buf[..self.n]);
        self.n = 0;
    }
}

fn report(name: &[u8], ok: bool, rc: isize) {
    let mut l = Line::new();
    l.s(if ok { b"[IPCTEST]   ok   " } else { b"[IPCTEST]  FAIL  " });
    l.s(name).s(b" rc=").i(rc);
    l.flush();
    // `overflow-checks = true` and `panic = "abort"`: a wrapping increment
    // here would reset the board. Saturating cannot.
    unsafe {
        CHECKS = CHECKS.saturating_add(1);
        if !ok {
            FAILURES = FAILURES.saturating_add(1);
        }
    }
}

/// The kernel must return exactly `want`.
fn expect_eq(name: &[u8], rc: isize, want: isize) {
    report(name, rc == want, rc);
}

/// The kernel must reject this call.
fn expect_err(name: &[u8], rc: isize) {
    report(name, rc < 0, rc);
}

/// The kernel must return something strictly positive.
fn expect_pos(name: &[u8], rc: isize) {
    report(name, rc > 0, rc);
}

/// A plain boolean assertion; `rc` is printed for context either way.
fn expect_true(name: &[u8], ok: bool, rc: isize) {
    report(name, ok, rc);
}

// ── Child → parent mailbox ──────────────────────────────────────────────────
//
// One kernel channel, created by the parent before any `fork()`, so every
// child inherits the id through COW. Records are a fixed 5 bytes:
// `[tag, value_u32_le]`. `channel_send` is unauthenticated (any task may
// write), `channel_recv` is owner-gated (only the creator may drain) — which
// is exactly the shape a verdict back-channel needs.
//
// The ring holds `RING_CAP` = 8 messages, so the parent drains at every wait
// and a child retries a full ring rather than dropping its verdict on the
// floor. A dropped verdict would read as a timeout, i.e. as a hang that did
// not happen.

/// Budget for one child verdict, in milliseconds of real `mtime`. Generous
/// on purpose: this kernel runs a benchmark suite, a brain-link handshake
/// spinning at ~400k yields per two seconds, and a telemetry task alongside
/// the test, and a starved child that eventually answers must not be reported
/// as a dead one. The QEMU scenario's own 180 s ceiling is the real backstop.
const WAIT_MS: u32 = 20_000;

const REC_LEN: usize = 5;
const MBOX_LEN: usize = 32;

const TAG_S2_TID: u8 = 3;
const TAG_S2_RC: u8 = 4;
const TAG_CLIENT_GOT: u8 = 5;
const TAG_GUESS_SHM: u8 = 6;
const TAG_GUESS_PORT: u8 = 7;
const TAG_GUESS_RING: u8 = 8;
const TAG_T_TID: u8 = 9;
const TAG_T_MASK: u8 = 10;
/// Phase E: server TID, what the server saw, and the client's TID + reply.
const TAG_E_SRV_TID: u8 = 12;
const TAG_E_SRV_SAW: u8 = 13;
const TAG_E_CLI_TID: u8 = 14;
const TAG_E_CLI_GOT: u8 = 15;
/// Phase A clients report at `TAG_CHILD_BASE + child_index`; the base is
/// above every fixed tag and `TAG_CHILD_BASE + RT_CHILDREN` stays inside
/// `MBOX_LEN`.
const TAG_CHILD_BASE: u8 = 16;
/// Verdict of the `fork()` register canary.
const TAG_CANARY: u8 = 11;

static mut CHAN: isize = -1;
static mut MBOX: [u32; MBOX_LEN] = [0; MBOX_LEN];
static mut MSEEN: [u8; MBOX_LEN] = [0; MBOX_LEN];

// `addr_of!` rather than plain indexing: these statics are written by this
// task only, but taking a reference to a `static mut` is a lint the CI
// warning gate treats as a failure. Raw-pointer access says what is meant and
// keeps the build clean.
fn mbox_set(tag: usize, v: u32) {
    if tag >= MBOX_LEN {
        return;
    }
    unsafe {
        core::ptr::addr_of_mut!(MBOX).cast::<u32>().add(tag).write(v);
        core::ptr::addr_of_mut!(MSEEN).cast::<u8>().add(tag).write(1);
    }
}

fn mbox_get(tag: usize) -> Option<u32> {
    if tag >= MBOX_LEN {
        return None;
    }
    unsafe {
        if core::ptr::addr_of!(MSEEN).cast::<u8>().add(tag).read() == 0 {
            None
        } else {
            Some(core::ptr::addr_of!(MBOX).cast::<u32>().add(tag).read())
        }
    }
}

/// Child side: post one verdict. Retries a full ring instead of losing it.
fn post(tag: u8, v: u32) {
    let b = v.to_le_bytes();
    let msg = [tag, b[0], b[1], b[2], b[3]];
    let ch = unsafe { CHAN } as u64;
    let mut tries = 0u32;
    while sys::chan_write(ch, &msg) != 0 && tries < 400 {
        sys::sleep(2);
        tries = tries.saturating_add(1);
    }
}

/// Parent side: drain everything queued into the mailbox.
fn pump() {
    let ch = unsafe { CHAN } as u64;
    let mut buf = [0u8; 64];
    loop {
        let n = sys::chan_read(ch, &mut buf);
        if n < REC_LEN as isize {
            // 0 = empty, -1 = error, short = not one of ours.
            return;
        }
        let v = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
        mbox_set(buf[0] as usize, v);
    }
}

/// CLINT ticks per millisecond on QEMU virt (`mtime` runs at 10 MHz).
const TICKS_PER_MS: isize = 10_000;

/// Parent side: wait up to `ms` for a tagged verdict. Returns `None` on
/// timeout — a bounded wait, so a wedged child costs one FAIL line and the
/// run continues to the next case instead of hanging the whole scenario.
///
/// The deadline is read from `uptime()` rather than accumulated from the
/// `sleep()` argument. Under load this kernel's timer jitter reaches whole
/// seconds (`[JITTER] timer_isr max_ns 2003100000`), so counting nominal
/// milliseconds turns a 5 s budget into an unpredictable wall-clock wait —
/// which is how a starved child gets misreported as a dead one.
fn wait_tag(tag: u8, ms: u32) -> Option<u32> {
    let deadline = sys::uptime() + ms as isize * TICKS_PER_MS;
    loop {
        pump();
        if let Some(v) = mbox_get(tag as usize) {
            return Some(v);
        }
        if sys::uptime() >= deadline {
            return None;
        }
        sys::sleep(2);
    }
}

// ── fork(), and the register state it does not give the child ──────────────
//
// **KERNEL DEFECT, found by this probe.** `fork_child_entry` enters the child
// through `sret_to_user` (`crates/sched/src/process.rs:412`), which restores
// only `pc`, `sp` and `satp` and explicitly zeroes `a0`..`a7`. Nothing copies
// the parent's `ra`, `gp`, `tp`, `t0`-`t6` or `s0`-`s11`: the child resumes
// user code with whatever those registers held in the *kernel* task that ran
// `fork_child_entry`.
//
// Two consequences, both observed:
//   * Any value the compiler kept in a callee-saved register is corrupt in
//     the child. The first version of this file passed each client its index
//     as a function argument; all eight children printed `child=0`, because
//     the loop counter lived in an s-register.
//   * `ra` is garbage, so **the child cannot return from the function that
//     called `fork`** — it would jump to a kernel address. The child path
//     below therefore ends in a diverging call and never returns.
//
// It is also a kernel-to-ring-3 information leak: whatever the kernel left in
// s0-s11 is readable from U-mode.
//
// [`fork_reg_canary`] asserts this directly and **is expected to fail until
// the kernel copies the parent's registers**. It is left red on purpose; the
// scaffolding below is what lets the *other* phases still run, and it is
// scaffolding, not a silenced assertion — the defect keeps its own failing
// check.

/// Value planted in `s11` across the fork ecall by [`fork_reg_canary`].
const FORK_CANARY: u64 = 0x5AFE_C0DE;
/// `SYS_FORK`. Issued as raw asm because the canary has to survive in a
/// specific register across the `ecall`, which a normal wrapper cannot
/// express.
const NR_FORK: u64 = 12;

/// Roles a forked child can take. Passed through **memory**, never through a
/// register or an argument — see the note above.
const ROLE_A_CLIENT: u32 = 1;
const ROLE_B_SERVER: u32 = 2;
const ROLE_B_CLIENT: u32 = 3;
const ROLE_C_GUESSER: u32 = 4;
const ROLE_D_EXIT: u32 = 5;
const ROLE_D_PROBE: u32 = 6;
const ROLE_CANARY: u32 = 7;
const ROLE_E_SERVER: u32 = 8;
const ROLE_E_CLIENT: u32 = 9;

static mut ROLE: u32 = 0;
static mut ROLE_ARG: u32 = 0;
/// `s11` as the CHILD sees it after the fork ecall.
static mut FORK_S11: u64 = 0;
/// `s11` as the PARENT sees it after the same ecall. Separate static: both
/// sides run the same code, and a single slot would only ever hold whichever
/// of the two wrote last — which for a COW fork is neither, since the write
/// lands in each task's own copy of the page.
static mut FORK_S11_PARENT: u64 = 0;

/// Fork, and run `role` in the child. Returns the child TID to the parent;
/// **never returns in the child**.
///
/// `role` and `arg` are written to statics *before* the ecall and re-read
/// from statics afterwards. Reading the parameters back would read registers
/// the child never received.
fn spawn(role: u32, arg: u32) -> isize {
    unsafe {
        ROLE = role;
        ROLE_ARG = arg;
    }
    let pid: isize;
    let observed: u64;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") NR_FORK,
            lateout("a0") pid,
            // `inout` on an explicit register: the canary is materialised in
            // s11 before the ecall and read back after it. In the child that
            // read returns whatever the kernel left there — which is the
            // measurement `fork_reg_canary` reports.
            inout("s11") FORK_CANARY => observed,
            // ── USERSPACE MITIGATION FOR A KERNEL DEFECT — DELETE WHEN FIXED
            //
            // Declaring every callee-saved register clobbered forces the
            // compiler to spill them to the stack before the ecall and reload
            // them after. The stack IS inherited correctly (same `sp`, COW
            // copy of the page), so the child comes back with usable values.
            //
            // Without this the child does not survive its first memory
            // access. Measured, not guessed: the compiler hoisted the address
            // of a static into `s10` before the ecall and emitted
            // `sd s11,24(s10)` after it; in the child `s10` held the kernel's
            // leftover `0x80605000`, the store went to `0x80605018`, and the
            // kernel answered `[PAGE FAULT] Killing user task`. Any forked
            // child is one hoisted base register away from that.
            //
            // This is NOT the fix and must not be read as one. The fix is for
            // `sret_to_user`'s fork path to copy the parent's register file;
            // `fork_reg_canary` stays red until it does. This list only buys
            // the other phases the chance to run at all — a probe whose every
            // child is killed before its first instruction measures nothing.
            // `sp`, `gp` and `tp` cannot be listed (rustc reserves them);
            // `gp` is safe here only because `user.ld` defines no
            // `__global_pointer$`, so nothing is gp-relative.
            out("ra") _,
            out("t0") _, out("t1") _, out("t2") _, out("t3") _,
            out("t4") _, out("t5") _, out("t6") _,
            // `s0`/`s1` cannot be listed either (LLVM reserves them), which is
            // why the child's first act is a PC-relative call into
            // `child_main` rather than a store through a base register that
            // might have been materialised before the ecall.
            out("s2") _, out("s3") _, out("s4") _,
            out("s5") _, out("s6") _, out("s7") _, out("s8") _,
            out("s9") _, out("s10") _,
            out("a1") _, out("a2") _, out("a3") _,
            out("a4") _, out("a5") _, out("a6") _,
            options(nostack),
        );
    }
    if pid == 0 {
        // CHILD. Only `pc`, `sp` and `a0` are ours.
        //
        // `observed` is handed on as an ARGUMENT rather than stored here: an
        // argument travels in `a0`, and the call is a PC-relative `auipc`
        // +`jalr`, so neither depends on a register the child never received.
        // Storing it here instead compiled to `sd s11,24(s10)` with `s10`
        // materialised *before* the ecall — and that store killed the child.
        child_main(observed);
    }
    unsafe { FORK_S11_PARENT = observed };
    pid
}

/// Child entry. Diverges — the child must never return through `ra`, which
/// holds whatever the kernel left in it.
///
/// Every static read below is addressed with a `auipc` computed *inside* this
/// function, i.e. after the fork, so no base register predates the ecall.
fn child_main(observed_s11: u64) -> ! {
    unsafe { FORK_S11 = observed_s11 };
    let role = unsafe { ROLE };
    let arg = unsafe { ROLE_ARG };
    // Announce before doing anything else. A child that dies between `fork`
    // and its first useful instruction is otherwise indistinguishable from a
    // child that was never scheduled, and this kernel does not always print a
    // fault when it kills one. `ROLE_D_EXIT` is excluded because phase D
    // spawns ninety of them.
    if role != ROLE_D_EXIT {
        let mut l = Line::new();
        l.s(b"[IPCTEST] child_main role=").i(role as isize)
            .s(b" arg=").i(arg as isize)
            .s(b" tid=").i(sys::getpid())
            .s(b" s11=").i(observed_s11 as isize);
        l.flush();
    }
    match role {
        ROLE_A_CLIENT => phase_a_client(arg),
        ROLE_B_SERVER => phase_b_server(),
        ROLE_B_CLIENT => phase_b_client(),
        ROLE_C_GUESSER => phase_c_guesser(),
        ROLE_D_EXIT => sys::exit(0),
        ROLE_D_PROBE => phase_d_probe(),
        ROLE_E_SERVER => phase_e_server(),
        ROLE_E_CLIENT => phase_e_client(),
        ROLE_CANARY => {
            let seen = unsafe { FORK_S11 };
            post(TAG_CANARY, if seen == FORK_CANARY { 1 } else { 0 });
            sys::exit(0)
        }
        _ => sys::exit(0),
    }
}

/// Does `fork()` hand the child the parent's callee-saved registers?
///
/// Expected RED until `sret_to_user`'s fork path copies them. Asserting it
/// separately is what keeps the workaround above from hiding the defect: the
/// other phases route their parameters through memory and pass, this one
/// tests the register file itself and fails.
fn fork_reg_canary() {
    let pid = spawn(ROLE_CANARY, 0);
    if pid < 0 {
        expect_err(b"0/fork for canary (unexpected failure)", pid);
        return;
    }
    // The parent's own registers are trivially intact; assert it anyway, so a
    // failure here separates "fork corrupts the CALLER" from "fork does not
    // populate the CHILD".
    expect_true(
        b"0/fork preserves the parent's s11",
        unsafe { FORK_S11_PARENT } == FORK_CANARY,
        unsafe { FORK_S11_PARENT } as isize,
    );
    match wait_tag(TAG_CANARY, WAIT_MS) {
        Some(1) => expect_true(b"0/fork preserves the child's s11", true, 1),
        Some(v) => expect_true(b"0/fork preserves the child's s11", false, v as isize),
        None => expect_true(b"0/canary child reported at all", false, -1),
    }
}

// ── Phase A: fast-IPC round trip, N clients × M iterations ─────────────────
//
// Shape taken verbatim from the scenario written at the end of
// `crates/sched-wake-tests/src/lib.rs` by the lane that closed the
// `wake_pending` half of K-C10. The parent is the SERVER; N forked children
// are the CLIENTS.
//
// **There is NO delay between ACCEPT and REPLY, on purpose.** That is the
// whole point: `SYS_IPC_FAST_CALL` allocates the slot, wakes the server, and
// only *then* blocks the caller. A server that answers inside that window on
// another hart has its `wake_fast_ipc_client` land on a task that is not yet
// blocked, the wake is lost, and the client sleeps forever. A `sleep()`
// between accept and reply makes the client always reach `task_block` first
// and hides the bug — a test that hides the bug it exists to find is worse
// than no test.
//
// **This needs `-smp >= 2`.** On one hart the interleaving cannot occur and
// this phase proves nothing. Ring 3 has no way to read the hart count — there
// is no syscall for it (`sys_taskinfo` is `0`, `SYS_PLATFORM_INFO` is a stub)
// — so the requirement is stated in the log and enforced by the `-smp 4` on
// the QEMU command line in `tools/ci_check.sh`. Naming the gap is the honest
// option; asserting something ring 3 cannot observe is not.
//
// **The server cannot see the request.** `SYS_IPC_FAST_ACCEPT` returns only
// the slot index; the dispatch arm discards the caller TID and the four
// request words, and nothing writes a1..a4 back into the server's frame. So
// the `seq ^ MAGIC` echo the scenario asks for is not implementable from ring
// 3 today. The strongest assertion the ABI permits is used instead: the
// server answers `REPLY_MAGIC + slot_idx`, and each client asserts the word
// it collects lies in the reply range and is **not** its own request word.
// That still catches IPC-2 exactly — a client collecting its own question as
// the answer — because every request word is unique per (child, iteration).

/// Clients forked for the race. The scenario asks for N ≥ 8.
const RT_CHILDREN: u32 = 8;
/// Calls per client. The scenario asks for M ≥ 200.
const RT_ITERS: u32 = 200;
/// Progress cadence, in iterations. Outside any measured batch (nothing is
/// timed here) and rare enough that the ~160 us cost of a UART write cannot
/// dominate. Its only job is to make a hang read as "wedged at seq=137".
const RT_PROGRESS: u32 = 32;
/// Server progress cadence. Printed between a reply and the next accept —
/// never between accept and reply, which would widen exactly the window the
/// phase is trying to close on.
const RT_SRV_PROGRESS: u32 = 256;

const REQ_MAGIC: u64 = 0x0011_0000;
const REPLY_MAGIC: u64 = 0x0022_0000;
/// Request words are `REQ_MAGIC + idx * RT_STRIDE + k`, so no two clients and
/// no two iterations ever share one. `RT_ITERS` must stay below this.
const RT_STRIDE: u64 = 1024;

/// A refused `fast_ipc_call` that took at least this many CLINT ticks was
/// almost certainly blocked and then woken with nothing to collect (the
/// spurious-wake signature); a faster one is an immediate rejection. QEMU
/// virt's mtime runs at 10 MHz, so this is ~100 us. **Heuristic**, and
/// labelled as such in the output: `a0` carries the reply word and the error
/// code in the same register, so ring 3 has no way to tell the two apart
/// exactly.
const RT_SPURIOUS_TICKS: isize = 1000;

static mut PARENT_TID: u32 = 0;

/// Pack a client's four counters into one mailbox word. `RT_ITERS` is 200, so
/// every field fits in a byte.
fn pack4(a: u32, b: u32, c: u32, d: u32) -> u32 {
    (a & 0xFF) | ((b & 0xFF) << 8) | ((c & 0xFF) << 16) | ((d & 0xFF) << 24)
}

fn phase_a_client(idx: u32) -> ! {
    let srv = unsafe { PARENT_TID };
    let mut ok = 0u32;
    let mut refused = 0u32;
    let mut refused_slow = 0u32;
    let mut echoed = 0u32;
    let mut bad = 0u32;

    let mut k = 0u32;
    while k < RT_ITERS {
        if k % RT_PROGRESS == 0 {
            let mut l = Line::new();
            l.s(b"[IPCTEST] child=").i(idx as isize).s(b" seq=").i(k as isize);
            l.flush();
        }
        let seq = REQ_MAGIC + idx as u64 * RT_STRIDE + k as u64;
        let t0 = sys::uptime();
        match sys::fast_ipc_call(srv, [seq, 0, 0, 0]) {
            Some(w) => {
                if w == seq {
                    // IPC-2: the slot still held the request when the client
                    // collected. This is the defect the phase exists for.
                    echoed = echoed.saturating_add(1);
                } else if w >= REPLY_MAGIC && w < REPLY_MAGIC + sys::FAST_IPC_MAX_SLOTS as u64 {
                    ok = ok.saturating_add(1);
                } else {
                    bad = bad.saturating_add(1);
                }
            }
            None => {
                refused = refused.saturating_add(1);
                if sys::uptime() - t0 >= RT_SPURIOUS_TICKS {
                    refused_slow = refused_slow.saturating_add(1);
                }
            }
        }
        k += 1;
    }

    let mut l = Line::new();
    l.s(b"[IPCTEST] child=").i(idx as isize)
        .s(b" done=").i(ok as isize)
        .s(b" refused=").i(refused as isize)
        .s(b" (blocked-first=").i(refused_slow as isize)
        .s(b") echo=").i(echoed as isize)
        .s(b" bad=").i(bad as isize);
    l.flush();

    post(TAG_CHILD_BASE + idx as u8, pack4(ok, refused, echoed, bad));
    sys::exit(0);
}

fn phase_a_race() {
    sys::println(b"[IPCTEST] A: fast-IPC race loop (needs -smp >= 2)");
    let me = sys::getpid();
    if me <= 0 {
        expect_err(b"A/getpid", me);
        return;
    }
    unsafe { PARENT_TID = me as u32 };

    let mut forked = 0u32;
    while forked < RT_CHILDREN {
        // Index goes through `ROLE_ARG`, i.e. through memory. Passed as an
        // argument it arrived as 0 in every child — see the fork note above.
        let p = spawn(ROLE_A_CLIENT, forked);
        if p < 0 {
            break;
        }
        forked += 1;
    }
    expect_eq(b"A/forked all clients", forked as isize, RT_CHILDREN as isize);
    if forked == 0 {
        return;
    }

    // Server loop. Tight on purpose — see the header.
    let total = forked.saturating_mul(RT_ITERS);
    let mut served = 0u32;
    let mut accept_fail = 0u32;
    let mut reply_fail = 0u32;
    while served < total {
        match sys::fast_ipc_accept() {
            Some(handle) => {
                // El handle lleva etiqueta de generacion; el indice se decodifica
                // SOLO para componer la palabra magica que el cliente valida.
                // Responder con el indice en vez del handle reabriria el ABA.
                let slot = (handle & sys::FAST_IPC_SLOT_MASK) as usize;
                let rc = sys::fast_ipc_reply(handle, [REPLY_MAGIC + slot as u64, 0, 0, 0]);
                if rc != 0 {
                    reply_fail = reply_fail.saturating_add(1);
                }
                served = served.saturating_add(1);
                if served % RT_SRV_PROGRESS == 0 {
                    let mut l = Line::new();
                    l.s(b"[IPCTEST] server served=").i(served as isize);
                    l.flush();
                }
            }
            None => {
                // A woken-with-nothing-pending accept. Bounded so the server
                // cannot spin forever if every client has died.
                accept_fail = accept_fail.saturating_add(1);
                if accept_fail > total {
                    break;
                }
            }
        }
    }
    let mut l = Line::new();
    l.s(b"[IPCTEST] server served=").i(served as isize)
        .s(b" accept_fail=").i(accept_fail as isize)
        .s(b" reply_fail=").i(reply_fail as isize);
    l.flush();

    expect_eq(b"A/server served every call", served as isize, total as isize);
    expect_eq(b"A/no failed reply", reply_fail as isize, 0);

    let mut i = 0u32;
    let mut all_ok = 0u32;
    while i < forked {
        match wait_tag(TAG_CHILD_BASE + i as u8, WAIT_MS) {
            Some(v) => {
                let ok = v & 0xFF;
                let refused = (v >> 8) & 0xFF;
                let echoed = (v >> 16) & 0xFF;
                let bad = (v >> 24) & 0xFF;
                all_ok = all_ok.saturating_add(ok);
                expect_eq(b"A/child completed every call", ok as isize, RT_ITERS as isize);
                expect_eq(b"A/child never refused", refused as isize, 0);
                expect_eq(b"A/reply != own request (IPC-2)", echoed as isize, 0);
                expect_eq(b"A/reply word in range", bad as isize, 0);
            }
            None => expect_true(b"A/child reported at all", false, i as isize),
        }
        i += 1;
    }

    let mut l = Line::new();
    l.s(b"[IPCTEST] all=").i(all_ok as isize).s(b" of ").i(total as isize);
    l.s(if all_ok == total { b" OK" } else { b" INCOMPLETE" });
    l.flush();
}

// ── Phase B: server impersonation ───────────────────────────────────────────

const GOOD2: u64 = 0x0033_0001;
const POISON: u64 = 0x0044_0BAD;
const REQ2: u64 = 0x0055_0002;

/// Milliseconds. The impostor must sweep while the exchange is live: after
/// the client has called and the server has claimed the slot, but before the
/// server answers.
const B_CLIENT_DELAY: u64 = 50;
const B_IMPOSTOR_DELAY: u64 = 200;
const B_SERVER_HOLD: u64 = 400;

static mut SRV2_TID: u32 = 0;

fn phase_b_server() -> ! {
    let me = sys::getpid();
    let mut l = Line::new();
    l.s(b"[IPCTEST] B: server tid=").i(me);
    l.flush();
    post(TAG_S2_TID, me as u32);
    match sys::fast_ipc_accept() {
        Some(handle) => {
            // Hold the claimed slot open so the impostor gets a real window.
            sys::sleep(B_SERVER_HOLD);
            let rc = sys::fast_ipc_reply(handle, [GOOD2, 0, 0, 0]);
            post(TAG_S2_RC, if rc == 0 { 0 } else { 1 });
        }
        None => post(TAG_S2_RC, 2),
    }
    sys::exit(0);
}

fn phase_b_client() -> ! {
    let mut l = Line::new();
    l.s(b"[IPCTEST] B: client tid=").i(sys::getpid()).s(b" target=").i(unsafe { SRV2_TID } as isize);
    l.flush();
    sys::sleep(B_CLIENT_DELAY);
    let got = sys::fast_ipc_call(unsafe { SRV2_TID }, [REQ2, 0, 0, 0]);
    post(
        TAG_CLIENT_GOT,
        match got {
            Some(w) => w as u32,
            None => u32::MAX,
        },
    );
    sys::exit(0);
}

fn phase_b() {
    sys::println(b"[IPCTEST] B: server impersonation");

    let sp = spawn(ROLE_B_SERVER, 0);
    if sp < 0 {
        expect_err(b"B/fork-server (unexpected failure)", sp);
        return;
    }
    let s2 = match wait_tag(TAG_S2_TID, WAIT_MS) {
        Some(t) => t,
        None => {
            expect_true(b"B/server announced its TID", false, -1);
            return;
        }
    };
    unsafe { SRV2_TID = s2 };

    let cp = spawn(ROLE_B_CLIENT, 0);
    if cp < 0 {
        expect_err(b"B/fork-client (unexpected failure)", cp);
        return;
    }

    // Impostor half. This task is neither the server nor the client of the
    // exchange now in flight; every one of these must be refused.
    sys::sleep(B_IMPOSTOR_DELAY);
    let mut accepted = 0u32;
    let mut slot = 0usize;
    while slot < sys::FAST_IPC_MAX_SLOTS {
        // Indices crudos = generacion 0. Siguen siendo handles bien formados,
        // asi que este barrido sigue probando la puerta de propiedad — y ahora
        // tambien la de generacion en cuanto una ranura se haya reciclado.
        if sys::fast_ipc_reply(slot as u64, [POISON, 0, 0, 0]) >= 0 {
            accepted = accepted.saturating_add(1);
        }
        slot += 1;
    }
    // Decision.
    expect_eq(b"B/impostor replies refused", accepted as isize, 0);

    // Actuation. The half this project keeps skipping: a refused return code
    // is not proof the payload did not land.
    match wait_tag(TAG_CLIENT_GOT, WAIT_MS) {
        Some(w) if w as u64 == GOOD2 => {
            expect_true(b"B/client got the real reply", true, w as isize)
        }
        Some(w) if w as u64 == POISON => {
            expect_true(b"B/client got the real reply (POISONED)", false, w as isize)
        }
        Some(w) => expect_true(b"B/client got the real reply", false, w as isize),
        None => expect_true(b"B/client reported at all", false, -1),
    }
    match wait_tag(TAG_S2_RC, WAIT_MS) {
        Some(rc) => expect_eq(b"B/legit server replied ok", rc as isize, 0),
        None => expect_true(b"B/legit server reported at all", false, -1),
    }
}

// ── Phase C: shm / port / io_ring ownership ─────────────────────────────────

/// Ids swept by the stranger. `MAX_SHM_REGIONS`, `MAX_PORTS` and
/// `MAX_IO_RINGS` are all 16; an out-of-range id is refused by the bound
/// check, which is harmless here.
const SWEEP_IDS: u64 = 16;

const SHM_MAGIC: u64 = 0x0066_1234_5678_9ABC;

static mut OWNED_SHM: isize = -1;
static mut OWNED_PORT: isize = -1;
static mut OWNED_RING: isize = -1;

/// Full lifecycle inside one task: create, map, write, read back, reject the
/// double map, release, and confirm the id is dead afterwards.
fn phase_c_cycle() {
    sys::println(b"[IPCTEST] C: shm map/write/unshare cycle");

    let id = sys::ipc_share(2, sys::SHM_RW);
    expect_true(b"C/ipc_share 2 pages RW", id >= 0, id);
    if id < 0 {
        return;
    }

    let va = sys::ipc_map(id as u64);
    expect_pos(b"C/ipc_map (creator)", va);
    if va <= 0 {
        return;
    }

    // A store fault here kills the task and the run ends with no verdict, so
    // announce the address first: the log then says which line faulted.
    let mut l = Line::new();
    l.s(b"[IPCTEST] C: writing shm va=").i(va);
    l.flush();
    let p = va as *mut u64;
    let read_back = unsafe {
        core::ptr::write_volatile(p, SHM_MAGIC);
        core::ptr::read_volatile(p)
    };
    expect_true(
        b"C/shm page is readable+writable",
        read_back == SHM_MAGIC,
        (read_back & 0xFFFF) as isize,
    );

    // One mapping per (task, region): the release path has a single VA slot
    // to tear down, so a second alias would outlive the refcount.
    expect_err(b"C/second ipc_map refused", sys::ipc_map(id as u64));

    expect_eq(b"C/ipc_unshare (drops map ref)", sys::ipc_unshare(id as u64), 0);

    // NOT a leak, and this assertion is deliberate. `shm_create` seeds the
    // creator with one reference of its own and `ipc_map` takes a second, so
    // one `ipc_unshare` returns the region to "created but unmapped" — the
    // creator can map it again. The first version of this test asserted the
    // opposite and failed here; the kernel was right and the test was wrong.
    // Pinning the real semantics is what stops the next reader "fixing" the
    // kernel to match a wrong expectation.
    let va2 = sys::ipc_map(id as u64);
    expect_pos(b"C/creator keeps its own ref", va2);

    expect_eq(b"C/ipc_unshare again", sys::ipc_unshare(id as u64), 0);
    expect_eq(b"C/ipc_unshare last ref", sys::ipc_unshare(id as u64), 0);

    // Now the last reference is gone, the pages are back in the PMM, and even
    // the creator cannot map it.
    expect_err(b"C/ipc_map after last unshare", sys::ipc_map(id as u64));
}

fn phase_c_guesser() -> ! {
    let mut shm_hits = 0u32;
    let mut port_hits = 0u32;
    let mut ring_hits = 0u32;

    let mut id = 0u64;
    while id < SWEEP_IDS {
        if sys::ipc_map(id) >= 0 {
            shm_hits = shm_hits.saturating_add(1);
        }
        if sys::port_bind(id, sys::PORT_SRC_TIMER, 0, 0xBAD) == 0 {
            port_hits = port_hits.saturating_add(1);
        }
        if sys::io_pending(id) >= 0 {
            ring_hits = ring_hits.saturating_add(1);
        }
        id += 1;
    }

    post(TAG_GUESS_SHM, shm_hits);
    post(TAG_GUESS_PORT, port_hits);
    post(TAG_GUESS_RING, ring_hits);
    sys::exit(0);
}

fn phase_c_gates() {
    sys::println(b"[IPCTEST] C: cross-task ownership gates");

    // Owner half first — without it this passes against a kernel that denies
    // everybody, which is the same mistake as validating a decision and never
    // the actuation.
    let shm = sys::ipc_share(1, sys::SHM_RW);
    let port = sys::port_create();
    let ring = sys::io_setup(0);
    expect_true(b"C/owner ipc_share", shm >= 0, shm);
    expect_true(b"C/owner port_create", port >= 0, port);
    expect_true(b"C/owner io_setup", ring >= 0, ring);
    if shm < 0 || port < 0 || ring < 0 {
        return;
    }
    unsafe {
        OWNED_SHM = shm;
        OWNED_PORT = port;
        OWNED_RING = ring;
    }
    expect_pos(b"C/owner ipc_map", sys::ipc_map(shm as u64));
    expect_eq(
        b"C/owner port_bind",
        sys::port_bind(port as u64, sys::PORT_SRC_TIMER, 0, 0xC0DE),
        0,
    );
    // `io_pending` (SYS_IO_WAIT), not `io_submit`. Both go through the same
    // `io_ring_access_ok` gate, but `io_ring_submit` also returns
    // `IO_ERR_NO_OPS` when no op table is installed — which it is not in this
    // configuration. A negative from `io_submit` therefore does not prove the
    // ownership gate fired, and the stranger's sweep below would be "passing"
    // for a reason that has nothing to do with ownership. `io_ring_pending`
    // returns 0 for an empty ring, so owner and stranger are cleanly
    // separated by sign.
    let own_ring = sys::io_pending(ring as u64);
    expect_true(b"C/owner io_pending", own_ring >= 0, own_ring);

    // Stranger half.
    let gp = spawn(ROLE_C_GUESSER, 0);
    if gp < 0 {
        expect_err(b"C/fork-guesser (unexpected failure)", gp);
        return;
    }
    match wait_tag(TAG_GUESS_SHM, WAIT_MS) {
        Some(n) => expect_eq(b"C/stranger mapped no shm", n as isize, 0),
        None => expect_true(b"C/guesser reported shm", false, -1),
    }
    match wait_tag(TAG_GUESS_PORT, WAIT_MS) {
        Some(n) => expect_eq(b"C/stranger bound no port", n as isize, 0),
        None => expect_true(b"C/guesser reported port", false, -1),
    }
    match wait_tag(TAG_GUESS_RING, WAIT_MS) {
        Some(n) => expect_eq(b"C/stranger drove no io_ring", n as isize, 0),
        None => expect_true(b"C/guesser reported ring", false, -1),
    }

    // Give the port and the region back; the pools are 16 deep and phase D
    // creates a lot of tasks after this.
    expect_eq(b"C/owner port_destroy", sys::port_destroy(port as u64), 0);
    expect_eq(b"C/owner ipc_unshare", sys::ipc_unshare(shm as u64), 0);
}

// ── Phase D: typed capability past task 64 ──────────────────────────────────

/// How many short-lived tasks to create before the typed-cap probe. TIDs are
/// monotone from 1 and the kernel has already used a couple of dozen by the
/// time userspace runs, so this comfortably clears `MAX_TASKS` = 64 with
/// margin — but the probe reports the TID it actually got rather than
/// trusting the arithmetic.
const D_SPAWNS: u32 = 90;
/// Milliseconds between spawns. The pool is 64 slots and `sys_wait` is
/// unimplemented, so each child must be scheduled and reaped before the next
/// fork or the pool runs dry.
const D_SPAWN_GAP: u64 = 2;
/// The pool can still be momentarily full; a refused fork is retried, not
/// treated as fatal, up to this many times.
const D_MAX_REFUSALS: u32 = 40;

const D_BIT_PORT_CREATE: u32 = 1;
const D_BIT_PORT_POLL_EMPTY: u32 = 2;
const D_BIT_PORT_DESTROY: u32 = 4;
const D_BIT_SHM_CREATE: u32 = 8;
const D_BIT_SHM_ACQUIRE: u32 = 16;
const D_MASK_ALL: u32 =
    D_BIT_PORT_CREATE | D_BIT_PORT_POLL_EMPTY | D_BIT_PORT_DESTROY | D_BIT_SHM_CREATE | D_BIT_SHM_ACQUIRE;

fn phase_d_probe() -> ! {
    post(TAG_T_TID, sys::getpid() as u32);

    let mut mask = 0u32;

    let cap = sys::port_create_typed();
    if cap > 0 {
        mask |= D_BIT_PORT_CREATE;
        let mut ev = [0u8; sys::PORT_EVENT_BYTES];
        // A port with nothing bound has an empty queue, so the typed poll
        // must report empty (-EAGAIN) rather than a cap error. Both are
        // negative; the point of the check is that the cap *resolved* far
        // enough to reach the queue at all, which a dead cap table cannot do.
        if sys::port_poll_typed(cap as u32, &mut ev) < 0 {
            mask |= D_BIT_PORT_POLL_EMPTY;
        }
        if sys::port_destroy_typed(cap as u32) == 0 {
            mask |= D_BIT_PORT_DESTROY;
        }
    }

    let shm = sys::shm_create_typed(1, sys::SHM_RW);
    if shm > 0 {
        mask |= D_BIT_SHM_CREATE;
        let mut info = [0u8; sys::SHM_INFO_BYTES];
        if sys::shm_acquire_typed(shm as u32, &mut info) == sys::SHM_INFO_BYTES as isize {
            mask |= D_BIT_SHM_ACQUIRE;
        }
        // One release for the acquire above, one for the create reference.
        sys::shm_release_typed(shm as u32);
        sys::shm_release_typed(shm as u32);
    }

    post(TAG_T_MASK, mask);
    sys::exit(0);
}

fn phase_d() {
    sys::println(b"[IPCTEST] D: typed cap past task 64");

    let mut created = 0u32;
    let mut refused = 0u32;
    while created < D_SPAWNS && refused < D_MAX_REFUSALS {
        let p = spawn(ROLE_D_EXIT, 0);
        if p < 0 {
            refused = refused.saturating_add(1);
            sys::sleep(5);
            continue;
        }
        created = created.saturating_add(1);
        sys::sleep(D_SPAWN_GAP);
    }
    let mut l = Line::new();
    l.s(b"[IPCTEST] D: spawned ").i(created as isize)
        .s(b" task(s), fork refused ").i(refused as isize).s(b" time(s)");
    l.flush();

    let p = spawn(ROLE_D_PROBE, 0);
    if p < 0 {
        expect_err(b"D/fork-probe (unexpected failure)", p);
        return;
    }

    let tid = match wait_tag(TAG_T_TID, WAIT_MS) {
        Some(t) => t,
        None => {
            expect_true(b"D/probe announced its TID", false, -1);
            return;
        }
    };
    // The whole point of the phase. If this fails the phase proved nothing,
    // so say so with the number instead of reporting a green mask.
    expect_true(b"D/probe TID > MAX_TASKS(64)", tid > 64, tid as isize);

    match wait_tag(TAG_T_MASK, WAIT_MS) {
        Some(m) => expect_eq(b"D/typed caps work at high TID", m as isize, D_MASK_ALL as isize),
        None => expect_true(b"D/probe reported its mask", false, -1),
    }
}

// ── Phase E: fast-IPC carries the REQUEST, not just the wake ────────────────
//
// Phase B already proves a fast-IPC exchange can complete and that an
// impostor cannot answer it. It does **not** prove the exchange transports
// anything: its server replies a constant, so it would pass unchanged on a
// kernel that told the server nothing but "someone called you". That is
// exactly what `SYS_IPC_FAST_ACCEPT` did until CARRIL 4 — it read
// `(slot, caller_tid, words)` from `fast_ipc_accept` and returned only the
// slot, while its own comment claimed the words were written into the
// server's trap frame by a waker that does not exist.
//
// This phase closes that hole with an echo *transform*: the client derives
// its request from its own TID, the server answers a value computed from all
// four request words, and the parent — which knows the client's TID and can
// compute the same function independently — asserts the answer. A server
// that never saw the request cannot produce it, and neither can a kernel
// that delivers stale or zeroed registers.
//
// **Three-way diagnosis, on purpose.** `fast_ipc_accept_req` pre-loads a
// sentinel into `a1` and reports `delivered = false` when the kernel did not
// write it back. So a red phase E says *which* of three things is true:
//   * `E/server received the request payload` FAIL → the trap handler in
//     `kernel/src/main.rs` still calls `syscall_dispatch` (the shim) instead
//     of `syscall_dispatch_out`, and the kernel-side delivery is inert;
//   * that check passes but `E/server saw the exact request words` FAILs →
//     a real delivery bug;
//   * nothing reports at all → K-C12, the forked child that never runs.
// Without the sentinel the first two are the same failing assertion, and a
// report would have to hedge between them.

/// Tag in the top bits of request word 0, so a stray word is not mistaken
/// for a request.
const E_REQ_TAG: u64 = 0x00E0_0000;
/// Tag in the top bits of the reply, same reason.
const E_REPLY_TAG: u64 = 0x0070_0000;
/// What the server answers when the kernel did not hand it the payload.
/// Distinct from every legal reply, so the client's verdict says "undelivered"
/// rather than "wrong value".
const E_UNDELIVERED: u64 = 0x0071_0000;

const E_SAW_DELIVERED: u32 = 1;
const E_SAW_WORDS: u32 = 2;
const E_SAW_CALLER: u32 = 4;
/// Set whenever the accept itself succeeded, delivered or not. Without it
/// `mask == 0` would mean two different things — "the accept returned None"
/// and "the accept succeeded but the kernel wrote no payload" — and all three
/// verdict lines below would print `rc=0` for either. `mask == 0` now means
/// no accept; `mask == 8` means accepted and undelivered.
const E_SAW_ACCEPTED: u32 = 8;

static mut SRV_E_TID: u32 = 0;

/// The request a client with TID `tid` sends. A pure function of the TID, so
/// the parent can reconstruct it without a second back-channel.
fn e_request(tid: u32) -> [u64; sys::FAST_IPC_MAX_WORDS] {
    let t = tid as u64;
    [
        E_REQ_TAG | (t & 0xFFFF),
        t.wrapping_mul(7).wrapping_add(0x1003),
        t.wrapping_mul(11).wrapping_add(0x2005),
        t.wrapping_mul(13).wrapping_add(0x3007),
    ]
}

/// The transform the server applies. Depends on **all four** words, so a
/// server that received only word 0 cannot produce it either.
///
/// Masked into 20 bits and tagged: the result travels back through `a0`,
/// where a negative `i64` means "call failed", and through the 32-bit
/// mailbox. Both constrain it to small non-negative values.
fn e_reply(w: &[u64; sys::FAST_IPC_MAX_WORDS]) -> u64 {
    E_REPLY_TAG | ((w[0] ^ w[1] ^ w[2] ^ w[3]) & 0x000F_FFFF)
}

fn phase_e_server() -> ! {
    let me = sys::getpid();
    let mut l = Line::new();
    l.s(b"[IPCTEST] E: server tid=").i(me);
    l.flush();
    post(TAG_E_SRV_TID, me as u32);

    match sys::fast_ipc_accept_req() {
        Some(req) => {
            let mut mask = E_SAW_ACCEPTED;
            if req.delivered {
                mask |= E_SAW_DELIVERED;
                // The client embeds its own `getpid()` in word 0; the kernel
                // reports the caller TID independently. Agreement between the
                // two is what verifies the TID half of the delivery — a
                // constant would verify nothing.
                if (req.words[0] & 0xFFFF) == (req.caller_tid as u64 & 0xFFFF) {
                    mask |= E_SAW_CALLER;
                }
                if req.words == e_request(req.caller_tid) {
                    mask |= E_SAW_WORDS;
                }
            }
            let mut l = Line::new();
            l.s(b"[IPCTEST] E: server slot=").i(req.slot as isize)
                .s(b" from=").i(req.caller_tid as isize)
                .s(b" delivered=").i(req.delivered as isize)
                .s(b" w0=").i(req.words[0] as isize);
            l.flush();
            post(TAG_E_SRV_SAW, mask);

            let answer = if req.delivered { e_reply(&req.words) } else { E_UNDELIVERED };
            sys::fast_ipc_reply(req.handle, [answer, 0, 0, 0]);
        }
        // No accept: nothing was claimed, so there is nothing to reply to and
        // no slot is leaked here. The client, however, is still blocked on
        // its own slot until its bounded retry gives up — which is why this
        // phase runs before A rather than sharing tasks with it.
        None => post(TAG_E_SRV_SAW, 0),
    }
    sys::exit(0);
}

fn phase_e_client() -> ! {
    let me = sys::getpid() as u32;
    post(TAG_E_CLI_TID, me);
    let words = e_request(me);
    let got = sys::fast_ipc_call(unsafe { SRV_E_TID }, words);
    post(
        TAG_E_CLI_GOT,
        match got {
            Some(w) => w as u32,
            None => u32::MAX,
        },
    );
    sys::exit(0);
}

fn phase_e() {
    sys::println(b"[IPCTEST] E: fast-IPC request delivery (echo transform)");

    let sp = spawn(ROLE_E_SERVER, 0);
    if sp < 0 {
        expect_err(b"E/fork-server (unexpected failure)", sp);
        return;
    }
    let s = match wait_tag(TAG_E_SRV_TID, WAIT_MS) {
        Some(t) => t,
        None => {
            expect_true(b"E/server announced its TID", false, -1);
            return;
        }
    };
    unsafe { SRV_E_TID = s };

    let cp = spawn(ROLE_E_CLIENT, 0);
    if cp < 0 {
        expect_err(b"E/fork-client (unexpected failure)", cp);
        return;
    }
    let ctid = match wait_tag(TAG_E_CLI_TID, WAIT_MS) {
        Some(t) => t,
        None => {
            // K-C12 territory: the child got a positive TID and never ran.
            expect_true(b"E/client announced its TID", false, -1);
            return;
        }
    };

    match wait_tag(TAG_E_SRV_SAW, WAIT_MS) {
        Some(mask) => {
            expect_true(
                b"E/server accepted the call",
                mask & E_SAW_ACCEPTED != 0,
                mask as isize,
            );
            expect_true(
                b"E/server received the request payload",
                mask & E_SAW_DELIVERED != 0,
                mask as isize,
            );
            expect_true(
                b"E/server saw the caller TID",
                mask & E_SAW_CALLER != 0,
                mask as isize,
            );
            expect_true(
                b"E/server saw the exact request words",
                mask & E_SAW_WORDS != 0,
                mask as isize,
            );
        }
        None => expect_true(b"E/server reported at all", false, -1),
    }

    // Actuation: the request reaching the server is only half of an RPC. The
    // parent recomputes the expected answer from the client's TID alone, so
    // this passes only if the reply is genuinely a function of the request.
    let want = e_reply(&e_request(ctid)) as isize;
    match wait_tag(TAG_E_CLI_GOT, WAIT_MS) {
        Some(w) if w as u64 == E_UNDELIVERED => expect_true(
            b"E/client got the transformed reply (UNDELIVERED)",
            false,
            w as isize,
        ),
        Some(w) => expect_eq(b"E/client got the transformed reply", w as isize, want),
        None => expect_true(b"E/client reported at all", false, -1),
    }
}

// ── Entry ───────────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::println(b"[IPCTEST] Starting - ring-3 IPC probe");

    let ch = sys::chan_create();
    if ch < 0 {
        // Without the back-channel no child can report a verdict, and a run
        // that cannot fail is worse than no run at all.
        let mut l = Line::new();
        l.s(b"[IPCTEST]  FAIL  chan_create rc=").i(ch);
        l.flush();
        sys::println(b"[IPCTEST] FAILED: 1 check(s)");
        sys::exit(1);
    }
    unsafe { CHAN = ch };

    // Phase A runs LAST, deliberately. It is expected to hang today (see the
    // header), and a hang ends the run: anything scheduled after it would
    // produce no verdict at all. Ordering the cheap, terminating phases first
    // means a wedged fast-IPC path costs one phase of coverage, not five.
    fork_reg_canary();
    phase_b();
    phase_c_cycle();
    phase_c_gates();
    phase_d();
    phase_e();
    phase_a_race();

    let failed = unsafe { FAILURES };
    let total = unsafe { CHECKS };
    let mut l = Line::new();
    l.s(b"[IPCTEST] ").i(total as isize).s(b" check(s) run");
    l.flush();
    if failed == 0 {
        sys::println(b"[IPCTEST] ALL PASSED");
        sys::exit(0);
    } else {
        let mut l = Line::new();
        l.s(b"[IPCTEST] FAILED: ").i(failed as isize).s(b" check(s)");
        l.flush();
        sys::exit(1);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::println(b"[IPCTEST] FAIL panic");
    sys::exit(101);
}
