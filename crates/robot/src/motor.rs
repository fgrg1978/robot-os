/// Motor control — port of robot/motor.c
///
/// Controls DC motors via PWM channels and direction GPIO pins.
/// Uses simulated GPIO/PWM from drivers crate.

use super::pid::Pid;

pub const MAX_MOTORS: usize = 4;

/// PWM period for motor channels. Matches `PwmChannel::new()`'s default
/// `period_ns` (see `drivers::pwm`), so this is an explicit statement of
/// the value QEMU/sim already used implicitly — not a new behavior.
/// On real vf2/k1 hardware the right motor driver PWM frequency is a
/// hardware decision (depends on the motor driver IC); 1 kHz is a common
/// safe default for brushed DC motor drivers but should be confirmed
/// against the actual motor driver datasheet before hardware bring-up.
pub const MOTOR_PWM_PERIOD_NS: u32 = 1_000_000; // 1 kHz

#[derive(Clone, Copy, PartialEq)]
pub enum MotorDir {
    Forward  = 0,
    Backward = 1,
    Brake    = 2,
    Coast    = 3,
}

#[derive(Clone, Copy)]
pub struct Motor {
    pub id:          u32,
    pub pwm_ch:      u32,
    pub dir_pin_a:   u32,
    pub dir_pin_b:   u32,
    pub direction:   MotorDir,
    pub speed_pct:   u32,    // 0-100%
    pub initialized: bool,
    pub pid:         Pid,
}

impl Motor {
    pub const fn new() -> Self {
        Motor {
            id:          0,
            pwm_ch:      0,
            dir_pin_a:   0,
            dir_pin_b:   0,
            direction:   MotorDir::Coast,
            speed_pct:   0,
            initialized: false,
            pid:         Pid::new(),
        }
    }
}

use robot_os_sync::SpinLock;
static MOTORS: SpinLock<[Motor; MAX_MOTORS]> = SpinLock::new([Motor::new(); MAX_MOTORS]);

/// Initialize a motor with its PWM channel and direction GPIO pins.
pub fn motor_init(id: u32, pwm_ch: u32, dir_a: u32, dir_b: u32) -> i32 {
    if id as usize >= MAX_MOTORS { return -1; }

    // Configure GPIO direction pins as outputs
    robot_os_drivers::gpio::gpio_set_direction(dir_a, robot_os_drivers::gpio::GpioDir::Output);
    robot_os_drivers::gpio::gpio_set_direction(dir_b, robot_os_drivers::gpio::GpioDir::Output);

    // Configure PWM. Period MUST be set before duty — on vf2/k1 real
    // hardware `pwm_set_duty_pct` writes the same compare register that
    // holds the period count (see `drivers::pwm` doc comment on
    // `pwm_set_duty_pct`: TRM-blocked register aliasing bug, not fixed
    // here), so at minimum the period must be programmed once up front.
    if robot_os_drivers::pwm::pwm_set_period(pwm_ch, MOTOR_PWM_PERIOD_NS) != 0 {
        robot_os_drivers::kprintln!(
            "[MOTOR] motor {}: pwm_set_period(ch={}) failed", id, pwm_ch
        );
    }
    robot_os_drivers::pwm::pwm_enable(pwm_ch);
    robot_os_drivers::pwm::pwm_set_duty_pct(pwm_ch, 0);

    let mut motors = MOTORS.lock();
    let m = &mut motors[id as usize];
    m.id          = id;
    m.pwm_ch      = pwm_ch;
    m.dir_pin_a   = dir_a;
    m.dir_pin_b   = dir_b;
    m.direction   = MotorDir::Coast;
    m.speed_pct   = 0;
    m.initialized = true;
    0
}

