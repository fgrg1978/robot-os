//! IO-wait and wake primitives (AQ0).
//!
//! Provides `task_block(reason)` to put the current task to sleep and
//! `wake_*()` functions to unblock tasks when their wait condition is met.

use crate::task::{WaitReason, MAX_TASKS};
use crate::smp::current_cpu_id;
use crate::scheduler;

/// Block the current task until `reason` is satisfied.
///
/// The task is removed from the ready queue and marked `Blocked`.
/// Another task is scheduled immediately. This function "returns"
/// when the task is woken up by a matching `wake_*()` call.
pub fn task_block(reason: WaitReason) {
    let cpu = current_cpu_id();
    scheduler::block_current(cpu, reason);
}

/// Wake all tasks blocked on a specific IRQ.
pub fn wake_by_irq(irq: u32) {
    wake_matching(|r| matches!(r, WaitReason::Irq(i) if *i == irq));
}

/// Wake all tasks blocked on a specific channel.
pub fn wake_by_channel(handle: u32) {
    wake_matching(|r| matches!(r, WaitReason::Channel(h) if *h == handle));
}

/// Wake all tasks blocked on a specific ring buffer.
pub fn wake_by_ring(ring_id: u32) {
    wake_matching(|r| matches!(r, WaitReason::Ring(id) if *id == ring_id));
}

/// Wake all tasks blocked on a specific port.
pub fn wake_by_port(port_id: u32) {
    wake_matching(|r| matches!(r, WaitReason::Port(id) if *id == port_id));
}

/// Wake all tasks whose timer deadline has expired.
pub fn wake_expired_timers(now_ticks: u64) {
    wake_matching(|r| matches!(r, WaitReason::Timer(deadline) if now_ticks >= *deadline));
}

/// Wake a task blocked on an RPC reply (matched by caller TID).
pub fn wake_by_rpc(caller_tid: u32) {
    wake_matching(|r| matches!(r, WaitReason::Rpc(tid) if *tid == caller_tid));
}

/// Wake the server task blocked waiting for a fast IPC call (by server TID).
pub fn wake_fast_ipc_server(server_tid: u32) {
    wake_matching(|r| matches!(r, WaitReason::FastIpcServer(tid) if *tid == server_tid));
}

/// Wake the client task blocked waiting for a fast IPC reply (by slot index).
pub fn wake_fast_ipc_client(slot_idx: u32) {
    wake_matching(|r| matches!(r, WaitReason::FastIpcClient(s) if *s == slot_idx));
}

/// Internal: scan all tasks and wake those matching the predicate.
fn wake_matching(pred: impl Fn(&WaitReason) -> bool) {
    for i in 0..MAX_TASKS {
        scheduler::try_wake_task(i, &pred);
    }
}
