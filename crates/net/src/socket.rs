/// BSD socket API — port of net/socket.c
///
/// Thin socket layer over tcp/udp.  16 socket slots total.

use robot_os_sync::SpinLock;
use super::{tcp, udp};
pub use robot_os_limits::MAX_SOCKETS;

// Domain
pub const AF_INET: u32  = 2;

// Type
pub const SOCK_STREAM: u32 = 1;   // TCP
pub const SOCK_DGRAM:  u32 = 2;   // UDP

// Proto
pub const IPPROTO_TCP: u32 = 6;
pub const IPPROTO_UDP: u32 = 17;

#[derive(Clone, Copy)]
pub struct SockAddr {
    pub family: u16,
    pub port:   u16,    // host byte order
    pub addr:   [u8; 4],
}

impl SockAddr {
    pub const fn new() -> Self {
        SockAddr { family: 0, port: 0, addr: [0; 4] }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum SockKind {
    Free,
    Tcp,
    Udp,
}

/// Owner stamp for sockets created from **kernel** context: the boot-time
/// brain link, the OTA listener, TFTP, and the shell's echo server. No real
/// TID can equal this (`NEXT_TID` is a wrapping counter that skips 0 and
/// would need to reach `u32::MAX` — and even then the value is only ever
/// *compared*, never used to reach a slot), so a kernel socket is
/// unreachable from userspace by construction rather than by a conditional.
pub const SOCK_OWNER_KERNEL: u32 = u32::MAX;

/// Owner stamp of a slot nobody owns (free, or created before an owner could
/// be determined). `current_task_tid()` returns 0 for "no current task", and
/// `NEXT_TID` starts at 1 and skips 0 on wrap, so 0 is never a live TID —
/// which makes it safe as the "matches nobody" value. [`socket_owner`]
/// reports it as `None`, so a caller comparing owners can never match it.
const SOCK_OWNER_NONE: u32 = 0;

#[derive(Clone, Copy)]
struct Socket {
    kind:   SockKind,
    slot:   i32,   // tcp conn index or udp socket index
    local:  SockAddr,
    remote: SockAddr,
    /// TID that created this socket, or [`SOCK_OWNER_KERNEL`].
    ///
    /// **WHY this field exists and what reads it:** `SOCKS` is one flat
    /// 16-entry array and the fd *is* the index into it, chosen by
    /// userspace. Every entry point below used to validate only
    /// `fd < MAX_SOCKETS`, so any task could walk fd 0..15 and read another
    /// task's inbound TCP stream, inject bytes into its outbound stream, or
    /// tear down its connection — the OTA channel and the brain link
    /// included. Sixteen guesses covered the whole table.
    ///
    /// The stamp is taken at **create/accept** time, not read from the
    /// scheduler at use time: kernel-side poll paths and worker threads run
    /// with `user_pt == 0`, where a "who is running now" check silently
    /// enforces nothing.
    ///
    /// The check itself lives in `crates/syscall` (`socket_access_ok` in
    /// `handlers.rs`), which is where the TID comes from — `crates/net` is
    /// scheduler-agnostic by design and must not depend on `crates/sched`.
    /// Read it through [`socket_owner`]. **A field with no check is worse
    /// than no field: it reads as protection while enforcing nothing.**
    owner:  u32,
}

impl Socket {
    const fn new() -> Self {
        Socket {
            kind:   SockKind::Free,
            slot:   -1,
            local:  SockAddr::new(),
            remote: SockAddr::new(),
            owner:  SOCK_OWNER_NONE,
        }
    }
}

struct SocketTable {
    sockets: [Socket; MAX_SOCKETS],
}

impl SocketTable {
    const fn new() -> Self {
        SocketTable { sockets: [Socket::new(); MAX_SOCKETS] }
    }

    fn alloc(&mut self) -> Option<usize> {
        for i in 0..MAX_SOCKETS {
            if self.sockets[i].kind == SockKind::Free { return Some(i); }
        }
        None
    }
}

static SOCKS: SpinLock<SocketTable> = SpinLock::new(SocketTable::new());

/// Create a new socket **owned by the kernel**. Returns a
/// file-descriptor-style index or -1.
///
/// This 3-argument form is the entry point for in-kernel users (the brain
/// link and the two-node TCP probe in `kernel/src/main.rs`, the OTA listener
/// and echo server in `crates/shell`). Sockets it hands out carry
/// [`SOCK_OWNER_KERNEL`] and are therefore never reachable through the
/// syscall gate. Userspace goes through [`socket_create_owned`].
pub fn socket_create(domain: u32, sock_type: u32, proto: u32) -> i32 {
    socket_create_owned(domain, sock_type, proto, SOCK_OWNER_KERNEL)
}

/// Create a new socket stamped with `owner`. Returns a
/// file-descriptor-style index or -1.
///
/// `owner` is the TID the socket belongs to, supplied by the caller rather
/// than read from the scheduler here so `crates/net` stays scheduler-
/// agnostic. Every later entry point is gated against this stamp — see the
/// `owner` field on `Socket` for what that prevents.
pub fn socket_create_owned(domain: u32, sock_type: u32, _proto: u32, owner: u32) -> i32 {
    if domain != AF_INET { return -1; }
    let kind = match sock_type {
        SOCK_STREAM => SockKind::Tcp,
        SOCK_DGRAM  => SockKind::Udp,
        _           => return -1,
    };
    let mut t = SOCKS.lock();
    let idx = t.alloc().ok_or(-1i32).unwrap_or_else(|_| usize::MAX);
    if idx == usize::MAX { return -1; }
    // Every field is re-initialised explicitly because slots are recycled.
    // `owner` in particular MUST be written here: leaving the previous
    // occupant's TID in place would invert the ownership check — the task
    // that created the socket could not use it, and the task that used to
    // own the slot could.
    t.sockets[idx].kind   = kind;
    t.sockets[idx].slot   = -1;
    t.sockets[idx].local  = SockAddr::new();
    t.sockets[idx].remote = SockAddr::new();
    t.sockets[idx].owner  = owner;
    idx as i32
}

/// TID that owns `fd`, or `None` if the fd is out of range, free, or
/// unowned.
///
/// This is the accessor the syscall layer's ownership gate reads; it exists
/// so `crates/net` can expose the stamp without importing the scheduler.
/// Mirrors `robot_os_ipc::shm_owner` / `port_owner`.
pub fn socket_owner(fd: i32) -> Option<u32> {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return None; }
    let t = SOCKS.lock();
    let s = &t.sockets[fd as usize];
    if s.kind == SockKind::Free || s.owner == SOCK_OWNER_NONE {
        return None;
    }
    Some(s.owner)
}

/// Bind a socket to a local address/port.
pub fn socket_bind(fd: i32, addr: &SockAddr) -> i32 {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return -1; }
    let fd = fd as usize;
    let kind = { SOCKS.lock().sockets[fd].kind };
    match kind {
        SockKind::Udp => {
            let slot = udp::bind(addr.port);
            if slot < 0 { return -1; }
            let mut t = SOCKS.lock();
            t.sockets[fd].slot  = slot;
            t.sockets[fd].local = *addr;
            0
        }
        SockKind::Tcp => {
            // TCP bind: just store the local port; tcp::listen() is called by socket_listen_bound().
            SOCKS.lock().sockets[fd].local = *addr;
            0
        }
        SockKind::Free => -1,
    }
}

