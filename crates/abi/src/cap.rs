//! Capability handle wire format.
//!
//! On the wire (across the syscall boundary, in topology files, in IPC
//! payloads) a capability is a `u32`:
//!
//! ```text
//!   bits 31..28  → kind tag      (CapKind)
//!   bits 27..24  → permissions   (CapPerms bitfield)
//!   bits 23..16  → generation    (monotonic per slot; wraps to 0 after 255)
//!   bits 15..0   → slot index    (per-task cap-table slot)
//! ```
//!
//! The kernel-internal `Cap<T>` typed wrapper (`crates/ipc/src/cap.rs`,
//! RFC-0003) is built on top of this representation, adding compile-time
//! kind safety. Both forms encode identically; only the kernel sees the
//! typed form.
//!
//! ## Why pack into 32 bits?
//!
//! Three reasons:
//!
//! 1. Forward compatibility with userspace `int fd` ABIs. A POSIX fd is
//!    typically 32-bit signed; we use 32-bit unsigned with `0` reserved
//!    for `CAP_NULL` so that any fd > 0 is treatable as a `CapHandle`.
//! 2. Fits in a single syscall arg register on every supported arch.
//! 3. Anti-forgery: the kernel rejects a handle whose generation doesn't
//!    match the cap table; even random guesses fail with overwhelming
//!    probability.

use core::num::NonZeroU32;

/// Wire-format capability handle.
///
/// `0` is reserved for `CAP_NULL`. Any non-zero handle is a candidate; the
/// kernel verifies kind + generation on dereference.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct CapHandle(pub u32);

/// Reserved null handle. Always invalid.
pub const CAP_NULL: CapHandle = CapHandle(0);

impl CapHandle {
    /// Construct from raw u32. Does not validate.
    #[inline]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Get the raw u32 representation.
    #[inline]
    pub const fn as_raw(self) -> u32 {
        self.0
    }

    /// Returns `true` if this is the null handle.
    #[inline]
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }

    /// Convert to `Option<NonZeroU32>` for ergonomic non-null usage.
    #[inline]
    pub const fn as_nonzero(self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.0)
    }

    // ── Bitfield helpers ────────────────────────────────────────────────

    const KIND_SHIFT: u32 = 28;
    const KIND_MASK: u32 = 0xF;
    const PERMS_SHIFT: u32 = 24;
    const PERMS_MASK: u32 = 0xF;
    const GEN_SHIFT: u32 = 16;
    const GEN_MASK: u32 = 0xFF;
    const SLOT_MASK: u32 = 0xFFFF;

    /// Pack kind + perms + generation + slot into a wire-format handle.
    #[inline]
    pub const fn pack(kind: CapKind, perms: CapPerms, generation: u8, slot: u16) -> Self {
        let v = ((kind as u32) & Self::KIND_MASK) << Self::KIND_SHIFT
            | ((perms.bits() as u32) & Self::PERMS_MASK) << Self::PERMS_SHIFT
            | ((generation as u32) & Self::GEN_MASK) << Self::GEN_SHIFT
            | (slot as u32) & Self::SLOT_MASK;
        Self(v)
    }

    /// Extract the encoded `CapKind` tag.
    #[inline]
    pub const fn kind(self) -> u8 {
        ((self.0 >> Self::KIND_SHIFT) & Self::KIND_MASK) as u8
    }

    /// Extract the permission bits.
    #[inline]
    pub const fn perms(self) -> CapPerms {
        CapPerms::from_bits_truncate(((self.0 >> Self::PERMS_SHIFT) & Self::PERMS_MASK) as u8)
    }

    /// Extract the generation counter.
    #[inline]
    pub const fn generation(self) -> u8 {
        ((self.0 >> Self::GEN_SHIFT) & Self::GEN_MASK) as u8
    }

    /// Extract the per-task slot index.
    #[inline]
    pub const fn slot(self) -> u16 {
        (self.0 & Self::SLOT_MASK) as u16
    }
}

impl core::fmt::Debug for CapHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_null() {
            return f.write_str("Cap(null)");
        }
        write!(
            f,
            "Cap(kind={}, perms={:?}, gen={}, slot={})",
            self.kind(),
            self.perms(),
            self.generation(),
            self.slot()
        )
    }
}

/// Capability kinds. Each kind corresponds to a typed `Cap<T>` in the
/// kernel; the wire-format tag is the discriminant.
///
/// `repr(u8)` so that the tag fits in the 4-bit kind field of the
/// wire-format handle (currently 16 distinct kinds).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
#[non_exhaustive]
pub enum CapKind {
    /// Reserved for `CAP_NULL`. Should never appear on a valid handle.
    Null = 0,
    /// IPC channel endpoint.
    Channel = 1,
    /// Shared memory region.
    Shm = 2,
    /// Event port.
    Port = 3,
    /// Hardware IRQ binding.
    Irq = 4,
    /// MMIO region.
    MmioRegion = 5,
    /// IO ring (io_ring submission/completion queues).
    IoRing = 6,
    /// Sensor descriptor.
    Sensor = 7,
    /// GPIO pin.
    Gpio = 8,
    /// I2C bus + address.
    I2c = 9,
    /// PWM channel.
    Pwm = 10,
    /// Motor channel.
    Motor = 11,
    /// File descriptor (FAT32 / tmpfs / procfs).
    File = 12,
    /// Socket descriptor.
    Socket = 13,
    /// Process / task handle.
    Task = 14,
    /// AI inference session.
    AiSession = 15,
}

