> **When to use this template:** Use `_template_experiment.md` for **EXPERIMENT RFCs** —
> proposals that claim a measurable improvement (performance, latency, throughput, memory,
> WCET) and need empirical validation against a baseline before they can be accepted or
> rejected. Use `_template.md` for **DESIGN RFCs** — architectural choices and new
> subsystems where correctness is argued by construction, not by measurement. If your RFC
> says "this will be faster / smaller / safer", it belongs here. If it says "this is the
> right structure", it belongs in `_template.md`. When in doubt: if you cannot write a
> falsifiable hypothesis with a baseline number, it is a design RFC.

# RFC-NNNN: {{Title}}

> **Status:** {{draft | experiment-running | accepted | rejected}}
>
> `draft` — hypothesis written, experiment not yet started.  
> `experiment-running` — the change is live in the tree; results not in yet.  
> `accepted` — exit criteria met; change is permanent; baselines updated.  
> `rejected` — experiment concluded; change reverted; negative result documented below.
> **Rejected RFCs are never deleted** — the negative result is the artefact.
>
> **Authors:** {{name \<email\>}}  
> **Created:** {{YYYY-MM-DD}}  
> **Last updated:** {{YYYY-MM-DD}}  
> **Supersedes:** —  
> **Superseded by:** —  
> **Companion design RFC:** {{RFC-NNNN or — if none}}

---

## Summary