/// Listen on a TCP socket using the local port stored during bind().
pub fn socket_listen_bound(fd: i32) -> i32 {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return -1; }
    let fd_idx = fd as usize;
    let (kind, port) = {
        let t = SOCKS.lock();
        (t.sockets[fd_idx].kind, t.sockets[fd_idx].local.port)
    };
    if kind != SockKind::Tcp { return -1; }
    if port == 0 { return -1; }
    let slot = tcp::listen(port);
    if slot < 0 { return -1; }
    SOCKS.lock().sockets[fd_idx].slot = slot;
    0
}

/// Accept an established connection on a listening TCP socket.
/// Returns a new socket fd for the accepted connection, or -1 if none ready yet.
pub fn socket_accept(fd: i32) -> i32 {
    socket_accept_owned(fd, SOCK_OWNER_KERNEL)
}

/// Accept a connection and stamp the **new** socket with `owner`.
///
/// `owner` is the accepting task, which is the right answer: an accepted
/// connection belongs to whoever accepted it, not to the listener's peer and
/// not to whichever task happens to be running when a later poll drains it.
/// Stamping here (rather than letting the new slot inherit whatever the
/// recycled entry held) is what stops an accepted OTA or brain connection
/// from landing in a slot another task can already reach.
///
/// Note this stamps only; the *permission* to accept on `fd` is checked by
/// the caller — `crates/syscall`, which knows the calling TID.
pub fn socket_accept_owned(fd: i32, owner: u32) -> i32 {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return -1; }
    let port = {
        let t = SOCKS.lock();
        let s = &t.sockets[fd as usize];
        if s.kind != SockKind::Tcp { return -1; }
        s.local.port
    };
    let conn_idx = tcp::accept(port);
    if conn_idx < 0 { return -1; }
    let mut t = SOCKS.lock();
    match t.alloc() {
        Some(i) => {
            t.sockets[i].kind       = SockKind::Tcp;
            t.sockets[i].slot       = conn_idx;
            t.sockets[i].local      = SockAddr::new();
            t.sockets[i].local.port = port;
            t.sockets[i].remote     = SockAddr::new();
            t.sockets[i].owner      = owner;
            i as i32
        }
        None => -1,
    }
}

