# RFC-0027: Per-Function WCET Budget Annotations (Experiment)

> **Status:** rejected (KILL2 — variance floor too high on QEMU substrate; see §Results). I1.1-I1.4 infra landed and works; the per-function CI **gate** is the part that failed. Fate of the macro/build-script infra (revert vs. retain as opt-in telemetry) is a separate decision, pending.  
> **Authors:** Fernando Rodriguez
> **Created:** 2026-05-28
> **Last updated:** 2026-05-28
> **Type:** experiment (uses `rfcs/_template_experiment.md` shape)
> **Supersedes:** —
> **Superseded by:** —

## Summary

Add a `#[wcet(N_us)]` attribute macro that, when applied to a Rust
function, automatically wraps the body in `wcet_begin()` /
`wcet_end()` instrumentation and emits the declared budget into a
generated `wcet_bounds[]` table.  The bench harness already captures
the runtime measurements; CI gates per-function maxima against
declared budgets.

This is Phase B item I1.  First apuesta arriesgada following Phase A's
measurement discipline.  Lower risk than later experiments because it
builds on the existing WCET framework (`crates/drivers/src/wcet.rs`)
which already implements the measurement primitives — we only add the
attribute-macro shell and the per-function attribution.

## Hypothesis

- **Claim**: Declaring per-function WCET budgets as `#[wcet(N_us)]`
  attribute macros, combined with CI gating on per-function maxima
  vs. declared budgets, catches at least **one latent WCET regression
  per quarter** that the existing 5 fixed-point framework misses.
- **Primary metric**: count of WCET-bound violations surfaced per CI
  run.  Reported in `bench/results/<sha>.json` as a new top-level key
  `wcet_per_fn.<function_name>.{min, max, avg, p99, violations,
  declared_bound_us}`.
- **Baseline number**: today, **0 per-function WCET points** are
  captured in `bench/results/`.  The existing 5 fixed points exist in
  `crates/drivers/src/wcet.rs:33-49` (`WCET_PID_LOOP`,
  `WCET_SENSOR_READ`, `WCET_CTX_SWITCH`, `WCET_TIMER_ISR`,
  `WCET_ACTUATOR_WRITE`) but only `timer_isr` is exercised by the
  current bench scenarios; the JSON at SHA `5e61db3c9fa7` shows
  `wcet_us.timer_isr.max = 1051 µs` and no other points populated.
- **Target number**: **5-10 annotated hot-path functions** all
  reporting bounded measurements in JSON within 8 weeks of merge.  At
  least 1 deliberate regression caught during acceptance testing.
- **Confidence**: medium.  The WCET framework already works; the
  attribute macro is mostly ergonomics.  Risk concentrates on (a)
  whether attribute expansion defeats inlining and shifts baselines,
  and (b) whether per-function variance under QEMU TCG noise stays
  below the ≥5% threshold our CI gate uses.
- **Time horizon**: **6-8 weeks** from merge.  4 weeks of soak across
  20+ commits to characterise noise; 2-4 weeks to plant a deliberate
  regression and verify the gate catches it.

## What would make this fail

Explicit kill criteria.  Hit any of these → status flips to
`rejected`, code reverted, negative result published in §Results.

1. **Inlining defeat**: any of `rtt_ms.p99`, `throughput.steady_msgs_per_s`,
   or `boot_ms` regresses by ≥5% in `bench/results/*.json` after the
   attribute is added to ≥3 functions on the hot path.  The attribute
   must be cost-free in the steady state, or we kill it.
2. **Variance floor too high**: any annotated function shows
   `(max - min) / avg > 30%` across the 3 bench runs.  If we can't
   distinguish a real regression from QEMU TCG flakiness for a
   function, the per-function bound has no informational value.
3. **No real bug found**: after the 8-week horizon, the only failures
   the gate has caught are noise / waivers.  No real latent regression
   surfaced.  This means the framework is decoration, not a tool.
4. **Footprint blow-up**: `footprint.text_bytes` regresses by ≥10%
   from the per-function wrappers (each `wcet_begin/wcet_end` call is
   ~12 instructions; if we instrument >50 functions and `.text` grows
   accordingly, the cost outweighs the benefit).