impl CapKind {
    /// Try to construct from the raw 4-bit tag value.
    #[inline]
    pub const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Null),
            1 => Some(Self::Channel),
            2 => Some(Self::Shm),
            3 => Some(Self::Port),
            4 => Some(Self::Irq),
            5 => Some(Self::MmioRegion),
            6 => Some(Self::IoRing),
            7 => Some(Self::Sensor),
            8 => Some(Self::Gpio),
            9 => Some(Self::I2c),
            10 => Some(Self::Pwm),
            11 => Some(Self::Motor),
            12 => Some(Self::File),
            13 => Some(Self::Socket),
            14 => Some(Self::Task),
            15 => Some(Self::AiSession),
            _ => None,
        }
    }
}

/// Permission bits packed into a capability handle.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct CapPerms(u8);

impl CapPerms {
    /// Read access.
    pub const READ: Self = Self(0b0001);
    /// Write access.
    pub const WRITE: Self = Self(0b0010);
    /// Execute / map-as-X access (MMIO regions).
    pub const EXEC: Self = Self(0b0100);
    /// Permission to duplicate to another task.
    pub const DUP: Self = Self(0b1000);

    /// No permissions (a stub / placeholder).
    pub const NONE: Self = Self(0);
    /// Read + Write.
    pub const RW: Self = Self(Self::READ.0 | Self::WRITE.0);
    /// Read + Write + Dup.
    pub const RW_DUP: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::DUP.0);
    /// All permissions.
    pub const ALL: Self = Self(0b1111);

    /// Construct from raw bits, masking to the valid range.
    #[inline]
    pub const fn from_bits_truncate(raw: u8) -> Self {
        Self(raw & 0b1111)
    }

    /// Extract raw bits.
    #[inline]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns `true` iff `self` contains all bits of `other`.
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Bitwise OR.
    #[inline]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Bitwise AND.
    #[inline]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
}

impl core::fmt::Debug for CapPerms {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut first = true;
        let mut emit = |s: &str| -> core::fmt::Result {
            if !first {
                f.write_str("|")?;
            }
            first = false;
            f.write_str(s)
        };
        if self.contains(Self::READ) {
            emit("R")?;
        }
        if self.contains(Self::WRITE) {
            emit("W")?;
        }
        if self.contains(Self::EXEC) {
            emit("X")?;
        }
        if self.contains(Self::DUP) {
            emit("D")?;
        }
        if first {
            f.write_str("∅")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_is_zero() {
        assert!(CAP_NULL.is_null());
        assert_eq!(CAP_NULL.as_raw(), 0);
    }

    #[test]
    fn pack_unpack_round_trip() {
        let h = CapHandle::pack(CapKind::Channel, CapPerms::RW_DUP, 0x42, 0x1234);
        assert_eq!(h.kind(), CapKind::Channel as u8);
        assert!(h.perms().contains(CapPerms::READ));
        assert!(h.perms().contains(CapPerms::WRITE));
        assert!(h.perms().contains(CapPerms::DUP));
        assert!(!h.perms().contains(CapPerms::EXEC));
        assert_eq!(h.generation(), 0x42);
        assert_eq!(h.slot(), 0x1234);
    }

    #[test]
    fn perms_contains() {
        assert!(CapPerms::ALL.contains(CapPerms::READ));
        assert!(CapPerms::ALL.contains(CapPerms::DUP));
        assert!(!CapPerms::READ.contains(CapPerms::WRITE));
        assert!(CapPerms::RW.contains(CapPerms::READ));
        assert!(CapPerms::RW.contains(CapPerms::WRITE));
        assert!(!CapPerms::RW.contains(CapPerms::DUP));
    }

    #[test]
    fn kind_from_raw() {
        assert_eq!(CapKind::from_raw(1), Some(CapKind::Channel));
        assert_eq!(CapKind::from_raw(15), Some(CapKind::AiSession));
        assert_eq!(CapKind::from_raw(16), None);
    }

    #[test]
    fn debug_format() {
        let h = CapHandle::pack(CapKind::Shm, CapPerms::RW, 1, 7);
        let s = alloc_string_dbg(&h);
        assert!(s.contains("kind=2"));
        assert!(s.contains("R|W"));
        assert!(s.contains("gen=1"));
        assert!(s.contains("slot=7"));
    }

    // Tiny helper to get a String only in tests; uses std for simplicity.
    fn alloc_string_dbg<T: core::fmt::Debug>(t: &T) -> String {
        format!("{:?}", t)
    }
}
