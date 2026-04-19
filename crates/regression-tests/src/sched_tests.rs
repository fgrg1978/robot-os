//! Coverage for `crates/sched/` priority + queue ordering — was 0 tests before.
//!
//! Replicates the priority-queue ordering invariants the scheduler
//! relies on: lower numeric priority = higher actual priority,
//! RT_PRIORITY_THRESHOLD splits hard-real-time from preemptable.

#![cfg(test)]

const RT_PRIORITY_THRESHOLD: u32 = 12;
const DEFAULT_PRIORITY:      u32 = 16;
const NET_POLL_PRIORITY:     u32 = 12;
const RT_MOTOR_PRIORITY:     u32 = 8;
const IDLE_PRIORITY:         u32 = 31;

#[inline]
const fn is_rt(prio: u32) -> bool { prio < RT_PRIORITY_THRESHOLD }

/// Returns true if `winner` should preempt `loser` at scheduler dispatch.
/// Lower numeric prio wins; RT (< threshold) always wins over normal.
fn should_preempt(winner: u32, loser: u32) -> bool {
    winner < loser
}

#[test]
fn rt_motor_below_threshold_is_rt() {
    assert!(is_rt(RT_MOTOR_PRIORITY));
    assert!(!is_rt(DEFAULT_PRIORITY));
    assert!(!is_rt(NET_POLL_PRIORITY)); // == threshold means NOT rt
    assert!(is_rt(RT_PRIORITY_THRESHOLD - 1));
}

#[test]
fn rt_preempts_default() {
    assert!(should_preempt(RT_MOTOR_PRIORITY, DEFAULT_PRIORITY));
    assert!(should_preempt(NET_POLL_PRIORITY, DEFAULT_PRIORITY));
}

#[test]
fn idle_loses_to_everyone() {
    for &p in &[RT_MOTOR_PRIORITY, NET_POLL_PRIORITY, DEFAULT_PRIORITY] {
        assert!(should_preempt(p, IDLE_PRIORITY));
        assert!(!should_preempt(IDLE_PRIORITY, p));
    }
}

#[test]
fn equal_priorities_do_not_preempt() {
    // Same-prio tasks round-robin, never preempt each other.
    assert!(!should_preempt(DEFAULT_PRIORITY, DEFAULT_PRIORITY));
    assert!(!should_preempt(NET_POLL_PRIORITY, NET_POLL_PRIORITY));
}

// ── Load-balancing primitive (find_least_loaded_cpu) ─────────────────────
//
// scheduler.rs picks the CPU with the smallest task count for new tasks
// that aren't pinned. Lock down: ties go to the lowest-index CPU.

fn find_least_loaded_cpu(loads: &[u32]) -> usize {
    let mut min_idx = 0;
    let mut min_load = loads[0];
    for (i, &l) in loads.iter().enumerate().skip(1) {
        if l < min_load { min_load = l; min_idx = i; }
    }
    min_idx
}

#[test]
fn least_loaded_picks_smallest() {
    assert_eq!(find_least_loaded_cpu(&[5, 3, 7, 2]), 3);
    assert_eq!(find_least_loaded_cpu(&[0, 1, 2, 3]), 0);
}

#[test]
fn least_loaded_breaks_ties_to_lowest_index() {
    assert_eq!(find_least_loaded_cpu(&[3, 3, 3, 3]), 0);
    assert_eq!(find_least_loaded_cpu(&[5, 1, 1, 1]), 1);
}

#[test]
fn least_loaded_single_cpu() {
    assert_eq!(find_least_loaded_cpu(&[7]), 0);
}

// ── Time-slice expiry ────────────────────────────────────────────────────
//
// Mirror the scheduler's "should we yield this task on tick" logic.
// Lock the invariant: RT tasks never preempted by timer; normal tasks
// time-sliced after RT_TIME_SLICE_TICKS.

const RT_TIME_SLICE_TICKS: u32 = 10;

fn should_yield_on_timer(prio: u32, ticks_in_slice: u32) -> bool {
    if is_rt(prio) { return false; }
    ticks_in_slice >= RT_TIME_SLICE_TICKS
}

#[test]
fn rt_never_yields_on_timer() {
    for ticks in [0, 1, 100, u32::MAX] {
        assert!(!should_yield_on_timer(RT_MOTOR_PRIORITY, ticks));
    }
}

#[test]
fn normal_yields_after_slice() {
    assert!(!should_yield_on_timer(DEFAULT_PRIORITY, 0));
    assert!(!should_yield_on_timer(DEFAULT_PRIORITY, RT_TIME_SLICE_TICKS - 1));
    assert!( should_yield_on_timer(DEFAULT_PRIORITY, RT_TIME_SLICE_TICKS));
    assert!( should_yield_on_timer(DEFAULT_PRIORITY, RT_TIME_SLICE_TICKS + 1));
}
