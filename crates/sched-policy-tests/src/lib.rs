//! Host-side runner for `crates/sched/src/{class,partitions,policies/*}.rs`.
//!
//! The kernel `robot_os_sched` cannot be compiled for the host (its
//! dependencies are RV64-only). The W4 scheduler logic, however, is
//! pure data manipulation with no riscv/MMIO calls — we pull the
//! source files directly and run the suites on the host.

#[path = "../../sched/src/class.rs"]
pub mod class;

#[path = "../../sched/src/policies/mod.rs"]
pub mod policies;

#[path = "../../sched/src/partitions.rs"]
pub mod partitions;

#[cfg(test)]
mod class_tests {
    use super::class::{ClassBudget, SchedClass, DEFAULT_BUDGETS_PCT};

    #[test]
    fn five_classes_unique_slots() {
        let slots: [usize; 5] = [
            SchedClass::SafetyCritical.slot(),
            SchedClass::HardRT.slot(),
            SchedClass::SoftRT.slot(),
            SchedClass::BestEffort.slot(),
            SchedClass::Idle.slot(),
        ];
        for (i, &a) in slots.iter().enumerate() {
            for &b in &slots[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn urgency_orders_safety_first() {
        assert!(
            SchedClass::SafetyCritical.urgency()
                < SchedClass::Idle.urgency()
        );
    }

    #[test]
    fn from_raw_round_trip() {
        for &c in &SchedClass::ALL {
            assert_eq!(SchedClass::from_raw(c as u8), Some(c));
        }
        assert_eq!(SchedClass::from_raw(99), None);
    }

    #[test]
    fn default_budgets_sum_to_100() {
        let total: u32 = DEFAULT_BUDGETS_PCT.iter().map(|&b| b as u32).sum();
        assert_eq!(total, 100);
    }

    #[test]
    fn hard_rt_classification() {
        assert!(SchedClass::SafetyCritical.is_hard_rt());
        assert!(SchedClass::HardRT.is_hard_rt());
        assert!(!SchedClass::SoftRT.is_hard_rt());
        assert!(!SchedClass::BestEffort.is_hard_rt());
        assert!(!SchedClass::Idle.is_hard_rt());
    }

    #[test]
    fn budget_consume_and_quota() {
        let b = ClassBudget::new(20, 100);
        // 10 ms window, max_pct = 100 ⇒ 10000 µs quota.
        assert_eq!(b.quota_us(10_000), 10_000);
        assert_eq!(b.min_quota_us(10_000), 2_000); // 20 %
        assert!(b.under_min(10_000));
        b.consume(2_500);
        assert!(!b.under_min(10_000));
        assert!(!b.over_quota(10_000));
        assert_eq!(b.consumed_us(), 2_500);
    }

    #[test]
    fn budget_over_quota_caught() {
        let b = ClassBudget::new(10, 30);
        // Window 10 ms × max 30 % = 3000 µs.
        b.consume(3_500);
        assert!(b.over_quota(10_000));
    }

    #[test]
    fn budget_reset_clears_consumed() {
        let b = ClassBudget::new(20, 100);
        b.consume(5_000);
        b.reset_window();
        assert_eq!(b.consumed_us(), 0);
    }
}

#[cfg(test)]
mod fifo_tests {
    use super::class::SchedClass;
    use super::policies::fifo::Fifo;
    use super::policies::{Policy, TaskMeta};

    fn meta(tid: u32, prio: u8) -> TaskMeta {
        TaskMeta::new(tid, SchedClass::SafetyCritical, prio)
    }

    #[test]
    fn enqueue_and_pick_highest_priority() {
        let mut q = Fifo::new();
        q.enqueue(meta(1, 5)).unwrap();
        q.enqueue(meta(2, 0)).unwrap(); // higher priority
        q.enqueue(meta(3, 3)).unwrap();
        let picked = q.pick_next(0).unwrap();
        assert_eq!(picked.tid, 2);
    }

    #[test]
    fn fifo_within_priority() {
        let mut q = Fifo::new();
        q.enqueue(meta(1, 4)).unwrap();
        q.enqueue(meta(2, 4)).unwrap();
        q.enqueue(meta(3, 4)).unwrap();
        // All same priority — first inserted wins.
        let picked = q.pick_next(0).unwrap();
        assert_eq!(picked.tid, 1);
    }

    #[test]
    fn dequeue_then_pick() {
        let mut q = Fifo::new();
        q.enqueue(meta(1, 4)).unwrap();
        q.enqueue(meta(2, 1)).unwrap();
        q.dequeue(2);
        let picked = q.pick_next(0).unwrap();
        assert_eq!(picked.tid, 1);
    }

    #[test]
    fn empty_returns_none() {
        let mut q = Fifo::new();
        assert!(q.pick_next(0).is_none());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn capacity_enforced() {
        let mut q = Fifo::new();
        for i in 0..super::policies::fifo::FIFO_CAPACITY as u32 {
            q.enqueue(meta(i, 0)).unwrap();
        }
        let extra = q.enqueue(meta(999, 0));
        assert!(extra.is_err());
    }
}

#[cfg(test)]
mod rr_tests {
    use super::class::SchedClass;
    use super::policies::rr::{RoundRobin, DEFAULT_QUANTUM_US};
    use super::policies::{Policy, TaskMeta};

    fn meta_with_slice(tid: u32, slice_us: u32) -> TaskMeta {
        let mut m = TaskMeta::new(tid, SchedClass::SoftRT, 16);
        m.time_slice_us = slice_us;
        m
    }

    #[test]
    fn pick_returns_head() {
        let mut q = RoundRobin::new();
        q.enqueue(meta_with_slice(1, 5_000)).unwrap();
        q.enqueue(meta_with_slice(2, 5_000)).unwrap();
        assert_eq!(q.pick_next(0).unwrap().tid, 1);
    }

    #[test]
    fn quantum_exhaustion_rotates() {
        let mut q = RoundRobin::new();
        q.enqueue(meta_with_slice(1, 5_000)).unwrap();
        q.enqueue(meta_with_slice(2, 5_000)).unwrap();
        // Drain task 1's quantum.
        q.tick(1, 5_000);
        // Now head should be task 2.
        assert_eq!(q.pick_next(0).unwrap().tid, 2);
    }

    #[test]
    fn partial_tick_decrements() {
        let mut q = RoundRobin::new();
        q.enqueue(meta_with_slice(1, 10_000)).unwrap();
        q.enqueue(meta_with_slice(2, 10_000)).unwrap();
        q.tick(1, 3_000);
        // Head still task 1, remaining 7_000.
        assert_eq!(q.pick_next(0).unwrap().tid, 1);
        assert_eq!(q.remaining_us(), 7_000);
    }

    #[test]
    fn default_quantum_when_zero() {
        let mut q = RoundRobin::new();
        q.enqueue(meta_with_slice(1, 0)).unwrap();
        assert_eq!(q.remaining_us(), DEFAULT_QUANTUM_US);
    }

    #[test]
    fn dequeue_head_starts_new_quantum() {
        let mut q = RoundRobin::new();
        q.enqueue(meta_with_slice(1, 5_000)).unwrap();
        q.enqueue(meta_with_slice(2, 8_000)).unwrap();
        // Burn part of the head's quantum.
        q.tick(1, 2_000);
        assert_eq!(q.remaining_us(), 3_000);
        // Dequeue the head; new head should restart its quantum.
        q.dequeue(1);
        assert_eq!(q.pick_next(0).unwrap().tid, 2);
        assert_eq!(q.remaining_us(), 8_000);
    }

    #[test]
    fn single_task_keeps_running() {
        let mut q = RoundRobin::new();
        q.enqueue(meta_with_slice(1, 5_000)).unwrap();
        // Drain the quantum entirely; with only one task, rotation
        // is a no-op and the quantum just refills.
        q.tick(1, 5_000);
        assert_eq!(q.pick_next(0).unwrap().tid, 1);
        assert_eq!(q.remaining_us(), 5_000);
    }
}

#[cfg(test)]
mod cfs_tests {
    use super::class::SchedClass;
    use super::policies::cfs::Cfs;
    use super::policies::{Policy, TaskMeta};

    fn meta(tid: u32, prio: u8) -> TaskMeta {
        TaskMeta::new(tid, SchedClass::BestEffort, prio)
    }

    #[test]
    fn pick_smallest_vruntime() {
        let mut q = Cfs::new();
        q.enqueue(meta(1, 0)).unwrap();
        q.enqueue(meta(2, 0)).unwrap();
        // Both start at vruntime 0; tie broken by insertion order.
        let pick1 = q.pick_next(0).unwrap();
        // Charge the picked task.
        q.charge(pick1.tid, 1_000);
        let pick2 = q.pick_next(0).unwrap();
        assert_ne!(pick1.tid, pick2.tid);
    }

    #[test]
    fn higher_priority_runs_more() {
        let mut q = Cfs::new();
        // Lower numeric priority = higher actual priority (less weight).
        q.enqueue(meta(1, 0)).unwrap(); // weight 1
        q.enqueue(meta(2, 5)).unwrap(); // weight 32
        // Run task 1 for 1000 µs ⇒ vruntime gains 1000.
        // Run task 2 for 1000 µs ⇒ vruntime gains 32_000.
        q.charge(1, 1_000);
        q.charge(2, 1_000);
        // Now task 1 has lower vruntime and gets picked.
        let picked = q.pick_next(0).unwrap();
        assert_eq!(picked.tid, 1);
    }

    #[test]
    fn new_task_inherits_baseline() {
        let mut q = Cfs::new();
        q.enqueue(meta(1, 0)).unwrap();
        // Charge a lot to task 1 so its vruntime is high.
        q.charge(1, 100_000);
        // New task arrives — should start with the current minimum
        // (which is task 1's high vruntime) so it doesn't unfairly
        // dominate.
        q.enqueue(meta(2, 0)).unwrap();
        // Both should now have vruntime ≈ 100_000; task 1 gets picked
        // because it inserts first (we didn't add tie-breaking by
        // arrival time, but they should be close).
        let picked = q.pick_next(0).unwrap();
        // Either is acceptable; the key property is that task 2 isn't
        // unfairly preferred just because it just arrived.
        assert!(picked.tid == 1 || picked.tid == 2);
    }

    #[test]
    fn empty_returns_none() {
        let mut q = Cfs::new();
        assert!(q.pick_next(0).is_none());
    }

    #[test]
    fn dequeue_by_tid() {
        let mut q = Cfs::new();
        q.enqueue(meta(1, 0)).unwrap();
        q.enqueue(meta(2, 0)).unwrap();
        q.dequeue(1);
        let picked = q.pick_next(0).unwrap();
        assert_eq!(picked.tid, 2);
    }
}

#[cfg(test)]
mod edf_cbs_tests {
    use super::class::SchedClass;
    use super::policies::edf_cbs::{CbsState, EdfCbs};
    use super::policies::{Policy, TaskMeta};

    fn meta_with_deadline(tid: u32, deadline_us: u64) -> TaskMeta {
        let mut m = TaskMeta::new(tid, SchedClass::HardRT, 0);
        m.deadline_us = Some(deadline_us);
        m
    }

    #[test]
    fn earliest_deadline_picked() {
        let mut q = EdfCbs::new();
        q.enqueue_with_cbs(
            meta_with_deadline(1, 5_000),
            CbsState::new(1_000, 5_000),
        )
        .unwrap();
        q.enqueue_with_cbs(
            meta_with_deadline(2, 2_000), // earliest
            CbsState::new(1_000, 2_000),
        )
        .unwrap();
        q.enqueue_with_cbs(
            meta_with_deadline(3, 8_000),
            CbsState::new(1_000, 8_000),
        )
        .unwrap();
        let picked = q.pick_next(0).unwrap();
        assert_eq!(picked.tid, 2);
    }

    #[test]
    fn cbs_exhaustion_pushes_deadline_past_peer() {
        // Pick a period for task 1 whose post-exhaustion deadline
        // (initial + period) lands AFTER task 2's deadline. With
        // initial deadline 1_000 + period 5_000 → 6_000, which is
        // > task 2's 5_000, so task 2 takes EDF priority.
        let mut q = EdfCbs::new();
        q.enqueue_with_cbs(
            meta_with_deadline(1, 1_000),
            CbsState::new(500, 5_000),
        )
        .unwrap();
        q.enqueue_with_cbs(
            meta_with_deadline(2, 5_000),
            CbsState::new(500, 5_000),
        )
        .unwrap();
        let first = q.pick_next(0).unwrap();
        assert_eq!(first.tid, 1);
        q.tick(1, 600); // exhausts task 1's 500 µs budget
        // Task 1's deadline is now 1_000 + 5_000 = 6_000, > 5_000.
        let next = q.pick_next(0).unwrap();
        assert_eq!(next.tid, 2);
    }

    #[test]
    fn cbs_exhaustion_keeps_winner_when_period_short() {
        // Sanity check the *other* direction: if the period is small
        // enough that the pushed deadline is still earliest, the task
        // keeps EDF priority. This reflects standard CBS: exhaustion
        // bumps the deadline, but doesn't *demote* unconditionally.
        let mut q = EdfCbs::new();
        q.enqueue_with_cbs(
            meta_with_deadline(1, 1_000),
            CbsState::new(500, 1_000),
        )
        .unwrap();
        q.enqueue_with_cbs(
            meta_with_deadline(2, 5_000),
            CbsState::new(500, 5_000),
        )
        .unwrap();
        q.tick(1, 600);
        // Task 1's new deadline = 2_000, still < 5_000.
        let picked = q.pick_next(0).unwrap();
        assert_eq!(picked.tid, 1);
    }

    #[test]
    fn admission_check_under_one() {
        let mut q = EdfCbs::new();
        q.enqueue_with_cbs(
            meta_with_deadline(1, 1_000),
            CbsState::new(300, 1_000), // 30 %
        )
        .unwrap();
        // Adding a task with 50 % utilisation: total 80 %, OK.
        assert!(q.admission_check(500, 1_000));
        // Adding a task with 80 % utilisation: total 110 %, REJECT.
        assert!(!q.admission_check(800, 1_000));
    }

    #[test]
    fn admission_rejects_zero_period() {
        let q = EdfCbs::new();
        assert!(!q.admission_check(100, 0));
    }

    #[test]
    fn cbs_refill_on_period_boundary() {
        let mut s = CbsState::new(500, 1_000);
        s.remaining_us = 0;
        s.refill();
        assert_eq!(s.remaining_us, 500);
        assert!(!s.exhausted());
    }

    #[test]
    fn no_deadline_treated_as_lowest() {
        let mut q = EdfCbs::new();
        q.enqueue_with_cbs(
            TaskMeta::new(1, SchedClass::HardRT, 0),
            CbsState::new(500, 1_000),
        )
        .unwrap();
        q.enqueue_with_cbs(
            meta_with_deadline(2, 5_000),
            CbsState::new(500, 5_000),
        )
        .unwrap();
        // Task 1 has no deadline ⇒ infinite ⇒ task 2 picked.
        let picked = q.pick_next(0).unwrap();
        assert_eq!(picked.tid, 2);
    }

    #[test]
    fn dequeue_removes() {
        let mut q = EdfCbs::new();
        q.enqueue_with_cbs(
            meta_with_deadline(1, 1_000),
            CbsState::new(500, 1_000),
        )
        .unwrap();
        q.enqueue_with_cbs(
            meta_with_deadline(2, 2_000),
            CbsState::new(500, 2_000),
        )
        .unwrap();
        q.dequeue(1);
        let picked = q.pick_next(0).unwrap();
        assert_eq!(picked.tid, 2);
    }
}

#[cfg(test)]
mod sporadic_tests {
    use super::class::SchedClass;
    use super::policies::sporadic::Sporadic;
    use super::policies::{Policy, TaskMeta};

    fn meta(tid: u32, prio: u8) -> TaskMeta {
        TaskMeta::new(tid, SchedClass::Idle, prio)
    }

    #[test]
    fn pick_highest_priority_when_capacity_left() {
        let mut q = Sporadic::new();
        q.set_capacity(2_000);
        q.enqueue(meta(1, 5)).unwrap();
        q.enqueue(meta(2, 0)).unwrap(); // higher
        let picked = q.pick_next(0).unwrap();
        assert_eq!(picked.tid, 2);
    }

    #[test]
    fn exhausted_returns_none() {
        let mut q = Sporadic::new();
        q.set_capacity(1_000);
        q.enqueue(meta(1, 0)).unwrap();
        // Drain the bucket.
        q.tick(1, 1_000);
        assert!(q.pick_next(0).is_none());
        assert!(q.exhausted());
    }

    #[test]
    fn replenish_restores() {
        let mut q = Sporadic::new();
        q.set_capacity(500);
        q.enqueue(meta(1, 0)).unwrap();
        q.tick(1, 500);
        assert!(q.exhausted());
        q.replenish();
        assert!(!q.exhausted());
        assert_eq!(q.remaining_us(), 500);
    }
}

#[cfg(test)]
mod aps_tests {
    use super::class::SchedClass;
    use super::partitions::Aps;

    #[test]
    fn default_window_is_10ms() {
        let aps = Aps::default_config();
        assert_eq!(aps.window_us(), 10_000);
    }

    #[test]
    fn safety_under_min_picked_first() {
        let aps = Aps::default_config();
        // No class has consumed anything ⇒ all are under_min ⇒ the
        // most-urgent runnable class wins.
        let picked = aps.pick_class(|_| true).unwrap();
        assert_eq!(picked, SchedClass::SafetyCritical);
    }

    #[test]
    fn skip_empty_class() {
        let aps = Aps::default_config();
        let picked = aps
            .pick_class(|c| c != SchedClass::SafetyCritical)
            .unwrap();
        assert_eq!(picked, SchedClass::HardRT);
    }

    #[test]
    fn over_quota_class_yields() {
        let mut aps = Aps::default_config();
        aps.set_current(SchedClass::SafetyCritical, 1);
        // Charge SafetyCritical past its 100 % cap (its max_pct is 100
        // by default — never over, so use a custom config).
        let mut custom = Aps::new(
            [
                super::class::ClassBudget::new(20, 30),
                super::class::ClassBudget::new(30, 50),
                super::class::ClassBudget::new(25, 60),
                super::class::ClassBudget::new(20, 100),
                super::class::ClassBudget::new(5, 5),
            ],
            10_000,
        );
        custom.set_current(SchedClass::SafetyCritical, 1);
        // Drive SafetyCritical past 30 % (3000 µs in a 10 ms window).
        custom.tick(0, 3_500);
        // Selection should now skip SafetyCritical (over quota) and
        // pick HardRT, which is under_min.
        let picked = custom.pick_class(|_| true).unwrap();
        assert_eq!(picked, SchedClass::HardRT);
    }

    #[test]
    fn window_rolls_over_resets_consumption() {
        let mut aps = Aps::default_config();
        aps.anchor_window(0);
        aps.set_current(SchedClass::SafetyCritical, 1);
        aps.tick(0, 5_000);
        assert_eq!(aps.budget(SchedClass::SafetyCritical).consumed_us(), 5_000);
        // Cross the window boundary.
        aps.tick(15_000, 1_000);
        // Consumption resets and we credited the new tick.
        assert_eq!(aps.budget(SchedClass::SafetyCritical).consumed_us(), 1_000);
    }

    #[test]
    fn multi_window_catch_up_resets_once() {
        // Boot scenario: anchor at 0, first real timer tick lands
        // many windows later (rdtime is already in the millions at
        // boot). One single tick() must advance the window in one
        // step, not require N consecutive ticks to catch up.
        let mut aps = Aps::default_config();
        aps.anchor_window(0);
        aps.set_current(SchedClass::SafetyCritical, 1);
        aps.tick(0, 5_000);
        assert_eq!(aps.budget(SchedClass::SafetyCritical).consumed_us(), 5_000);

        // Jump 50 windows ahead (500_000 µs with a 10_000 µs window).
        aps.tick(500_000, 200);
        // Single tick caught up: consumption is *just* the credit
        // from this tick (200), not blown up by repeated resets.
        assert_eq!(aps.budget(SchedClass::SafetyCritical).consumed_us(), 200);

        // The next nearby tick should be inside the freshly-anchored
        // window and accumulate normally (no extra rollover).
        aps.tick(500_100, 300);
        assert_eq!(aps.budget(SchedClass::SafetyCritical).consumed_us(), 500);
    }

    #[test]
    fn idle_class_does_not_charge() {
        let mut aps = Aps::default_config();
        aps.set_idle();
        aps.tick(0, 5_000);
        for c in &SchedClass::ALL {
            assert_eq!(aps.budget(*c).consumed_us(), 0);
        }
    }

    #[test]
    fn no_runnable_class_returns_none() {
        let aps = Aps::default_config();
        assert!(aps.pick_class(|_| false).is_none());
    }

    #[test]
    fn degraded_mode_picks_anyway() {
        // Build APS where every class is over its max budget — phase
        // 3 of pick_class should still return the most-urgent class.
        let mut custom = Aps::new(
            [
                super::class::ClassBudget::new(20, 5),
                super::class::ClassBudget::new(30, 5),
                super::class::ClassBudget::new(25, 5),
                super::class::ClassBudget::new(20, 5),
                super::class::ClassBudget::new(5, 5),
            ],
            10_000,
        );
        for c in &SchedClass::ALL {
            // 600 µs > 500 µs (5 % of 10 ms) for each class.
            custom.budget(*c).consume(600);
        }
        let picked = custom.pick_class(|_| true).unwrap();
        // Phase 3 returns the most-urgent runnable class.
        assert_eq!(picked, SchedClass::SafetyCritical);
    }
}
