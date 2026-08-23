//! [`UserDriverProxy`] — kernel-side proxy that implements
//! [`Driver`] by forwarding requests through the
//! `robot_os_driver_server` registry to a userspace driver process
//! (E11.AQ3).
//!
//! # Why
//!
//! The whole point of [`crate::api::DriverIsolation`] is that
//! consumers should not care whether a driver runs in the kernel or
//! in a user process. The kernel-side [`Driver`] trait is uniform;
//! what differs is **who runs the code**.
//!
//! - In-kernel: e.g. [`crate::uart_driver::UartDriver`] — methods
//!   compile to direct Rust calls.
//! - User-process: [`UserDriverProxy`] — methods serialize the call
//!   into a [`driver_server::DriverRequest`], enqueue it, wait for
//!   the matching [`driver_server::DriverReply`], copy the payload
//!   back. The actual hardware logic lives in a user task that
//!   previously called `sys_driver_register`.
//!
//! # Phase 1 simplifications
//!
//! - Synchronous poll-wait for the reply, capped by
//!   [`PROXY_MAX_POLL_ITERS`]. Future revision will block on the
//!   scheduler's wait queue (when the wait-on-driver-token primitive
//!   exists).
//! - `client_tid` is hard-coded to [`PROXY_CALLER_TID_KERNEL`]
//!   because the [`Driver`] trait does not yet carry the caller
//!   identity. The dispatch layer that calls into the proxy can
//!   capture the real tid via `robot_os_sched::current_task_tid()`
//!   and pass it via a setter or via a wrapper at the syscall
//!   bridge — chosen in a follow-up RFC so the trait stays narrow.

use crate::api::{Driver, DriverError, DriverIsolation, DriverManifest};
use robot_os_driver_server as ds;

// ──────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────

/// Hard cap on the busy-wait loop for the reply token. The userspace
/// driver task is expected to reply within this many iterations; if
/// not, `handle_request` returns [`DriverError::Busy`] so the caller
/// can retry rather than wedging a CPU.
///
/// Sized for a worst-case userspace turnaround of ~100ms at a
/// typical poll rate; production will replace with a proper wait
/// queue (see module-level "Phase 1 simplifications" note).
pub const PROXY_MAX_POLL_ITERS: u32 = 1_000_000;

/// Sentinel client_tid used while the trait does not carry caller
/// identity. The userspace driver sees this as "the kernel asked".
pub const PROXY_CALLER_TID_KERNEL: u32 = u32::MAX;

// ──────────────────────────────────────────────────────────────────────────
// Errors specific to the proxy plumbing (mapped to DriverError)
// ──────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProxyError {
    /// Reply did not arrive within [`PROXY_MAX_POLL_ITERS`].
    Timeout,
    /// `driver_submit_request` returned `0` — kind not registered
    /// or per-kind queue full.
    SubmitFailed,
    /// Reply `out_len` exceeded the caller-supplied output buffer.
    OutputTooLarge,
}

impl From<ProxyError> for DriverError {
    fn from(e: ProxyError) -> Self {
        match e {
            ProxyError::Timeout => DriverError::Busy,
            ProxyError::SubmitFailed => DriverError::Busy,
            ProxyError::OutputTooLarge => DriverError::BadOutput,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Proxy
// ──────────────────────────────────────────────────────────────────────────

/// Kernel-side proxy for a userspace driver. Constructed once per
/// (kind, manifest) and registered into [`crate::runtime::REGISTRY`].
pub struct UserDriverProxy {
    manifest: DriverManifest,
}

impl UserDriverProxy {
    /// Construct a proxy for the userspace driver described by
    /// `manifest`. Panics in debug builds if the manifest's
    /// isolation is not [`DriverIsolation::UserProcess`] — using
    /// a proxy for an in-kernel driver is a programming error.
    pub const fn new(manifest: DriverManifest) -> Self {
        debug_assert!(matches!(
            manifest.isolation,
            DriverIsolation::UserProcess { .. }
        ));
        Self { manifest }
    }

    /// Returns the user task id that handles this proxy, or `None`
    /// if the manifest's isolation has been changed at runtime.
    pub fn target_tid(&self) -> Option<u32> {
        match self.manifest.isolation {
            DriverIsolation::UserProcess { tid } => Some(tid),
            _ => None,
        }
    }

    /// Internal: serialize one synchronous request → reply cycle.
    fn invoke(
        &self,
        op: u32,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, ProxyError> {
        let kind = self.manifest.kind;
        let token = ds::driver_submit_request(
            kind,
            PROXY_CALLER_TID_KERNEL,
            op,
            input,
            output.len().min(ds::DRIVER_REPLY_PAYLOAD_BYTES) as u16,
        );
        if token == 0 {
            return Err(ProxyError::SubmitFailed);
        }

        let mut reply = ds::DriverReply::zeroed();
        let mut iters: u32 = 0;
        while iters < PROXY_MAX_POLL_ITERS {
            if ds::driver_try_take_reply(kind, token, &mut reply) {
                let len = reply.out_len as usize;
                if len > output.len() {
                    return Err(ProxyError::OutputTooLarge);
                }
                output[..len].copy_from_slice(&reply.output[..len]);
                return Ok(len);
            }
            core::hint::spin_loop();
            iters = iters.wrapping_add(1);
        }
        Err(ProxyError::Timeout)
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Driver impl
// ──────────────────────────────────────────────────────────────────────────

impl Driver for UserDriverProxy {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    fn init(&self) -> Result<(), DriverError> {
        // No-op: a userspace driver self-initialises during its
        // own `sys_driver_register` call. The proxy has no
        // hardware state of its own.
        Ok(())
    }

    fn handle_request(
        &self,
        op: u32,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, DriverError> {
        self.invoke(op, input, output).map_err(Into::into)
    }

    fn handle_irq(&self, irq: u32) {
        // For a userspace-isolated driver, the kernel IRQ path
        // wakes the user task — it does not handle the IRQ inline.
        // `driver_signal_irq` latches the IRQ flag; the user task
        // observes it on its next `sys_driver_wait`.
        let _ = ds::driver_signal_irq(irq);
    }

    fn shutdown(&self) -> Result<(), DriverError> {
        // Cooperative: the userspace driver tears down when its
        // process exits or when it calls `sys_driver_unregister`.
        // The proxy itself owns no resources.
        Ok(())
    }
}
