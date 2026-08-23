//! Host-side parser tests for `robot_os_topology`, plus the P1
//! topology→cap_store cap-seed bridge suite (RFC-0003/RFC-0005 migration).
//!
//! The kernel-side crate is `no_std` and bound to RISC-V; running its
//! parser tests on the host requires this excluded crate (matching the
//! `ota-tests` pattern). Build / run:
//!
//! ```bash
//! cd crates/topology-tests
//! cargo test
//! ```
//!
//! # The cap-seed bridge modules
//!
//! `robot_os_topology` itself has no RV64-only dependencies (see its
//! `Cargo.toml`: `robot_os_abi` + `robot_os_crypto` + `robot_os_limits`,
//! all host-friendly), so it is a plain crate dependency above. The bridge
//! it feeds (`crates/ipc/src/cap_seed.rs`) lives in `robot_os_ipc`, which
//! IS RV64-only as a whole crate — so, same trick as `crates/cap-tests`,
//! the handful of `ipc` source files the bridge actually needs are pulled
//! in directly via `#[path]` instead, with host stand-ins swapped in via
//! Cargo dependency renames for the two RV64-only things they call into
//! (`robot_os_sync`, `robot_os_sched`) plus a third for this suite
//! specifically (`robot_os_drivers`, needed by `gpio_cap.rs`/`i2c_cap.rs`/
//! `pwm_cap.rs`/`motor_cap.rs`). See `shims/*/src/lib.rs` for what each
//! stand-in actually provides.

#[path = "../../ipc/src/cap.rs"]
pub mod cap;

#[path = "../../ipc/src/cap_store.rs"]
pub mod cap_store;

#[path = "../../ipc/src/gpio_cap.rs"]
pub mod gpio_cap;

#[path = "../../ipc/src/i2c_cap.rs"]
pub mod i2c_cap;

#[path = "../../ipc/src/pwm_cap.rs"]
pub mod pwm_cap;

#[path = "../../ipc/src/motor_cap.rs"]
pub mod motor_cap;

#[path = "../../ipc/src/cap_seed.rs"]
pub mod cap_seed;

#[cfg(test)]
mod sched_tests {
    use robot_os_topology::{
        parse_sched, AdmissionError, ParseError, PolicyKind, Preemption, Topology,
    };

    /// A minimal but realistic SCHED.TOML — three classes summing to 100 %.
    const SCHED_OK: &[u8] = b"\
[class.safety_critical]
cpu_budget_min_pct  = 15
cpu_budget_max_pct  = 100
policy              = \"fifo\"
priority_range      = [0, 7]
preemption          = \"always\"

[class.hard_rt]
cpu_budget_min_pct  = 30
cpu_budget_max_pct  = 50
policy              = \"edf\"
admission_control   = true

[class.best_effort]
cpu_budget_min_pct  = 5
cpu_budget_max_pct  = 100
policy              = \"cfs\"
priority_range      = [16, 30]

[sched]
partition_window_us = 5000
";

    #[test]
    fn parse_three_classes() {
        let mut topo = Topology::empty();
        parse_sched(SCHED_OK, &mut topo).unwrap();
        assert_eq!(topo.classes_len(), 3);
        let safety = topo.classes()[0];
        assert_eq!(safety.cpu_budget_min_pct, 15);
        assert_eq!(safety.policy, PolicyKind::Fifo);
        assert_eq!(safety.preemption, Preemption::Always);
        let hard = topo.classes()[1];
        assert_eq!(hard.policy, PolicyKind::Edf);
        assert!(hard.admission_control);
        let be = topo.classes()[2];
        assert_eq!(be.policy, PolicyKind::Cfs);
        assert_eq!(be.priority_range, (16, 30));
        assert_eq!(topo.sched_config().partition_window_us, 5000);
    }

    #[test]
    fn budget_overflow_rejected_by_admission() {
        let toml = b"\
[class.a]
cpu_budget_min_pct = 60
policy             = \"fifo\"

[class.b]
cpu_budget_min_pct = 60
policy             = \"fifo\"
";
        let mut topo = Topology::empty();
        parse_sched(toml, &mut topo).unwrap();
        assert_eq!(
            topo.admission_check(),
            Err(AdmissionError::BudgetOverflow)
        );
    }

