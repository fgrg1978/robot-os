/// Newtype wrappers for physical and virtual addresses.
///
/// These prevent accidentally mixing physical and virtual addresses.

#[cfg(not(feature = "esp32c3"))]
use robot_os_arch::mmu::{PAGE_SIZE, PAGE_SHIFT};

// ESP32-C3: mmu module is gated out — define page constants locally.
#[cfg(feature = "esp32c3")]
const PAGE_SIZE: usize = 4096;
#[cfg(feature = "esp32c3")]
const PAGE_SHIFT: usize = 12;

/// A physical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PhysAddr(pub usize);

/// A virtual address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtAddr(pub usize);

impl PhysAddr {
    #[inline]
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    #[inline]
    pub const fn as_usize(self) -> usize {
        self.0
    }

    /// Convert to a raw pointer (only valid for identity-mapped regions).
    #[inline]
    pub fn as_ptr<T>(self) -> *const T {
        self.0 as *const T
    }

    /// Convert to a mutable raw pointer (only valid for identity-mapped regions).
    #[inline]
    pub fn as_mut_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }

    #[inline]
    pub const fn page_number(self) -> usize {
        self.0 >> PAGE_SHIFT
    }

    #[inline]
    pub const fn page_offset(self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }

    #[inline]
    pub const fn is_page_aligned(self) -> bool {
        self.0 & (PAGE_SIZE - 1) == 0
    }

    #[inline]
    pub const fn page_align_up(self) -> Self {
        Self((self.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1))
    }

    #[inline]
    pub const fn page_align_down(self) -> Self {
        Self(self.0 & !(PAGE_SIZE - 1))
    }

    #[inline]
    pub const fn offset(self, bytes: usize) -> Self {
        Self(self.0 + bytes)
    }
}

impl VirtAddr {
    #[inline]
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    #[inline]
    pub const fn as_usize(self) -> usize {
        self.0
    }

    #[inline]
    pub const fn is_page_aligned(self) -> bool {
        self.0 & (PAGE_SIZE - 1) == 0
    }

    #[inline]
    pub const fn page_align_up(self) -> Self {
        Self((self.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1))
    }
}

impl core::fmt::LowerHex for PhysAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::LowerHex::fmt(&self.0, f)
    }
}

impl core::fmt::LowerHex for VirtAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::LowerHex::fmt(&self.0, f)
    }
}
