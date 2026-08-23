//! DEV02 — USB device-mode controller trait + DWC2 stub.
//!
//! The kernel calls into this trait when entering DFU recovery
//! mode (see [`robot_os_ota::recovery`]).  An impl wraps the
//! board-specific USB OTG/device controller, exposing the four
//! operations DFU needs:
//!
//!   - enumerate (advertise the DFU descriptor blob)
//!   - poll the control endpoint for setup packets
//!   - read OUT data-stage bytes (host→device, DFU_DNLOAD payload)
//!   - write IN data-stage bytes (device→host, DFU_GETSTATUS reply)
//!
//! No bulk endpoints are needed — DFU rides entirely on the
//! control endpoint (EP0).  That keeps the controller surface
//! minimal and means even a half-broken USB stack can usually
//! still flash recovery firmware.
//!
//! ## Status: PRE-HARDWARE STUB
//!
//! Without the physical VF2 / K1 board to validate timing,
//! register layout, and PHY initialisation, only the trait
//! contract and the dispatch wiring are implementable today.
//! The DWC2 impl below is a **scaffold**: every method returns
//! `Err(UsbDeviceError::NotImplemented)`.  When the JH7110 board
//! arrives:
//!
//!   1. Fill in `init()` against the JH7110 USB2.0 OTG controller
//!      registers (datasheet "JH7110 PDM USB OTG SS").
//!   2. Fill in `poll_setup()` to read EP0 OUT FIFO + drain a
//!      complete 8-byte setup packet.
//!   3. Fill in `read_out_data` / `write_in_data` against the
//!      EP0 OUT/IN FIFO control registers (DXEPCTL, DXEPTSIZ).
//!   4. Fill in `enumerate()` with the bus reset wait + the
//!      descriptor table the host requests via `GET_DESCRIPTOR`.
//!
//! Until then this module exists so the rest of the recovery
//! integration (state machine, descriptors, trigger logic) can
//! compile and be tested.

#![allow(dead_code)]

/// Operations a USB device-mode controller must support to host
/// the DFU class. Everything else (bulk endpoints, isochronous,
/// suspend/resume) is out of scope for recovery.
pub trait UsbDeviceController {
    /// Initialise the controller hardware: clock, PHY, mode
    /// select (force-device), interrupt mask, EP0 FIFO sizing.
    /// Called once at recovery-mode boot.
    fn init(&mut self) -> Result<(), UsbDeviceError>;

    /// Wait for bus reset + address-assigned, then advertise the
    /// descriptor blob built by
    /// [`robot_os_dfu::DescriptorBuilder`]. Returns once the host
    /// has issued `SET_CONFIGURATION(1)` and we're ready to take
    /// DFU class requests.
    fn enumerate(&mut self, descriptor_blob: &[u8]) -> Result<(), UsbDeviceError>;

    /// Poll the EP0 OUT FIFO.  Returns `Ok(Some(setup))` when a
    /// complete 8-byte setup packet has arrived, `Ok(None)` when
    /// no packet is ready, `Err` on bus error.
    fn poll_setup(&mut self) -> Result<Option<[u8; 8]>, UsbDeviceError>;

    /// Read up to `out.len()` bytes from EP0 OUT data stage.
    /// Returns the number actually transferred.
    fn read_out_data(&mut self, out: &mut [u8]) -> Result<usize, UsbDeviceError>;

    /// Write `data` to EP0 IN data stage as the response to the
    /// pending control transfer.
    fn write_in_data(&mut self, data: &[u8]) -> Result<(), UsbDeviceError>;

    /// STALL the current control transfer.  Used when the DFU
    /// state machine rejects a request (returns an `Err`).
    fn stall_ep0(&mut self) -> Result<(), UsbDeviceError>;

    /// Pulse the bus disconnect line so the host treats us as a
    /// fresh device on the next attach.  Used after manifestation
    /// to re-enumerate as the now-flashed runtime image.
    fn bus_reset_device_side(&mut self) -> Result<(), UsbDeviceError>;
}

/// Errors that the controller surface can return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbDeviceError {
    NotImplemented,
    Timeout,
    BusError,
    InvalidState,
}

