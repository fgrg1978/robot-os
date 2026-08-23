//! Production registry static — wraps the host-testable
//! [`robot_os_drivers_api::Registry`] in a `SpinLock` so it can be
//! shared across CPUs.
//!
//! ## Phase 1 (today)
//! - In-kernel drivers in `crates/drivers/*` are still wired
//!   statically (no entry here yet).
//! - Userspace drivers register through `robot_os_driver_server`
//!   (E11.AQ3) and the kernel-side `UserDriverProxy` adapts them.
//! - This registry is **empty in production** and only exercised by
//!   tests + the eventual loader prototype.
//!
//! ## Phase 4 (target)
//! A disk/network loader instantiates drivers from manifests and
//! calls `REGISTRY.lock().register(...)`. Consumers look up
//! `dyn Driver` by `kind` regardless of isolation, so the
//! in-kernel ↔ userspace split becomes invisible.

use robot_os_drivers_api::Registry;
use robot_os_sync::SpinLock;

pub use robot_os_drivers_api::{RegistryError, REGISTRY_MAX_DRIVERS};

/// Global driver registry. Locked because `register` mutates and
/// `find_by_kind` is invoked from arbitrary CPUs.
pub static REGISTRY: SpinLock<Registry> = SpinLock::new(Registry::empty());

// Host-side tests for the underlying `Registry` struct live in
// `crates/drivers-api-tests`.
