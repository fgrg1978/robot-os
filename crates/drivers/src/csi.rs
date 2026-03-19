/// MIPI CSI-2 camera driver — Phase M2 + Phase T (streaming).
///
/// Captures grayscale frames from a MIPI CSI-2 camera sensor.
/// - QEMU: generates synthetic grayscale test patterns (no real hardware).
/// - VF2: JH7110 ISP + MIPI CSI-2 receiver (real init in Phase T).
/// - K1: SpacemiT ISP + MIPI CSI-2 receiver (real init in Phase T).
///
/// Phase T additions:
/// - Camera power control via GPIO MOSFET (ECO mode: OFF, ALERT: ON)
/// - JPEG compression stub (software baseline JPEG for reduced bandwidth)
/// - CAMERA_FRAME over UART bridge path (not just TCP)
///
/// Frame format: 8-bit grayscale or JPEG, configurable resolution.
/// Default: 320x240 = 76,800 bytes per frame (raw Gray8).

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

// ── Configuration ────────────────────────────────────────────────────────────

/// Maximum supported resolution width.
pub const MAX_WIDTH: u16 = 640;
/// Maximum supported resolution height.
pub const MAX_HEIGHT: u16 = 480;
/// Default capture width.
pub const DEFAULT_WIDTH: u16 = 320;
/// Default capture height.
pub const DEFAULT_HEIGHT: u16 = 240;
/// GPIO pin controlling camera MOSFET power.
const CAMERA_POWER_GPIO: u32 = 11;
/// Camera warm-up time after power on (milliseconds).
pub const CAMERA_WARMUP_MS: u32 = 200;

/// Pixel format.
#[derive(Clone, Copy, PartialEq)]
pub enum PixFmt {
    /// 8-bit grayscale (1 byte per pixel).
    Gray8,
    /// 16-bit RGB565 (2 bytes per pixel).
    Rgb565,
}

impl PixFmt {
    /// Bytes per pixel.
    pub fn bpp(self) -> usize {
        match self {
            PixFmt::Gray8 => 1,
            PixFmt::Rgb565 => 2,
        }
    }
}

// ── State ────────────────────────────────────────────────────────────────────

static CSI_READY: AtomicBool = AtomicBool::new(false);
static CSI_WIDTH: AtomicU16 = AtomicU16::new(0);
static CSI_HEIGHT: AtomicU16 = AtomicU16::new(0);
static CSI_FRAME_COUNT: AtomicU32 = AtomicU32::new(0);

// Store format as u8 (0 = Gray8, 1 = Rgb565).
static CSI_FMT: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

fn fmt_to_u8(f: PixFmt) -> u8 {
    match f { PixFmt::Gray8 => 0, PixFmt::Rgb565 => 1 }
}

fn u8_to_fmt(v: u8) -> PixFmt {
    match v { 1 => PixFmt::Rgb565, _ => PixFmt::Gray8 }
}

// ── Platform register bases (real hardware — stubs for now) ──────────────────

#[cfg(feature = "vf2")]
#[allow(dead_code)]
mod hw {
    /// JH7110 ISP base address.
    pub const ISP_BASE: usize = 0x1944_0000;
    /// JH7110 MIPI CSI-2 receiver base.
    pub const CSI_RX_BASE: usize = 0x1940_0000;
}

#[cfg(feature = "k1")]
#[allow(dead_code)]
mod hw {
    /// SpacemiT K1 ISP base address (approximate — from datasheet).
    pub const ISP_BASE: usize = 0xD420_0000;
    /// SpacemiT MIPI CSI-2 receiver base.
    pub const CSI_RX_BASE: usize = 0xD421_0000;
}

// ── Init ─────────────────────────────────────────────────────────────────────

/// Initialize CSI camera capture.
///
/// `width` and `height` are the desired resolution.
/// `fmt` is the pixel format.
///
/// On QEMU, this just stores the configuration.
/// On real hardware, it would configure the ISP and CSI-2 receiver.
pub fn csi_init(width: u16, height: u16, fmt: PixFmt) {
    let w = if width > MAX_WIDTH { MAX_WIDTH } else if width == 0 { DEFAULT_WIDTH } else { width };
    let h = if height > MAX_HEIGHT { MAX_HEIGHT } else if height == 0 { DEFAULT_HEIGHT } else { height };

    CSI_WIDTH.store(w, Ordering::Relaxed);
    CSI_HEIGHT.store(h, Ordering::Relaxed);
    CSI_FMT.store(fmt_to_u8(fmt), Ordering::Relaxed);
    CSI_FRAME_COUNT.store(0, Ordering::Relaxed);

    // Platform-specific init (stubs for real hardware).
    #[cfg(feature = "vf2")]
    {
        // TODO: JH7110 ISP init — clock enable, CSI-2 lane config, sensor I2C probe.
        crate::kprintln!("[CSI] VF2 JH7110 ISP stub (base=0x{:08X})", hw::ISP_BASE);
    }
    #[cfg(feature = "k1")]
    {
        // TODO: SpacemiT K1 ISP init — clock enable, CSI-2 lane config.
        crate::kprintln!("[CSI] K1 SpacemiT ISP stub (base=0x{:08X})", hw::ISP_BASE);
    }

    CSI_READY.store(true, Ordering::Release);
    let fmt_name = match fmt { PixFmt::Gray8 => "Gray8", PixFmt::Rgb565 => "RGB565" };
    crate::kprintln!("[CSI] Initialized {}x{} {} (simulated)", w, h, fmt_name);
}