// ── DWC2 stub for VisionFive 2 / K1 ────────────────────────────
//
// JH7110 has a Synopsys DWC2 USB 2.0 OTG controller. Real impl
// would map MMIO at the platform-specific base and program the
// device-mode register set. Currently every method returns
// NotImplemented so the trait can be wired up at boot time
// without crashing the host build.

/// JH7110 USB OTG base (datasheet).
#[cfg(feature = "vf2")]
pub const DWC2_BASE_VF2: usize = 0x1721_0000;

/// K1 USB OTG base (CanaanCore datasheet pending — placeholder).
#[cfg(feature = "k1")]
pub const DWC2_BASE_K1: usize = 0x9180_0000;

pub struct Dwc2Controller {
    base: usize,
}

impl Dwc2Controller {
    /// Construct against an explicit base address. The board's
    /// `usb_init` call site picks the right one via the
    /// `DWC2_BASE_*` constants above.
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    /// Address we'll talk to (so QEMU stubs / tests can verify
    /// the dispatch reached the controller wrapper).
    pub const fn base(&self) -> usize { self.base }
}

impl UsbDeviceController for Dwc2Controller {
    fn init(&mut self) -> Result<(), UsbDeviceError> {
        // TODO(post-hardware): program PCGCCTL (clock), GUSBCFG
        // (PHY type + force-device), DCFG (device address +
        // periodic frame interval), GINTMSK (enable USBRST,
        // ENUMDONE, RXFLVL, IEPINT, OEPINT), reset core via
        // GRSTCTL.CSRST, wait for GRSTCTL.AHBIDLE.
        Err(UsbDeviceError::NotImplemented)
    }

    fn enumerate(&mut self, _descriptor_blob: &[u8]) -> Result<(), UsbDeviceError> {
        // TODO(post-hardware): wait for USBRST → reset device
        // address (DCFG[DAD] = 0), wait for ENUMDONE → read
        // DSTS.ENUMSPD, configure EP0 max packet via DOEPCTL0
        // / DIEPCTL0, drive descriptor responses from interrupt
        // handler.
        Err(UsbDeviceError::NotImplemented)
    }

    fn poll_setup(&mut self) -> Result<Option<[u8; 8]>, UsbDeviceError> {
        Err(UsbDeviceError::NotImplemented)
    }

    fn read_out_data(&mut self, _out: &mut [u8]) -> Result<usize, UsbDeviceError> {
        Err(UsbDeviceError::NotImplemented)
    }

    fn write_in_data(&mut self, _data: &[u8]) -> Result<(), UsbDeviceError> {
        Err(UsbDeviceError::NotImplemented)
    }

    fn stall_ep0(&mut self) -> Result<(), UsbDeviceError> {
        Err(UsbDeviceError::NotImplemented)
    }

    fn bus_reset_device_side(&mut self) -> Result<(), UsbDeviceError> {
        Err(UsbDeviceError::NotImplemented)
    }
}

// ── Null controller — for QEMU / host tests ─────────────────────

/// Stub controller that always succeeds with empty responses.
/// Lets the recovery hand-off compile and be exercised via the
/// state machine without any real USB hardware.
pub struct NullDeviceController;

impl UsbDeviceController for NullDeviceController {
    fn init(&mut self) -> Result<(), UsbDeviceError> { Ok(()) }
    fn enumerate(&mut self, _: &[u8]) -> Result<(), UsbDeviceError> { Ok(()) }
    fn poll_setup(&mut self) -> Result<Option<[u8; 8]>, UsbDeviceError> { Ok(None) }
    fn read_out_data(&mut self, _: &mut [u8]) -> Result<usize, UsbDeviceError> { Ok(0) }
    fn write_in_data(&mut self, _: &[u8]) -> Result<(), UsbDeviceError> { Ok(()) }
    fn stall_ep0(&mut self) -> Result<(), UsbDeviceError> { Ok(()) }
    fn bus_reset_device_side(&mut self) -> Result<(), UsbDeviceError> { Ok(()) }
}