5. **CI false positives**: if the per-function gate fires on >20% of
   PRs without a real underlying regression (mostly noise crossings),
   developers will start waiving the gate by reflex.  The gate must be
   high signal — a CI gate that's ignored is worse than no gate.

## Exit criteria

### Success path
- ≥ 5 functions annotated and bound, all with `(max - min) / avg ≤ 20%`
  across 3 bench runs after 4 weeks of soak.
- At least 1 *real* regression caught by the per-function gate that the
  existing 5 fixed-points missed.  Documented in §Results.
- No primary-bench-metric (`rtt`, `throughput`, `boot`, `footprint`)
  regressed >5% from the baseline.

**On success**: status → `accepted`.  `bench/baselines.json` extended
with per-function bound declarations as the new normal.  Document the
pattern + link to the macro source in `docs/CONFIG.md`.

### Failure path
- Any of the kill criteria above fires.
- OR: 8 weeks pass with no real regression caught (the gate didn't
  prove its worth, even if it didn't actively hurt).

**On failure**: status → `rejected`.  Revert the attribute macro and
the gate code.  Document what we learned in §Results — including
*specifically* what the gate did NOT catch that we hoped it would.
That negative result is the value of running the experiment.

## Architectural risk if it succeeds

Hidden costs of a "win" we have to enumerate up-front:

1. **API surface growth**: every kernel hot-path function gains an
   attribute, making the API surface visually noisier.  Mitigation:
   group attributes under a `#[cfg(feature = "wcet")]` so production
   builds without WCET instrumentation strip them entirely.
2. **Build complexity**: proc-macro expansion adds compile-time work.
   Measurement: time `cargo build --release --features qemu` before
   and after; budget ≤10% slow-down.
3. **WCET point ID exhaustion**: today `WCET_MAX_POINTS = 16` in
   `crates/drivers/src/wcet.rs:30`.  Per-function attribution
   trivially exceeds 16.  Need to lift to dynamic via a build-script
   that counts annotated functions, OR (preferred) bump
   `WCET_MAX_POINTS` to 64 / 128 with the same `pub use
   robot_os_limits::WCET_MAX_POINTS` source-of-truth pattern RFC-0026
   established.  This is a `crates/limits/Kconfig.timing` addition.
4. **Cap-table coupling**: this experiment is independent of
   `CapKind` ABI (RFC-0025 follow-up) — does NOT touch it.
5. **Brain-side noise**: brain pytest baseline must stay at **1279
   passing** post-merge.  The bridge tests in
   `test_kernel_consts_bridge.py` reference `KERNEL_*` constants;
   adding new per-function constants on the kernel side won't break
   them (the bridge is selective per RFC-0026 §"brain-side mirror").

## Detailed design

### `#[wcet(...)]` attribute macro

New crate `crates/wcet-macro/` (proc-macro, host-only).  Same
exclusion pattern as `crates/phanes-config/` and `crates/limits/`'s
build deps.

Usage:

```rust
use robot_os_drivers::wcet;

#[wcet(50_us)]
pub fn motor_pid_step(motor: &mut Motor, dt: u32) -> i16 {
    // ... body unchanged ...
}
```

Expansion:

```rust
pub fn motor_pid_step(motor: &mut Motor, dt: u32) -> i16 {
    let __wcet_start = robot_os_drivers::wcet::wcet_begin();
    let __wcet_point = robot_os_drivers::wcet::POINT_MOTOR_PID_STEP;
    let __result = {
        // ... original body ...
    };
    robot_os_drivers::wcet::wcet_end(__wcet_point, __wcet_start);
    __result
}
```

The point ID is allocated at compile time by a build script that
walks the source for `#[wcet(...)]` annotations and writes a
generated `pub const POINT_<UPPER_NAME>: u8 = N;` table into
`crates/drivers/src/wcet_points_generated.rs`.  Sorted by function
name for stable IDs across rebuilds — relevant because
`bench/baselines.json` references these IDs.

#### Host-build compatibility

Some annotated files are also pulled into host-test crates via
`#[path = "../../behavior/src/brain_protocol.rs"]` (the pattern
`regression-tests/src/property.rs` uses to exercise the exact
on-the-wire parser source).  Those host crates do NOT depend on
`robot_os_drivers`, so the bare expansion would emit unresolved
`::robot_os_drivers::wcet::*` paths.

Fix: the macro gates its call sites with `cfg(target_os = "none")`.
All kernel targets (riscv64 / aarch64 / x86_64 baremetal) carry
`target_os = "none"`; host targets are `macos` / `linux`.  Under
host build the instrumentation disappears and the closure body
runs unmeasured.  The host-test crate only needs `wcet-macro` as a
dev-dependency to satisfy the `use wcet_macro::wcet;` import —
no runtime crate is pulled in.

This unblocks annotating `brain_protocol::parse_packet`, which
was skipped in I1.3 specifically because of this host-build
constraint.  After landing the gate, parse_packet is point id 12
(`brain_protocol_parse_packet`) with a 50 µs budget.

### Wiring to bench harness

`tools/bench_e2e_collect.py` already parses `[WCET]` lines from the
kernel log.  Today it only matches the 5 fixed names.  Extend the
regex to capture any name registered in `wcet_points_generated.rs`
(the build script ALSO emits a JSON-formatted index at
`crates/drivers/wcet_points.json` that the collector reads to
configure its matcher).

The JSON output schema grows a new top-level `wcet_per_fn` dict
(parallel to existing `wcet_us`), keyed by function name.

### Auto-report under QEMU (I1.5 reliability fix)

The bench harness injects `wcet\r\n` into the kernel shell after a
25 s shell-wait.  Under QEMU TCG SMP-4 the UART IRQ can be routed
to a hart that is mid-translate, dropping the keystroke and
yielding zero `[WCET]` lines for the entire run.  The fixed-point
fallback `[ISR-WCET]` probes cover only timer_isr sub-parts —
the 6 annotated functions would never emit samples this way.

Solution: `kernel/src/main.rs::system_wdt_task` now auto-fires
`wcet_report()` + `jitter_report()` every
`WCET_AUTOREPORT_WDT_ITERS = 60` watchdog iterations
(≈ 30 s at 500 ms tick).  Gated `#[cfg(feature = "qemu")]` —
on real hardware the shell works reliably and the operator can
dump on demand.

A 40 s steady scenario therefore sees at least one report;
collector `parse_wcet` is name-agnostic and `_split_wcet` routes
each row by name into `wcet_us` (fixed) or `wcet_per_fn` (gen).

### CI gate

`tools/bench_compare.py` already handles arbitrary metric paths via
`iter_leaves()`.  The new metrics under `wcet_per_fn.*` get
direction-tagged as "smaller-better" automatically (suffix `.max` /
`.avg` / `.p99` is recognised).  `bench/baselines.json` grows
declared-bound entries; the comparator gates on these.

## Implementation plan with measurement checkpoints

| Phase | Work | Measurement gate | Effort |
|-------|------|-------------------|--------|
| **I1.1** Macro skeleton | New `crates/wcet-macro/` with empty no-op attribute.  Apply to 1 function (`motor_pid_step`).  Build all 6 configs. | All bench primary metrics within 2% of baseline.  No new bugs. | ~2 days |
| **I1.2** Generated point table | Build script enumerates annotations.  Emit `wcet_points_generated.rs` + `wcet_points.json`.  Bump `WCET_MAX_POINTS` via Kconfig. | 1 annotation visible in `bench/results/<sha>.json` under `wcet_per_fn.motor_pid_step`. | ~3 days |
| **I1.3** Annotate 5-10 hot paths | `motor_pid_step`, `tcp_send_segment`, `parse_packet`, `arbitrate` (subsumption), `auth_envelope::wrap`, `scheduler::pick_next`, `cap_check`. | All show min/max in JSON within 30% spread across 3 runs.  Build size growth ≤5%. | ~1 week |
| **I1.4** Wire CI gate | `bench/baselines.json` extended with declared bounds (initial values = 1.2 × observed mean).  `bench_compare.py` validation suite extended. | A planted regression (e.g. add a 10 µs `core::hint::black_box` loop to `motor_pid_step`) triggers a CI failure. | ~3 days |
| **I1.5** Soak | 4 weeks of CI runs.  Track false-positive rate, true-regression catches. | Surfaced in this RFC's §Results. | 4 weeks |

Total active effort: **~2 weeks**.  Calendar: **6-8 weeks** with the
soak window.

## Drawbacks

- Macro expansion adds ~12 instructions per annotated function call.
  Tolerable for hot paths called <10 kHz; bad for tight inner loops.
- Inlining the attribute might be defeated by the function-wrapping
  pattern.  Mitigation: macro emits `#[inline(always)]` if the
  source function already had it.
- Tooling surface grows by 1 proc-macro crate + 1 build script.
  Worth it only if §Results justifies it.
- We learn whether the macro adds value only AFTER the 8-week soak.
  This experiment is slow to confirm OR refute.

## Alternatives

**A — Do nothing** (the null hypothesis).  Existing 5 fixed points
cover the most critical paths today.  Maintenance is zero.  But: no
per-function attribution, no way to surface a regression in a
specific function vs. the aggregate; the 5 points are coarse-grained.
Baseline cost: status quo.

**B — Manual `wcet_begin()/wcet_end()` per hot path, no macro.**
Less ergonomic; developers will forget; harder to enforce.

**C — Static analysis only**: compile-time call-graph walker that
sums declared bounds along call chains.  Rejected for the first
experiment because it's substantially more work (~3-4 weeks just for
the analyzer), and we don't yet know whether the runtime measurements
even justify investing in static analysis.  If I1 succeeds, this is
the natural follow-up — RFC-0028.

