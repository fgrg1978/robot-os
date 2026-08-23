/// IPC message-passing channels — fixed-pool, lock-free ring buffer per channel.
///
/// Each channel holds up to 8 messages of up to 64 bytes each.
/// Thread-safe via `SpinLock` from `robot_os_sync`.
///
/// API:
///   channel_create()             — allocate a channel, return index
///   channel_send(ch, data)       — enqueue up to 64 bytes
///   channel_recv(ch, buf)        — dequeue one message
///   channel_destroy(ch)          — free the channel
///   channel_info()               — print channel pool stats

use robot_os_sync::SpinLock;
use wcet_macro::wcet;
pub use robot_os_limits::MAX_CHANNELS;

// ── Caller attribution ───────────────────────────────────────────────────────
//
// **WHY this exists (Carril D / channel ownership).** `channel_recv` and
// `channel_destroy` take a raw pool index and used to authorize nothing, so
// any ring-3 task could walk `0..MAX_CHANNELS` and steal another task's
// messages or free its channel out from under it. To authorize, the function
// must know *who* is calling.
//
// The caller is read here rather than passed in as a parameter, matching what
// `signal.rs` already does in this same crate. The alternative — the explicit
// `(caller_tid, privileged)` parameters used by `handle_revoke` — would change
// the arity of functions called from `crates/bench/src/ipc.rs` and
// `kernel/src/main.rs`, neither of which is in this lane. Nothing inside
// `crates/ipc` ever calls these on behalf of another task (unlike `io_ring`'s
// async worker), so self-attribution denies nothing legitimate.
//
// Cost: `current_task_tid()` and `current_user_pt()` are both a per-CPU index
// load plus one array read (`scheduler.rs:1579` / `:1617`) — no locks, no
// scans, ~4 loads total. That is the whole reason this shape was chosen over
// `handle_owned_by`, whose 256-entry locked sweep costs +2.5 µs typical and
// +97.8 µs on a miss (measured, see `handle.rs:194`). Channels are the
// optimized IPC path; a scan here would be a regression, a field compare is
// noise against the 1879 ns syscall floor.
//
/// Returns `(caller_tid, privileged)`. `privileged` is the house convention:
/// a kernel task (`current_user_pt() == 0`) bypasses every ownership check,
/// exactly as `cap_check`, `cap_store`'s typed callers, and `port_access_ok` do.
#[cfg(not(test))]
#[inline(always)]
fn caller_ctx() -> (u32, bool) {
    (
        robot_os_sched::current_task_tid(),
        robot_os_sched::current_user_pt() == 0,
    )
}

/// Host-test stand-in for [`caller_ctx`]. `robot_os_sched` cannot be built for
/// the host (RV64-only dependencies), so the host suite drives the identity
/// through these atomics instead. Never compiled into the kernel.
#[cfg(test)]
pub mod test_ctx {
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    pub static TID: AtomicU32 = AtomicU32::new(0);
    pub static PRIVILEGED: AtomicBool = AtomicBool::new(true);

