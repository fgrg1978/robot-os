/// BSD socket API — port of net/socket.c
///
/// Thin socket layer over tcp/udp.  16 socket slots total.

use robot_os_sync::SpinLock;
use super::{tcp, udp};

pub const MAX_SOCKETS: usize = 16;

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

#[derive(Clone, Copy)]
struct Socket {
    kind:   SockKind,
    slot:   i32,   // tcp conn index or udp socket index
    local:  SockAddr,
    remote: SockAddr,
}

impl Socket {
    const fn new() -> Self {
        Socket {
            kind:   SockKind::Free,
            slot:   -1,
            local:  SockAddr::new(),
            remote: SockAddr::new(),
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

/// Create a new socket.  Returns file-descriptor-style index or -1.
pub fn socket_create(domain: u32, sock_type: u32, _proto: u32) -> i32 {
    if domain != AF_INET { return -1; }
    let kind = match sock_type {
        SOCK_STREAM => SockKind::Tcp,
        SOCK_DGRAM  => SockKind::Udp,
        _           => return -1,
    };
    let mut t = SOCKS.lock();
    let idx = t.alloc().ok_or(-1i32).unwrap_or_else(|_| usize::MAX);
    if idx == usize::MAX { return -1; }
    t.sockets[idx].kind   = kind;
    t.sockets[idx].slot   = -1;
    t.sockets[idx].local  = SockAddr::new();
    t.sockets[idx].remote = SockAddr::new();
    idx as i32
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
            i as i32
        }
        None => -1,
    }
}

/// Connect a TCP socket to a remote address.
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

/// Close a socket.
pub fn socket_close(fd: i32) {
    if fd < 0 || fd as usize >= MAX_SOCKETS { return; }
    let fd = fd as usize;
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
    SOCKS.lock().sockets[fd].kind = SockKind::Free;
}
