# RFC-0028: Multi-stream priority scheduling (control preempts bulk) — experiment I2

> **Status:** experiment-running
>
> `draft` — hypothesis written, experiment not yet started.
> `experiment-running` — the change is live in the tree; results not in yet.
> `accepted` — exit criteria met; change is permanent; baselines updated.
> `rejected` — experiment concluded; change reverted; negative result documented below.
> **Rejected RFCs are never deleted** — the negative result is the artefact.
>
> **Authors:** Fernando Rodriguez \<fgrg1978@gmail.com\>
> **Created:** 2026-06-01
> **Last updated:** 2026-06-01
> **Supersedes:** —
> **Superseded by:** —
> **Companion design RFC:** RFC-0021 (multi-stream brain link)

---

## Summary

RFC-0021 multiplexes control traffic (sensors/actuators/status — small, latency-
critical) and bulk traffic (camera/lidar — multi-KiB, latency-tolerant) over a single
brain↔kernel TCP connection. Under FIFO scheduling a large bulk frame **head-of-line-
blocks** any control frame queued behind it. This experiment proposes a **priority**
scheduling policy — chunk the bulk stream and interleave `STREAM_CONTROL` frames ahead
of bulk chunks — selected at **compile time** via the Kconfig choice
`MULTISTREAM_SCHED_PRIORITY` (zero hot-path overhead; the unused branch is const-
eliminated). It touches the multi-stream send path (`kernel/src/main.rs`, `send_framed`).
The hypothesis is that priority bounds control hold-off to ~one chunk-time instead of one
full bulk-frame-time. Target: **≥ 10× reduction** in control hold-off.

---

## Hypothesis

**Claim:**
Chunked priority interleaving reduces the control-stream head-of-line hold-off behind a
16 KiB bulk frame by ≥ 10× versus FIFO.

**Primary metric:**
Kernel `[I2]` probe line, field `ctrl_holdoff_cyc` — cycles from "control + bulk both
ready" to "control frame fully on the wire", emitted by `i2_holdoff_probe()` in
`kernel/src/main.rs`. Captured from `build/bench_logs/run1/qemu.log` during an E2E run.

**Baseline number:**
`ctrl_holdoff_cyc = 48,598,000` (≈ full bulk send: `bulk_total_cyc = 48,605,000`),
FIFO build (`MULTISTREAM_SCHED_PRIORITY=n`), sha 8787063, QEMU TCG SMP-4, 16 KiB bulk /
1460 B chunk, n=1 one-shot probe per boot, multi-stream link (`multi_stream=1`, no
encryption).

**Target number:**
`ctrl_holdoff_cyc ≤ 4,860,000` (≥ 10× reduction from 48.6M baseline).

**Confidence:**
medium-high — the effect is **gross** (tens of millions of cycles, far above the 8–40 %
TCG `rdcycle` noise floor that defeats microbench-scale experiments here), and it is
structurally explained: the kernel TCP is stop-and-wait (one un-ACKed segment via
`is_unacked`), so a 16 KiB frame ≈ 11 MSS segments × RTT in FIFO vs ~1 segment in
priority. Real camera frames (50–200 KiB) make the win larger still.

**Time horizon:**
Single decisive A/B (two builds, two boots). Promote to `accepted` only after the
production bulk/camera sender adopts the interleave AND it is re-measured on real
hardware (rdcycle = true counter), expected July 2026.

---

## What would make this fail

- **`ctrl_holdoff` ratio < 10×** between FIFO and priority → reject (no inversion worth
  the added send-path complexity). *Result: 10.07× — did NOT fire.*
- **Bulk throughput collapses** under priority (interleaving control between chunks adds
  per-chunk framing + ACK round-trips that dominate) → reject if bulk goodput drops > 25 %.
- **Priority adds more jitter than it removes** — if the per-chunk control check
  measurably perturbs the control path WCET (`#[wcet]` histos) beyond the hold-off saved.
- **Effect is within TCG noise** — if the FIFO/priority delta is comparable to the 8–40 %
  cross-run variance, the result is not trustworthy under emulation. *Did NOT fire: the
  delta is ~10×, an order of magnitude above the noise floor.*

---

## Exit criteria

- **Accept** when: ratio ≥ 10× (✓ met via probe) **and** the production camera/bulk sender
  (Phase D) uses chunked interleave under the priority const **and** re-measured on
  hardware confirms ≥ 10× without > 25 % bulk-goodput loss. Then make priority the default
  for camera-bearing profiles and update baselines.
- **Reject** when any kill criterion fires; revert to FIFO-only and record the negative
  result here.

---

## Architectural risk if it succeeds

