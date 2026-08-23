//! Topology → cap_store bridge — RFC-0003/RFC-0005 migration phase P1
//! ("W2" in `docs/plan/MIGRATION_PLAN.md`).
//!
//! `crates/topology` parses `CAPS.TOML` into `CapSpec { kind, perms,
//! target }` triples, but that crate deliberately has **no** dependency on
//! `crates/ipc` (see its `Cargo.toml`) — parsing static configuration and
//! minting live kernel capabilities are different layers. Nothing in the
//! tree ever closed that loop: `default_minimal()`'s cap grants, and any
//! future signed `CAPS.TOML`, were parsed and then never consumed. This
//! module is the missing link, callable from the one place that already
//! depends on both crates: the kernel binary.
//!
//! ## Ordering (load-bearing — read before calling this from anywhere new)
//!
//! [`cap_store::grant`] resolves `tid` through `cap_store::slot_for`, which
//! calls `claim_slot` — and `claim_slot` **wipes** the slot's table if the
//! slot's recorded owner does not already match `tid` (see
//! `crates/ipc/src/cap_store.rs` module doc, "Slot reuse"). Consequences:
//!
//!   - Minting for a `tid` whose task-pool slot has not been claimed yet
//!     (i.e. before `robot_os_sched`'s spawn path has run for it) has no
//!     defined outcome: `idx_for_tid` will not resolve an unclaimed TID and
//!     every mint here returns `None`.
//!   - The FIRST `cap_store` call for a freshly-claimed slot is what
//!     performs the owner-mismatch wipe. As long as this function's mints
//!     are the first (or among the first) `cap_store` operations for `tid`,
//!     that wipe — if it fires at all — clears out only the previous
//!     occupant's leftovers, never anything this function just wrote.
//!
//! **The only call site today (`kernel/src/main.rs`'s autorun block) is
//! safe by construction, not by care**: it calls
//! `robot_os_sched::current_task_tid()` from *inside* the already-running
//! autorun task, i.e. strictly after that task's own pool slot was claimed
//! by the scheduler's spawn path. This is the exact same guarantee the
//! legacy `handle_grant` seeding right next to it already leans on ("the
//! TID does not change across `exec_user`" — see that comment). A future
//! caller that tries to seed a TID *before* spawning it (e.g. a hypothetical
//! "pre-provision caps for a not-yet-created task" path) would violate this
//! and must not use this function that way.
//!
//! ## No delegation quota consumed
//!
//! Every arm below bottoms out in [`cap_store::grant`] (via the `*_grant_cap`
//! minters, or directly for `Channel` — see [`seed_one_cap`]'s doc). None of
//! them call [`cap_store::delegate`], so seeding never touches
//! [`cap_store::MAX_INBOUND_DELEGATIONS`]; that quota exists for ring-3
//! `SYS_CAP_GRANT` and is untouched by boot-time seeding. Verified by
//! reading `gpio_cap.rs`, `i2c_cap.rs`, `pwm_cap.rs`, `motor_cap.rs` and
//! `crates/syscall/src/handlers.rs::kernel_grant_channel_cap` — all five are
//! one-line `cap_store::grant` wrappers.

use crate::cap::{CapHandle, CapKind, CapPerms};