    #[test]
    fn unknown_policy_rejected() {
        let toml = b"\
[class.weird]
cpu_budget_min_pct = 10
policy             = \"made_up\"
";
        let mut topo = Topology::empty();
        let r = parse_sched(toml, &mut topo);
        assert!(matches!(r, Err(ParseError::UnknownEnumValue)));
    }

    #[test]
    fn unterminated_section_rejected() {
        let toml = b"[class.broken\n";
        let mut topo = Topology::empty();
        let r = parse_sched(toml, &mut topo);
        assert!(matches!(r, Err(ParseError::UnterminatedSection)));
    }

    #[test]
    fn unknown_section_rejected() {
        let toml = b"[unknown.thing]\n";
        let mut topo = Topology::empty();
        let r = parse_sched(toml, &mut topo);
        assert!(matches!(r, Err(ParseError::UnknownSection)));
    }

    #[test]
    fn unknown_field_rejected() {
        let toml = b"\
[class.x]
cpu_budget_min_pct = 5
made_up_field = 1
";
        let mut topo = Topology::empty();
        let r = parse_sched(toml, &mut topo);
        assert!(matches!(r, Err(ParseError::UnknownField)));
    }

    #[test]
    fn priority_range_parses() {
        let toml = b"\
[class.x]
cpu_budget_min_pct = 5
policy = \"rr\"
priority_range = [3, 9]
time_slice_ms = 12
";
        let mut topo = Topology::empty();
        parse_sched(toml, &mut topo).unwrap();
        let c = topo.classes()[0];
        assert_eq!(c.priority_range, (3, 9));
        assert_eq!(c.time_slice_ms, 12);
    }

    #[test]
    fn comments_and_blank_lines_ok() {
        let toml = b"\
# Top of file comment.

# Another comment.
[class.x]    # inline comment
cpu_budget_min_pct = 5  # trailing comment
policy             = \"fifo\"

# Trailing comment.
";
        let mut topo = Topology::empty();
        parse_sched(toml, &mut topo).unwrap();
        assert_eq!(topo.classes_len(), 1);
    }

    #[test]
    fn duplicate_class_caught() {
        let toml = b"\
[class.x]
cpu_budget_min_pct = 1
policy = \"fifo\"

[class.x]
cpu_budget_min_pct = 2
policy = \"rr\"
";
        let mut topo = Topology::empty();
        let r = parse_sched(toml, &mut topo);
        assert!(matches!(
            r,
            Err(ParseError::Admission(AdmissionError::DuplicateClass))
        ));
    }
}

#[cfg(test)]
mod caps_tests {
    use robot_os_abi::cap::{CapKind, CapPerms};
    use robot_os_topology::{parse_caps, parse_sched, AdmissionError, ParseError, Topology};

    const SCHED_PRIMER: &[u8] = b"\
[class.safety_critical]
cpu_budget_min_pct = 15
policy             = \"fifo\"

[class.hard_rt]
cpu_budget_min_pct = 30
policy             = \"edf\"

[class.best_effort]
cpu_budget_min_pct = 5
policy             = \"cfs\"
";