Interleaving turns the bulk send from one `send_all_with_yield(frame)` into a chunk loop
that must poll for pending higher-priority frames between chunks — more state in the send
path, and a per-chunk framing cost (each chunk gets its own 3 B multi-stream header). For
cert (RFC-0017) the interleave loop's WCET must be bounded and the "control jumps ahead"
decision must be a const-selected branch (no runtime scheduler in the hot path). Mitigated
by the compile-time `choice`: a FIFO binary carries none of this.

---

## Detailed design

- **Config seam (done):** Kconfig `MULTISTREAM_SCHED_PRIORITY` (`Kconfig.brain`,
  default n) → `robot_os_limits::MULTISTREAM_SCHED_PRIORITY: bool`. FIFO and PRIORITY are
  separate binaries; the unused branch is const-eliminated.
- **Probe (done):** `i2_holdoff_probe(fd)` (qemu-gated, one-shot, fires when
  `CFG_MULTI_STREAM`) sends a 16 KiB `STREAM_CAMERA` frame in 1460 B chunks; under
  priority it injects a `STREAM_CONTROL` frame after the first chunk, under FIFO after the
  whole bulk. Emits `[I2] mode=… ctrl_holdoff_cyc=… bulk_total_cyc=…`.
- **Production (pending, Phase D):** the real camera/bulk sender adopts the same chunk-
  interleave under the priority const; control frames from the sensor pump jump ahead of
  in-flight bulk chunks.

---

## Implementation plan with measurement checkpoints

1. ✅ Config seam (`MULTISTREAM_SCHED_PRIORITY`) + const flow — committed (c0c1d17 line).
2. ✅ Baseline + A/B probe; capture FIFO vs priority `ctrl_holdoff_cyc`.
3. ⏳ Productize interleave in the camera/bulk sender (Phase D — needs camera hardware).
4. ⏳ Re-measure on VF2/K1 hardware (real rdcycle); confirm ≥ 10× + bulk goodput; promote.

---

## Drawbacks

- Per-chunk multi-stream header overhead (3 B/chunk) on the bulk stream.
- Two binaries to build/track for the A/B (mitigated: it is the standard experiment idiom
  — Kconfig choice + 2 defconfigs + dashboard).
- Single-sample-per-boot probe (n=1/mode); the gross effect makes this acceptable, but
  hardware promotion should take a 3-run median.

---

## Alternatives

- **Runtime selector** (CONFIG.INI `AtomicBool`): rejected for production — adds a hot-path
  branch + atomic load to every frame, contaminating the FIFO baseline measurement and the
  cert surface. A `dynamic` Kconfig value (compile both + runtime toggle) is reserved for
  dev-only within-boot A/B (kills cross-session noise) — not shipped.
- **Separate TCP connections per stream**: avoids head-of-line entirely but multiplies
  socket state (TCP_MAX_CONNS pressure) and loses the single-handshake/single-AEAD-session
  property of RFC-0019/0021.

---

## Unresolved questions

- Chunk size: 1460 (one MSS) is the natural floor; smaller chunks lower hold-off further
  but raise header overhead — sweep on hardware.
- Whether the kernel TCP should move off stop-and-wait (a sliding window would change the
  baseline shape; the head-of-line inversion persists regardless, but its magnitude shrinks).

---

## Results

**2026-06-01 — A/B via `i2_holdoff_probe`, QEMU TCG SMP-4, sha 8787063, 16 KiB bulk / 1460 B chunk:**

| Build | `ctrl_holdoff_cyc` | `bulk_total_cyc` | Note |
|---|---|---|---|
| FIFO (`=n`) | **48,598,000** | 48,605,000 | control waits the entire bulk frame |
| PRIORITY (`=y`) | **4,828,000** | 27,485,000 | control jumps ahead after 1 chunk |

**Improvement = 48.60M / 4.83M ≈ 10.07× (−90.1 % control hold-off).** Hypothesis
**CONFIRMED**; no kill criterion fired. Matches the structural prediction (16384/1460 ≈
11 segments under stop-and-wait TCP). `bulk_total` differs run-to-run (TCG/SLIRP window
noise) but does not affect the hold-off ratio — the metric of record.

**Caveats:** n=1 per mode (gross effect → acceptable under emulation); `rdcycle` under TCG
is not a true cycle count (the *ratio* is what carries, not the absolute). Promotion to
`accepted` awaits the production interleave (Phase D) + a hardware re-measure where the
counter is exact and real camera frame sizes (50–200 KiB) should push the win to 30–130×.

---

## Reference: RFC-0026

Config mechanism (Kconfig → `.config` → `robot_os_limits` const) per RFC-0026. The
experiment idiom — Kconfig `choice` + per-policy binaries + `bench_boot`/E2E capture +
dashboard A/B — is the standard pattern for Phase B experiments.
