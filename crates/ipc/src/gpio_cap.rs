//! Cap<Gpio> typed wrappers — RFC-0003 W5 batch 5.1.
//!
//! Unlike Channel/Port/Shm/IoRing (which are *created* by user
//! syscalls), GPIO pins are physical resources that already
//! exist on the board. There is no `gpio_create_cap` syscall —
//! the topology loader (RFC-0005) grants `Cap<Gpio>(pin)` to
//! the task that owns the pin during boot. Userspace only sees
//! the per-op syscalls below; the grant is privileged.
//!
//! Lives in `crates/ipc/` (not `crates/drivers/`) because
//! `drivers → ipc` would create a Cargo dependency cycle. `ipc`
//! already imports `drivers`, so reaching `drivers::gpio::*`
//! from here is fine.

use crate::cap::{Cap, CapError, CapPerms, CapTable};
use robot_os_drivers::gpio::{
    gpio_read, gpio_set_direction, gpio_write, GpioDir, GPIO_MAX_PINS,
};

/// Errors returned by the typed `gpio_*_cap` functions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GpioCapError {
    /// Capability dereference failed (stale / wrong kind / missing perms).
    Cap(CapError),
    /// Resource ID stored in the cap is out of [`GPIO_MAX_PINS`] range —
    /// indicates a corrupt cap-table or a wrongly-granted slot.
    BadPin,
    /// Underlying driver rejected the operation (pin invalid / not
    /// configured for the requested direction).
    DriverFault,
    /// `set_dir` got a value other than 0 (input) or 1 (output).
    BadDirValue,
}

impl From<CapError> for GpioCapError {
    fn from(e: CapError) -> Self {
        Self::Cap(e)
    }
}

/// Topology-loader entry: grant `tid` a `Cap<Gpio>` for `pin`.
/// `perms` controls whether the holder may read (`READ`), write
/// (`WRITE`), or set direction (encoded as `WRITE` — direction
/// changes ARE writes from a capability standpoint).
///
/// Returns `None` if `tid` or `pin` is invalid, or the cap-table
/// is full.
pub fn gpio_grant_cap(
    tid: u32,
    pin: u32,
    perms: CapPerms,
) -> Option<Cap<crate::cap::targets::Gpio>> {
    if (pin as usize) >= GPIO_MAX_PINS {
        return None;
    }
    crate::cap_store::grant::<crate::cap::targets::Gpio>(tid, perms, pin)
}

/// Typed `gpio_read`: validate cap (requires `READ`), read pin
/// level (0 or 1).
pub fn gpio_read_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Gpio>,
) -> Result<u32, GpioCapError> {
    let pin = table.get(cap, CapPerms::READ)?;
    if (pin as usize) >= GPIO_MAX_PINS {
        return Err(GpioCapError::BadPin);
    }
    let v = gpio_read(pin);
    if v < 0 {
        Err(GpioCapError::DriverFault)
    } else {
        Ok(v as u32)
    }
}

/// Typed `gpio_write`: validate cap (requires `WRITE`), set pin
/// level (only the low bit of `val` is used).
pub fn gpio_write_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Gpio>,
    val: u32,
) -> Result<(), GpioCapError> {
    let pin = table.get(cap, CapPerms::WRITE)?;
    if (pin as usize) >= GPIO_MAX_PINS {
        return Err(GpioCapError::BadPin);
    }
    if gpio_write(pin, val & 1) == 0 {
        Ok(())
    } else {
        Err(GpioCapError::DriverFault)
    }
}

/// Typed `gpio_set_direction`: validate cap (requires `WRITE`),
/// set pin direction. `dir = 0` ⇒ Input, `dir = 1` ⇒ Output;
/// anything else is rejected with [`GpioCapError::BadDirValue`].
pub fn gpio_set_dir_cap(
    table: &CapTable,
    cap: Cap<crate::cap::targets::Gpio>,
    dir: u32,
) -> Result<(), GpioCapError> {
    let pin = table.get(cap, CapPerms::WRITE)?;
    if (pin as usize) >= GPIO_MAX_PINS {
        return Err(GpioCapError::BadPin);
    }
    let dir = match dir {
        0 => GpioDir::Input,
        1 => GpioDir::Output,
        _ => return Err(GpioCapError::BadDirValue),
    };
    if gpio_set_direction(pin, dir) == 0 {
        Ok(())
    } else {
        Err(GpioCapError::DriverFault)
    }
}
