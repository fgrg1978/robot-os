# Kconfig.robot — Robot type and kinematic configuration
#
# RFC-0026 Phase C1.  Selects the mechanical platform and sets
# per-robot kinematic defaults.  Runtime override via CONFIG.INI.

menu "Robot Configuration"

# ---------------------------------------------------------------------------
# Robot type
# ---------------------------------------------------------------------------

choice
    prompt "Robot type"
    default ROBOT_WHEELED
    help
      Selects the mechanical platform.  This controls which policy
      module is compiled in (policy/wheeled, policy/drone, etc.) and
      the default actuator output format.

    config ROBOT_WHEELED
        bool "Wheeled (2-channel differential drive)"
        help
          Two-wheel or four-wheel differential drive robot.
          Actuator output: (speed_l, speed_r) signed 16-bit values.
          Default for ground robots.

    config ROBOT_DRONE
        bool "Drone (4-channel quadrotor PWM)"
        help
          Quadrotor UAV.  Actuator output: (throttle, roll, pitch, yaw)
          as 16-bit PWM duty cycles.  Enables drone safety guards
          (tilt limit, altitude limit, GPS fence).

    config ROBOT_HUMANOID
        bool "Humanoid (N-channel joint angles)"
        help
          Multi-DOF bipedal or arm robot.  Actuator output: joint angle
          array.  Currently a stub — full inverse kinematics deferred.

    config ROBOT_ACKERMANN
        bool "Ackermann (car-like, 2-channel: throttle + steering)"
        help
          Rear-wheel drive with Ackermann front steering.
          Actuator output: (throttle, steering_angle).

endchoice

# ---------------------------------------------------------------------------
# Kinematic constants (runtime-overridable via CONFIG.INI)
# ---------------------------------------------------------------------------

config TICKS_PER_M
    int "Encoder ticks per metre"
    range 1 1000000
    default 1000
    help
      Number of encoder pulses corresponding to 1 metre of travel.
      Depends on wheel circumference and encoder resolution.
      Runtime override: CONFIG.INI key `ticks_per_m`.
      Current codebase default: 1000 (CFG_TICKS_PER_M).

config WHEEL_BASE_MM
    int "Wheel base (mm)"
    range 1 10000
    default 200
    help
      Distance between left and right drive wheels in millimetres.
      Used by the differential-drive kinematic model to compute
      turning radius from wheel speed difference.
      Runtime override: CONFIG.INI key `wheel_base_mm`.
      Current codebase default: 200 mm (CFG_WHEEL_BASE_MM).

config MOTOR_MAX_SPEED
    int "Motor maximum speed value (0-100 scale)"
    range 0 100
    default 100
    help
      Maximum allowable speed command (dimensionless 0-100 scale).
      Safety module clamps all speed outputs to this value.
      Runtime override: CONFIG.INI key `motor_max_speed`.
      Current codebase default: 100 (CFG_MOTOR_MAX_SPEED).

# ---------------------------------------------------------------------------
# PID defaults (runtime-overridable via CONFIG.INI)
# ---------------------------------------------------------------------------

config PID_KP_DEFAULT
    int "PID proportional gain default (×1000 fixed-point)"
    range 0 1000000
    default 1000
    help
      Proportional gain multiplied by 1000 to avoid float in the
      config INI (1000 = 1.0).  Used by the motor PID controller as
      the compile-time default before CONFIG.INI is loaded.
      Runtime override: CONFIG.INI keys `pid_kp`.
      Current codebase default: 1 (stored as AtomicU32 = 1 = 0.001).

config PID_KI_DEFAULT
    int "PID integral gain default (×1000 fixed-point)"
    range 0 1000000
    default 0
    help
      Integral gain × 1000.  0 = no integral term.
      Runtime override: CONFIG.INI key `pid_ki`.
      Current codebase default: 0.

config PID_KD_DEFAULT
    int "PID derivative gain default (×1000 fixed-point)"
    range 0 1000000
    default 0
    help
      Derivative gain × 1000.  0 = no derivative term.
      Runtime override: CONFIG.INI key `pid_kd`.
      Current codebase default: 0.

