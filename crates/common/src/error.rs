/// Unified kernel error type.
/// Converted to negative errno at the syscall boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelError {
    /// Out of memory (physical pages or heap)
    OutOfMemory,
    /// Invalid argument
    InvalidArg,
    /// Address not page-aligned
    NotAligned,
    /// Page already mapped
    AlreadyMapped,
    /// Page not mapped
    NotMapped,
    /// Resource not found
    NotFound,
    /// Double free detected
    DoubleFree,
    /// Capacity exceeded (e.g., PT metadata array full)
    CapacityFull,
    /// Permission denied
    PermissionDenied,
    /// Generic I/O error
    IoError,
}

/// Kernel-wide Result type alias.
pub type KResult<T> = Result<T, KernelError>;
