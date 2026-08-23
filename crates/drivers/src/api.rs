//! Re-export of the host-testable [`robot_os_drivers_api`] surface.
//!
//! A2.5: the canonical surface (trait + manifest + enum + errors +
//! `MmioRange` + `Registry`) lives in `crates/drivers-api` so it can
//! be unit-tested on the host without the RISC-V transitive deps
//! of this kernel-side crate. This module re-exports it for the
//! existing `crate::api::*` consumers (uart_driver, user_driver_proxy).

pub use robot_os_drivers_api::{
    Driver, DriverError, DriverIsolation, DriverManifest, MmioRange,
    Registry, RegistryError, DRIVER_MANIFEST_VERSION, REGISTRY_MAX_DRIVERS,
};

// Host-side tests live in `crates/drivers-api-tests`.
