/// VirtIO Network driver — port of virtio_net_init / virtio_net_send / recv.
///
/// Probes QEMU virt MMIO bus for a VirtIO net device (device_id = 1).
/// Sets up RX queue (0) and TX queue (1).

use super::{
    VirtioDev, Virtq,
    VIRTIO_DEV_NET, VIRTIO_QUEUE_SIZE,
    VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE,
    probe, init as virtio_init, virtq_init,
    virtq_alloc_desc, virtq_free_desc, virtq_submit, virtq_poll,
    mmio_read, mmio_write, VIRTIO_MMIO_STATUS, VIRTIO_STATUS_DRIVER_OK,
};
use robot_os_sync::SpinLock;

// ---- VirtIO Net header (prepended to every packet) ----
//
// The legacy header is 10 bytes (no num_buffers field). Modern v1.0+ adds a
// 2-byte num_buffers when VIRTIO_NET_F_MRG_RXBUF is negotiated. We do NOT
// negotiate any features beyond the implicit defaults, so QEMU uses the
// 10-byte legacy header — confirmed empirically by inspecting raw RX bytes.
// Using 12 here silently shifts the Ethernet frame by 2 bytes and makes
// every ethertype check fail (ARP / IP never recognised → no networking).

#[repr(C)]
struct VirtioNetHdr {
    flags:       u8,
    gso_type:    u8,
    hdr_len:     u16,
    gso_size:    u16,
    csum_start:  u16,
    csum_offset: u16,
}

impl VirtioNetHdr {
    const fn zeroed() -> Self {
        VirtioNetHdr {
            flags: 0, gso_type: 0, hdr_len: 0, gso_size: 0,
            csum_start: 0, csum_offset: 0,
        }
    }
}

const NET_HDR_SIZE: usize = core::mem::size_of::<VirtioNetHdr>();

// ---- RX buffers ----

const RX_BUF_SIZE: usize = 1526; // 1514 ETH + 12 VirtIO net hdr (with padding)
const NUM_RX_BUFS: usize = VIRTIO_QUEUE_SIZE / 2;

// ---- TX buffers ----

/// Max Ethernet frame we will transmit (no jumbo frames).
const TX_BUF_SIZE: usize = 1514;
/// One slot per descriptor index so a frame's buffer is identified by the very
/// descriptor that references it — no separate allocator, and the buffer is
/// live for exactly as long as the device owns the descriptor.
const NUM_TX_BUFS: usize = VIRTIO_QUEUE_SIZE;

struct NetState {
    dev:     VirtioDev,
    rxq:     Virtq,
    txq:     Virtq,
    mac:     [u8; 6],
    ready:   bool,
    rx_bufs: [[u8; RX_BUF_SIZE]; NUM_RX_BUFS],
    /// Driver-owned TX staging. The device is never handed a caller address:
    /// `send` returns as soon as the frame is queued, so a caller buffer could
    /// be reused or popped off the stack while the device is still reading it.
    tx_bufs: [[u8; TX_BUF_SIZE]; NUM_TX_BUFS],
    /// Frames dropped because the TX ring was still full after reclaiming.
    tx_dropped: u32,
}

impl NetState {
    const fn zeroed() -> Self {
        NetState {
            dev:     VirtioDev::zeroed(),
            rxq:     Virtq::zeroed(),
            txq:     Virtq::zeroed(),
            mac:     [0u8; 6],
            ready:   false,
            rx_bufs: [[0u8; RX_BUF_SIZE]; NUM_RX_BUFS],
            tx_bufs: [[0u8; TX_BUF_SIZE]; NUM_TX_BUFS],
            tx_dropped: 0,
        }
    }
}

unsafe impl Send for NetState {}

static NET: SpinLock<NetState> = SpinLock::new(NetState::zeroed());

// QEMU virt MMIO base for VirtIO devices: 0x10001000 .. 0x10008000 (8 slots)
const VIRTIO_MMIO_BASE: usize = 0x1000_1000;
const VIRTIO_MMIO_STRIDE: usize = 0x1000;
const VIRTIO_MMIO_SLOTS: usize = 8;

