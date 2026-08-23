//! Host-side tests for the `robot_os_abi` crate.
//!
//! The kernel-side crate is `no_std` and bound to RISC-V; running the
//! `#[cfg(test)]` suites inside it requires this excluded crate.

#[cfg(test)]
mod cap_tests {
    use robot_os_abi::cap::{CapHandle, CapKind, CapPerms, CAP_NULL};

    #[test]
    fn null_is_zero() {
        assert!(CAP_NULL.is_null());
        assert_eq!(CAP_NULL.as_raw(), 0);
    }

    #[test]
    fn pack_unpack_round_trip() {
        let h = CapHandle::pack(CapKind::Channel, CapPerms::RW_DUP, 0x42, 0x1234);
        assert_eq!(h.kind(), CapKind::Channel as u8);
        assert!(h.perms().contains(CapPerms::READ));
        assert!(h.perms().contains(CapPerms::WRITE));
        assert!(h.perms().contains(CapPerms::DUP));
        assert!(!h.perms().contains(CapPerms::EXEC));
        assert_eq!(h.generation(), 0x42);
        assert_eq!(h.slot(), 0x1234);
    }

    #[test]
    fn perms_contains_logic() {
        assert!(CapPerms::ALL.contains(CapPerms::READ));
        assert!(CapPerms::ALL.contains(CapPerms::DUP));
        assert!(!CapPerms::READ.contains(CapPerms::WRITE));
        assert!(CapPerms::RW.contains(CapPerms::READ));
        assert!(CapPerms::RW.contains(CapPerms::WRITE));
        assert!(!CapPerms::RW.contains(CapPerms::DUP));
    }

    #[test]
    fn kind_from_raw_recognises_all() {
        assert_eq!(CapKind::from_raw(0), Some(CapKind::Null));
        assert_eq!(CapKind::from_raw(1), Some(CapKind::Channel));
        assert_eq!(CapKind::from_raw(15), Some(CapKind::AiSession));
        assert_eq!(CapKind::from_raw(16), None);
        assert_eq!(CapKind::from_raw(255), None);
    }

    #[test]
    fn nonzero_view() {
        let h = CapHandle::pack(CapKind::Shm, CapPerms::RW, 1, 7);
        assert!(h.as_nonzero().is_some());
        assert!(CAP_NULL.as_nonzero().is_none());
    }

    #[test]
    fn perms_union_intersection() {
        let r = CapPerms::READ;
        let w = CapPerms::WRITE;
        let rw = r.union(w);
        assert!(rw.contains(CapPerms::READ));
        assert!(rw.contains(CapPerms::WRITE));
        assert_eq!(rw.intersection(CapPerms::READ).bits(), CapPerms::READ.bits());
    }
}

#[cfg(test)]
mod error_tests {
    use robot_os_abi::error::Errno;

    #[test]
    fn round_trip_all_known_errnos() {
        let cases = [
            Errno::EPERM,
            Errno::ENOENT,
            Errno::EIO,
            Errno::EBADF,
            Errno::EAGAIN,
            Errno::ENOMEM,
            Errno::EACCES,
            Errno::EFAULT,
            Errno::EBUSY,
            Errno::EEXIST,
            Errno::ENODEV,
            Errno::EINVAL,
            Errno::ENOSYS,
            Errno::ECAPKIND,
            Errno::ECAPPERMS,
            Errno::ECAPSTALE,
            Errno::ETOPOLOGY,
            Errno::ESAFETY,
            Errno::EAUTH,
            Errno::EREPLAY,
            Errno::EOTASIG,
            Errno::EROLLBACK,
            Errno::EQUOTA,
            Errno::EABIVERSION,
        ];
        for e in cases {
            let ret = e.to_syscall_ret();
            assert!(ret < 0, "{:?} maps to non-negative", e);
            assert_eq!(
                Errno::from_syscall_ret(ret),
                Some(e),
                "round-trip failure for {:?}",
                e
            );
        }
    }

    #[test]
    fn non_negative_is_not_error() {
        assert_eq!(Errno::from_syscall_ret(0), None);
        assert_eq!(Errno::from_syscall_ret(42), None);
        assert_eq!(Errno::from_syscall_ret(i64::MAX), None);
    }

    #[test]
    fn unknown_negative_returns_none() {
        // -999 is not in our errno table.
        assert_eq!(Errno::from_syscall_ret(-999), None);
    }