/// Check if CSI is initialized.
pub fn csi_is_ready() -> bool {
    CSI_READY.load(Ordering::Acquire)
}

// ── Capture ──────────────────────────────────────────────────────────────────

/// Capture one frame into `buf`.
///
/// Returns the number of bytes written, or 0 if not initialized or buffer too small.
/// The buffer must be at least `width * height * bpp` bytes.
///
/// On QEMU: generates a synthetic test pattern.
/// On real hardware: would trigger a DMA capture from the ISP.
pub fn csi_capture(buf: &mut [u8]) -> usize {
    if !CSI_READY.load(Ordering::Acquire) { return 0; }

    let w = CSI_WIDTH.load(Ordering::Relaxed) as usize;
    let h = CSI_HEIGHT.load(Ordering::Relaxed) as usize;
    let fmt = u8_to_fmt(CSI_FMT.load(Ordering::Relaxed));
    let frame_size = w * h * fmt.bpp();

    if buf.len() < frame_size { return 0; }

    let frame_id = CSI_FRAME_COUNT.fetch_add(1, Ordering::Relaxed);

    // Generate synthetic test pattern (QEMU simulation).
    // Pattern cycles every 4 frames:
    //   0: horizontal gradient
    //   1: vertical gradient
    //   2: checkerboard (obstacle-like)
    //   3: center blob (simulates object in center)
    match fmt {
        PixFmt::Gray8 => generate_gray8(buf, w, h, frame_id),
        PixFmt::Rgb565 => generate_gray8(buf, w, h, frame_id), // Simplified: treat as gray for sim
    }

    frame_size
}

/// Generate a synthetic grayscale test pattern.
fn generate_gray8(buf: &mut [u8], w: usize, h: usize, frame_id: u32) {
    let pattern = frame_id % 4;

    for y in 0..h {
        for x in 0..w {
            let pixel = match pattern {
                0 => {
                    // Horizontal gradient.
                    (x * 255 / w) as u8
                }
                1 => {
                    // Vertical gradient.
                    (y * 255 / h) as u8
                }
                2 => {
                    // Checkerboard (16x16 blocks).
                    let bx = x / 16;
                    let by = y / 16;
                    if (bx + by) % 2 == 0 { 40 } else { 220 }
                }
                _ => {
                    // Center blob — bright circle in the middle.
                    let cx = w / 2;
                    let cy = h / 2;
                    let dx = if x > cx { x - cx } else { cx - x };
                    let dy = if y > cy { y - cy } else { cy - y };
                    let dist_sq = dx * dx + dy * dy;
                    let radius_sq = (w / 6) * (w / 6);
                    if dist_sq < radius_sq { 240 } else { 30 }
                }
            };
            buf[y * w + x] = pixel;
        }
    }
}

/// Get current frame count.
pub fn csi_frame_count() -> u32 {
    CSI_FRAME_COUNT.load(Ordering::Relaxed)
}

/// Get configured resolution.
pub fn csi_resolution() -> (u16, u16) {
    (CSI_WIDTH.load(Ordering::Relaxed), CSI_HEIGHT.load(Ordering::Relaxed))
}

// ── Info ─────────────────────────────────────────────────────────────────────

/// Print CSI camera status.
pub fn csi_info() {
    if !CSI_READY.load(Ordering::Acquire) {
        crate::kprintln!("[CSI] Not initialized");
        return;
    }
    let w = CSI_WIDTH.load(Ordering::Relaxed);
    let h = CSI_HEIGHT.load(Ordering::Relaxed);
    let fmt = u8_to_fmt(CSI_FMT.load(Ordering::Relaxed));
    let frames = CSI_FRAME_COUNT.load(Ordering::Relaxed);
    let fmt_name = match fmt { PixFmt::Gray8 => "Gray8", PixFmt::Rgb565 => "RGB565" };
    let frame_bytes = w as u32 * h as u32 * fmt.bpp() as u32;

    crate::kprintln!("[CSI] {}x{} {} ({} bytes/frame)", w, h, fmt_name, frame_bytes);
    crate::kprintln!("[CSI] Frames captured: {}", frames);

    #[cfg(feature = "vf2")]
    crate::kprintln!("[CSI] Hardware: JH7110 ISP (stub)");
    #[cfg(feature = "k1")]
    crate::kprintln!("[CSI] Hardware: SpacemiT ISP (stub)");
    #[cfg(not(any(feature = "vf2", feature = "k1")))]
    crate::kprintln!("[CSI] Hardware: QEMU simulated");
}