/// Initialize the VirtIO network device.  Returns Ok(()) if found.
pub fn init() -> Result<(), ()> {
    let mut net = NET.lock();

    // Probe all VirtIO MMIO slots for a net device
    for i in 0..VIRTIO_MMIO_SLOTS {
        let base = VIRTIO_BASE + i * VIRTIO_MMIO_STRIDE;
        let mut dev = VirtioDev::zeroed();
        if unsafe { probe(base, &mut dev) }.is_err() { continue; }
        if dev.device_id != VIRTIO_DEV_NET { continue; }

        // Initialize the device
        unsafe { virtio_init(&mut dev) }?;

        // Set up RX queue (0) and TX queue (1)
        unsafe { virtq_init(&mut dev, 0, &mut net.rxq) }?;
        unsafe { virtq_init(&mut dev, 1, &mut net.txq) }?;

        // Read MAC address from config space (bytes 0-5).
        //
        // `mmio_read` is a 32-bit access, so it must only be issued at 4-byte
        // aligned offsets. The previous version looped `CONFIG + 0..6` and took
        // the low byte of each result: offsets 1/2/3 are unaligned, the device
        // rounds them down to offset 0, and the low byte of that word is always
        // MAC[0]. It therefore produced [m0,m0,m0,m0,m4,m4] — for QEMU's
        // default 52:54:00:12:34:56 that is 52:52:52:52:34:34, a MAC the guest
        // never actually owns. SLIRP tolerated the bogus address, so it went
        // unnoticed until two guests had to ARP for each other.
        //
        // Read the two aligned words instead and unpack. VirtIO MMIO config
        // space is little-endian and RISC-V is LE, so byte n of the config
        // space is byte n of the word, LSB first.
        let w0 = unsafe { mmio_read(dev.base, super::VIRTIO_MMIO_CONFIG) };
        let w1 = unsafe { mmio_read(dev.base, super::VIRTIO_MMIO_CONFIG + 4) };
        net.mac[0] =  w0        as u8;
        net.mac[1] = (w0 >>  8) as u8;
        net.mac[2] = (w0 >> 16) as u8;
        net.mac[3] = (w0 >> 24) as u8;
        net.mac[4] =  w1        as u8;
        net.mac[5] = (w1 >>  8) as u8;

        // DRIVER_OK
        let s = unsafe { mmio_read(dev.base, VIRTIO_MMIO_STATUS) };
        unsafe { mmio_write(dev.base, VIRTIO_MMIO_STATUS, s | VIRTIO_STATUS_DRIVER_OK) };

        // Collect RX buffer pointers before mutably borrowing rxq
        let mut rx_ptrs = [0u64; NUM_RX_BUFS];
        for b in 0..NUM_RX_BUFS {
            rx_ptrs[b] = net.rx_bufs[b].as_ptr() as u64;
        }

        // Populate RX queue with buffers
        for b in 0..NUM_RX_BUFS {
            if let Some(desc_idx) = unsafe { virtq_alloc_desc(&mut net.rxq) } {
                unsafe {
                    (*net.rxq.desc.add(desc_idx)).addr  = rx_ptrs[b];
                    (*net.rxq.desc.add(desc_idx)).len   = RX_BUF_SIZE as u32;
                    (*net.rxq.desc.add(desc_idx)).flags = VIRTQ_DESC_F_WRITE;
                    (*net.rxq.desc.add(desc_idx)).next  = 0;
                    virtq_submit(&dev, 0, desc_idx, &mut net.rxq);
                }
            }
        }

        net.dev   = dev;
        net.ready = true;

        crate::kprintln!(
            "[NET] VirtIO net: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            net.mac[0], net.mac[1], net.mac[2],
            net.mac[3], net.mac[4], net.mac[5]
        );
        return Ok(());
    }

    Err(())
}

/// Get the MAC address of the network interface.
pub fn get_mac() -> [u8; 6] {
    NET.lock().mac
}

/// Returns true if the VirtIO net device was successfully initialized.
pub fn is_ready() -> bool {
    NET.lock().ready
}