/// Mint one typed capability for `tid` from a topology `CapSpec`'s decoded
/// fields (`kind`, `perms`, `target` — see `robot_os_topology::CapSpec`).
///
/// `target`'s syntax is kind-specific and matches the conventions already
/// documented in `crates/topology/src/types.rs` and RFC-0005:
///
///   - `Gpio`:   `"gpio.<pin>"`               (e.g. `"gpio.5"`)
///   - `Pwm`:    `"pwm.<channel>"`             (e.g. `"pwm.0"`)
///   - `Motor`:  `"motor.<motor_id>"`          (e.g. `"motor.0"`, `"motor.1"`)
///   - `I2c`:    `"bus.<bus>/0x<addr hex>"`    (e.g. `"bus.0/0x68"`)
///   - `Channel`: a bare decimal `u32` channel id (e.g. `"3"`)
///
/// Returns `None` if the kind has no typed minter yet (`Sensor` — no
/// `sensor_grant_cap`, phase P2; `Irq`, `MmioRegion`, `IoRing`, `Port`,
/// `Shm`, `File`, `Socket`, `Task`, `AiSession` — no minter and, for most,
/// no typed syscall either), if `target` does not parse under its kind's
/// convention, or if the underlying minter itself refuses (unknown
/// tid / cap-table full / out-of-range pin or channel).
///
/// **`Channel` gap, documented rather than guessed at.** Topology's actual
/// default channel targets are name-like paths (`"/safety/estop"`,
/// `"/brain/control"` — see `crates/topology/src/builder.rs`), not numeric
/// ids; there is no name→channel-id registry in the tree yet (nothing
/// creates a topology-declared channel by name at boot). A bare-integer
/// target is accepted here because it is the only convention a boot-time
/// mint could use without inventing that registry; path-shaped targets are
/// correctly skipped (`None`), not silently mis-parsed.
///
/// **Why `Channel` mints directly against `cap_store` instead of calling
/// `crates/syscall/src/handlers.rs::kernel_grant_channel_cap`**: that
/// function lives in `robot_os_syscall`, which depends on `robot_os_ipc` —
/// the reverse dependency (`ipc → syscall`) would be a cycle. The call
/// below is the identical one-liner `kernel_grant_channel_cap` wraps
/// (`cap_store::grant::<targets::Channel>`, verified by reading both), kept
/// local so this module's only kernel-facing dependency stays `robot_os_ipc`
/// itself — which is also what keeps it host-testable via the same
/// `#[path]` + shim trick `crates/cap-tests`/`crates/ipc-lease-tests` use
/// (see `crates/topology-tests`).
pub fn seed_one_cap(tid: u32, kind: CapKind, perms: CapPerms, target: &str) -> Option<CapHandle> {
    match kind {
        CapKind::Gpio => parse_dotted(target, "gpio")
            .and_then(|pin| crate::gpio_cap::gpio_grant_cap(tid, pin, perms))
            .map(|c| c.raw()),
        CapKind::Pwm => parse_dotted(target, "pwm")
            .and_then(|ch| crate::pwm_cap::pwm_grant_cap(tid, ch, perms))
            .map(|c| c.raw()),
        CapKind::Motor => parse_dotted(target, "motor")
            .and_then(|id| crate::motor_cap::motor_grant_cap(tid, id, perms))
            .map(|c| c.raw()),
        CapKind::I2c => parse_i2c(target)
            .and_then(|(bus, addr)| crate::i2c_cap::i2c_grant_cap(tid, bus, addr, perms))
            .map(|c| c.raw()),
        CapKind::Channel => parse_plain_u32(target)
            .and_then(|id| {
                crate::cap_store::grant::<crate::cap::targets::Channel>(tid, perms, id)
            })
            .map(|c| c.raw()),
        // No typed minter yet — see the doc comment above for the full list
        // and why each one is a documented gap, not an oversight.
        _ => None,
    }
}

/// Parse `"<prefix>.<u32>"`, e.g. `"motor.0"`, `"gpio.5"`, `"pwm.2"`.
fn parse_dotted(s: &str, prefix: &str) -> Option<u32> {
    let rest = s.strip_prefix(prefix)?.strip_prefix('.')?;
    rest.parse::<u32>().ok()
}

/// Parse `"bus.<u8>/0x<hex u8>"`, e.g. `"bus.0/0x68"` — the I2C target
/// convention documented in `crates/topology/src/types.rs` and RFC-0005.
fn parse_i2c(s: &str) -> Option<(u8, u8)> {
    let rest = s.strip_prefix("bus.")?;
    let (bus_s, addr_s) = rest.split_once('/')?;
    let bus = bus_s.parse::<u8>().ok()?;
    let addr_hex = addr_s.strip_prefix("0x")?;
    u8::from_str_radix(addr_hex, 16).ok().map(|addr| (bus, addr))
}

