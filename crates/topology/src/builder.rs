//! Programmatic topology builders.
//!
//! Useful when:
//!
//! - The kernel ships a built-in default topology (QEMU bring-up, dev
//!   builds where no signed CAPS.TOML is present yet).
//! - Tests want to construct a topology without parsing TOML.
//!
//! The shapes here are **not** part of the cert-relevant boot path —
//! the production kernel always loads a signed TOML pair from FAT32.
//! These helpers exist for development only.

use robot_os_abi::cap::{CapKind, CapPerms};

use crate::types::{
    CapSpec, ClassSpec, MaybeStr, PolicyKind, Preemption, SchedConfig, Topology,
};

// ──────────────────────────────────────────────────────────────────────────
// Static literals — used as borrows in the resulting topology
// ──────────────────────────────────────────────────────────────────────────

const NAME_SAFETY: &[u8] = b"safety_critical";
const NAME_HARD_RT: &[u8] = b"hard_rt";
const NAME_SOFT_RT: &[u8] = b"soft_rt";
const NAME_BEST_EFFORT: &[u8] = b"best_effort";
const NAME_IDLE: &[u8] = b"idle";

const TASK_SUPERVISOR: &[u8] = b"supervisor";
const TASK_BRAIN_LINK: &[u8] = b"brain_link";
// RFC-0003 migration phase P1: the autorun ELF (`kernel/src/main.rs`'s
// `autorun_task`) is not spawned from a topology `TaskSpec` today — no
// name→spawn wiring exists yet, that's a separate piece of work — but its
// cap grants ARE looked up by this name (`find_task(b"autorun")`) so the
// P1 topology→cap_store bridge has something declarative to seed instead
// of the kernel hard-coding motor ids a second time.
const TASK_AUTORUN: &[u8] = b"autorun";

const RESOURCE_ESTOP: &[u8] = b"/safety/estop";
const RESOURCE_BRAIN: &[u8] = b"/brain/control";
// Matches the legacy `HandleKind::Motor(0)`/`Motor(1)` RW grant the autorun
// seed already makes (`kernel/src/main.rs`) — dual-mode migration, both
// paths grant identical authority. `motor.N` is the RFC-0005 target
// convention (see `rfcs/RFC-0005-static-topology.md`'s worked example).
const RESOURCE_MOTOR_0: &[u8] = b"motor.0";
const RESOURCE_MOTOR_1: &[u8] = b"motor.1";

