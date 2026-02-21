//! Dead-reckoning odometry — Phase 17 + G2.
//!
//! Phase G2: uses `encoder::ticks_per_m()` / `encoder::wheel_base_mm()`
//! runtime getters instead of compile-time constants.
//!
//! Gated on `target_pointer_width = "64"` because `AtomicI64` is not
//! lock-free on RV32.

#[cfg(target_pointer_width = "64")]
mod inner {
    use core::sync::atomic::{AtomicI64, Ordering};
    use crate::encoder;

    static DIST_MM:      AtomicI64 = AtomicI64::new(0);
    static HEADING_CDEG: AtomicI64 = AtomicI64::new(0);
    static PREV_L:       AtomicI64 = AtomicI64::new(0);
    static PREV_R:       AtomicI64 = AtomicI64::new(0);

    pub fn odom_update(ticks_l: i64, ticks_r: i64) {
        let prev_l = PREV_L.load(Ordering::Relaxed);
        let prev_r = PREV_R.load(Ordering::Relaxed);
        let dl = ticks_l - prev_l;
        let dr = ticks_r - prev_r;
        PREV_L.store(ticks_l, Ordering::Relaxed);
        PREV_R.store(ticks_r, Ordering::Relaxed);

        let tpm = encoder::ticks_per_m();
        let wb  = encoder::wheel_base_mm();

        let dist_delta = (dl + dr) * 1_000 / (2 * tpm);
        let head_delta = ((dr - dl) as i128 * 36_000_000
            / (tpm as i128 * wb as i128)) as i64;

        DIST_MM.fetch_add(dist_delta, Ordering::Relaxed);
        HEADING_CDEG.fetch_add(head_delta, Ordering::Relaxed);
    }

    pub fn odom_get() -> (i64, i64) {
        (DIST_MM.load(Ordering::Relaxed), HEADING_CDEG.load(Ordering::Relaxed))
    }

    pub fn odom_reset() {
        DIST_MM.store(0, Ordering::Relaxed);
        HEADING_CDEG.store(0, Ordering::Relaxed);
        PREV_L.store(0, Ordering::Relaxed);
        PREV_R.store(0, Ordering::Relaxed);
    }
}

#[cfg(target_pointer_width = "64")]
pub use inner::{odom_update, odom_get, odom_reset};

// RV32 stubs
#[cfg(target_pointer_width = "32")]
pub fn odom_update(_ticks_l: i64, _ticks_r: i64) {}
#[cfg(target_pointer_width = "32")]
pub fn odom_get() -> (i64, i64) { (0, 0) }
#[cfg(target_pointer_width = "32")]
pub fn odom_reset() {}