/// Connect a TCP socket to a remote address.
/// Non-blocking connect: queues the SYN and returns immediately, leaving the
/// connection in `SynSent`.
///
/// Deliberately does NOT wait for the handshake. The only in-tree caller runs
/// during boot (`kernel/src/main.rs`), before the scheduler can preempt, so a
/// wait here would have to busy-spin — and busy-spinning a hart through a
/// three-way handshake is worse than returning early and letting the caller's
/// own retry loop deal with it.
///
/// Userspace goes through [`socket_connect_with_yield`] instead, which has
/// somewhere to yield to and therefore can offer real POSIX semantics.
pub fn socket_connect(fd: i32, addr: &SockAddr, src_port: u16) -> i32 {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return -1; }
    let fd = fd as usize;
    let kind = { SOCKS.lock().sockets[fd].kind };
    if kind != SockKind::Tcp { return -1; }
    let slot = tcp::connect(addr.addr, addr.port, src_port);
    if slot < 0 { return -1; }
    let mut t = SOCKS.lock();
    t.sockets[fd].slot   = slot;
    t.sockets[fd].remote = *addr;
    0
}

/// Maximum `yield_fn` calls spent waiting for the three-way handshake before
/// giving up. Generous because it is a yield count, not a duration: how much
/// wall-clock it buys depends entirely on what else is runnable.
const CONNECT_MAX_YIELDS: u32 = 2_000_000;

/// Connect and **wait for the handshake to finish**, yielding meanwhile.
///
/// `socket_connect` used to return 0 the moment `tcp::connect` had queued the
/// SYN, leaving the connection in `SynSent`. Since `tcp::send_data` refuses
/// anything that is not `Established`, an application doing the obvious
/// thing —
///
/// ```text
///     connect(...);          // -> 0, "Connected!"
///     send(...);             // -> -1
/// ```
///
/// — saw a successful connect followed by an immediate send failure, over and
/// over. `userspace/brain_client` sat in exactly that loop, and the log it
/// produced ("Connected!" then "Send failed") pointed at the transport rather
/// than at connect's semantics.
///
/// POSIX is unambiguous here: a blocking `connect()` does not report success
/// until the connection is established. `yield_fn` is injected so `crates/net`
/// stays scheduler-agnostic, the same pattern `connect_with_yield` and
/// `send_all_with_yield` already use.
pub fn socket_connect_with_yield<F: FnMut()>(
    fd: i32,
    addr: &SockAddr,
    src_port: u16,
    mut yield_fn: F,
) -> i32 {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return -1; }
    let fd = fd as usize;
    let kind = { SOCKS.lock().sockets[fd].kind };
    if kind != SockKind::Tcp { return -1; }

    let slot = tcp::connect_with_yield(addr.addr, addr.port, src_port,
                                       &mut yield_fn);
    if slot < 0 { return -1; }

    // Wait out the handshake. Anything that is neither Established nor still
    // in SynSent (a RST closes the connection) is a failure, and reporting it
    // as such is the whole point: an unreachable peer must not look connected.
    let mut yields: u32 = 0;
    loop {
        match tcp::conn_state(slot as usize) {
            tcp::TcpState::Established => break,
            tcp::TcpState::SynSent => {}
            _ => return -1,
        }
        if yields >= CONNECT_MAX_YIELDS { return -1; }
        yield_fn();
        yields += 1;
    }

    let mut t = SOCKS.lock();
    t.sockets[fd].slot   = slot;
    t.sockets[fd].remote = *addr;
    0
}

/// Listen on a TCP socket.
pub fn socket_listen(fd: i32, port: u16) -> i32 {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return -1; }
    let fd = fd as usize;
    let kind = { SOCKS.lock().sockets[fd].kind };
    if kind != SockKind::Tcp { return -1; }
    let slot = tcp::listen(port);
    if slot < 0 { return -1; }
    let mut t = SOCKS.lock();
    t.sockets[fd].slot = slot;
    0
}

/// Send data on a connected socket.
pub fn socket_send(fd: i32, data: &[u8]) -> i32 {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return -1; }
    let (kind, slot) = {
        let t = SOCKS.lock();
        (t.sockets[fd as usize].kind, t.sockets[fd as usize].slot)
    };
    if slot < 0 { return -1; }
    match kind {
        SockKind::Tcp => tcp::send_data(slot as usize, data),
        _ => -1,
    }
}

