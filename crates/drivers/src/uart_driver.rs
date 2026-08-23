//! UART as a [`Driver`] impl — first migration to the RFC-0002 API.
//!
//! This is **a thin wrapper** over the existing free-function UART
//! API (`crate::uart::*`). The legacy API stays in place because:
//!
//! - The `kprint!`/`kprintln!` macros and the kernel panic path
//!   need a zero-overhead synchronous putc that exists from very
//!   early boot — before the registry is alive.
//! - Moving every caller to `dyn Driver` dispatch is a separate,
//!   larger refactor that follow-up RFCs will cover.
//!
//! What this module *does* prove: a real hardware driver fits the
//! [`Driver`] trait shape cleanly, and a single static instance can
//! be registered into [`runtime::REGISTRY`] for client tasks that
//! want the unified API.

use crate::api::{
    Driver, DriverError, DriverIsolation, DriverManifest, MmioRange,
};
use crate::uart;
use core::sync::atomic::{AtomicBool, Ordering};
use robot_os_abi::cap::CapPerms;

// ──────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────

/// Mirrors `robot_os_driver_server::DRV_KIND_UART`. Duplicated to
/// avoid a `drivers → driver_server` Cargo dependency cycle; the
/// two must stay numerically equal.
const DRV_KIND_UART: u32 = 0x0004;

/// MMIO window claimed by the NS16550A driver. NS16550A exposes 8
/// 1-byte registers; we map a full 256-byte page for alignment.
const UART_MMIO_BYTES: u64 = 0x100;

/// UART driver ops. Stable wire numbers — bump
/// [`super::api::DRIVER_MANIFEST_VERSION`] on any breaking change.
pub const UART_OP_WRITE: u32 = 0;
/// Non-blocking read of one byte into `output[0]`. Returns 1 on
/// success, 0 (with `Ok`) if no byte is available.
pub const UART_OP_READ_NB: u32 = 1;
/// Switch the UART to IRQ-driven RX mode.
pub const UART_OP_ENABLE_IRQ: u32 = 2;

// ──────────────────────────────────────────────────────────────────────────
// Driver state
// ──────────────────────────────────────────────────────────────────────────

/// Stateful wrapper over the free-function NS16550A driver. The
/// actual hardware state lives in the static globals in
/// [`crate::uart`]; this struct only carries the manifest and a
/// single atomic init flag. All `Driver` methods take `&self` so a
/// `static UART_DRIVER: UartDriver` can be safely shared through
/// the registry from any CPU.
pub struct UartDriver {
    initialized: AtomicBool,
    manifest: DriverManifest,
}

impl UartDriver {
    /// Construct an uninitialised UART driver. `const` so a static
    /// instance can be created without a runtime hook.
    pub const fn new() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            manifest: DriverManifest::new(
                DRV_KIND_UART,
                "ns16550a-uart",
                DriverIsolation::InKernel,
                CapPerms::RW,
            )
            .with_mmio(MmioRange::new(
                crate::platform::hw::UART_BASE as u64,
                UART_MMIO_BYTES,
            ))
            .with_irq(uart::UART_IRQ),
        }
    }
}

impl Default for UartDriver {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Driver impl
// ──────────────────────────────────────────────────────────────────────────

impl Driver for UartDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    fn init(&self) -> Result<(), DriverError> {
        // Idempotent: a second init() must not touch hardware. The
        // boot path already calls `uart::init()` long before the
        // registry is alive; this guard keeps the trait-level init
        // a no-op in that common case. `compare_exchange` ensures
        // only one caller across CPUs runs `uart::init()`.
        if self
            .initialized
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            uart::init();
        }
        Ok(())
    }

    fn handle_request(
        &self,
        op: u32,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, DriverError> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err(DriverError::NotInitialized);
        }
        match op {
            UART_OP_WRITE => {
                for &b in input {
                    uart::putc(b);
                }
                Ok(0)
            }
            UART_OP_READ_NB => {
                if output.is_empty() {
                    return Err(DriverError::BadOutput);
                }
                match uart::try_getc() {
                    Some(c) => {
                        output[0] = c;
                        Ok(1)
                    }
                    None => Ok(0),
                }
            }
            UART_OP_ENABLE_IRQ => {
                uart::enable_irq();
                Ok(0)
            }
            _ => Err(DriverError::BadOp),
        }
    }

    fn handle_irq(&self, _irq: u32) {
        uart::irq_handler();
    }

    fn shutdown(&self) -> Result<(), DriverError> {
        // NS16550A has no power-down sequence in our usage. Clearing
        // the init flag is enough so a subsequent `init()` is a
        // no-op-aware reinit.
        self.initialized.store(false, Ordering::Release);
        Ok(())
    }
}