    /// Pretend the next calls come from `tid`, kernel-mode iff `privileged`.
    pub fn set(tid: u32, privileged: bool) {
        TID.store(tid, Ordering::SeqCst);
        PRIVILEGED.store(privileged, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[inline(always)]
fn caller_ctx() -> (u32, bool) {
    use core::sync::atomic::Ordering;
    (
        test_ctx::TID.load(Ordering::SeqCst),
        test_ctx::PRIVILEGED.load(Ordering::SeqCst),
    )
}

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum payload bytes per message.
pub const MSG_MAX_LEN: usize = 64;

/// Number of message slots per channel (ring buffer capacity).
pub const RING_CAP: usize = 8;

// ── Message ──────────────────────────────────────────────────────────────────

#[derive(Copy, Clone)]
struct Message {
    data: [u8; MSG_MAX_LEN],
    len:  u16,
}

impl Message {
    const fn zeroed() -> Self {
        Message { data: [0u8; MSG_MAX_LEN], len: 0 }
    }
}

// ── Channel ──────────────────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq)]
enum ChannelState {
    Free,
    Active,
}

#[derive(Copy, Clone)]
struct Channel {
    state:    ChannelState,
    ring:     [Message; RING_CAP],
    head:     u32,    // read  position (consumer)
    tail:     u32,    // write position (producer)
    tx_count: u32,    // total messages sent
    rx_count: u32,    // total messages received
    /// TID of the task that called [`channel_create`] — the **receiving**
    /// end. Read by [`channel_recv`] and [`channel_destroy`] to authorize.
    ///
    /// `0` is the vacant marker: `current_task_tid()` returns 0 only for
    /// "no current task", and `NEXT_TID` starts at 1 and skips 0 on wrap,
    /// so no live task can ever match it. A free or kernel-boot-time slot
    /// therefore denies every ring-3 caller by construction (fail-closed).
    ///
    /// It also doubles as a generation check: the field is rewritten on
    /// every `channel_create`, so a stale index whose slot was recycled by
    /// another task fails the compare instead of silently aliasing.
    owner:    u32,
}

impl Channel {
    const fn zeroed() -> Self {
        Channel {
            state:    ChannelState::Free,
            ring:     [Message::zeroed(); RING_CAP],
            head:     0,
            tail:     0,
            tx_count: 0,
            rx_count: 0,
            owner:    0,
        }
    }

    /// Number of messages in the ring.
    fn count(&self) -> usize {
        let t = self.tail as usize;
        let h = self.head as usize;
        if t >= h { t - h } else { RING_CAP - h + t }
    }

    /// True when ring is full.
    fn is_full(&self) -> bool {
        (self.tail + 1) % RING_CAP as u32 == self.head
    }

    /// True when ring is empty.
    fn is_empty(&self) -> bool {
        self.head == self.tail
    }
}

// ── Global channel pool ──────────────────────────────────────────────────────

struct ChannelPool {
    channels: [Channel; MAX_CHANNELS],
}

impl ChannelPool {
    const fn new() -> Self {
        ChannelPool {
            channels: [Channel::zeroed(); MAX_CHANNELS],
        }
    }
}

static POOL: SpinLock<ChannelPool> = SpinLock::new(ChannelPool::new());

// ── Public API ───────────────────────────────────────────────────────────────

/// Allocate a new channel from the fixed pool.
/// Returns `Some(index)` on success, `None` if the pool is exhausted.
///
/// The calling task becomes the channel's **owner**: the only task allowed
/// to [`channel_recv`] from it or [`channel_destroy`] it. See
/// [`channel_send`] for why the *send* direction stays open.
pub fn channel_create() -> Option<usize> {
    // Read the caller before taking the lock — neither accessor locks, but
    // keeping the critical section to pure pool work is the house style.
    let (owner, _privileged) = caller_ctx();
    let mut pool = POOL.lock();
    for i in 0..MAX_CHANNELS {
        if pool.channels[i].state == ChannelState::Free {
            pool.channels[i] = Channel::zeroed();
            pool.channels[i].state = ChannelState::Active;
            pool.channels[i].owner = owner;
            return Some(i);
        }
    }
    None
}

/// TID that owns channel `ch`, or `None` for an out-of-range or inactive
/// slot.
///
/// Mirrors `port_owner` so the syscall layer can build a `channel_access_ok`
/// guard at the dispatch boundary if it wants the check in both places.
pub fn channel_owner(ch: usize) -> Option<u32> {
    if ch >= MAX_CHANNELS {
        return None;
    }
    let pool = POOL.lock();
    if pool.channels[ch].state == ChannelState::Active {
        Some(pool.channels[ch].owner)
    } else {
        None
    }
}

/// Send up to 64 bytes on channel `ch`.
///
/// Returns 0 on success, -1 on error (invalid index, channel not active,
/// data too long, or ring full).
///
/// # WHY there is deliberately no ownership check here
///
/// Sending to a channel you do not own is not an attack, it is the
/// **protocol**: `SYS_IPC_CALL` (`crates/syscall/src/dispatch.rs:232`) has
/// the *client* call `channel_send` on the *server's* channel and then block
/// for a reply. Gating send on ownership would break every RPC in the tree.
///
/// A channel is a two-party object, so the right long-term shape is a
/// grantable *send right* held by the sender. This kernel has two candidate
/// mechanisms and neither is usable yet:
///
///  * `handle_owned_by(tid, HandleKind::Channel(ch), …)` — no longer even
///    available: `HandleKind::Channel` was deleted (2026-08-24) as dead code,
///    minted only by the `SYS_HANDLE_GRANT` decode and consulted by no
///    `cap_check` anywhere. It would have cost a 256-entry sweep under
///    `lock_irqsave` anyway — +2.5 µs typical, +97.8 µs on a miss (historical
///    numbers). On the optimized IPC path, against a 1879 ns syscall floor,
///    that would have been a 50× regression. Moot now either way.
///  * The typed `Cap<Channel>` path ([`channel_send_cap`]) is the correct
///    mechanism and is O(1), but ring 3 cannot obtain a cap: there is no
///    `SYS_CAP_GRANT`, only `kernel_grant_channel_cap`
///    (`crates/syscall/src/handlers.rs:1459`) called from kernel seed setup.
///
/// So the unauthenticated-sender problem is **documented, not closed** — it
/// needs an ABI addition, which is a decision for the maintainer, not
/// something to improvise here. What *is* closed is the far worse half:
/// message theft and cross-task destroy, both gated below at O(1).
#[wcet(40_us)]
pub fn channel_send(ch: usize, data: &[u8]) -> i32 {
    if ch >= MAX_CHANNELS || data.len() > MSG_MAX_LEN {
        return -1;
    }

    let mut pool = POOL.lock();
    let chan = &mut pool.channels[ch];

    if chan.state != ChannelState::Active {
        return -1;
    }
    if chan.is_full() {
        return -1;
    }

    let slot = chan.tail as usize;
    // Copy payload into ring slot
    let dst = &mut chan.ring[slot];
    let n = data.len();
    dst.data[..n].copy_from_slice(data);
    dst.len = n as u16;

    chan.tail = (chan.tail + 1) % RING_CAP as u32;
    chan.tx_count = chan.tx_count.wrapping_add(1);
    0
}

// ──────────────────────────────────────────────────────────────────────────
// Cap<Channel> typed wrappers (RFC-0003 W3 reference path)
// ──────────────────────────────────────────────────────────────────────────
//
// # Reference flow
//
// ```ignore
// use robot_os_ipc::cap::{Cap, CapPerms, CapTable, targets::Channel};
// use robot_os_ipc::channel::{channel_create, channel_send_cap};
//
// // 1. Create the resource (returns the legacy `usize` channel ID).
// let ch_id = channel_create().unwrap();
//
// // 2. Mint a typed cap into the per-task table.
// let mut table = CapTable::empty();
// let cap: Cap<Channel> = table
//     .grant(CapPerms::RW, ch_id as u32)
//     .unwrap();
//
// // 3. Send via the typed entry — the kind, generation and perms are
// //    all checked on dereference.
// channel_send_cap(&table, cap, b"hello").unwrap();
// ```
//
// Wave W4+ will replace `CapTable::empty()` here with the per-task
// table populated from the static topology declared in CAPS.TOML
// (RFC-0005). Until then, callers construct an explicit table.

/// Errors returned by the typed `channel_*_cap` functions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChannelCapError {
    /// Capability dereference failed (stale, wrong kind, missing perms).
    Cap(crate::cap::CapError),
    /// Underlying channel is closed or the channel index is invalid.
    Closed,
    /// Send buffer is full.
    Full,
    /// Receive buffer was empty.
    Empty,
    /// Data exceeds [`MSG_MAX_LEN`] or buffer is undersized.
    BadArg,
}

impl From<crate::cap::CapError> for ChannelCapError {
    fn from(e: crate::cap::CapError) -> Self {
        Self::Cap(e)
    }
}

/// Typed `channel_send` taking a `Cap<Channel>` instead of an integer
/// handle. RFC-0003 reference migration path.
///
/// The cap is dereferenced through `table` with `WRITE` permission; the
/// resource ID stored in the slot is the channel index that gets passed
/// to the existing `channel_send`.
pub fn channel_send_cap(
    table: &crate::cap::CapTable,
    cap: crate::cap::Cap<crate::cap::targets::Channel>,
    data: &[u8],
) -> Result<(), ChannelCapError> {
    if data.len() > MSG_MAX_LEN {
        return Err(ChannelCapError::BadArg);
    }
    let ch_idx = table.get(cap, crate::cap::CapPerms::WRITE)? as usize;
    match channel_send(ch_idx, data) {
        0 => Ok(()),
        _ => {
            // The legacy API uses `-1` for both Closed and Full; rather
            // than introduce a breaking change there, distinguish here.
            let pool = POOL.lock();
            if ch_idx >= MAX_CHANNELS
                || pool.channels[ch_idx].state != ChannelState::Active
            {
                Err(ChannelCapError::Closed)
            } else {
                Err(ChannelCapError::Full)
            }
        }
    }
}

/// Typed `channel_recv`. Requires `READ` permission. Returns the number
/// of bytes copied into `buf`, or an error.
pub fn channel_recv_cap(
    table: &crate::cap::CapTable,
    cap: crate::cap::Cap<crate::cap::targets::Channel>,
    buf: &mut [u8],
) -> Result<usize, ChannelCapError> {
    let ch_idx = table.get(cap, crate::cap::CapPerms::READ)? as usize;
    // `gate: None` — the cap *is* the authorization. Routing this through
    // the legacy owner check would break the topology model the typed path
    // exists for: `kernel_grant_channel_cap` mints a READ cap on a channel
    // the kernel seed task created, so the grantee is by definition not the
    // owner. A cap that has already passed kind + generation + perms is a
    // stronger proof than the owner field, not a weaker one.
    let n = recv_core(ch_idx, buf, None);
    if n > 0 {
        Ok(n as usize)
    } else if n == 0 {
        Err(ChannelCapError::Empty)
    } else {
        Err(ChannelCapError::Closed)
    }
}

/// Receive one message from channel `ch` into `buf`.
///
/// Returns the number of bytes copied (> 0) on success,
/// 0 if the ring is empty, -1 on error (invalid index, channel not active,
/// or the caller is not the owner).
///
/// # WHY the ownership check exists
///
/// `SYS_IPC_RECEIVE` / `SYS_CHAN_READ` (`dispatch.rs:847`) pass a raw ring-3
/// integer bounded only by `MAX_CHANNELS`, and receiving is **destructive** —
/// the message is popped. Without this check any task could sweep
/// `0..MAX_CHANNELS` and drain every other task's inbox: it reads the
/// payloads (confidentiality) *and* the rightful receiver never sees them
/// (integrity, and for an RPC server a hang, since the client is blocked
/// waiting for a reply that will now never be produced). Same class as the
/// `port` / `io_ring` / `shm` fixes: the owner field existed nowhere, so the
/// call could not authorize even in principle.
///
/// Cost: one `u32` compare against a field already in the cache line being
/// touched, plus the two O(1) scheduler loads in [`caller_ctx`].
#[wcet(40_us)]
pub fn channel_recv(ch: usize, buf: &mut [u8]) -> i32 {
    let (caller, privileged) = caller_ctx();
    // Kernel tasks bypass, exactly as `port_access_ok` / `cap_check` do.
    recv_core(ch, buf, if privileged { None } else { Some(caller) })
}

/// Shared receive body. `gate == Some(tid)` enforces `owner == tid`;
/// `gate == None` means the caller has already been authorized by a
/// stronger mechanism (kernel privilege, or a typed `Cap<Channel>`).
#[inline]
fn recv_core(ch: usize, buf: &mut [u8], gate: Option<u32>) -> i32 {
    if ch >= MAX_CHANNELS {
        return -1;
    }

    let mut pool = POOL.lock();
    let chan = &mut pool.channels[ch];

    if chan.state != ChannelState::Active {
        return -1;
    }
    if let Some(caller) = gate {
        if chan.owner != caller {
            return -1;
        }
    }
    if chan.is_empty() {
        return 0;
    }

    let slot = chan.head as usize;
    let msg = &chan.ring[slot];
    let n = (msg.len as usize).min(buf.len());
    buf[..n].copy_from_slice(&msg.data[..n]);

    chan.head = (chan.head + 1) % RING_CAP as u32;
    chan.rx_count = chan.rx_count.wrapping_add(1);
    n as i32
}

/// Free channel `ch`, returning it to the pool.
///
/// Returns 0 on success, -1 if the index is out of range or the caller is
/// neither the owner nor a kernel task.
///
/// # WHY the ownership check exists
///
/// `SYS_IPC_DESTROY` (`handlers.rs:1354`) took a raw ring-3 index and
/// unconditionally zeroed the slot. That is a one-instruction denial of
/// service against any other task on the board — including a userspace
/// driver's command channel — and worse, the slot is immediately
/// re-allocatable, so the attacker can then `channel_create` it back and
/// become the owner of an id the victim still believes it holds. On a robot
/// that is a control-path outage, not a nuisance.
///
/// Signature changed from `()` to `i32` so the syscall layer can return
/// `E_PERM` instead of a silent success.
pub fn channel_destroy(ch: usize) -> i32 {
    if ch >= MAX_CHANNELS {
        return -1;
    }
    let (caller, privileged) = caller_ctx();
    let mut pool = POOL.lock();
    if !privileged
        && pool.channels[ch].state == ChannelState::Active
        && pool.channels[ch].owner != caller
    {
        return -1;
    }
    pool.channels[ch] = Channel::zeroed();
    // state is already Free after zeroed()
    0
}

/// Wipe the whole pool. Host-test hygiene only — the suite shares one static
/// `POOL`, so each test starts from a known state. Never built into the
/// kernel: a reachable "free every channel on the board" entry point is
/// precisely the DoS that [`channel_destroy`] above closes.
#[cfg(test)]
pub fn __channel_reset_for_tests() {
    let mut pool = POOL.lock();
    for i in 0..MAX_CHANNELS {
        pool.channels[i] = Channel::zeroed();
    }
}

/// Print channel pool statistics to the console via SBI legacy putchar.
///
/// Uses the RISC-V SBI legacy console putchar (EID 0x01) so the IPC crate
/// does not need a dependency on `robot_os_drivers`.
pub fn channel_info() {
    let pool = POOL.lock();

    let mut active = 0usize;
    for i in 0..MAX_CHANNELS {
        if pool.channels[i].state == ChannelState::Active {
            active += 1;
        }
    }

    sbi_puts("[IPC] Channel pool: ");
    sbi_put_usize(active);
    sbi_puts("/");
    sbi_put_usize(MAX_CHANNELS);
    sbi_puts(" active  (ring_cap=");
    sbi_put_usize(RING_CAP);
    sbi_puts(", msg_max=");
    sbi_put_usize(MSG_MAX_LEN);
    sbi_puts(")\n");

    for i in 0..MAX_CHANNELS {
        let ch = &pool.channels[i];
        if ch.state == ChannelState::Active {
            sbi_puts("[IPC]   ch[");
            sbi_put_usize(i);
            sbi_puts("]  queued=");
            sbi_put_usize(ch.count());
            sbi_puts("  tx=");
            sbi_put_u32(ch.tx_count);
            sbi_puts("  rx=");
            sbi_put_u32(ch.rx_count);
            sbi_puts("\n");
        }
    }
}

// ── SBI legacy console putchar (EID=0x01) ────────────────────────────────────
//
// Minimal self-contained printing so this crate does not depend on
// robot_os_drivers.  SBI legacy putchar is universally supported on
// QEMU virt, VF2, and K1.

#[cfg(target_arch = "riscv64")]
fn sbi_putchar(c: u8) {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0x01usize,   // SBI legacy extension: console putchar
            in("a0") c as usize,
            options(nomem, nostack),
        );
    }
}

/// Non-RISC-V builds have no SBI. The only such build is the host test
/// runner (`crates/ipc-chan-tests`), which pulls this file in with `#[path]`;
/// `a7`/`a0` are RISC-V register names and will not assemble anywhere else.
/// `channel_info` is a diagnostic print, so dropping the output is the
/// correct degradation — the pool logic under test is untouched.
#[cfg(not(target_arch = "riscv64"))]
fn sbi_putchar(_c: u8) {}

fn sbi_puts(s: &str) {
    for b in s.bytes() {
        sbi_putchar(b);
    }
}

fn sbi_put_usize(mut v: usize) {
    if v == 0 {
        sbi_putchar(b'0');
        return;
    }
    let mut buf = [0u8; 20]; // max digits for u64
    let mut pos = buf.len();
    while v > 0 {
        pos -= 1;
        buf[pos] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    for &b in &buf[pos..] {
        sbi_putchar(b);
    }
}

fn sbi_put_u32(v: u32) {
    sbi_put_usize(v as usize);
}