**D — Per-function HW perf counters**: use RISC-V `mhpmcounter*` to
capture per-function cycles directly without the runtime
instrumentation.  Requires M-mode privilege management and is not
portable across the 3 ISAs.  Deferred to post-hardware experimentation.

## Unresolved questions

1. **Bound declaration units**: `#[wcet(50_us)]` vs `#[wcet(50_cycles)]`
   vs `#[wcet_budget("50us")]`.  Microseconds for human readability,
   but cycles are what `rdcycle` returns.  Proposal: microseconds in
   the attribute, conversion handled by the macro using the
   compile-time `TIMER_FREQ` from `robot_os_limits`.
2. **Cross-ISA portability**: cycles measurement differs per ISA
   (`rdcycle` on riscv64, `pmccntr_el0` on aarch64, `rdtsc` on
   x86_64).  Today `wcet_begin` already abstracts this in
   `crates/drivers/src/wcet.rs`.  Macro stays portable; the platform
   layer does the right thing.
3. **Disabled in production?**  `#[cfg(feature = "wcet")]` gating
   would strip the wrappers entirely.  Open question whether to
   default-on or default-off.  Proposal: default-on (matches the
   current `WCET` framework's behaviour), opt-out via `--features
   no-wcet`.

## Results

**Verdict: `rejected`.** Kill criterion #2 (variance floor too high) fires
decisively. The macro and build-script infrastructure (I1.1-I1.4) work as
designed; what failed is the experiment's actual claim — a per-function CI
gate with *informational value* — on the only measurement substrate
available today (QEMU TCG SMP).

- **Verdict**: `rejected` (per the §"Failure path" pre-registered action
  for KILL2).
- **Data**: 10 `bench/results/*.json` runs, 2026-05-28 → 2026-06-02, all
  `defconfig=qemu`, SMP-4. 11 functions annotated (target was 5-10):
  `brain_protocol_parse_packet`, `auth_envelope_wrap`/`_unwrap`,
  `arbiter_arbitrate`, `arp_lookup`, `ip_send`, `tcp_send_segment`,
  `scheduler_schedule`, `cap_get`, `channel_send`/`_recv`.

- **KILL2 — per-function `(max - min) / avg`** (the clean, fixed-binary
  evidence): **0 of 11 functions** fall within the 30% kill threshold;
  success required ≥5 within 20%. The *within-run* sample spread (a single
  fixed binary, so this is pure measurement noise — not code change)
  ranges from **347%** (`tcp_send_segment`) to **298,809%**
  (`scheduler_schedule`). Representative absolute samples expose the
  artefact directly: `arp_lookup` min=0 µs / max=36,800 µs; an ARP cache
  lookup does not take 36 ms. The numbers are not timing — they are noise.

- **Root cause (verified, not assumed — see `crates/drivers/src/wcet.rs:76-99`,
  `:161-175`)**: `wcet_begin/end` read `rdcycle`, which is cycle-accurate
  *on hardware*. But under QEMU `-smp 4`, TCG single-threads all four
  virtual harts onto one host thread. While the measured hart is between
  `wcet_begin` and `wcet_end`, the emulator time-slices to the other three
  harts and `rdcycle` keeps advancing — so a function's "elapsed cycles"
  silently includes unrelated cross-hart work. The kernel authors already
  knew this and disabled *bound enforcement* under `feature="qemu"`
  (`WCET_BOUND_* = 0`) to suppress `[WCET] VIOLATION` spam — but the bench
  collector still records the inflated samples, and `bench_compare.py`
  gates `wcet_per_fn.*.max` against `baselines.json` using exactly those
  inflated samples. The gate therefore compares noise to noise.

- **Not evaluable from this data (stated honestly)**: KILL1 (inlining
  defeat) and KILL4 (footprint blow-up) are *before/after-the-attribute*
  comparisons on a *fixed* SHA. The soak runs span different commits, so
  cross-run deltas in `rtt_p99`, `throughput`, and `text_bytes` conflate
  real code changes with noise and cannot be cited as evidence either for
  or against those criteria. KILL2 is sufficient on its own.

- **Real regressions caught**: 0. Not because none occurred, but because
  the gate cannot resolve a regression against 100-1000× substrate noise.
  The success criterion "≥1 real regression caught that the 5 fixed points
  missed" is unmet and — given the substrate — unmeetable as specified.

- **What we learned**:
  1. **The blocker is the shared measurement substrate, not the macro.**
     Real per-function WCET needs the cycle counter to measure *on-CPU*
     time only. That requires either real hardware (VF2 / K1, where the TCG
     cross-hart confound does not exist) or a deterministic single-hart
     QEMU mode — and `icount` is already known to get *worse* under SMP.
     This is the same root cause that gates the rest of Phase B's "N×"
     measurements; I1 inherited it rather than introducing it.
  2. **A cycle counter is not a stopwatch under an SMP emulator.** `rdcycle`
     deltas are only valid WCET when the measured code held the CPU for the
     whole interval. On TCG SMP that invariant is silently violated. Any
     future timing instrumentation must either pin to 1 hart, subtract
     off-CPU time, or run on hardware.
  3. **The infra (proc-macro + source-scanning build script + generated
     point table) is sound and cheap** and could be reused as opt-in
     *runtime telemetry on real hardware* — but that is a new proposal with
     its own success criteria, NOT a back-door acceptance of this gate.
     Deferred to post-hardware (July 2026).

- **Follow-up action (pending user decision)**: the §"Failure path"
  prescribes reverting the macro + gate code. Whether to fully revert vs.
  retain the macro as `#[cfg(feature = "wcet")]`-gated, default-OFF
  telemetry (no CI gate) is a conscious call left to the author, recorded
  here so "rejected" does not quietly become "kept".

## Reference: RFC-0026

This experiment depends on Phase A of RFC-0026:
- `bench/results/<sha>.json` schema (this RFC adds `wcet_per_fn.*`).
- `tools/bench_compare.py` direction-aware comparison (auto-handles
  the new `.max` / `.p99` suffixes).
- `bench/baselines.json` declared bounds (new entries added by this
  experiment).
- `tools/bench_dashboard.py` sparkline rendering (auto-picks up new
  metrics, no change needed).

It also depends on the Kconfig framework for the `WCET_MAX_POINTS`
bump.
