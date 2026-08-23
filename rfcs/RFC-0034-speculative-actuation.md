# RFC-0034: Speculative Actuation — Predictive Brain→Kernel Channel

> **Status:** accepted (capability v1 — the predictive channel; the speculative
> APPLY/benefit is HW-deferred, see §Results). AI-native roadmap Fase 2.
> **Authors:** Fernando Rodriguez
> **Created:** 2026-06-04
> **Last updated:** 2026-06-04
> **Type:** design + capability (measured benefit deferred to hardware)
> **Builds on:** RFC-0033 (Fase-1 safety envelope — gates speculative output),
> RFC-0029 (I-13 transactional ticks — see §"Why not I-13").

---

## Summary

"Branch prediction for physical actuation": the brain sends its predicted NEXT
actuator command (+ confidence) so the kernel can act on it ahead of the
confirmed command, bounded by the Fase-1 safety envelope. This RFC lands
**capability v1 — the predictive channel** (a new `PKT_PREDICT` packet, brain
emitter, kernel receiver). The speculative APPLY (acting early) is where the
latency benefit lives; that benefit is only honestly measurable on real
hardware, so the apply policy is specified here and deferred (gated, default off).

## Motivation

The dominant latency in an AI robot is `world → brain inference → command →
actuation`. A mature OS never acts before a confirmed command — sound, but it
leaves that latency unhidden. PHANES can break that dogma *safely* because three
co-designed pieces exist: an AI predictor (the brain), a safety envelope that
bounds any speculative motion (RFC-0033), and a double-buffered motor path so a
wrong guess simply isn't published.

## What landed (capability v1 — the channel)

- **Protocol `PKT_PREDICT = 0x89`** (brain↔kernel, kept in sync manually):
  payload = `ActuatorCmd` bytes + 1 confidence byte (0..=255).
  - Kernel: `brain_protocol.rs` `PredictCmd` + `decode_predict_cmd` (`split_at(len-1)`).
  - Brain: `protocol.py` `PredictCmd` (`to_bytes`/`from_bytes`).
- **Brain emitter** (real prediction source, not invented): `planner/speculative.py`
  `predict_next(plan, step, policy, is_scripted)` translates the *next committed
  plan step* through the same policy (scripted mode → confidence 255; LLM-planned
  → 230). `SkillRunner` emits it after each step via an optional `send_predict`
  (backward-compatible: `None` → no predictions). `server._send_predict_cmd`.
- **Kernel receiver**: both rx paths (TCP + UART) decode `PKT_PREDICT` and log
  the predicted command — observable proof the channel works.

## Why NOT I-13 for rollback (corrected from the Fase-2 sketch)

The original Fase-2 idea said "mispredictions roll back via I-13". Investigation
showed that's wrong: I-13 (`txn_try_rollback`) is **strictly trap-driven** — it
needs a real `TrapFrame` and only restarts the whole task at its entry. There is
no voluntary/programmatic rollback API, and reusing it for speculation would need
a new path + a full lock-discipline re-audit. **We don't need it:** the motor
path is already double-buffered (commands staged during the tick, published once
at tick end), so a mispredicted/unconfirmed command is simply *never published*.
That — plus the RFC-0033 envelope — is the safety mechanism, not I-13.

## The APPLY policy (specified, deferred to hardware)

When `SPECULATIVE_ACTUATION` (a future Kconfig, default off) is enabled, and a
fresh prediction with confidence ≥ threshold exists, and no confirmed command
has arrived this cycle: apply the predicted command early **through
`motor_envelope`** (Fase-1) so it can never exceed the safe envelope; the next
confirmed command supersedes it. Safe by construction: envelope-bounded,
default-off, real-command-wins.

**Conservative variant rejected (measured reasoning):** pre-computing the PID
output for the prediction is not worth it — the PID compute is ~µs (not the
bottleneck; the bottleneck is brain→command, ms+), and a speculative PID tick
corrupts the shared PID integrator (would need a shadow controller). Optimising
it would repeat the RFC-0032 (I6) trap of optimising a non-bottleneck.

## Results

**Verdict: capability v1 accepted; benefit deferred to HW.**

- **Channel:** landed and verified — kernel 6/6 build configs clean; brain
  `pytest` clean (PredictCmd round-trip + wire-layout-matches-kernel +
  `predict_next`; executor + protocol suites green; full suite collects with no
  import breakage). Backward-compatible (predictions off unless `send_predict`
  wired / `SPECULATIVE_ACTUATION` on).
- **Benefit (latency hidden by speculation):** NOT measured. It is `world →
  responds` loop timing, which lives on the noisy SMP+net QEMU path; injecting a
  synthetic brain latency and "measuring" we hid it would be manufacturing the
  result (the RFC-0027/RFC-0032 substrate ceiling again). Deferred to VF2/K1
  bring-up (2026-07) with a real brain + link, where `rdcycle` is a true counter.
- **Not boot-tested here:** the rx path lives in `behavior_task` (hart-2-affined)
  + needs net + a brain peer, so the predictive loop can't run in a 1-hart QEMU
  boot. Validated by compile + unit tests; loop validation deferred to the
  full-SMP `qemu-full-smp` + stub-brain harness / HW.

## Follow-ups

- Kconfig `SPECULATIVE_ACTUATION` + the envelope-gated early-apply wiring (the
  benefit layer), measured on HW.
- Top-k predictions (v1 is top-1); confidence-threshold tuning.
- `qemu-full-smp` + stub-brain loop test emitting/consuming `PKT_PREDICT`.