/// Free a completed TX chain (hdr -> data) starting at `head`.
///
/// `.next` must be read before each free: `virtq_free_desc` overwrites it to
/// thread the descriptor back onto the free list.
unsafe fn tx_reclaim_chain(net: &mut NetState, head: usize) {
    let qsize = net.txq.num as usize;
    let mut idx = head;
    // Bounded and range-checked. `virtq_free_desc` scrubs the descriptor on
    // free (`flags`/`addr`/`len` zeroed, `.next` rethreaded onto the free
    // list), so a walk that reaches an already-freed descriptor sees no
    // NEXT flag and terminates there instead of following the free list to
    // the 0xFFFF terminator. The flags/next reads below still happen before
    // the free, and the loop stays bounded by qsize as defence in depth
    // against a duplicate completion from the device.
    for _ in 0..qsize {
        if idx >= qsize { return; }
        let d = net.txq.desc.add(idx);
        let has_next = (*d).flags & VIRTQ_DESC_F_NEXT != 0;
        let next = (*d).next as usize;
        virtq_free_desc(&mut net.txq, idx);
        if !has_next { return; }
        idx = next;
    }
}

/// Queue a raw Ethernet frame for transmission. `data` must include the
/// Ethernet header; the VirtIO net header is prepended automatically.
///
/// Asynchronous: returns once the frame is queued, without waiting for the
/// device to consume it. It previously spun on `virtq_poll` until each frame
/// completed, with `NET.lock()` held. Two problems, both fixed here:
///
///  * **Deadlock.** The spin was unbounded and the lock blocked every other
///    network path, RX draining included. Two of these kernels on one link
///    wedge each other permanently: both fill the peer's pipe, both block in
///    TX, and neither can drain the RX that would release the other.
///
///  * **Throughput.** Stalling on every frame caps TX at one frame per
///    completion round-trip. Gigabit is ~81k frames/s at 1500 bytes; a
///    synchronous handshake per frame cannot reach that at any clock rate.
///
/// Returning early means the device reads the buffer after we return, so the
/// frame is staged into driver-owned `tx_bufs` instead of pointing at caller
/// memory — otherwise a stack buffer could be reused mid-DMA. Completed chains
/// are reclaimed lazily at the head of the next call, so a descriptor the
/// device still owns is never recycled underneath it.
pub fn send(data: &[u8]) -> Result<(), ()> {
    if data.is_empty() || data.len() > TX_BUF_SIZE { return Err(()); }

    let mut net = NET.lock();
    if !net.ready { return Err(()); }

    // The only place descriptors come back — which is what makes the early
    // return safe.
    while let Some(done) = unsafe { virtq_poll(&mut net.txq) } {
        unsafe { tx_reclaim_chain(&mut net, done) };
    }

    // Two descriptors per frame: VirtIO header, then payload.
    let hdr_idx = match unsafe { virtq_alloc_desc(&mut net.txq) } {
        Some(i) => i,
        None    => { net.tx_dropped = net.tx_dropped.saturating_add(1); return Err(()); }
    };
    let data_idx = match unsafe { virtq_alloc_desc(&mut net.txq) } {
        Some(i) => i,
        None    => {
            unsafe { virtq_free_desc(&mut net.txq, hdr_idx) };
            net.tx_dropped = net.tx_dropped.saturating_add(1);
            return Err(());
        }
    };

    // The header is constant and read-only, so one shared static is fine even
    // with several frames in flight; only the payload is per-descriptor.
    static TX_HDR: VirtioNetHdr = VirtioNetHdr::zeroed();

    let len = data.len();
    net.tx_bufs[data_idx][..len].copy_from_slice(data);
    let buf_ptr = net.tx_bufs[data_idx].as_ptr() as u64;
    let dev = net.dev;

    unsafe {
        (*net.txq.desc.add(hdr_idx)).addr  = &TX_HDR as *const _ as u64;
        (*net.txq.desc.add(hdr_idx)).len   = NET_HDR_SIZE as u32;
        (*net.txq.desc.add(hdr_idx)).flags = VIRTQ_DESC_F_NEXT;
        (*net.txq.desc.add(hdr_idx)).next  = data_idx as u16;

        (*net.txq.desc.add(data_idx)).addr  = buf_ptr;
        (*net.txq.desc.add(data_idx)).len   = len as u32;
        (*net.txq.desc.add(data_idx)).flags = 0;
        (*net.txq.desc.add(data_idx)).next  = 0;

        virtq_submit(&dev, 1, hdr_idx, &mut net.txq);
    }

    Ok(())
}

/// Frames dropped because the TX ring was full. Diagnostic only.
pub fn tx_dropped() -> u32 { NET.lock().tx_dropped }


