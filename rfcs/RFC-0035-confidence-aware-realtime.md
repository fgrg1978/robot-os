# RFC-0035: Confidence-Aware Real-Time (Confidence-Scaled Motor Envelope)

> **Status:** accepted (design RFC — capability + by-construction cost; no
> measured ratio. AI-native roadmap Fase 3, item #6.)
> **Authors:** Fernando Rodriguez
> **Created:** 2026-06-07
> **Last updated:** 2026-06-07
> **Type:** design (correctness/safety by construction)
> **Builds on:** RFC-0033 (bounded runtime safety monitor — extends its envelope).

---

## Summary

The robot self-limits when the AI is unsure: the brain marks a command
low-confidence and the kernel tightens the motor output envelope for it. This
extends the Fase-1 monitor (RFC-0033) with a confidence input, so an uncertain
reactive-LLM action is physically bounded tighter than a deterministic plan step.
A design capability — "uncertain → cautious" is true by construction; the value
is a safety property (AI uncertainty bounds physical actuation), not a perf ratio.

## Motivation

Not all AI commands are equally trustworthy: a deterministic scripted/plan step
is far more reliable than a single-shot reactive-LLM action off one VLM frame. A
mature OS treats every command identically (it has no notion of the issuer's
confidence). PHANES co-designs the brain and kernel, so the kernel *can* act on
the AI's own uncertainty.

## Design

- **Wire:** `FLAG_LOW_CONFIDENCE = 0x04` on `ActuatorCmd.flags` (a previously
  free bit — no ABI break; brain `protocol.py` + kernel `brain_protocol.rs` in
  sync). `ActuatorCmd::is_low_confidence()`.
- **Brain:** sets the flag on reactive-LLM actions (`server.py`, the
  `policy.from_text` path), never on an emergency stop. Deterministic plan /
  scripted steps leave it clear (high confidence).
- **Kernel:** the rx path records the latest command's confidence
  (`safety::cmd_set_low_confidence`, a global like `estop`). `motor_envelope`
  (the RFC-0033 chokepoint gate) applies a tighter cap —
  `SAFETY_LOW_CONFIDENCE_CAP_PCT = 40` vs the normal `SAFETY_WHEELED_MAX_SPEED_PCT
  = 80` — when the current command is low-confidence. ESTOP still overrides all.

### Why this placement
Reuses the RFC-0033 chokepoint (the single MotorCmd→PWM point), so the
confidence cap is structurally unbypassable, exactly like the base envelope. The
confidence is a global set at ingest and read at the chokepoint — coarse (it
tracks the most recent brain command, not a per-command tag through the
arbiter), which is acceptable and conservative for v1 (documented).

## Cost — by construction

One extra atomic load + a `min` in `motor_envelope`. O(1), no measurement
needed; this is a design capability, not an experiment. "Low confidence → lower
cap" is true by construction — the deliverable is the safety property, not a
ratio. (Consistent with the session's discipline: don't dress a by-construction
behavior as a measured win.)

## Limitations / future

- v1 is a binary low/normal flag. A graded confidence (scale the cap
  continuously) would need a confidence byte on the command (ABI) — deferred.
- Coarse staleness: a low-confidence command keeps the cap low until a
  high-confidence command clears it. Conservative (safe); the watchdog
  safe-stops on comms loss regardless.
- Drone/humanoid envelopes (their own actuation paths) not yet confidence-aware.
- Composes with RFC-0034: a low-confidence *prediction* should likewise gate
  speculative apply by its confidence byte (already carried in `PredictCmd`).

## Results

Capability landed. Kernel 6/6 build configs clean; brain `pytest` green
(`FLAG_LOW_CONFIDENCE` value + round-trip; protocol + speculative suites; full
suite collects with no import breakage). No measured ratio (by construction).
Loop behavior (reactive command → tighter cap on hardware) validated by compile +
unit tests; live-loop confirmation rides with the RFC-0034 / HW harness.