    #[test]
    fn cap_specific_errnos_have_expected_values() {
        // These wire-format numbers are part of the ABI freeze.
        assert_eq!(Errno::ECAPKIND as i64, 200);
        assert_eq!(Errno::ECAPPERMS as i64, 201);
        assert_eq!(Errno::ECAPSTALE as i64, 202);
    }
}

#[cfg(test)]
mod types_tests {
    use core::mem::size_of;
    use robot_os_abi::types::{MotorOutput, RobotInfo, SafetyProfile, SensorState};

    /// These sizes are part of the ABI freeze. Changing them is a
    /// breaking change requiring a major version bump and an RFC.
    #[test]
    fn sensor_state_size_is_stable() {
        assert_eq!(size_of::<SensorState>(), 48);
    }

    #[test]
    fn motor_output_size_is_stable() {
        assert_eq!(size_of::<MotorOutput>(), 12);
    }

    #[test]
    fn robot_info_size_is_stable() {
        assert_eq!(size_of::<RobotInfo>(), 8);
    }

    #[test]
    fn safety_profile_size_is_stable() {
        assert_eq!(size_of::<SafetyProfile>(), 24);
    }
}

#[cfg(test)]
mod syscall_nr_tests {
    use robot_os_abi::syscall_nr::*;

    /// These syscall numbers are wire-format. Changing them is an
    /// ABI break requiring a major version bump and an RFC.
    #[test]
    fn frozen_syscall_numbers() {
        assert_eq!(SYS_TEST, 0);
        assert_eq!(SYS_EXIT, 3);
        assert_eq!(SYS_FORK, 12);
        assert_eq!(SYS_OPEN, 20);
        assert_eq!(SYS_IPC_CREATE, 100);
        assert_eq!(SYS_IPC_SEND, 101);
        assert_eq!(SYS_GPIO_READ, 200);
        assert_eq!(SYS_PWM_ENABLE, 210);
        assert_eq!(SYS_NET_INFO, 260);
        assert_eq!(SYS_DRV_REGISTER, 300);
        assert_eq!(SYS_ROBOT_INIT, 320);
        assert_eq!(SYS_KILL, 350);
        assert_eq!(SYS_SOCKET, 370);
        assert_eq!(SYS_SERVICE_REGISTER, 390);
        assert_eq!(SYS_BRK, 400);
        assert_eq!(SYS_SECCOMP, 430);
        assert_eq!(SYS_IO_SETUP, 503);
        assert_eq!(SYS_HANDLE_GRANT, 515);
        assert_eq!(SYS_DRIVER_REGISTER, 520);
        assert_eq!(SYS_CHAN_WRITE_TYPED, 528);
        assert_eq!(SYS_CHAN_READ_TYPED, 529);
        assert_eq!(SYS_PORT_CREATE_TYPED, 530);
        assert_eq!(SYS_PORT_POLL_TYPED, 531);
        assert_eq!(SYS_PORT_DESTROY_TYPED, 532);
        assert_eq!(SYS_SHM_CREATE_TYPED, 533);
        assert_eq!(SYS_SHM_ACQUIRE_TYPED, 534);
        assert_eq!(SYS_SHM_RELEASE_TYPED, 535);
        assert_eq!(SYS_IORING_CREATE_TYPED, 536);
        assert_eq!(SYS_IORING_SUBMIT_TYPED, 537);
        assert_eq!(SYS_IORING_DESTROY_TYPED, 538);
        // W5 batch 5.1 — Cap<Gpio>.
        assert_eq!(SYS_GPIO_READ_TYPED, 539);
        assert_eq!(SYS_GPIO_WRITE_TYPED, 540);
        assert_eq!(SYS_GPIO_SET_DIR_TYPED, 541);
        // W5 batch 5.2 — Cap<I2c>.
        assert_eq!(SYS_I2C_READ_TYPED, 542);
        assert_eq!(SYS_I2C_WRITE_TYPED, 543);
        assert_eq!(SYS_I2C_DETECT_TYPED, 544);
        assert_eq!(I2C_TYPED_MAX_BYTES, 256);
        // W5 batch 5.3 — Cap<Pwm> (fills 528..=549).
        assert_eq!(SYS_PWM_ENABLE_TYPED, 545);
        assert_eq!(SYS_PWM_DISABLE_TYPED, 546);
        assert_eq!(SYS_PWM_SET_PERIOD_TYPED, 547);
        assert_eq!(SYS_PWM_SET_DUTY_TYPED, 548);
        assert_eq!(SYS_PWM_SET_DUTY_PCT_TYPED, 549);
        // W5 batch 5.4 — Cap<Motor> (opens 550..=569 extension).
        assert_eq!(SYS_MOTOR_SET_TARGET_TYPED, 550);
        assert_eq!(SYS_MOTOR_TICK_TYPED, 551);
        assert_eq!(SYS_MOTOR_ENABLE_TYPED, 552);
        assert_eq!(SYS_MOTOR_ENABLED_TYPED, 553);
        assert_eq!(SYS_MOTOR_SET_GAINS_TYPED, 554);
        assert_eq!(SYS_MOTOR_RESET_TYPED, 555);
        assert_eq!(MOTOR_TICK_OUT_BYTES, 8);
        // RFC-0002 Driver registry bridge.
        assert_eq!(SYS_DRV_INVOKE, 311);
        assert_eq!(DRIVER_INVOKE_MAX_INPUT_BYTES, 256);
        assert_eq!(DRIVER_INVOKE_MAX_OUTPUT_BYTES, 256);
    }

