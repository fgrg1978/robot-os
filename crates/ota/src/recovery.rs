//! DEV02 — recovery-mode entry decision.
//!
//! Decides whether the kernel should boot into USB DFU recovery
//! mode (see `crates/dfu/`) instead of normal operation. Three
//! independent triggers, any of which arms recovery:
//!
//!   1. **Crash-counter boot-loop** — the watchdog crash counter
//!      (set on panic, reset on `OTA_BOOT_GOOD_DELAY_S` of clean
//!      uptime) has exceeded its threshold. The kernel is stuck
//!      in a panic-reboot cycle; rolling back to the previous
//!      slot didn't help. Last resort: hand control to USB so the
//!      operator can re-flash from a workstation.
//!   2. **Recovery button held** — a board-specific GPIO pin
//!      reads asserted at boot. Lets a human force recovery even
//!      when the kernel boots successfully. Pin number is
//!      board-specific so we take it as a parameter.
//!   3. **Magic flag in BOOTMETA** — set by user-space (`ota
//!      recovery`) before a controlled reboot. Useful when the
//!      operator wants to update via DFU without waiting for the
//!      next maintenance window.
//!
//! All three inputs are pure functions; this module wires them
//! into a single [`recovery_mode_should_enter`] decision. The
//! actual hand-off to DFU (descriptor enumeration, USB controller
//! init) lives in the device-mode controller driver.
//!
//! `#![allow(dead_code)]` is intentional pre-wiring: kernel boot
//! code calls `recovery_mode_should_enter` (will, once the boot
//! path adds the trigger check post-hardware-arrival) but for
//! the riscv64 + QEMU build path today nothing references it.
//! The host tests in `crates/ota-tests` exercise the whole
//! surface; without the allow the kernel build warns on every
//! const + enum variant.

#![allow(dead_code)]

/// Crash count over which the boot-loop trigger arms. Mirrors
/// `crates/drivers::wdt::CRASH_BOOT_LOOP_THRESHOLD` so we don't
/// take a circular crate dep on `robot_os_drivers`.
pub const RECOVERY_BOOT_LOOP_THRESHOLD: u32 = 3;

/// Magic value written to `BootMeta.user_flag` to force recovery
/// on the next reboot. Chosen as a hex word that's recognisable
/// in a hex dump (`DEV0 DFU`).
pub const RECOVERY_FORCE_FLAG: u32 = 0xDEF0_DF02;

/// Inputs the kernel reads before calling [`recovery_mode_should_enter`].
/// Bundled as a struct so future inputs (e.g. UART-break detection)
/// can be added without changing the function signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RecoveryInputs {
    /// Crash counter from the watchdog driver.
    pub crash_count:           u32,
    /// True iff the board's recovery button GPIO is pulled
    /// asserted at boot. `None` when the board has no recovery
    /// button (e.g. QEMU) — that disables this trigger only.
    pub recovery_button_held:  Option<bool>,
    /// Value of `BootMeta.user_flag` from FAT32. `None` means
    /// BOOTMETA couldn't be read (e.g. corrupt FS); the trigger
    /// stays disarmed in that case so we don't accidentally
    /// loop into DFU forever on flash damage.
    pub bootmeta_user_flag:    Option<u32>,
}

/// Why we decided to enter (or skip) recovery mode. Returned
/// alongside the bool so boot-time logs can explain the choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryReason {
    /// No trigger armed; boot normally.
    NotArmed,
    /// Crash counter ≥ [`RECOVERY_BOOT_LOOP_THRESHOLD`].
    BootLoop { crashes: u32 },
    /// Recovery button held at boot.
    ButtonHeld,
    /// User-space requested recovery via [`RECOVERY_FORCE_FLAG`].
    UserRequested,
}

impl RecoveryReason {
    pub const fn should_enter(self) -> bool {
        !matches!(self, RecoveryReason::NotArmed)
    }
}

/// Decide. Pure function — no I/O, no kernel state mutation.
/// Order of evaluation matches operator expectations: an explicit
/// "user requested" beats "boot loop" beats "button held"
/// (so an operator who chose recovery via the user flag sees that
/// reason in the log even if the kernel was already in a boot
/// loop).
pub const fn recovery_mode_should_enter(inputs: RecoveryInputs) -> RecoveryReason {
    // 1. User-requested via BOOTMETA flag.
    if let Some(flag) = inputs.bootmeta_user_flag {
        if flag == RECOVERY_FORCE_FLAG {
            return RecoveryReason::UserRequested;
        }
    }
    // 2. Boot loop.
    if inputs.crash_count >= RECOVERY_BOOT_LOOP_THRESHOLD {
        return RecoveryReason::BootLoop { crashes: inputs.crash_count };
    }
    // 3. Recovery button.
    if let Some(true) = inputs.recovery_button_held {
        return RecoveryReason::ButtonHeld;
    }
    RecoveryReason::NotArmed
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> RecoveryInputs {
        RecoveryInputs::default()
    }

    #[test]
    fn no_inputs_does_not_enter() {
        assert_eq!(
            recovery_mode_should_enter(defaults()),
            RecoveryReason::NotArmed,
        );
    }

    #[test]
    fn crash_loop_triggers() {
        let inp = RecoveryInputs {
            crash_count: RECOVERY_BOOT_LOOP_THRESHOLD,
            ..defaults()
        };
        assert!(matches!(
            recovery_mode_should_enter(inp),
            RecoveryReason::BootLoop { .. },
        ));
    }

    #[test]
    fn button_triggers_only_when_held() {
        let mut inp = defaults();
        inp.recovery_button_held = Some(false);
        assert_eq!(
            recovery_mode_should_enter(inp),
            RecoveryReason::NotArmed,
        );
        inp.recovery_button_held = Some(true);
        assert_eq!(
            recovery_mode_should_enter(inp),
            RecoveryReason::ButtonHeld,
        );
    }

    #[test]
    fn no_button_means_trigger_disabled() {
        let inp = RecoveryInputs {
            recovery_button_held: None,
            ..defaults()
        };
        assert_eq!(
            recovery_mode_should_enter(inp),
            RecoveryReason::NotArmed,
        );
    }

    #[test]
    fn user_flag_beats_boot_loop() {
        // Even with a raging boot loop, an explicit user request
        // is the reason reported (clearer in the log).
        let inp = RecoveryInputs {
            crash_count: 99,
            bootmeta_user_flag: Some(RECOVERY_FORCE_FLAG),
            ..defaults()
        };
        assert_eq!(
            recovery_mode_should_enter(inp),
            RecoveryReason::UserRequested,
        );
    }

    #[test]
    fn wrong_user_flag_value_ignored() {
        let inp = RecoveryInputs {
            bootmeta_user_flag: Some(0xDEAD_BEEF),
            ..defaults()
        };
        assert_eq!(
            recovery_mode_should_enter(inp),
            RecoveryReason::NotArmed,
        );
    }

    #[test]
    fn missing_bootmeta_does_not_lock_us_out_of_recovery() {
        // BOOTMETA unreadable — user flag is None. Boot loop
        // trigger must still work as fallback.
        let inp = RecoveryInputs {
            crash_count: RECOVERY_BOOT_LOOP_THRESHOLD + 1,
            bootmeta_user_flag: None,
            ..defaults()
        };
        assert!(recovery_mode_should_enter(inp).should_enter());
    }
}