    #[test]
    fn parse_one_task_with_three_caps() {
        let caps = b"\
[task.rt_motor]
class    = \"hard_rt\"
priority = 5
caps = [
    { kind = \"motor\",       target = \"motor.0\", perm = \"rw\" },
    { kind = \"motor\",       target = \"motor.1\", perm = \"rw\" },
    { kind = \"channel-sub\", target = \"/cmd/motor\", perm = \"r\" },
]
";
        let mut topo = Topology::empty();
        parse_sched(SCHED_PRIMER, &mut topo).unwrap();
        parse_caps(caps, &mut topo).unwrap();
        assert_eq!(topo.tasks_len(), 1);
        let task = topo.tasks()[0];
        assert_eq!(task.priority, 5);
        let caps_slice = topo.caps_of(&task);
        assert_eq!(caps_slice.len(), 3);
        assert_eq!(caps_slice[0].kind, CapKind::Motor);
        assert_eq!(caps_slice[0].perms, CapPerms::RW);
        assert_eq!(caps_slice[2].kind, CapKind::Channel);
        assert_eq!(caps_slice[2].perms, CapPerms::READ);
    }

    #[test]
    fn cross_reference_unknown_class_rejected() {
        let caps = b"\
[task.lonely]
class = \"does_not_exist\"
caps = []
";
        let mut topo = Topology::empty();
        parse_sched(SCHED_PRIMER, &mut topo).unwrap();
        parse_caps(caps, &mut topo).unwrap();
        assert_eq!(
            topo.admission_check(),
            Err(AdmissionError::UnknownClass)
        );
    }

    #[test]
    fn null_kind_rejected_with_admission_error() {
        // "null" is not in the kind table → UnknownEnumValue from parser.
        let caps = b"\
[task.bad]
caps = [
    { kind = \"unknown_kind\", target = \"x\", perm = \"r\" },
]
";
        let mut topo = Topology::empty();
        parse_sched(SCHED_PRIMER, &mut topo).unwrap();
        let r = parse_caps(caps, &mut topo);
        assert!(matches!(r, Err(ParseError::UnknownEnumValue)));
    }

    #[test]
    fn empty_caps_array_ok() {
        let caps = b"\
[task.passive]
caps = []
";
        let mut topo = Topology::empty();
        parse_sched(SCHED_PRIMER, &mut topo).unwrap();
        parse_caps(caps, &mut topo).unwrap();
        assert_eq!(topo.tasks_len(), 1);
        assert_eq!(topo.caps_of(&topo.tasks()[0]).len(), 0);
    }

    #[test]
    fn duplicate_task_rejected() {
        let caps = b"\
[task.x]
caps = []

[task.x]
caps = []
";
        let mut topo = Topology::empty();
        parse_sched(SCHED_PRIMER, &mut topo).unwrap();
        let r = parse_caps(caps, &mut topo);
        assert!(matches!(
            r,
            Err(ParseError::Admission(AdmissionError::DuplicateTask))
        ));
    }

    #[test]
    fn perm_string_combinations() {
        let caps = b"\
[task.x]
caps = [
    { kind = \"shm\", target = \"a\", perm = \"r\" },
    { kind = \"shm\", target = \"b\", perm = \"rw\" },
    { kind = \"shm\", target = \"c\", perm = \"rwx\" },
    { kind = \"shm\", target = \"d\", perm = \"rwxd\" },
]
";
        let mut topo = Topology::empty();
        parse_sched(SCHED_PRIMER, &mut topo).unwrap();
        parse_caps(caps, &mut topo).unwrap();
        let task_caps = topo.caps_of(&topo.tasks()[0]);
        assert_eq!(task_caps[0].perms, CapPerms::READ);
        assert_eq!(task_caps[1].perms, CapPerms::RW);
        assert!(task_caps[2].perms.contains(CapPerms::EXEC));
        assert!(task_caps[3].perms.contains(CapPerms::DUP));
    }

    #[test]
    fn missing_kind_field_rejected() {
        let caps = b"\
[task.x]
caps = [
    { target = \"a\", perm = \"r\" },
]
";
        let mut topo = Topology::empty();
        parse_sched(SCHED_PRIMER, &mut topo).unwrap();
        let r = parse_caps(caps, &mut topo);
        assert!(matches!(r, Err(ParseError::MissingField)));
    }

    #[test]
    fn realistic_caps_example_from_rfc() {
        // Verbatim from RFC-0005 (slightly trimmed).
        let caps = b"\
[task.rt_motor]
class    = \"hard_rt\"
priority = 5
caps = [
    { kind = \"motor\",       target = \"motor.0\",     perm = \"rw\" },
    { kind = \"motor\",       target = \"motor.1\",     perm = \"rw\" },
    { kind = \"encoder\",     target = \"encoder.0\",   perm = \"r\" },
    { kind = \"encoder\",     target = \"encoder.1\",   perm = \"r\" },
    { kind = \"channel-sub\", target = \"/cmd/motor\",  perm = \"r\" },
    { kind = \"channel-pub\", target = \"/state/motor\",perm = \"w\" },
]

[task.sensor_ahrs]
class    = \"hard_rt\"
priority = 4
caps = [
    { kind = \"i2c\",         target = \"bus.0/0x68\",  perm = \"rw\" },
    { kind = \"i2c\",         target = \"bus.0/0x76\",  perm = \"rw\" },
    { kind = \"channel-pub\", target = \"/sensors/imu\",perm = \"w\" },
    { kind = \"channel-pub\", target = \"/sensors/baro\",perm = \"w\" },
]

[task.behavior]
class    = \"best_effort\"
priority = 0
caps = [
    { kind = \"channel-sub\", target = \"/sensors/imu\",  perm = \"r\" },
    { kind = \"channel-sub\", target = \"/sensors/baro\", perm = \"r\" },
    { kind = \"channel-pub\", target = \"/cmd/motor\",    perm = \"w\" },
    { kind = \"service-call\", target = \"policy.run\",   perm = \"rw\" },
]
";
        let mut topo = Topology::empty();
        parse_sched(SCHED_PRIMER, &mut topo).unwrap();
        parse_caps(caps, &mut topo).unwrap();
        assert_eq!(topo.tasks_len(), 3);
        topo.admission_check().unwrap();
    }
}

#[cfg(test)]
mod verify_tests {
    use robot_os_topology::{verify_signature, VerifyError};

    #[test]
    fn zero_signature_rejected() {
        let toml = b"[task.a]\ncaps = []\n";
        let sig = [0u8; 64];
        let key = [0u8; 32];
        let r = verify_signature(toml, &sig, &key);
        assert_eq!(r, Err(VerifyError::InvalidSignature));
    }

    #[test]
    fn wrong_signature_size_caught() {
        let toml = b"[task.a]\n";
        let key = [0u8; 32];
        let r = verify_signature(toml, &[0u8; 30], &key);
        assert_eq!(r, Err(VerifyError::BadSignatureLen));
    }

    #[test]
    fn wrong_key_size_caught() {
        let toml = b"[task.a]\n";
        let sig = [0u8; 64];
        let r = verify_signature(toml, &sig, &[0u8; 16]);
        assert_eq!(r, Err(VerifyError::BadKeyLen));
    }
}

#[cfg(test)]
mod builder_tests {
    use robot_os_topology::{default_minimal, MaybeStr};

    #[test]
    fn default_topology_has_all_five_classes() {
        let t = default_minimal();
        assert_eq!(t.classes_len(), 5);
        // Names match.
        let names: [&[u8]; 5] = [
            b"safety_critical",
            b"hard_rt",
            b"soft_rt",
            b"best_effort",
            b"idle",
        ];
        for (cls, want) in t.classes().iter().zip(names.iter()) {
            assert_eq!(cls.name, MaybeStr::from_bytes(want));
        }
    }

    #[test]
    fn default_topology_has_supervisor_and_brain_link() {
        let t = default_minimal();
        // supervisor, brain_link, autorun (P1 cap-seed migration entry —
        // see `default_topology_has_autorun_motor_grants` below).
        assert_eq!(t.tasks_len(), 3);
        let supervisor = t
            .find_task(&MaybeStr::from_bytes(b"supervisor"))
            .expect("supervisor present");
        let class = t.find_class(&supervisor.class_name).unwrap();
        assert_eq!(class.name, MaybeStr::from_bytes(b"safety_critical"));
        let brain = t
            .find_task(&MaybeStr::from_bytes(b"brain_link"))
            .expect("brain_link present");
        assert!(t.find_class(&brain.class_name).is_some());
    }

    #[test]
    fn default_topology_has_autorun_motor_grants() {
        use robot_os_abi::cap::{CapKind, CapPerms};
        let t = default_minimal();
        let autorun = t
            .find_task(&MaybeStr::from_bytes(b"autorun"))
            .expect("autorun present — the P1 bridge seeds from this entry");
        let caps = t.caps_of(autorun);
        assert_eq!(caps.len(), 2);
        assert!(caps
            .iter()
            .all(|c| c.kind == CapKind::Motor && c.perms == CapPerms::RW));
    }

    #[test]
    fn default_topology_admission_passes() {
        let t = default_minimal();
        t.admission_check()
            .expect("default minimal topology must pass admission");
    }
}

#[cfg(test)]
mod state_tests {
    use robot_os_topology::{default_minimal, get, init, is_ready, InitError};

    /// Note: the state slot is a `static`. We can only exercise the
    /// init→ready transition once per test binary. Rust runs each
    /// `#[test]` in its own thread inside one process, so this test
    /// can't be split — we drive the full sequence in one shot.
    #[test]
    fn init_then_get_then_double_init_fails() {
        // Pre: slot is empty, get() returns None.
        assert!(!is_ready());
        assert!(get().is_none());

        // Init succeeds.
        init(default_minimal()).expect("first init must succeed");
        assert!(is_ready());

        // get() now returns the loaded topology.
        let t = get().expect("post-init get must yield topology");
        assert_eq!(t.classes_len(), 5);
        // supervisor, brain_link, autorun (P1 cap-seed migration entry).
        assert_eq!(t.tasks_len(), 3);

        // A second init must fail with AlreadyInit.
        let r = init(default_minimal());
        assert_eq!(r, Err(InitError::AlreadyInit));

        // get() still returns the originally-loaded topology.
        assert!(get().is_some());
    }
}

// ──────────────────────────────────────────────────────────────────────────
// P1 cap-seed bridge — RFC-0003/RFC-0005 migration
// ──────────────────────────────────────────────────────────────────────────
//
// `crates/ipc/src/cap_seed.rs` decodes a `CapSpec {kind, perms, target}` (the
// exact triple `robot_os_topology::CapSpec` carries) and mints the
// corresponding typed cap. These tests build declarations the same way
// `default_minimal()` does (`Topology::push_task`), run them through the
// bridge, and verify the result the same way the typed syscall handlers
// consume it: decode the returned raw handle back into a `Cap<T>` and
// dereference it through `cap_store::get`.
#[cfg(test)]
mod cap_seed_bridge_tests {
    use robot_os_abi::cap::{CapKind, CapPerms};
    use robot_os_topology::{CapSpec, MaybeStr, Topology};
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::cap::{targets, Cap, CapError};
    use crate::cap_store;

    static NEXT_ID: AtomicU32 = AtomicU32::new(1);

    /// Hand out a fresh, never-reused TID bound to its own task-pool slot in
    /// the `robot_os_sched` shim — mirrors `crates/cap-tests`' `fresh_task`.
    fn fresh_tid() -> u32 {
        let tid = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let slot = tid as usize;
        assert!(
            slot < robot_os_sched::task::MAX_TASKS,
            "cap-seed bridge suite has outgrown the task pool"
        );
        robot_os_sched::shim_bind(tid, slot);
        tid
    }

    /// A decl with GPIO/I2C/PWM/Motor grants produces exactly those
    /// minted caps — the part A requirement, end to end: `Topology` decl →
    /// bridge → `cap_store::get` sees the right resource id under each
    /// kind's own permission requirement.
    #[test]
    fn a_decl_with_gpio_i2c_pwm_motor_grants_produces_exactly_those_mints() {
        let mut topo = Topology::empty();
        topo.push_task(
            MaybeStr::from_bytes(b"probe"),
            MaybeStr::from_bytes(b"any"),
            0,
            &[
                CapSpec {
                    kind: CapKind::Gpio,
                    perms: CapPerms::RW,
                    target: MaybeStr::from_bytes(b"gpio.9"),
                },
                CapSpec {
                    kind: CapKind::I2c,
                    perms: CapPerms::READ,
                    target: MaybeStr::from_bytes(b"bus.1/0x76"),
                },
                CapSpec {
                    kind: CapKind::Pwm,
                    perms: CapPerms::WRITE,
                    target: MaybeStr::from_bytes(b"pwm.3"),
                },
                CapSpec {
                    kind: CapKind::Motor,
                    perms: CapPerms::RW,
                    target: MaybeStr::from_bytes(b"motor.1"),
                },
            ],
        )
        .unwrap();
        let task = topo.find_task(&MaybeStr::from_bytes(b"probe")).unwrap();
        let tid = fresh_tid();

        let mut minted = Vec::new();
        for cap in topo.caps_of(task) {
            let handle = crate::cap_seed::seed_one_cap(tid, cap.kind, cap.perms, cap.target.as_str())
                .unwrap_or_else(|| panic!("expected a typed minter for {:?}", cap.kind));
            minted.push((cap.kind, handle));
        }
        assert_eq!(minted.len(), 4, "exactly the four declared grants, no more, no less");

        for (kind, handle) in minted {
            match kind {
                CapKind::Gpio => assert_eq!(
                    cap_store::get(tid, Cap::<targets::Gpio>::from_raw(handle), CapPerms::READ),
                    Ok(9)
                ),
                CapKind::I2c => assert_eq!(
                    cap_store::get(tid, Cap::<targets::I2c>::from_raw(handle), CapPerms::READ),
                    Ok((1u32 << 8) | 0x76)
                ),
                CapKind::Pwm => assert_eq!(
                    cap_store::get(tid, Cap::<targets::Pwm>::from_raw(handle), CapPerms::WRITE),
                    Ok(3)
                ),
                CapKind::Motor => assert_eq!(
                    cap_store::get(tid, Cap::<targets::Motor>::from_raw(handle), CapPerms::WRITE),
                    Ok(1)
                ),
                other => panic!("unexpected kind in this decl: {:?}", other),
            }
        }
    }

    /// Kinds with no typed minter yet (Sensor — P2 gap) or a target that
    /// does not parse under its kind's convention are skipped (`None`), not
    /// guessed at or panicked on.
    #[test]
    fn unminted_kinds_and_unparseable_targets_are_skipped() {
        let tid = fresh_tid();
        assert!(crate::cap_seed::seed_one_cap(tid, CapKind::Sensor, CapPerms::READ, "sensor.0")
            .is_none());
        assert!(crate::cap_seed::seed_one_cap(tid, CapKind::Irq, CapPerms::READ, "irq.3").is_none());
        assert!(crate::cap_seed::seed_one_cap(tid, CapKind::Motor, CapPerms::RW, "motor.left")
            .is_none());
        // Path-shaped channel target — no name->id registry (documented gap).
        assert!(
            crate::cap_seed::seed_one_cap(tid, CapKind::Channel, CapPerms::RW, "/safety/estop")
                .is_none()
        );
    }

    /// The negative half of part B's validation requirement: without
    /// seeding, the exact lookup the typed syscall handlers perform
    /// (`Cap::from_raw` → `cap_store::get`) reports `Stale`, matching
    /// `ECAPSTALE` at the syscall boundary.
    #[test]
    fn without_seeding_the_typed_consumer_path_sees_stale() {
        let tid = fresh_tid();
        let forged: Cap<targets::Motor> = Cap::NULL;
        assert_eq!(
            cap_store::get(tid, forged, CapPerms::WRITE),
            Err(CapError::Stale)
        );
    }

    /// `default_minimal()`'s "autorun" entry (added for the P1 migration —
    /// see `crates/topology/src/builder.rs`) round-trips through the same
    /// bridge call `kernel/src/main.rs`'s autorun block makes, and the
    /// result satisfies the pair-write rule `motor_cap.rs::require_pair_write`
    /// enforces for every actuation syscall.
    #[test]
    fn default_minimal_autorun_entry_seeds_a_write_satisfying_motor_pair() {
        let topo = robot_os_topology::default_minimal();
        let task = topo
            .find_task(&MaybeStr::from_bytes(b"autorun"))
            .expect("autorun task declared in default_minimal()");
        let tid = fresh_tid();

        let mut minted = 0;
        for cap in topo.caps_of(task) {
            if crate::cap_seed::seed_one_cap(tid, cap.kind, cap.perms, cap.target.as_str())
                .is_some()
            {
                minted += 1;
            }
        }
        assert_eq!(minted, 2, "Motor(0) RW + Motor(1) RW");

        // Same primitive `require_pair_write` uses — proves the seed is
        // locatable by kind+resource, not just "some cap got minted".
        let pair_ok = cap_store::with_table(tid, |t| {
            t.holds_kind_resource_with(CapKind::Motor, 0, CapPerms::WRITE)
                && t.holds_kind_resource_with(CapKind::Motor, 1, CapPerms::WRITE)
        });
        assert_eq!(pair_ok, Some(true));
    }
}