{{3–5 sentences. State: (1) what kernel change is proposed; (2) which subsystem or hot path
it touches; (3) the specific property it should improve; (4) the measurable target.
Example: "Capability-aware scheduling bins tasks by their active cap set before inserting
them into the runqueue. It touches the scheduler hot path in `crates/sched/src/run.rs`.
The hypothesis is that separating cap-heavy tasks from ISR-style tasks reduces p99 TCP RTT
under the 200-task CI load. The target is ≥ 20 % reduction in `rtt_ms.p99`."}}

---

## Hypothesis

> This is the heart of the experiment. Fill every sub-field before raising any code PR.
> The hypothesis must be falsifiable — if you cannot state what a failure looks like,
> you do not have a hypothesis.

**Claim:**  
{{One sentence with a direction, a metric, and a magnitude. E.g.:
"Capability-aware scheduling reduces p99 TCP RTT by ≥ 20 % under 200-task load."}}

**Primary metric:**  
{{Exactly one metric from `bench/results/*.json`. Cite the JSON field path.
E.g.: `bench/results/tcp_rtt.json` → field `rtt_ms.p99`.
See `bench/README.md` for the available metrics and baseline access.}}

**Baseline number:**  
{{Current value measured at a specific commit. Must include: value, units, SHA, sample
count, stddev. E.g.: `4.8 ms at sha 5e61db3, n=900 samples, stddev 0.6 ms`.
Do not write "TBD" — measure before writing the RFC.}}

**Target number:**  
{{What the metric must reach after the change, and the direction. E.g.: `≤ 3.8 ms
(≥ 20 % reduction from 4.8 ms baseline)`. A two-sided claim ("between X and Y") is
acceptable if the improvement has a natural ceiling.}}

**Confidence:**  
{{One of: `low` / `medium` / `high`. Explain why in 1–2 sentences. E.g.: `medium —
scheduler theory predicts the effect, but TCG-SMP rdcycle artefacts may mask it in CI;
will re-measure on real hardware`.}}

**Time horizon:**  
{{How long the experiment runs and across how many CI commits before a verdict is
reached. E.g.: `2 weeks of CI runs spanning ≥ 20 distinct commits.` Be specific; open-
ended experiments drift into permanent WIP.}}

---

## What would make this fail

> An experiment with no kill criteria is a wish. List each criterion as a concrete,
> checkable condition. The moment any criterion fires, the experiment moves to
> `rejected` regardless of how the primary metric is trending.

- [ ] Primary metric moves **less than half** of the claimed direction after
  {{N}} consecutive CI runs (e.g.: `rtt_ms.p99` stays above `{{4.4 ms}}`).
- [ ] Secondary metric **`{{metric_name}}`** (e.g. `throughput_mbps.p50`) **regresses
  ≥ {{5 %}}** from its own baseline `{{baseline value}}`.
- [ ] WCET violations on bound point **`{{point}}`** (e.g. `sched::run_next`) increase
  by **≥ {{10 %}}** measured by `crates/wcet-probes`.
- [ ] Static kernel binary size (`.text` + `.rodata` + `.bss`) grows by **≥ {{N}} KiB**
  on the `profile-edge` build without a corresponding RFC justifying it.
- [ ] Brain `pytest` pass count **drops below {{1192}}** (current baseline).
- [ ] Any of the 5 kernel build configs (`qemu`, `vf2`, `k1`, `no-ml`, `no-mmu`) fails
  to compile clean.
- [ ] {{Add domain-specific kill criterion here.}}

---

## Exit criteria

> State explicitly what "done" means in both directions. The experiment must close; it
> cannot stay in `experiment-running` indefinitely.

### Success path → status `accepted`

1. Primary metric **`{{rtt_ms.p99}}`** meets or beats target **`{{≤ 3.8 ms}}`** in
   **≥ {{5}} consecutive CI runs** on the same kernel config (`{{qemu}}`).
2. No kill criterion fired during the entire experiment window.
3. All 5 kernel build configs still compile clean with 0 errors, 0 warnings.
4. Brain `pytest` pass count ≥ {{1192}}.
5. Update `bench/baselines.json` (see Phase A6 / bench infrastructure) with the new
   baseline at the merge SHA.
6. Promote status to `accepted`; fill in the **Results** section below.

### Failure path → status `rejected`

1. Any kill criterion fires, **OR** the primary metric has not moved in the claimed
   direction after {{N}} CI runs (time horizon exhausted).
2. Revert the change (or gate it behind a `cfg` flag and disable by default).
3. Promote status to `rejected`.
4. **Fill in the Results section.** The negative result is the deliverable — do not
   close the RFC without documenting what was learned and why it failed.

---

## Architectural risk if it succeeds

> Many improvements carry hidden costs that only become visible after the change is
> accepted and permanent. Force yourself to enumerate them now, while it is still easy
> to say "actually, let's not".

- **ABI / data-structure growth:** {{E.g.: "Capability-aware scheduling adds a
  `bandwidth_hint: u32` field to the cap table entry; cap-table size grows from 64 B
  to 72 B per entry, +8 KiB at `MAX_CAPS_TOTAL = 4096` on `profile-edge`."}}
- **API surface growth:** {{New syscalls, new CONFIG.INI keys, new packet types — each
  is a commitment the safety case (RFC-0017) must track.}}
- **Build complexity:** {{New cargo features, new build-time probes, new CI steps.}}
- **Audit trail / cert scope:** {{If this touches a cert-scope module, the cert evidence
  must be updated (RFC-0017 § audit boundary).}}
- **Interaction with in-flight RFCs:** {{List any other RFC whose design assumptions
  this change invalidates, even subtly.}}
- {{Add further risks.}}

---

## Detailed design

> The design must already exist — you do not propose an experiment for code you have not
> sketched. Keep this section shorter than a design RFC's; the detail lives in code,
> not here. Pointer to companion design RFC if the scope warrants one.

### Interface / API

{{New syscalls, packet types, CONFIG.INI keys, or exported Rust traits introduced.}}

### Data structures

{{Changed or new structs, enums, or constants. Reference the actual source paths.}}

### Behaviour and edge cases

{{How does the change behave at the limits? What happens on error?}}

### Interaction with other subsystems

{{Scheduler, net stack, OTA, brain protocol, cap table — whichever the change touches.}}

---

## Implementation plan with measurement checkpoints

> Each phase has its own measurement gate. A later phase does not start if an earlier
> measurement gate fails. This makes the experiment incremental and reversible.

| Phase | Scope | Expected measurement | Gate |
|-------|-------|----------------------|------|
| **X1** — {{baseline sanity}} | Land the scaffolding with the feature **disabled** by default. Run the bench harness. | Primary metric must match the recorded baseline within 1 stddev. | Re-measure before X2. |
| **X2** — {{first signal}} | Enable the feature under a `cfg` flag, CI-only. | Primary metric should show first movement toward target. | If no movement after {{3}} runs, re-examine hypothesis. |
| **X3** — {{full exposure}} | Enable by default on `profile-edge`. | Primary metric meets or exceeds target. | All exit criteria checked. |
| {{X4 …}} | {{Add phases as needed.}} | | |

---

## Drawbacks

{{What are the costs of this change independent of whether the hypothesis is true?
Implementation complexity, learning curve, increased build times, new CI dependencies,
memory overhead if the experiment is accepted.}}

---

## Alternatives

> **Do nothing** (always the first alternative): the baseline cost of not making this
> change is **`{{baseline value}}`** for **`{{metric_name}}`**. If this is acceptable for
> the current deployment scope, the experiment should not run. State explicitly why
> the baseline is or is not acceptable.

- **Alternative A — {{description}}:** {{why not chosen or deferred}}.
- **Alternative B — {{description}}:** {{why not chosen or deferred}}.
- {{Add further alternatives.}}

---

## Unresolved questions

{{Things that must be resolved before the experiment can move from `draft` to
`experiment-running`. Each item should have an owner and a deadline.}}

- [ ] {{Question 1}} — owner: {{name}}, resolve by: {{date or phase gate}}.
- [ ] {{Question 2.}}

---

## Results

> Fill this section **after** the experiment concludes. It is required regardless of
> verdict. Do not leave it empty when closing the RFC.

**Experiment concluded:** {{YYYY-MM-DD or "pending"}}  
**Final SHA measured at:** {{sha or "pending"}}  
**Final primary-metric value:** {{value ± stddev, n=samples, or "pending"}}  
**Baseline (for comparison):** {{original baseline value from Hypothesis section}}  
**Kill criteria fired:** {{none / list which ones}}  
**Verdict:** {{accepted | rejected | pending}}

### If accepted

{{Brief statement of which phases produced the signal and which CI runs provided
the ≥ M consecutive successes. Reference the commit that updated
`bench/baselines.json`.}}

### If rejected

> This is the most valuable section. Failed experiments are knowledge, not waste.
> Readers of future RFCs that propose similar ideas should find this section and
> understand why the approach does not work before investing time.

{{Explain:
- What the data actually showed (primary metric trace, any secondary metric movement).
- Which kill criterion fired first, or why the time horizon expired without signal.
- What the root cause is believed to be (even if uncertain).
- What a different approach might look like if the underlying goal is still worth
  pursuing.}}

---

## Reference: RFC-0026

This template assumes the `bench/` infrastructure exists (Phase A5 — harness,
Phase A6 — `bench/baselines.json` ownership, Phase A7 — dashboard). If the
infrastructure is not yet in place, cite `bench/README.md` for the available metrics
and baseline access once it lands. Do not run an experiment that cannot be measured;
a hypothesis without a measurement path is not an experiment.

See also: [RFC-0026] (forthcoming) for the bench infrastructure specification.
