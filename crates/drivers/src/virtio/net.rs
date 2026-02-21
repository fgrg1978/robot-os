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

#[repr(C)]
struct VirtioNetHdr {
    flags:       u8,
    gso_type:    u8,
    hdr_len:     u16,
    gso_size:    u16,
    csum_start:  u16,
    csum_offset: u16,
    num_buffers: u16,
}

impl VirtioNetHdr {
    const fn zeroed() -> Self {
        VirtioNetHdr {
            flags: 0, gso_type: 0, hdr_len: 0, gso_size: 0,
            csum_start: 0, csum_offset: 0, num_buffers: 0,
        }
    }
}

const NET_HDR_SIZE: usize = core::mem::size_of::<VirtioNetHdr>();

// ---- RX buffers ----

const RX_BUF_SIZE: usize = 1526; // 1514 ETH + 12 VirtIO net hdr (with padding)
const NUM_RX_BUFS: usize = VIRTIO_QUEUE_SIZE / 2;

struct NetState {
    dev:     VirtioDev,
    rxq:     Virtq,
    txq:     Virtq,
    mac:     [u8; 6],
    ready:   bool,
    rx_bufs: [[u8; RX_BUF_SIZE]; NUM_RX_BUFS],
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

        // Read MAC address from config space (bytes 0-5)
        for j in 0..6 {
            net.mac[j] = unsafe {
                mmio_read(dev.base, super::VIRTIO_MMIO_CONFIG + j as u32) as u8
            };
        }

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

/// Send a raw Ethernet frame.  `data` should include the Ethernet header.
/// Prepends a zeroed VirtIO net header automatically.
pub fn send(data: &[u8]) -> Result<(), ()> {
    let mut net = NET.lock();
    if !net.ready { return Err(()); }

    // We need 2 descriptors: one for VirtIO header, one for packet data
    let hdr_idx = match unsafe { virtq_alloc_desc(&mut net.txq) } {
        Some(i) => i,
        None    => return Err(()),
    };
    let data_idx = match unsafe { virtq_alloc_desc(&mut net.txq) } {
        Some(i) => i,
        None    => {
            unsafe { virtq_free_desc(&mut net.txq, hdr_idx) };
            return Err(());
        }
    };

    // VirtIO net header (static zeroed — no GSO/checksum offload needed)
    static TX_HDR: VirtioNetHdr = VirtioNetHdr::zeroed();

    // Copy device handle before mutably borrowing txq (borrow split workaround)
    let dev = net.dev;
    unsafe {
        (*net.txq.desc.add(hdr_idx)).addr  = &TX_HDR as *const _ as u64;
        (*net.txq.desc.add(hdr_idx)).len   = NET_HDR_SIZE as u32;
        (*net.txq.desc.add(hdr_idx)).flags = VIRTQ_DESC_F_NEXT;
        (*net.txq.desc.add(hdr_idx)).next  = data_idx as u16;

        (*net.txq.desc.add(data_idx)).addr  = data.as_ptr() as u64;
        (*net.txq.desc.add(data_idx)).len   = data.len() as u32;
        (*net.txq.desc.add(data_idx)).flags = 0;
        (*net.txq.desc.add(data_idx)).next  = 0;

        virtq_submit(&dev, 1, hdr_idx, &mut net.txq);
    }

    // Poll until TX completes (spin-wait — no interrupt-driven TX for now)
    loop {
        if let Some(done_idx) = unsafe { virtq_poll(&mut net.txq) } {
            unsafe { virtq_free_desc(&mut net.txq, done_idx) };
            break;
        }
    }

    Ok(())
}

/// Poll for a received Ethernet frame.  Copies data into `buf`, returns byte count.
/// Returns 0 if no packet is available.
pub fn poll_recv(buf: &mut [u8]) -> usize {
    let mut net = NET.lock();
    if !net.ready { return 0; }

    let desc_idx = match unsafe { virtq_poll(&mut net.rxq) } {
        Some(i) => i,
        None    => return 0,
    };

    // The descriptor points to one of our rx_bufs
    let rx_buf_ptr = unsafe { (*net.rxq.desc.add(desc_idx)).addr } as usize;
    // Find which rx_buf slot this is
    let slot_opt = (0..NUM_RX_BUFS).find(|&s| {
        net.rx_bufs[s].as_ptr() as usize == rx_buf_ptr
    });
    if let Some(slot) = slot_opt {
        // Skip VirtIO net header (first NET_HDR_SIZE bytes)
        let packet = &net.rx_bufs[slot][NET_HDR_SIZE..];
        let n = packet.len().min(buf.len());
        buf[..n].copy_from_slice(&packet[..n]);

        // Re-queue the buffer (copy dev before mutable rxq borrow)
        let dev = net.dev;
        unsafe {
            (*net.rxq.desc.add(desc_idx)).flags = VIRTQ_DESC_F_WRITE;
            virtq_submit(&dev, 0, desc_idx, &mut net.rxq);
        }

        return n;
    }

    // Buffer not found — free descriptor and return 0
    unsafe { virtq_free_desc(&mut net.rxq, desc_idx) };
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
