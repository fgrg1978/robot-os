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

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum number of channels in the fixed pool.
pub const MAX_CHANNELS: usize = 16;

/// Maximum payload bytes per message.
const MSG_MAX_LEN: usize = 64;

/// Number of message slots per channel (ring buffer capacity).
const RING_CAP: usize = 8;

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
pub fn channel_create() -> Option<usize> {
    let mut pool = POOL.lock();
    for i in 0..MAX_CHANNELS {
        if pool.channels[i].state == ChannelState::Free {
            pool.channels[i] = Channel::zeroed();
            pool.channels[i].state = ChannelState::Active;
            return Some(i);
        }
    }
    None
}

/// Send up to 64 bytes on channel `ch`.
///
/// Returns 0 on success, -1 on error (invalid index, channel not active,
/// data too long, or ring full).
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

/// Receive one message from channel `ch` into `buf`.
///
/// Returns the number of bytes copied (> 0) on success,
/// 0 if the ring is empty, -1 on error (invalid index or channel not active).
pub fn channel_recv(ch: usize, buf: &mut [u8]) -> i32 {
    if ch >= MAX_CHANNELS {
        return -1;
    }

    let mut pool = POOL.lock();
    let chan = &mut pool.channels[ch];

    if chan.state != ChannelState::Active {
        return -1;
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
pub fn channel_destroy(ch: usize) {
    if ch >= MAX_CHANNELS {
        return;
    }
    let mut pool = POOL.lock();
    pool.channels[ch] = Channel::zeroed();
    // state is already Free after zeroed()
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