    #[test]
    fn typed_caps_in_reserved_range() {
        // 528..=549 reserved for cap-typed migrations (RFC-0003);
        // 550..=569 extends it for hardware-cap families (W5
        // batch 5.4+). Both checks coexist below.
        assert!((528..550).contains(&SYS_CHAN_WRITE_TYPED));
        assert!((528..550).contains(&SYS_CHAN_READ_TYPED));
        assert!((528..550).contains(&SYS_PORT_CREATE_TYPED));
        assert!((528..550).contains(&SYS_PORT_POLL_TYPED));
        assert!((528..550).contains(&SYS_PORT_DESTROY_TYPED));
        assert!((528..550).contains(&SYS_SHM_CREATE_TYPED));
        assert!((528..550).contains(&SYS_SHM_ACQUIRE_TYPED));
        assert!((528..550).contains(&SYS_SHM_RELEASE_TYPED));
        assert!((528..550).contains(&SYS_IORING_CREATE_TYPED));
        assert!((528..550).contains(&SYS_IORING_SUBMIT_TYPED));
        assert!((528..550).contains(&SYS_IORING_DESTROY_TYPED));
        assert!((528..550).contains(&SYS_GPIO_READ_TYPED));
        assert!((528..550).contains(&SYS_GPIO_WRITE_TYPED));
        assert!((528..550).contains(&SYS_GPIO_SET_DIR_TYPED));
        assert!((528..550).contains(&SYS_I2C_READ_TYPED));
        assert!((528..550).contains(&SYS_I2C_WRITE_TYPED));
        assert!((528..550).contains(&SYS_I2C_DETECT_TYPED));
        assert!((528..550).contains(&SYS_PWM_ENABLE_TYPED));
        assert!((528..550).contains(&SYS_PWM_DISABLE_TYPED));
        assert!((528..550).contains(&SYS_PWM_SET_PERIOD_TYPED));
        assert!((528..550).contains(&SYS_PWM_SET_DUTY_TYPED));
        assert!((528..550).contains(&SYS_PWM_SET_DUTY_PCT_TYPED));
        // 550..=569 extension range — hardware-cap families that
        // didn't fit in 528..=549.
        assert!((550..570).contains(&SYS_MOTOR_SET_TARGET_TYPED));
        assert!((550..570).contains(&SYS_MOTOR_TICK_TYPED));
        assert!((550..570).contains(&SYS_MOTOR_ENABLE_TYPED));
        assert!((550..570).contains(&SYS_MOTOR_ENABLED_TYPED));
        assert!((550..570).contains(&SYS_MOTOR_SET_GAINS_TYPED));
        assert!((550..570).contains(&SYS_MOTOR_RESET_TYPED));
    }

    #[test]
    fn no_collisions_in_assigned_range() {
        // Smoke check: a handful of numbers don't collide.
        let nrs = [
            SYS_TEST,
            SYS_EXIT,
            SYS_FORK,
            SYS_OPEN,
            SYS_IPC_CREATE,
            SYS_GPIO_READ,
            SYS_CHAN_WRITE_TYPED,
            SYS_CHAN_READ_TYPED,
        ];
        for (i, &a) in nrs.iter().enumerate() {
            for &b in &nrs[i + 1..] {
                assert_ne!(a, b, "syscall numbers collide: {a} vs {b}");
            }
        }
    }
}

#[cfg(test)]
mod abi_version_tests {
    use robot_os_abi::ABI_VERSION;

    #[test]
    fn abi_version_is_v1() {
        assert_eq!(ABI_VERSION, 1);
    }
}