/// Receive data from a socket (non-blocking).
pub fn socket_recv(fd: i32, buf: &mut [u8]) -> i32 {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return -1; }
    let (kind, slot) = {
        let t = SOCKS.lock();
        (t.sockets[fd as usize].kind, t.sockets[fd as usize].slot)
    };
    if slot < 0 { return -1; }
    match kind {
        SockKind::Tcp => {
            let n = tcp::recv(slot as usize, buf);
            if n == 0 {
                // Return -1 when connection is closing and no data remains.
                let state = tcp::conn_state(slot as usize);
                if state == tcp::TcpState::CloseWait || state == tcp::TcpState::Closed {
                    return -1;
                }
            }
            n
        }
        SockKind::Udp => udp::recv(slot as usize, buf),
        SockKind::Free => -1,
    }
}

/// Tear down slot `fd` (already range-checked) and return it to the pool.
///
/// The transport-level close runs with `SOCKS` released, matching what
/// `socket_close` always did: `tcp::close` / `udp::unbind` take their own
/// locks, and holding `SOCKS` across them would invert the lock order that
/// `net_poll` already relies on.
fn close_slot(fd: usize) {
    let (kind, slot) = {
        let t = SOCKS.lock();
        (t.sockets[fd].kind, t.sockets[fd].slot)
    };
    if slot >= 0 {
        match kind {
            SockKind::Tcp => tcp::close(slot as usize),
            SockKind::Udp => udp::unbind(slot as usize),
            SockKind::Free => {}
        }
    }
    // Reset the whole entry, not just `kind`. Clearing `owner` back to
    // "nobody" matters as much as freeing the slot: a stale TID left behind
    // here is inherited by the next task that draws the same fd, which is the
    // ownership check silently passing for the wrong task.
    SOCKS.lock().sockets[fd] = Socket::new();
}

/// Close a socket.
pub fn socket_close(fd: i32) {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return; }
    close_slot(fd as usize);
}

/// Close every socket owned by `tid` — called from the task-exit hook.
///
/// **WHY the exit hook must do this:** the owner stamp is what gates access,
/// and `NEXT_TID` wraps, so a socket left behind by a dead task is inherited
/// wholesale by the next task that draws the same TID — the gate would then
/// hand a stranger a live TCP stream and report it as correctly owned. It
/// also leaks the underlying `tcp`/`udp` slot for the life of the board,
/// and there are only 16 of them.
///
/// Kernel-owned sockets are never touched: the brain link, the OTA listener
/// and TFTP all run with [`SOCK_OWNER_KERNEL`], and tearing one down
/// mid-flight because some unrelated task exited would drop the channel the
/// robot is being commanded over. `SOCK_OWNER_NONE` (0) is excluded for the
/// same reason — `current_task_tid()` returns 0 for "no current task", so a
/// hook firing with 0 must be a no-op rather than a table-wide sweep.
///
/// Idempotent and safe to call for a TID that owns nothing.
///
/// **CONTEXT NOTE for whoever wires the task-exit hook.** This is not a pure
/// bookkeeping sweep like `shm_release_all`: for a socket in `Established`
/// or `CloseWait`, `tcp::close` **transmits a FIN synchronously** through
/// `send_segment` → the NIC driver. That is the correct behaviour (the peer
/// must learn the connection is gone rather than time out), and it is the
/// same work `socket_close` has always done from ordinary task context, but
/// it does mean the exit path now touches the NIC. Specifically:
///
///  * It does **not** yield and does **not** block — a single frame is
///    pushed into the TX ring, or silently dropped if ARP is unresolved.
///  * No lock is held across the transmit: `close_slot` releases `SOCKS`
///    before calling `tcp::close`, which in turn releases the `TCP` lock
///    before calling `send_segment`.
///  * `SOCKS` uses a plain `SpinLock::lock()`, not `lock_irqsave()`. That is
///    sound only because `SOCKS` is unreachable from IRQ context — the RX
///    path goes `ip::handle` → `tcp::handle_checked`, which touches `TCP`,
///    never this table. Do not add an IRQ-context caller without converting
///    the whole module to `lock_irqsave()` first; see `crates/ipc/port.rs`
///    for the same-hart deadlock this would otherwise create.
pub fn socket_release_all(tid: u32) {
    if tid == SOCK_OWNER_KERNEL || tid == SOCK_OWNER_NONE { return; }
    for fd in 0..MAX_SOCKETS {
        let owned = {
            let t = SOCKS.lock();
            let s = &t.sockets[fd];
            s.kind != SockKind::Free && s.owner == tid
        };
        if owned {
            close_slot(fd);
        }
    }
}
