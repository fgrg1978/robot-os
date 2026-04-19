# SC02 — seL4 IPC research notes

> **Status:** research, not implementation. Pre-requisite for AQ3 (drivers
> in userspace) which is itself post-hardware (Julio 2026+).

## Why seL4 here

When we move drivers out of the kernel into user-space services (AQ3),
the IPC path becomes the most-frequent kernel operation. seL4 has the
fastest, most-formally-verified synchronous IPC of any open OS, so its
patterns are the right baseline before we design our own.

## seL4 IPC primer (pages we care about)

- **Endpoints**: kernel-managed message queues. Senders block on
  `seL4_Send`, receivers on `seL4_Recv`. The kernel hands a
  fixed-size message register (≤ 120 bytes per call on RV64) directly
  between the two threads with **no copy** when both are blocked.
- **Notifications**: a binary semaphore-like signalling primitive,
  cheaper than endpoints when the message is "something happened".
  Useful for ISR → driver-server wakeups.
- **Capability transfer**: senders can attach 1–2 caps (file
  descriptors, frame caps) to a message; receiver inherits them.
- **Direct context switch (DCS)**: when sender's priority ≤
  receiver's, kernel switches directly without going through the
  scheduler. Sub-microsecond on hot path.
- **Fastpath**: hand-written assembly path for the common case
  (synchronous Call/Reply on same-priority threads); ~150 cycles.

## What we'd port to robot-os

| seL4 idea | Our analogue (planned) | Notes |
|-----------|-----------------------|-------|
| Endpoint  | `Channel<T>` (already exists in `crates/ipc`) | Add direct-switch fast path |
| Notification | `signal_init` (exists) | Add edge/level distinction |
| Cap transfer | Add `fd_dup_to_recv` syscall | Needed for FS server |
| Fastpath  | Hand-roll for `SYS_FAST_IPC_CALL` | Hot path on AQ3 |
| Reply caps | Single-shot reply object | Simplifies request-reply RPC |

## Open questions for AQ3 design

1. **Buffer ownership semantics.** seL4 uses zero-copy for the IPC
   register file (≤ 120 B). For our larger driver-data buffers
   (camera frame, packet) we need a separate path. Options:
   - Shared mapping (already done in M03 zerocopy.rs).
   - Lease + flush (M04 lease IPC) — handoff ownership.
   - Decide which is canonical for AQ3 *before* writing the syscall.
2. **Priority inheritance.** seL4 implements PI for endpoints (sender
   donates to receiver during blocking). Robot-os currently does
   not — RT tasks would suffer priority inversion if they wait on a
   default-priority driver server. Need to add PI before AQ3.
3. **Fastpath-eligibility constraints.** seL4 fastpath requires:
   same priority, same time-slice budget, no badge, no caps. We
   should match these and audit which IPC sites can take fastpath.
4. **Verification scope.** We won't formally verify (out of scope),
   but we should run model-checking (Loom or Kani) on the IPC
   state machine.

## References

- seL4 reference manual §4 (IPC) — primary.
- Heiser et al., *seL4: From General Purpose to a Proof of
  Information Flow Enforcement* (S&P 2013).
- Klein et al., *seL4: Formal Verification of an OS Kernel* (SOSP
  2009) — endpoint semantics and proof of IPC correctness.
- L4Re (Genode) endpoint mechanism — pragmatic implementation
  patterns we can copy without porting verification.

## Action items (deferred until AQ3)

1. Sketch a `Channel<T>` fastpath assembly version (RV64).
2. Implement priority inheritance on existing `Channel<T>`.
3. Add `cap_transfer_to_recv` syscall + cap descriptor table.
4. Write a microbenchmark suite to measure IPC RTT before/after
   each optimisation.
5. Document the formal invariants we want to maintain (even if not
   verified): no kernel-stack growth on IPC, bounded blocking time,
   no priority inversion.

This doc is a living research note — update when our understanding
shifts. **It is NOT a green-light to implement AQ3** — that
requires hardware in hand and post-Julio sequencing.
