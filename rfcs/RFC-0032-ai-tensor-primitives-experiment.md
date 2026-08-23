# RFC-0032: AI-Tensor Primitives (Experiment)

> **Status:** deferred (I6 — no gross win on the honest emulation substrate;
> integer-only inference payoff is on FPU-less hardware → revisit at VF2/K1
> bring-up, 2026-07). **Not deleted: the measured negative result is the artifact.**
> **Authors:** Fernando Rodriguez
> **Created:** 2026-06-03
> **Last updated:** 2026-06-03
> **Type:** experiment (`_template_experiment.md` shape)
> **Companion:** Phase B item I6 (AI-tensor primitives)

---

## Summary

Phase B item I6 asked whether a faster AI-tensor primitive (vs the existing
`crates/ml` int8 path) is a measurable win. After a baseline cost-breakdown
measurement, the answer on the **default (hardfloat) QEMU substrate is: no gross
win exists.** I6 is categorically a 1.2–1.4× optimization study, not an 8–10×
experiment like I2/I3. The honest move is to defer to real hardware rather than
manufacture a benchmark. This RFC records the measurement and the reasoning so a
future "let's optimize the tensor path" proposal starts from data, not from
scratch.

## Hypothesis (as scoped)

**Claim (intended):** an AI-tensor primitive reduces `conv2d_int8` cost by a
gross factor (≥ 5×), measured as a cycle ratio on a one-shot probe (like I2/I3).

## What we measured (baseline breakdown)

A qemu-gated probe ran `conv2d_int8` at a realistic layer (in_c=16, 32×32,
out_c=16, 3×3, stride 1, pad 1 → 16384 outputs, 144 MACs/output) and split the
cost between the i32 MAC loop and the f32 requantize tail:

```
[I6] full_cyc=96,695,000  requant_cyc=22,851,000 (~24%)  mac_cyc~=73,844,000 (~76%)
```

This already overturned the armchair estimate (requant assumed ~1%, measured
~24%) — **measure, don't assume.** But the 24% is against a **degenerate
baseline**, which is the decisive finding:

### Degenerate-baseline finding (real, verifiable)

`conv2d_int8` computes `real_val = acc as f32 * output_params.scale`, then
`QuantParams::quantize` computes `val / self.scale + zero_point`. The `scale`
**cancels** — the float requant effectively computes `relu(acc) + zero_point`
via a redundant float-multiply + float-**divide** per output element. The 24%
is the cost of *redundant float ops* (the divide dominates: ~1394 cyc/output),
NOT the cost of a correct requant. So:

- Measuring an integer requant against *this* baseline benchmarks a strawman
  (the I4 tautology, one level down).
- Against a *correct, non-redundant* float requant the integer win shrinks well
  below 24% (one mul + two int↔float casts remain, no divide).

## Why deferred (not accepted, not a tautology)

- **MAC loop (76%)** is the only place a non-degenerate win lives (bounds-check
  hoisting removes per-tap branch work). Estimated modest (1.2–1.4×), not gross,
  and on this substrate undistinguishable from microbench noise at n=1.
- The only "gross" framing was integer-only requant measured on a **softfloat**
  config — rejected as manufacturing the benchmark (choosing a non-default
  config to inflate the ~1% tail).
- Integer-only inference IS the right edge-AI technique, but its payoff is on
  **FPU-less / SIMD hardware** (VF2/K1), where QEMU TCG cannot model the gain
  (it inflates emulated cycles and does not represent real vector throughput).

## Results

**Verdict: `deferred`.** No gross win on honest silicon-emulation. Revisit at
hardware bring-up (2026-07) where (a) `rdcycle` is a true counter and (b) the
FPU-less / SIMD costs that make integer-only inference and SWAR MACs worthwhile
actually exist. Probe removed (no dead code); the measurement lives here.

**What we learned:**
1. Always measure the cost breakdown before picking an optimization target — the
   1%→24% correction proves the point (cf. RFC-0027 I1).
2. A 24% number against broken code is worse than no number; verify the baseline
   is correct before optimizing against it.
3. The compute-microbench substrate (QEMU TCG) cannot host a faithful tensor
   speedup experiment — the wins live in HW features TCG does not model. This is
   the same substrate ceiling that bounded RFC-0027 (I1).

## Follow-ups (separate from this experiment)

- **Cleanup (not an experiment):** `conv2d_int8`'s requant has a redundant
  float mul/divide (`scale` cancels) and is arguably not doing correct per-layer
  rescaling. Worth fixing as plain code hygiene + a correctness review of the
  int8 requant math — tracked separately, not gated by this RFC.
- Re-open I6 at HW bring-up with: bounds-hoisted MAC, SWAR/packed int8 MAC, and
  a *correct* integer-only requant, measured on VF2/K1.