/// Poll for a received Ethernet frame.  Copies data into `buf`, returns byte count.
/// Returns 0 if no packet is available.
pub fn poll_recv(buf: &mut [u8]) -> usize {
    let mut net = NET.lock();
    if !net.ready { return 0; }

    // Use _with_len so we know the actual frame length — without it we'd
    // hand the network stack the entire RX_BUF_SIZE and ethertype/headers
    // would be parsed out of stale buffer contents (silently broken: ARP
    // never matches, TCP SYN never seen, accept() never returns).
    let (desc_idx, dev_len) = match unsafe {
        crate::virtio::virtq_poll_with_len(&mut net.rxq)
    } {
        Some(x) => x,
        None    => return 0,
    };

    // The descriptor points to one of our rx_bufs
    let rx_buf_ptr = unsafe { (*net.rxq.desc.add(desc_idx)).addr } as usize;
    // Find which rx_buf slot this is
    let slot_opt = (0..NUM_RX_BUFS).find(|&s| {
        net.rx_bufs[s].as_ptr() as usize == rx_buf_ptr
    });
    if let Some(slot) = slot_opt {
        // dev_len includes the VirtIO net header; subtract it to get the
        // Ethernet frame length.
        //
        // Clamp against our own buffer: `dev_len` is written by the DEVICE into
        // the used ring and is not trustworthy. `virtq_poll_with_len` documents
        // that the caller must cap it, and this caller did not — a device
        // reporting more than RX_BUF_SIZE produced an out-of-range slice, which
        // panics, and `panic = "abort"` makes that a board reset triggered by
        // an inbound frame.
        let avail     = RX_BUF_SIZE.saturating_sub(NET_HDR_SIZE);
        let frame_len = dev_len.saturating_sub(NET_HDR_SIZE).min(avail);
        let packet = &net.rx_bufs[slot][NET_HDR_SIZE..NET_HDR_SIZE + frame_len];
        let n = packet.len().min(buf.len());
        buf[..n].copy_from_slice(&packet[..n]);

        // Re-queue the buffer (copy dev before mutable rxq borrow).
        // Rewrite addr/len as well as flags: it costs two stores and restores
        // the descriptor↔buffer invariant even if something scribbled on the
        // descriptor table (virtq_free_desc scrubs addr/len to 0 these days,
        // so a descriptor that ever passed through the free list would
        // otherwise be re-queued pointing at address 0).
        let dev = net.dev;
        let addr = net.rx_bufs[slot].as_ptr() as u64;
        unsafe {
            let d = net.rxq.desc.add(desc_idx);
            (*d).addr  = addr;
            (*d).len   = RX_BUF_SIZE as u32;
            (*d).flags = VIRTQ_DESC_F_WRITE;
            (*d).next  = 0;
            virtq_submit(&dev, 0, desc_idx, &mut net.rxq);
        }

        return n;
    }

    // Descriptor's addr doesn't match any rx_buf (device corruption, or a
    // scrubbed descriptor that leaked through the free list). Freeing it here
    // — the previous behaviour — permanently shrank the RX ring: with only
    // NUM_RX_BUFS buffers, a handful of these and the node goes deaf. RX
    // descriptors are paired 1:1 with rx_bufs[desc_idx] at init (allocated in
    // order from a fresh free list), so when the index is in range we can
    // restore that pairing and put the buffer back in service. Only an
    // out-of-range index — which virtq_poll_with_len already rejects — would
    // fall through to a free.
    if desc_idx < NUM_RX_BUFS {
        let dev  = net.dev;
        let addr = net.rx_bufs[desc_idx].as_ptr() as u64;
        unsafe {
            let d = net.rxq.desc.add(desc_idx);
            (*d).addr  = addr;
            (*d).len   = RX_BUF_SIZE as u32;
            (*d).flags = VIRTQ_DESC_F_WRITE;
            (*d).next  = 0;
            virtq_submit(&dev, 0, desc_idx, &mut net.rxq);
        }
    } else {
        unsafe { virtq_free_desc(&mut net.rxq, desc_idx) };
    }
    0
}

/// Print network device info.
pub fn info() {
    let net = NET.lock();
    if !net.ready {
        crate::kprintln!("[NET] VirtIO net: not initialized");
        return;
    }
    crate::kprintln!(
        "[NET] VirtIO net ready — MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        net.mac[0], net.mac[1], net.mac[2],
        net.mac[3], net.mac[4], net.mac[5]
    );
}

// Alias for clarity
const VIRTIO_BASE: usize = VIRTIO_MMIO_BASE;
