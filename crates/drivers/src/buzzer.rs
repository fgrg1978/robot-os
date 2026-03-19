/// Buzzer/speaker driver — PWM-based piezo buzzer for audio feedback.
///
/// Drives a piezo buzzer connected to a PWM output pin.
/// Provides tone generation, beep patterns, and startup melody.

use crate::pwm::{pwm_enable, pwm_disable, pwm_set_period, pwm_set_duty_pct};
use crate::clint::get_time;
use robot_os_sync::SpinLock;

// ── Musical note frequencies (Hz) ──────────────────────────────────────────

/// C4 (Middle C)
pub const TONE_C4: u16 = 262;
/// D4
pub const TONE_D4: u16 = 294;
/// E4
pub const TONE_E4: u16 = 330;
/// F4
pub const TONE_F4: u16 = 349;
/// G4
pub const TONE_G4: u16 = 392;
/// A4 (concert pitch)
pub const TONE_A4: u16 = 440;
/// B4
pub const TONE_B4: u16 = 494;
/// C5
pub const TONE_C5: u16 = 523;

// ── System tone frequencies (Hz) ──────────────────────────────────────────

/// Alert/warning tone
pub const TONE_ALERT:   u16 = 2000;
/// Error tone
pub const TONE_ERROR:   u16 = 500;
/// OK / acknowledgment tone
pub const TONE_OK:      u16 = 1000;
/// Startup tone
pub const TONE_STARTUP: u16 = 800;

// ── Timing constants ──────────────────────────────────────────────────────

/// Default PWM duty cycle percentage for buzzer output
pub const BUZZER_DEFAULT_DUTY_PCT: u32 = 50;

/// Short beep duration (milliseconds)
const BEEP_DURATION_MS: u32 = 100;
/// Alert beep duration (milliseconds)
const ALERT_BEEP_MS: u32 = 80;
/// Gap between alert beeps (milliseconds)
const ALERT_GAP_MS: u32 = 60;
/// Number of beeps in an alert pattern
const ALERT_BEEP_COUNT: u32 = 3;
/// Duration of each note in startup melody (milliseconds)
const STARTUP_NOTE_MS: u32 = 120;
/// Gap between startup melody notes (milliseconds)
const STARTUP_GAP_MS: u32 = 40;

/// Nanoseconds per second (for frequency-to-period conversion)
const NS_PER_SEC: u32 = 1_000_000_000;

/// CLINT ticks per millisecond (approximate, ~10 MHz timebase)
const CLINT_TICKS_PER_MS: u64 = 10_000;

// ── Driver state ──────────────────────────────────────────────────────────

struct BuzzerState {
    pwm_channel: u32,
    initialized: bool,
}

impl BuzzerState {
    const fn new() -> Self {
        BuzzerState {
            pwm_channel: 0,
            initialized: false,
        }
    }
}

static BUZZER: SpinLock<BuzzerState> = SpinLock::new(BuzzerState::new());

// ── Internal helpers ──────────────────────────────────────────────────────

/// Blocking delay using CLINT timer.
fn delay_ms(ms: u32) {
    let ticks = ms as u64 * CLINT_TICKS_PER_MS;
    let start = get_time();
    while get_time().wrapping_sub(start) < ticks {}
}

/// Convert frequency in Hz to PWM period in nanoseconds.
fn freq_to_period_ns(freq_hz: u16) -> u32 {
    if freq_hz == 0 { return 0; }
    NS_PER_SEC / freq_hz as u32
}

// ── Public API ────────────────────────────────────────────────────────────

/// Initialize the buzzer on the given PWM channel.
pub fn buzzer_init(pwm_channel: u8) {
    let mut state = BUZZER.lock();
    state.pwm_channel = pwm_channel as u32;
    state.initialized = true;
    // Ensure the channel is off at init
    pwm_disable(state.pwm_channel);
}

/// Play a tone at `freq_hz` for `duration_ms` milliseconds (blocking).
pub fn buzzer_tone(freq_hz: u16, duration_ms: u32) {
    buzzer_on(freq_hz);
    delay_ms(duration_ms);
    buzzer_off();
}

/// Start a continuous tone at the given frequency.
pub fn buzzer_on(freq_hz: u16) {
    let state = BUZZER.lock();
    if !state.initialized { return; }
    let ch = state.pwm_channel;
    drop(state);

    if freq_hz == 0 {
        pwm_disable(ch);
        return;
    }

    let period_ns = freq_to_period_ns(freq_hz);
    pwm_set_period(ch, period_ns);
    pwm_set_duty_pct(ch, BUZZER_DEFAULT_DUTY_PCT);
    pwm_enable(ch);
}

/// Stop any currently playing tone.
pub fn buzzer_off() {
    let state = BUZZER.lock();
    if !state.initialized { return; }
    let ch = state.pwm_channel;
    drop(state);

    pwm_disable(ch);
}

/// Play a short beep at `TONE_OK`.
pub fn buzzer_beep() {
    buzzer_tone(TONE_OK, BEEP_DURATION_MS);
}

/// Play an alert pattern: 3 short beeps at `TONE_ALERT`.
pub fn buzzer_alert() {
    for i in 0..ALERT_BEEP_COUNT {
        buzzer_tone(TONE_ALERT, ALERT_BEEP_MS);
        if i + 1 < ALERT_BEEP_COUNT {
            delay_ms(ALERT_GAP_MS);
        }
    }
}

/// Play a startup melody: ascending C4 - E4 - G4 - C5.
pub fn buzzer_startup() {
    const MELODY: [u16; 4] = [TONE_C4, TONE_E4, TONE_G4, TONE_C5];
    for (i, &note) in MELODY.iter().enumerate() {
        buzzer_tone(note, STARTUP_NOTE_MS);
        if i + 1 < MELODY.len() {
            delay_ms(STARTUP_GAP_MS);
        }
    }
}
