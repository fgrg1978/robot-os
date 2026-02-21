/// Motor control — port of robot/motor.c
///
/// Controls DC motors via PWM channels and direction GPIO pins.
/// Uses simulated GPIO/PWM from drivers crate.

use super::pid::Pid;

pub const MAX_MOTORS: usize = 4;

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

    // Configure PWM
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