config CONTROL_PLANE_ASYNC
    bool "Control plane: stackless async (cooperative) instead of preemptive"
    default n
    help
      Experiment I-12 (RFC-0030). Compile-time CHOICE of how control tasks
      run — a per-binary policy, zero hot-path overhead (the unused branch is
      const-eliminated). Selected here at `make menuconfig` / `make config`;
      surfaces as `robot_os_limits::CONTROL_PLANE_ASYNC`.

      n (default) = PREEMPTIVE: tasks have their own kernel stack and a full
        context switch (save/restore 31 GPRs + FP + CSRs) on every yield.
        Measured `sched.task_yield` ~= 2200 cyc.
      y = ASYNC: control tasks are monomorphised `Future`s (compile-time-sized
        state machines) polled by a table-driven cooperative executor — no
        per-task stack, no register-file save/restore; resuming a task is a
        `poll()` function call. See `asyncrt.poll_resume` bench for the floor.

      EXPERIMENTAL — the async executor is not yet built; today this flag wires
      the seam + the measurement (RFC-0030). Promote when the executor lands
      and the A/B (`task_yield` vs `poll_resume`) is confirmed.

config CONTROL_TXN_TICKS
    bool "Control plane: transactional ticks (rollback recoverable faults)"
    default y
    help
      Experiment I-13 (RFC-0029) — ACCEPTED, default ON. A per-hart checkpoint
      is armed once at the motor control task's entry; a recoverable fault
      (illegal instruction / misaligned load-store) inside the control loop is
      rolled back — motors safe-stop and the task restarts at its entry —
      instead of `panic=abort` halting the kernel mid-motion.

      y (default) = a recoverable fault in the control loop is survivable
        (safe-stop + restart; abort counted in TXN_ABORTS). Verified:
        survived=1, aborts=1, 0 FATAL (RFC-0029 §Results).
      n = the fault is FATAL (legacy: kernel logs + shuts down). The rollback
        branch is const-eliminated → trap handler byte-identical to legacy.

      Surfaces as `robot_os_limits::CONTROL_TXN_TICKS`. No-fault hot path is
      unchanged (the arm runs once per task entry, not per tick; the trap-hook
      check only runs on the rare exception path). Recoverable-cause whitelist
      is conservative (excludes page faults + ecall). Remaining hardening:
      cert review of the trap-handler hook (RFC-0017).

config LEASE_PRIORITY_INHERITANCE
    bool "Leases/caps: donate scheduler CLASS to a blocking lessee (anti-inversion)"
    default n
    help
      Experiment I3 (RFC-0031, capability-aware scheduling). Compile-time
      A/B seam; the unused branch is const-eliminated (zero hot-path cost).
      Surfaces as `robot_os_limits::LEASE_PRIORITY_INHERITANCE`.

      The scheduler is class-first (Adaptive Partitioning picks a class, then
      that class's policy picks a task; priority is only WITHIN a class). So a
      high-class lessor (SafetyCritical/HardRT) blocked in `lease_wait_return`
      on a low-class lessee (BestEffort) suffers cross-class priority inversion
      that a priority-only boost (pi_mutex) cannot fix — any runnable
      mid-class task starves the lessee, so the lease never returns. With
      non-expiring leases (expire_ticks=0) this inversion is unbounded.

      n (default) = no inheritance: the lessor blocks until the lessee is
        eventually scheduled or the lease expires.
      y = donate the blocked lessor's CLASS (not just priority) to the lessee
        until it returns the lease, then restore. Bounds inversion to the
        lessee's critical section.

      EXPERIMENTAL — measured via `i3_lease_inversion_probe` (`[I3]` line,
      `lease_inversion_cyc`), build-time A/B like I2 (RFC-0028). Kill criteria
      live on the UNCONTENDED acquire/release cost + WCET + lost-restore
      correctness on expiry, NOT on the inversion ratio (gross by construction).

endmenu # Robot Configuration