/// Build the default minimal topology.
///
/// Five RFC-0004 scheduler classes + two seed tasks (`supervisor`,
/// `brain_link`). Budgets sum to **100 %** so admission_check passes.
///
/// All borrowed strings are `'static` literals declared above; the
/// returned `Topology<'static>` can be parked in a static slot.
pub fn default_minimal() -> Topology<'static> {
    let mut topo = Topology::empty();

    // Five classes — RFC-0004 default budgets, summing to exactly 100 %.
    let classes = [
        (
            NAME_SAFETY,
            ClassSpec {
                name: MaybeStr::from_bytes(NAME_SAFETY),
                cpu_budget_min_pct: 20,
                cpu_budget_max_pct: 100,
                policy: PolicyKind::Edf,
                priority_range: (0, 7),
                preemption: Preemption::Always,
                time_slice_ms: 0,
                admission_control: true,
            },
        ),
        (
            NAME_HARD_RT,
            ClassSpec {
                name: MaybeStr::from_bytes(NAME_HARD_RT),
                cpu_budget_min_pct: 30,
                cpu_budget_max_pct: 60,
                policy: PolicyKind::Edf,
                priority_range: (8, 15),
                preemption: Preemption::Always,
                time_slice_ms: 0,
                admission_control: true,
            },
        ),
        (
            NAME_SOFT_RT,
            ClassSpec {
                name: MaybeStr::from_bytes(NAME_SOFT_RT),
                cpu_budget_min_pct: 25,
                cpu_budget_max_pct: 60,
                policy: PolicyKind::Rr,
                priority_range: (16, 23),
                preemption: Preemption::TimerOnly,
                time_slice_ms: 10,
                admission_control: false,
            },
        ),
        (
            NAME_BEST_EFFORT,
            ClassSpec {
                name: MaybeStr::from_bytes(NAME_BEST_EFFORT),
                cpu_budget_min_pct: 20,
                cpu_budget_max_pct: 100,
                policy: PolicyKind::Cfs,
                priority_range: (24, 30),
                preemption: Preemption::TimerOnly,
                time_slice_ms: 0,
                admission_control: false,
            },
        ),
        (
            NAME_IDLE,
            ClassSpec {
                name: MaybeStr::from_bytes(NAME_IDLE),
                cpu_budget_min_pct: 5,
                cpu_budget_max_pct: 5,
                policy: PolicyKind::Sporadic,
                priority_range: (31, 31),
                preemption: Preemption::Never,
                time_slice_ms: 0,
                admission_control: false,
            },
        ),
    ];

    for (_, c) in classes.iter() {
        topo.push_class(*c)
            .expect("default_minimal_topology: too many classes");
    }

    // Tasks. Two seed tasks: a safety supervisor + the brain-link.
    topo.push_task(
        MaybeStr::from_bytes(TASK_SUPERVISOR),
        MaybeStr::from_bytes(NAME_SAFETY),
        0,
        &[CapSpec {
            kind: CapKind::Channel,
            perms: CapPerms::RW,
            target: MaybeStr::from_bytes(RESOURCE_ESTOP),
        }],
    )
    .expect("default_minimal_topology: supervisor push failed");

    topo.push_task(
        MaybeStr::from_bytes(TASK_BRAIN_LINK),
        MaybeStr::from_bytes(NAME_BEST_EFFORT),
        0,
        &[CapSpec {
            kind: CapKind::Channel,
            perms: CapPerms::RW,
            target: MaybeStr::from_bytes(RESOURCE_BRAIN),
        }],
    )
    .expect("default_minimal_topology: brain_link push failed");

    // P1 migration seed: Motor(0)/Motor(1) RW — the two grants that have a
    // typed minter today (`motor_grant_cap`). Deliberately does NOT declare
    // the legacy autorun seed's 10 Sensor(0..=9) RO grants: `Sensor` has no
    // typed minter yet (`sensor_grant_cap` is migration phase P2) — see
    // `crates/ipc/src/cap_seed.rs`'s doc for the gap list. Inventing a
    // Sensor CapSpec here would silently document a capability the bridge
    // can never actually mint.
    topo.push_task(
        MaybeStr::from_bytes(TASK_AUTORUN),
        MaybeStr::from_bytes(NAME_HARD_RT),
        0,
        &[
            CapSpec {
                kind: CapKind::Motor,
                perms: CapPerms::RW,
                target: MaybeStr::from_bytes(RESOURCE_MOTOR_0),
            },
            CapSpec {
                kind: CapKind::Motor,
                perms: CapPerms::RW,
                target: MaybeStr::from_bytes(RESOURCE_MOTOR_1),
            },
        ],
    )
    .expect("default_minimal_topology: autorun push failed");

    topo.set_sched_config(SchedConfig {
        partition_window_us: 10_000,
    });

    debug_assert!(topo.admission_check().is_ok());

    topo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_minimal_is_well_formed() {
        let topo = default_minimal();
        assert_eq!(topo.classes_len(), 5);
        // supervisor, brain_link, autorun (P1 migration seed).
        assert_eq!(topo.tasks_len(), 3);
        assert!(topo.admission_check().is_ok());

        // Budgets sum to exactly 100.
        let total: u32 = topo
            .classes()
            .iter()
            .map(|c| c.cpu_budget_min_pct as u32)
            .sum();
        assert_eq!(total, 100);
    }

    #[test]
    fn autorun_task_declares_both_drivetrain_motor_grants() {
        let topo = default_minimal();
        let task = topo
            .find_task(&MaybeStr::from_bytes(TASK_AUTORUN))
            .expect("autorun task must be declared for the P1 bridge to seed");
        let caps = topo.caps_of(task);
        assert_eq!(caps.len(), 2);
        assert!(caps
            .iter()
            .all(|c| c.kind == CapKind::Motor && c.perms == CapPerms::RW));
        assert_eq!(caps[0].target, MaybeStr::from_bytes(RESOURCE_MOTOR_0));
        assert_eq!(caps[1].target, MaybeStr::from_bytes(RESOURCE_MOTOR_1));
    }

    #[test]
    fn default_minimal_is_static() {
        // Compile-time check: the returned topology has 'static lifetime.
        fn assert_static<T: 'static>(_: &T) {}
        let topo = default_minimal();
        assert_static(&topo);
    }
}