/// Set motor direction and speed (0-100%).
pub fn motor_set(id: u32, dir: MotorDir, speed_pct: u32) -> i32 {
    // Refuse to drive motors after a panic: motor_stop_panic() has already
    // brought them to a safe state and must not be undone by a straggling
    // control path on another hart.
    if robot_os_common::is_panicked() { return -1; }
    if id as usize >= MAX_MOTORS { return -1; }
    let (pwm_ch, dir_a, dir_b) = {
        let m = MOTORS.lock();
        if !m[id as usize].initialized { return -1; }
        (m[id as usize].pwm_ch, m[id as usize].dir_pin_a, m[id as usize].dir_pin_b)
    };

    let speed = speed_pct.min(100);

    match dir {
        MotorDir::Forward  => {
            robot_os_drivers::gpio::gpio_write(dir_a, 1);
            robot_os_drivers::gpio::gpio_write(dir_b, 0);
        }
        MotorDir::Backward => {
            robot_os_drivers::gpio::gpio_write(dir_a, 0);
            robot_os_drivers::gpio::gpio_write(dir_b, 1);
        }
        MotorDir::Brake => {
            robot_os_drivers::gpio::gpio_write(dir_a, 1);
            robot_os_drivers::gpio::gpio_write(dir_b, 1);
        }
        MotorDir::Coast => {
            robot_os_drivers::gpio::gpio_write(dir_a, 0);
            robot_os_drivers::gpio::gpio_write(dir_b, 0);
        }
    }
    robot_os_drivers::pwm::pwm_set_duty_pct(pwm_ch, speed);

    let mut motors = MOTORS.lock();
    let m = &mut motors[id as usize];
    m.direction  = dir;
    m.speed_pct  = speed;
    0
}

/// Stop a motor (coast).
pub fn motor_stop(id: u32) -> i32 {
    motor_set(id, MotorDir::Coast, 0)
}

/// Emergency motor stop for the panic handler — bypasses the `MOTORS`
/// spinlock and calls the lock-free `_panic` GPIO/PWM variants instead
/// of `gpio_write`/`pwm_set_duty_pct`.
///
/// Deliberately sacrifices mutual exclusion, same rationale as
/// `drivers::gpio::gpio_write_panic` / `drivers::pwm::pwm_set_duty_pct_panic`
/// (see their doc comments): if another hart holds `MOTORS` — or the
/// `GPIO`/`PWM` locks reached through the normal `motor_stop` ->
/// `motor_set` path — at the moment of a panic, waiting for any of them
/// would spin forever and the panic message would never reach UART.
/// Getting the actuators to a safe (coast) state and the crash reason
/// printed matters more than leaving `Motor` bookkeeping consistent
/// while the kernel is already crashing. This is why the `Motor` fields
/// are read via `SpinLock::get_mut_unchecked` instead of `.lock()`, and
/// why this function does not write back `direction`/`speed_pct` into
/// `MOTORS` afterward the way `motor_set` does — the kernel is halting
/// or rebooting right after, so there is nothing left to read that state.
///
/// # Safety
/// May race with a concurrent `motor_init`/`motor_set` on another hart
/// for the same `id`, producing a torn read of `Motor` (e.g. a stale
/// `pwm_ch` paired with a fresher `dir_pin_a`). Only call this from the
/// panic handler.
pub fn motor_stop_panic(id: u32) {
    if id as usize >= MAX_MOTORS { return; }

    let (pwm_ch, dir_a, dir_b, initialized) = {
        let motors = unsafe { MOTORS.get_mut_unchecked() };
        let m = &motors[id as usize];
        (m.pwm_ch, m.dir_pin_a, m.dir_pin_b, m.initialized)
    };
    if !initialized { return; }

    // Coast: both direction pins low, 0% duty — mirrors the
    // `MotorDir::Coast` arm of `motor_set` above.
    robot_os_drivers::gpio::gpio_write_panic(dir_a, 0);
    robot_os_drivers::gpio::gpio_write_panic(dir_b, 0);
    robot_os_drivers::pwm::pwm_set_duty_pct_panic(pwm_ch, 0);
}

/// Brake a motor (short-circuit for fast stop).
pub fn motor_brake(id: u32) -> i32 {
    motor_set(id, MotorDir::Brake, 0)
}

/// Print motor status.
pub fn motor_info() {
    let motors = MOTORS.lock();
    for i in 0..MAX_MOTORS {
        let m = &motors[i];
        if m.initialized {
            let dir = match m.direction {
                MotorDir::Forward  => "FWD",
                MotorDir::Backward => "REV",
                MotorDir::Brake    => "BRK",
                MotorDir::Coast    => "CST",
            };
            robot_os_drivers::kprintln!(
                "[MOTOR] Motor {}: pwm_ch={}, dir={}, speed={}%",
                i, m.pwm_ch, dir, m.speed_pct
            );
        }
    }
}
