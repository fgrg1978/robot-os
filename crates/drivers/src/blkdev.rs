//! Block device abstraction — Phase A/B (VF2 + K1 boot).
//!
//! Routes sector I/O to the appropriate backend:
//! - QEMU (`default`):        VirtIO block device.
//! - VisionFive 2 (`vf2`):   SDHCI MMC slot 1 — microSD (0x1603_0000).
//! - SpacemiT K1  (`k1`):    SDHCI MMC slot 0 — SD socket (0xD428_0000).
//!
//! The FAT32 layer and kernel boot use this module exclusively so that
//! no storage-related code needs to be gated on platform features outside
//! this file.

// ── QEMU / VirtIO backend ─────────────────────────────────────────────────────

#[cfg(not(any(feature = "vf2", feature = "k1")))]
pub fn init() -> Result<(), ()> {
    crate::virtio::blk::init()
}

#[cfg(not(any(feature = "vf2", feature = "k1")))]
pub fn capacity_sectors() -> u64 {
    crate::virtio::blk::capacity_sectors()
}

#[cfg(not(any(feature = "vf2", feature = "k1")))]
pub fn read(sector: u64, count: u32, buf: &mut [u8]) -> Result<(), ()> {
    crate::virtio::blk::read(sector, count, buf)
}

#[cfg(not(any(feature = "vf2", feature = "k1")))]
pub fn write(sector: u64, count: u32, buf: &[u8]) -> Result<(), ()> {
    crate::virtio::blk::write(sector, count, buf)
}

// ── Real-hardware SDHCI backend (VisionFive 2 + SpacemiT K1) ─────────────────
//
// Both boards use an SDHCI-v3 controller; only the boot slot differs:
//   VF2: microSD = SDIO1 (MmcSlot::Sd  = MMC1_BASE 0x1603_0000)
//   K1:  SD card = SDHCI0 (MmcSlot::Emmc = MMC0_BASE 0xD428_0000)
//        (K1 naming: "Emmc" slot 0 physically wires to the removable SD socket)

/// Boot storage slot: microSD on VF2, SD card socket on K1.
#[cfg(feature = "vf2")]
const BOOT_SLOT: crate::mmc::MmcSlot = crate::mmc::MmcSlot::Sd;
#[cfg(feature = "k1")]
const BOOT_SLOT: crate::mmc::MmcSlot = crate::mmc::MmcSlot::Emmc;

#[cfg(any(feature = "vf2", feature = "k1"))]
pub fn init() -> Result<(), ()> {
    if crate::mmc::mmc_init(BOOT_SLOT) { Ok(()) } else { Err(()) }
}

#[cfg(any(feature = "vf2", feature = "k1"))]
pub fn capacity_sectors() -> u64 {
    crate::mmc::mmc_capacity(BOOT_SLOT)
}

#[cfg(any(feature = "vf2", feature = "k1"))]
pub fn read(sector: u64, count: u32, buf: &mut [u8]) -> Result<(), ()> {
    crate::mmc::mmc_read(BOOT_SLOT, sector, count, buf)
}

#[cfg(any(feature = "vf2", feature = "k1"))]
pub fn write(sector: u64, count: u32, buf: &[u8]) -> Result<(), ()> {
    crate::mmc::mmc_write(BOOT_SLOT, sector, count, buf)
}