/// Parse a bare decimal `u32` — see [`seed_one_cap`]'s `Channel` doc for why
/// this, and only this, is accepted for channel targets today.
fn parse_plain_u32(s: &str) -> Option<u32> {
    s.parse::<u32>().ok()
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────
//
// `gpio_cap.rs`/`i2c_cap.rs`/`pwm_cap.rs`/`motor_cap.rs` all reach into
// `robot_os_drivers`, which is RV64-only (via `robot_os_arch`), so these
// tests — like the modules above — only run when pulled in by a host test
// crate that supplies stand-ins for `robot_os_sync`/`robot_os_sched`/
// `robot_os_drivers`. See `crates/topology-tests`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cap::CapError;

    #[test]
    fn seeds_gpio_i2c_pwm_motor_and_channel_by_target_convention() {
        let tid = tests_support::fresh_tid();

        let gpio = seed_one_cap(tid, CapKind::Gpio, CapPerms::RW, "gpio.5").unwrap();
        let i2c = seed_one_cap(tid, CapKind::I2c, CapPerms::RW, "bus.0/0x68").unwrap();
        let pwm = seed_one_cap(tid, CapKind::Pwm, CapPerms::WRITE, "pwm.2").unwrap();
        let motor0 = seed_one_cap(tid, CapKind::Motor, CapPerms::RW, "motor.0").unwrap();
        let chan = seed_one_cap(tid, CapKind::Channel, CapPerms::READ, "7").unwrap();

        use crate::cap::{targets, Cap};
        assert_eq!(
            crate::cap_store::get(tid, Cap::<targets::Gpio>::from_raw(gpio), CapPerms::READ),
            Ok(5)
        );
        assert_eq!(
            crate::cap_store::get(tid, Cap::<targets::I2c>::from_raw(i2c), CapPerms::READ),
            Ok((0u32 << 8) | 0x68)
        );
        assert_eq!(
            crate::cap_store::get(tid, Cap::<targets::Pwm>::from_raw(pwm), CapPerms::WRITE),
            Ok(2)
        );
        assert_eq!(
            crate::cap_store::get(tid, Cap::<targets::Motor>::from_raw(motor0), CapPerms::WRITE),
            Ok(0)
        );
        assert_eq!(
            crate::cap_store::get(tid, Cap::<targets::Channel>::from_raw(chan), CapPerms::READ),
            Ok(7)
        );
    }

    #[test]
    fn unparseable_or_unminted_kinds_are_skipped_not_panicked() {
        let tid = tests_support::fresh_tid();
        // Path-shaped channel target — no name→id registry, must be skipped.
        assert!(seed_one_cap(tid, CapKind::Channel, CapPerms::READ, "/safety/estop").is_none());
        // Malformed motor target.
        assert!(seed_one_cap(tid, CapKind::Motor, CapPerms::RW, "motor").is_none());
        assert!(seed_one_cap(tid, CapKind::Motor, CapPerms::RW, "motor.left").is_none());
        // No typed minter at all yet (P2 gap).
        assert!(seed_one_cap(tid, CapKind::Sensor, CapPerms::READ, "sensor.0").is_none());
        assert!(seed_one_cap(tid, CapKind::Irq, CapPerms::READ, "irq.3").is_none());
    }

    #[test]
    fn without_seeding_the_typed_consumer_path_sees_stale() {
        // Mirrors exactly what `crates/syscall/src/handlers.rs`'s
        // `sys_motor_set_target_typed` does: decode a raw handle the caller
        // never actually received, and dereference it through cap_store.
        use crate::cap::{targets::Motor, Cap};
        let tid = tests_support::fresh_tid();
        let forged: Cap<Motor> = Cap::NULL;
        assert_eq!(
            crate::cap_store::get(tid, forged, CapPerms::WRITE),
            Err(CapError::Stale)
        );
    }

    /// Test-only TID allocation, matching the pattern `crates/cap-tests`
    /// uses: `robot_os_sched::shim_bind` publishes an identity in the host
    /// scheduler shim, so `cap_store::slot_for` can resolve it.
    mod tests_support {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT_ID: AtomicU32 = AtomicU32::new(1);

        pub fn fresh_tid() -> u32 {
            let tid = NEXT_ID.fetch_add(1, Ordering::SeqCst);
            let slot = tid as usize;
            assert!(
                slot < robot_os_sched::task::MAX_TASKS,
                "cap_seed test suite has outgrown the task pool"
            );
            robot_os_sched::shim_bind(tid, slot);
            tid
        }
    }
}
