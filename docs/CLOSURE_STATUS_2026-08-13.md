# Closure Status — 2026-08-13

> **Note (2026-08-18).** The `esp32c3` build target discussed in this
> document (including the note about its 3-layer gap) was removed from the
> tree that day — it never compiled, never was in CI. It remains parked in
> `newfeatures/esp32c3/`. The rest of this document is unchanged.

> Single status document, written to close months of review and move to
> real hardware tests (VF2). It does not replace `KERNEL_REVIEW_NOTES.md`
> (detailed history, batch by batch, with full reasoning) — it is its
> actionable summary. Everything claimed here is traceable to a concrete
> section of those notes.

> **Update (batch 7, same day).** The original draft of this document became
> outdated the same day it was written: an independent audit (Fable),
> explicitly tasked to try to refute it, found several real problems —
> some blockers — in things given as closed here. The round of fixes that
> followed ("batch 7") closed all but two, left open on purpose (see
> sections 4 and 9 below). The content of this document already reflects
> that final state. For the full story, design reasoning and verification
> detail of each fix, see `KERNEL_REVIEW_NOTES.md`, sections dated
> `2026-08-13`: *"Independent audit (Fable)..."*, *"Batch 7, G: task
> lifecycle race fix"*, *"Batch 7 (rest): BOOTMETA, WDT, sensors,
> memory/Kconfig, E1, PWM"*, *"D2: secure boot success path"* and
> *"E2: `boot.cmd` branch `active_slot=r`"*. This document remains the
> executive summary — it does not duplicate that detail.

## 0. Repo Status Right Now

- Latest commit: `8919bc7` (2026-08-11). Contains batches 1-5 complete (see
  section 1). Committed by user via his usual `git commit --amend` flow.
- **Working tree: 9 uncommitted files** (batch 6, 2026-08-11): `Cargo.lock`,
  `crates/ota-tests/src/lib.rs`, `crates/ota/Cargo.toml`, `crates/ota/src/lib.rs`,
  `crates/ota/src/pure.rs`, `crates/ota/src/secure_boot.rs`,
  `crates/shell/src/lib.rs`, `crates/topology-tests/Cargo.lock`,
  `kernel/src/main.rs`. Content: Kconfig `OTA_MAX_IMAGE_SIZE_MB` truly wired,
  BOOTMETA extended to 3 slots (`SLOT_R` representable), boot stack reserved in
  PMM. **Compiled (7/7 configs) and tested (22 test crates, 21 clean + 1
  preexisting flaky). Not committed — user decision.**
- **Batch 7 (same day, see note above) added more changes** on other files
  (`context_switch*.S`, `scheduler.rs`, `sched/` (new field/feature),
  `platform.rs`, `secure_boot.rs` (paths), `tools/boot.cmd`, sensor drivers,
  `crates/limits`/Kconfig, `kernel/linker.ld`, PWM) — commit state of those
  files not verified when writing this update; see `KERNEL_REVIEW_NOTES.md`
  for exact per-file detail.
- **Nothing has been lost at any point in the session.** Explicitly verified
  after a false alarm (see notes, "what's left?" entry from 2026-08-11).

## 1. The 11 Confirmed 2/2 Findings (Original Audit) — Final State

| # | Finding | Status |
|---|---|---|
| 1 | `scheduler.rs:251` counts bits instead of tasks | ✅ FIXED |
| 2 | `vdso.rs:105` multi-writer seqlock without serialization | ✅ FIXED |
| 3 | `port.rs:119` IRQ/syscall race without lock | ✅ FIXED |
| 4 | `pwm.rs:187` PWMCMP shared between period and duty | ✅ FIXED (batch 7) — register model corrected against primary source (Linux `drivers/pwm/pwm-sifive.c`): `PWMCFG` (period) is a single register shared by 4 channels, `PWMCMP(i)` is the genuinely per-channel register (duty). Only 4 real channels, not 8. Validated by compilation + line-by-line review — QEMU uses `sim` module, not `mmio`, so the real path has never executed; first real execution will be in hardware bring-up. |
| 5 | `gpio.rs:139` RMW without lock on real MMIO path | ✅ FIXED |
| 6 | `pwm.rs:177` gripper stub that always fails | 🟡 DEFERRED BY DESIGN DECISION (batch 7) — no longer "blocked by lack of TRM": the scheduler tick (10 ms) cannot represent an RC servo pulse (1-2 ms), and `PWMCFG` is a single period register shared by 4 channels, so motors (~1 kHz) and servo (~50 Hz) also cannot coexist in the same PWM instance at different frequencies. Requires a dedicated high-resolution timer design that has not started. Meanwhile: the correct register model and channel bound-check (4 real) make the gripper fail with log instead of silently writing to an out-of-range MMIO address. |
| 7 | `secure_boot.rs:208` Ed25519 verifier not called | ✅ FIXED — wired behind `secure-boot-enforced` feature, fail-closed validated on QEMU. **SUCCESS path also validated in batch 7 (D2)** — see section 5 bis and section 9, Phase 5. |
| 8 | `secure_boot.rs:235` 2 MiB buffer on stack | ✅ FIXED — moved to `.bss`, chunked read |
| 9 | `panic.rs:133` deadlock from `FS` lock in panic handler | ✅ FIXED — `try_lock` + emergency path without locks for motor/gpio/pwm/esc |
| 10 | (duplicate of #7) | ✅ FIXED |
| 11 | `main.rs:544` CRC without rollback | ✅ FIXED — fallback to `last_good`, validated on QEMU with hand-fabricated disk (2 scenarios) |

**10 of 11 closed. 1 deferred by explicit design decision (not by lack of
datasheet — the TRM/primary source was already consulted and applied).**

## 2. New Bugs Found and Fixed DURING the Work (Not in Original Audit)

All verified in execution, not just by reading:

- 🔴 `smp.rs:31` `hart_start` throws SBI error code → phantom CPU that swallows tasks forever. **FIXED** (`wake_harts` returns the real live prefix).
- 🔴 `scheduler.rs:1373` + 4 more places — cross-CPU access to ready queues without `CPU_LOCKS`. **FIXED**, including `do_schedule()` itself (the important place, was not in the original finding). **Nuance added by Fable audit and closed in batch 7: see "5 bis" — the fix protected the STRUCTURE of the queues but left out of lock the state transitions of a task during the context switch itself.**
- 🔴 Boot stack of hart 0 never reserved in PMM — allocator could hand out pages within the boot stack itself. **FIXED**.
- 🔴 `libsys::vdso_uptime_ticks()` read the wrong offset from seqlock (returned the sequence counter, not the uptime). **FIXED**.
- 🔴 Jitter report underflow — `last` shared among 4 harts, subtracting "now" from one hart against "before" from another. **FIXED**, root cause confirmed arithmetically.
- 🟡 Obsolete/misleading comment in `smp.rs:58` about `tp`. **Not fixed** (one-liner candidate).
- Rescue of boot tasks stranded on a hart that did not start (`rebalance_from_offline_cpus`). **FIXED, but NEVER EXECUTED** — only runs if a `hart_start` fails, and that doesn't happen on QEMU. Verified by reading and compilation, zero times by real execution. **Additional risk identified by Fable and still open: on real VF2 the boot hart is never 0 (physical hart 0 is the S7, no S-mode/MMU) — see section 9, Phases 0/4, and FIX C.**

### 2 bis. Bugs Found and Fixed in Batch 7 (Post-Fable Audit, 2026-08-13)

Four of these came from new findings by Fable audit; one (D2) was found
during the batch's own execution, not by audit. Full detail, design and
validation in `KERNEL_REVIEW_NOTES.md` (sections "Batch 7, G", "Batch 7
(rest)" and "D2"):

- 🔴 **Task lifecycle race (G).** `block_current()`/`do_schedule()` changed a task's state (`Blocked`→`Ready`+queued) before `context_switch.S` finished saving its context; a waker on another hart could redispatch it with context still invalid — the same task running on two harts with the same stack. Also `find_earliest_deadline()` (EDF picker) had no lock and accepted `Running` tasks from any CPU (today latent: zero callers of `task_set_deadline`, but the subsystem had to be correct). **FIXED**: new `Task::context_saving: AtomicBool` + `mark_context_saved()` called from `context_switch.S` (and its `esp32c3` variant, for Rust-side consistency even though the race is impossible on single-core), spin-gate in `do_schedule()`, `find_earliest_deadline()` now only accepts `Running` as self-continuation, new `DeadlinePickGuard` IRQ-safe serializes scan+claim on the EDF branch. `context_switch_rvv.S` (QEMU benchmark only, unused by any real target) is deferred on purpose and gated to not hang. Validated: 6/7 configs compile clean (see `esp32c3` note below), QEMU `-smp 4` soak 20/20 clean boots, 0 panics/deadlocks, 0 WCET violations, real workers crossing CPU 2/3 (2000 iterations) directly exercising the new spin-gate.
- 🔴 **Plain BOOTMETA never rewritten at runtime (OTA blocker on real hardware).** `ota_write_boot_meta()` only wrote `.A`/`.B` records; the plain file `/fat/BOOTMETA` that `tools/boot.cmd`/U-Boot actually reads (`env import -t`) was only read as migration, never written — any slot change decided by kernel (OTA, rollback, CRC fallback) never reached U-Boot on a real boot; only worked editing the plain file by hand. **FIXED** (FIX A): `ota_write_boot_meta()` also rewrites the plain file via new `fs_write_plain_boot_meta()`; `.A`/`.B` records remain the source of truth against a partial write of the plain file.
- 🔴 **WDT: wrong clock + armed too early.** `hw_wdt_init` calculated ticks with `TIMER_FREQ` (4 MHz, RISC-V timer clock) instead of WDT's real clock (APB, ~24 MHz) — a requested `500ms` gave ~87 ms effective. Also armed in Phase D (early boot), and the only `wdt_kick()` lives in the timer ISR, which doesn't start until the scheduler — an unfed window that predicted looping reset before the prompt on first Phase 0 of hardware. **FIXED** (FIX B): new constant `WDT_CLK_HZ` (24 MHz, corroborated by preexisting file comments) replaces `TIMER_FREQ` in `wdt.rs`; arming moved to just before `sched::start()`.
- 🔴 **`I2C0_BASE` aliased to `UART1_BASE` in `platform.rs` (vf2).** Finding carried from original audit with 0/2 votes, now confirmed against primary source (`jh7110.dtsi` mainline Linux): `I2C0_BASE` was at `0x1001_0000` (actual UART1 address). **FIXED**, corrected to `0x1003_0000`.
- 🔴 **`secure_boot.rs`: `/fat/`-prefixed paths incompatible with real U-Boot/VFS convention.** See section 5 bis — found by running the success path with real key and image (D2), not by inspection.

## 3. User Decisions Already Made and Executed

- **Secure boot**: compile-time feature, fail-closed if active, identical in debug/production, no bypass. Validated: with the feature it does not boot (127 lines of boot, no scheduler, no shell); without it, boots normally and only warns. **Batch 7 (D2) additionally validated the SUCCESS path**: real image signed with production key (`tools/keys/prod_priv.bin`), verified against embedded `prod_pub.bin`, clean boot 3/3 repeats — see section 5 bis.
- **Active slot CRC**: falls to `last_good` if verifies; if nothing verifies, keeps booting screaming (no reboot, no machine halt — explicit decision to not turn bit-rot into brick). Validated with 2 scenarios on QEMU with hand-fabricated FAT32 disk.
- **`OTA_MAX_IMAGE_SIZE`**: wired to real Kconfig (before: hardcoded to 2 MiB despite Kconfig saying 8 MiB and nobody reading it). Split into two different constants (acceptance vs verification buffer) with a compile-time `assert!` keeping them coherent.
- **BOOTMETA of 3 slots**: `SLOT_R` is now representable in the format. Also, in batch 7 **`tools/boot.cmd` already branches by `"r"`** (FIX E2, see section 5 bis) and **the plain `/fat/BOOTMETA` that U-Boot actually reads already rewrites at runtime** (FIX A). Remains unresolved that nothing populates `image_size_r`/`image_crc_r`/`fw_version_r` — R is representable and reachable by U-Boot, but no factory tool writes those fields yet; the recovery flow to R remains a non-real candidate.

## 4. User Decisions Still Not Made

- [x] ~~Expand kernel window from 8 MiB?~~ **DONE in batch 7 (E1)**: `kernel/linker.ld` (QEMU/default) expanded to 32 MiB (vf2/k1/fleet already had margin). `_stack_start`/`_stack_end`, pinned to top of window, relocated themselves — verified on QEMU (`0x821f0000-0x82200000`), clean boot.
- [x] ~~Teach `boot.cmd` to branch by `SLOT_R`?~~ **DONE in batch 7 (E2)**: branch `elif test "${active_slot}" = "r"` added, symmetric to A/B (tries `KERN_R.BIN`, falls to `KERN_A.BIN` if fails). Structure if/elif/else/fi reviewed by hand (correct balance, 9/9), and — in a follow-up round after close — **actually compiled**: `u-boot-tools` installed (Homebrew) specifically for this, `mkimage -C none -A riscv -T script -d tools/boot.cmd build/boot.scr` exit 0, valid hush-shell syntax confirmed by U-Boot tool itself, not just by hand review. Still not **executed**: no real U-Boot environment on QEMU in this session (this project's boots use `-kernel` directly on OpenSBI, bypassing U-Boot entirely); that would need compiling real U-Boot from source for QEMU target binary. Pending real execution with U-Boot (hardware or QEMU with full U-Boot).
- [ ] **Version `KERNEL_REVIEW_NOTES.md`?** Still `??` (untracked). It is the only copy of months of reasoning.
- [ ] **Commit batches 6 and 7?** Compiled and tested, awaiting your word.

## 5. Work Initiated Today and PAUSED Without Applying (Due to Hardware Pivot)

Investigated and verified by reading when this section was originally written
— **resumed and applied in batch 7** (see section 5 bis immediately below).
Original analysis preserved for its historical value:

- **"Free" memory gain**: `net::tcp::TCP` (1.02 MiB), and other symbols in `.data`, have a single non-zero field in their initializer (`rto_ticks`, `cwnd`, `ssthresh`, `remote_mss`, `remote_window` in `TcpConn::new()`) that forces the entire structure to `.data` instead of `.bss` — costing space in the OTA image unnecessarily. Analyzed in depth: `reset_conn_state()` already re-establishes 4 of the 5 fields on each real connection activation; the fifth (`remote_window`) is read only after handshake and `send_data()` already treats 0 as safe case ("window closed, retry"). `TcpState::Closed = 0` explicit, so zero-init falls into the correct state.
- **Three sizing holes to Kconfig**: `sched::task::STACK_SIZE` should come from `KERNEL_STACK_SIZE_KB` (Kconfig symbol that exists, generated as `KERNEL_STACK_SIZE_BYTES`, and **nobody consumes it** — third instance of the "configured and never wired" pattern in this session). `mm::pmm::MAX_PAGES` should derive from `RAM_SIZE` (which already exists, already on the right axis `BOARD_*`) instead of binary `cfg(feature="small-mem")`. `net::tcp::TCP_BUF_SIZE` needs new Kconfig symbol with power-of-two protection (code uses `TCP_BUF_SIZE - 1` as modulo mask — a non-power-of-two value would silently corrupt the ring buffer).
- **Structural bug found, not confirmed in any current defconfig**: `PROFILE_EMBEDDED` activates `robot_os_mm/small-mem` (`MAX_PAGES=128` → 512 KiB addressable) without looking at the board. If someone put `PROFILE_EMBEDDED=y` with `BOARD_VF2=y` (legitimate combination: "tight limits on a big board"), PMM would manage 512 KiB on an 8 GB board. No current defconfig triggers it — **the coupling of `PROFILE_*`/`BOARD_*` axes in itself remains unresolved**, only the sizing of `MAX_PAGES` was solved (see 5 bis).

### 5 bis. The Above, Closed in Batch 7

The three sizing holes, corrected (verified in the binary, not just by
reading):

- `TcpConn::new()`: all 5 fields, zeroed. `TCP` confirmed back in `.bss`,
  1,069,720 bytes.
- `sched::task::STACK_SIZE` now comes from
  `robot_os_limits::KERNEL_STACK_SIZE_BYTES` (Kconfig
  `KERNEL_STACK_SIZE_KB`, lives in `Kconfig.limits`). `TASK_STACKS`
  remains exactly 1,048,576 bytes in `.bss` (16 KiB × 64 tasks) — no
  regression in the default. New invariant in `build.rs`: cannot leave 0
  useful bytes after the 4 KiB guard page.
- `mm::pmm::MAX_PAGES` now derives from `robot_os_limits::RAM_SIZE` (real
  MiB per board) instead of a flat 16 GiB ceiling. Verified: PMM bitmap
  went from 512 KiB to ~8 KiB with `RAM_SIZE=256` and no longer appears
  among the largest `.bss` symbols in the QEMU binary.
- `net::tcp::TCP_BUF_SIZE`: the Kconfig symbol already existed
  (`Kconfig.network`, default 131072, identical to hardcoded value) —
  just needed wiring. New power-of-two invariant in `build.rs` (value is
  used as ring mask).

The `PROFILE_EMBEDDED`/`BOARD_VF2` coupling (third bullet above) **remains
unresolved** — was not part of batch 7 scope.

**D2 — secure boot, success path, real bug found.** Executed directly
(Bash/mtools/QEMU, no agents): image signed with real production key
(`tools/keys/prod_priv.bin`), FAT32 disk hand-fabricated with
`KERN_A.BIN`+`KERN_A.SIG`+`BOOTMETA`, booted with
`secure-boot-enforced`. First attempt (files inside a `fat/` subdirectory)
verified the signature but showed inconsistent log ("no firmware recorded")
that led to investigate further — and revealed a real bug, independent of
what D2 asked to check: `ota_verify_slot`/`ota_read_boot_meta` go through
VFS layer, which mounts `/fat` as mountpoint and STRIPS it from the path
before searching (searches at volume root); `secure_boot_verify_slot_detailed`
used DIRECT FAT32 driver access, which walks the path literal — with
`/fat/KERN_A.SIG` that means "find a real subdirectory called `fat`".
`tools/boot.cmd` (the real U-Boot source of truth) uses `fatload` with no
subdirectory: the only layout U-Boot can produce is volume root. With that
real layout, `secure_boot_verify_slot_detailed` would never find the `.SIG`
— **`secure-boot-enforced` would have hung in `loop { wfi() }` on every
real boot**, as soon as someone activated the feature. Not found by code
inspection — the code on each side (VFS vs. direct access) is internally
coherent and "looks correct" read in isolation; only running it with real
data exposed it. **FIXED** (`crates/ota/src/secure_boot.rs`): signature
paths moved to root-relative (`/KERN_{A,B,R}.SIG`), new constant/function
`secure_boot_bin_path()` so image reading stops reusing the `/fat/`-prefixed
path (correct for its VFS callers, but not for direct access). Validated:
disk reconstructed with everything at root — CRC OK + signature verified
together, no FATAL, 3/3 clean boots; 6/6 relevant configs compile clean after
the fix.

**Resume only if deciding to pursue that** now applies only to the
`PROFILE_*`/`BOARD_*` coupling — the rest of this section closed in batch 7,
not part of the critical hardware path still pending.

## 6. Verification Backlog (Not Touched This Session)

- **~14 "probably real" findings from original audit, with only 0-1 of 2
  verification votes — STILL UNTOUCHED except one**:
  `context_switch.S:83` (tp + EDF migration — see section 8; **do not
  confuse with lifecycle race (G), which is a separate problem already
  closed in batch 7**), `waitqueue.rs:91` (lost-wakeup),
  `scheduler.rs:617` (leak of `user_pt` on each exit), 5 `static mut`
  without lock in `crates/ipc/*` (same pattern as `port.rs`, not fixed in
  its siblings), `tcp.rs:965`/`:721` (RST without sequence check, MSS
  without clamp). The only one from this batch actually closed is
  `wdt.rs:113`, via batch 7 WDT fix (see section 2 bis).
- **15 candidates from `os-drivers`, 0/2 votes — 4 of them (the critical
  ones from "Next step" section: GPS, barometer, ina219,
  `platform.rs`/I2C0-UART1) closed in batch 7** (see section 2 bis and
  section 9, Phases 1/2). **The remaining 11 still unverified.**
- **8 of 17 automatic audit fronts never covered**: `os-fs-ota`,
  `os-syscall`, `os-flight-nav`, `os-config`, and **all 4 from
  robot-brain**. The Python repo remains completely unaudited. No changes.
- **Line-by-line manual review**: paused at `kernel/src/main.rs`, line
  ~166 of ~4,000 (Phase 1, UART+DTB). `linker.ld` and `boot.S` complete;
  SMP bring-up reviewed. No new progress — batch 7 touched specific points
  in `main.rs` (WDT arming) for the specific fix, not as part of this
  systematic review.
- **New in this update**: the independent Fable audit itself (2026-08-13)
  acts as a second round of external verification on the sample it covered
  (vdso, panic handler, cross-CPU locking, BOOTMETA 3 slots, boot stack,
  CRC-fallback chain) — confirmed those fixes as correct and found the 5
  new problems already listed in section 2 bis, all closed except the
  non-contiguous harts one (FIX C, deferred on purpose — see section 9).

## 7. Technical Debt We Are Generating

**~18 fixes applied in the original session, zero new tests written by us.**
Validation has been: compile 6-7 configs + boot on QEMU repeatedly (24+
panic-free boots after scheduler fix) + in the CRC case, hand-fabricated
FAT32 disk with two scenarios. The 22 existing test crates pass (minus the
preexisting flaky pair from `regression-tests`), but **none of those 22
crates cover code we've touched** — no `sched-tests` exercises
`rebalance_from_offline_cpus` or the new cross-CPU locking.

**Batch 7 repeats the same pattern, with the same debt.** ~12 additional
fixes/changes, again zero new unit tests. Validation levels up at one point
(fix G had dedicated 20-boot soak with real cross-CPU scheduling load, closer
to a real test than the rest), but still no specific automated coverage for
`context_saving`/`DeadlinePickGuard`, the `secure_boot.rs` path fix (D2
validated by hand once, not in CI), nor the real PWM MMIO path (never
executed, neither on QEMU nor in tests — the `sim` module is all that runs).
The "fixed but no dedicated test" backlog grew, not shrunk.

**Real data, not estimated**: in a follow-up round the 21 existing host test
crates were executed (`cargo test --release` on each) — 21/21 clean, zero
failures, including the 2 timing tests from `regression-tests` marked as
occasionally flaky (did not fail this time; that does not mean they
"fixed" — they remain flaky by clock design, see explicit instruction not
to touch them). This confirms nothing touched in batch 7 broke EXISTING
coverage — not that NEW coverage exists for the new mechanisms listed
above, which still does not.

## 8. The Thread That Has Reappeared Four Times Without Closing

🔴 **`context_switch.S:83`** — `tp` (hart identity) is saved/restored as task context; the EDF scheduler can migrate the task to another physical hart, leaving a stale `tp` that corrupts `current_cpu_id()`. Has appeared in:
1. Original audit finding (0-1 votes).
2. vdso fix design (agent cited it to reject my "only hart 0 writes" proposal).
3. Rescue of stranded tasks (`rebalance_from_offline_cpus` had to manually rewrite `task.context.tp` when moving a task).
4. Still not fixed at its origin.

**Still not fixed after batch 7.** Fix G (section 2 bis) closes a different
and related problem — synchronization between a task's state change and its
actual context save — but does not touch `tp` identity itself. Do not confuse
the two: G is closed, this is not.

It is not a patch — it is a design question about how this kernel identifies a
CPU. Any future scheduler/SMP work should start here.

---

## 9. HARDWARE TEST PLAN (VisionFive 2)

### Governing Principle

The kernel core (boot, scheduler, IPC, panic handling, secure boot, OTA) has
been validated extensively on QEMU. **Drivers on the real MMIO path of VF2
have never executed even once** — QEMU always uses the simulation path. Each
time we looked at the real path, it was worse than the simulated one. The plan
assumes that. **After batch 7, most of those "worse than simulated" have been
fixed by reading/primary source — but they still have never executed even once
on real silicon; that is exactly what remains missing and what this plan
covers.**

### Phase -1 — Before Powering On: Dump the Real DTB

**New after Fable audit.** FIX C (physical→logical hart_id mapping) and
validation of the hart contiguity assumption used by
`wake_harts`/`rebalance_from_offline_cpus` depend on data that does not
exist today: the real board DTB. Before Phase 0:

1. Dump the complete DTB from the real VF2 (or at least the `cpu` nodes, with
   their `reg`/`status`).
2. Confirm against that DTB: is physical hart 0 the S7 (no S-mode/MMU, as
   StarFive documents) and therefore the real boot hart will be 1..4? Are the
   usable harts contiguous from the boot hart?
3. The DTB parser (`crates/dtb/src/lib.rs:298`) counts every `device_type=cpu`
   node **without looking at `status`** — a `disabled` core counts the same for
   `num_cpus`. Confirm whether this matters with real DTB in hand before
   trusting `[BOOT] Online CPUs: N`.

**FIX C is deferred on purpose until having this data — it is the only task
from the close round left intentionally open.**

### Phase 0 — Pre-flight, No Motors, No Sensors Connected

Goal: confirm the kernel boots on real silicon and the shell responds.

1. Flash the `vf2` image (without `secure-boot-enforced`, without `wcet`).
2. UART console connected. Verify complete boot log: DTB parsed, `[BOOT] Online CPUs: 4`,
   boot phases, `[SCHED] Starting scheduler`, prompt `robot>`.
3. Basic shell commands: `help`, list tasks, `gpio_info`.
4. **Batch 6 item to watch**: the log `[MM] Boot stack reserved: ...` should appear,
   confirming the boot stack reservation executes on real hardware like on QEMU.
5. **New batch 7 item to watch**: the WDT (armed with corrected `WDT_CLK_HZ`, just before
   `sched::start()`) should not reset the board before reaching the prompt. If it resets in
   a loop anyway, it signals the real hardware clock differs from the 24 MHz assumption
   (documented as "probably correct" against Linux clock tree, not confirmed on silicon —
   see note at end of this section), not necessarily a fix failure.
6. **`[BOOT] Online CPUs: N` — read with caution** (see Phase -1): if `N` is not 4, do not
   automatically assume a broken hart before confirming against real DTB whether it is a
   topology issue (boot hart ≠ 0) rather than a genuine failure.

**Stop criterion**: if it doesn't reach the prompt, do not advance to Phase 1. It is the
core we already validated — a failure here would be a genuine hardware surprise, not
something we already knew broken (except WDT and hart topology, see points 5 and 6
above, already anticipated).

### Phase 1 — I2C Bus: The Critical Blocker, Before Everything Else

🟢 **`platform.rs:96` fixed in batch 7** — `I2C0_BASE`/`UART1_BASE` were aliased to the
same MMIO address (`0x1001_0000`); corrected to `0x1003_0000` against mainline
`jh7110.dtsi` Linux (primary source cited, not just inspection). **Still not executed on
real hardware** — remains the first step to confirm on the board, but is now not "unverified
by reading", it is a fix applied with primary source.

1. Confirm on real board that the corrected I2C0 responds where expected.
2. If something does not add up despite the fix: cross-check against JH7110 TRM before
   touching anything else — nothing that follows makes sense with the bus badly mapped.

**This step continues conditioning all of Phase 2.** Do not skip it.

### Phase 2 — Sensors, Passive Read, No Acting On Anything

With I2C bus confirmed correct. **The 4+2 known sensor bugs were fixed in batch 7**
(verified against formulas/known reference values, not against real hardware — QEMU
has no real sensors to exercise):

1. **IMU** (`imu_init`): connect, read WHO_AM_I, confirm `IMU_READY` reflects reality.
   **Fixed in batch 7**: I2C config writes now checked (before ignored and sensor marked
   `READY` anyway if they failed). Confirm in the bench that behavior holds with real
   sensor.
2. **Barometer** (`baro_init`/`baro_read`): same write-check pattern, **fixed**. Also,
   **`baro.rs:163` — +80m bias fixed**: the `dig_p7` term was missing from BMP280
   pressure compensation (official Bosch formula), now added. And **fixed premature read
   issue**: before it triggered forced measurement and read immediately without waiting
   for conversion (~40 ms); now polls the "measuring" bit bounded. Confirm altitude
   against known reference in bench — formulas are fixed, but this is first time they run
   against physical sensor.
3. **GPS**: `gps.rs:394` — **fixed ~5% error in minutes** (`min_frac * 10` was extra,
   `min_frac` already came scaled to 7 digits). Verified with known vector ("4807.038" →
   70,380,000, before gave 73,800,000). Confirm position against known reference in
   bench.
4. **ina219 (battery monitor)**: `ina219.rs:127` — **fixed bug that accumulated mAh
   without dividing by 3600** (triggered failsafe ~3.6 s after "battery spent"). Now
   accumulates raw and divides on read, not per-poll. **Note**: `ina219_poll` still has 0
   callers in tree — bug was latent, fixed anyway because it wires up in this bring-up.
   Confirm failsafe does NOT trigger alone after boot.

For the 4 points above: they are logic/math fixes validated against formulas and known
reference values, not against physical sensors — this phase is first time they confirm
with real hardware.

### Phase 3 — Actuators, On Bench, No Load, No Props/Drive Wheels

1. **GPIO**: this session's fix (`GPIO_MMIO_LOCK`) has never been exercised on real
   hardware under real SMP contention. Simple toggle first.
2. **PWM/motors**: **register model fixed in batch 7** against primary source (Linux
   `pwm-sifive.c`) — `PWMCFG` shared for period, `PWMCMP(i)` per-channel for duty
   (before aliased, the original root cause of the bug). Validated only by compilation +
   review — QEMU uses `sim` module, never `mmio`; **this is the first real execution of
   the PWM MMIO path in the entire project history.**
3. **Gripper**: deferred on purpose, not blocked by lack of data. Scheduler tick (10 ms)
   cannot represent an RC servo pulse (1-2 ms) and `PWMCFG` is a single period register
   shared — motors and servo cannot coexist in the same PWM instance at different
   frequencies. Requires dedicated high-resolution timer design, not started. Meanwhile,
   the corrected bound-check (4 real channels, not 8) makes the gripper fail with log
   instead of silently writing to a made-up MMIO address — confirm on console that it
   indeed fails with the expected message.
4. **Panic handler under real contention**: if something fails in this phase in a way
   that triggers a panic, it is the first time the emergency no-lock path
   (`motor_stop_panic`, `gpio_write_panic`) runs outside QEMU. Pay attention to whether
   the panic message comes out complete on UART.

### Phase 4 — SMP Under Real Hardware

1. Confirm `[SMP] Starting N secondary harts...` starts real harts — **but see Phase -1/0**:
   on real VF2 the boot hart is never 0 (physical hart 0 is the S7, which cannot run this
   kernel), and `wake_harts`/`rebalance_from_offline_cpus` assume boot hart = 0 and
   contiguity from there. Without FIX C, the first real boot may incorrectly trigger the
   rescue mechanism (drain live hart queues toward the dead S7 queue). If `[BOOT] Online
   CPUs` comes out lower than expected, suspect this before a physically broken hart.
2. Rescue of stranded tasks (`rebalance_from_offline_cpus`) **has never executed even once**,
   neither on QEMU nor on hardware. No easy way to force a real hart failure to test it —
   remains as accepted, documented, non-blocking risk for the rest of tests (except for
   point 1 above).
3. **Task lifecycle race (G) was closed in batch 7** and extensively validated on QEMU
   (20/20 clean boots, 2000 iterations crossing CPU 2/3, 0 WCET violations) — but this
   remains the first time it is exercised outside an idealized QEMU machine. If there is
   a way to stress the scheduler (create many tasks, force migrations), it is the
   opportunity to confirm it on real silicon.

### Phase 5 — OTA and Secure Boot, Optional, With Care

1. Test the CRC fallback flow on real hardware by replicating the experiment we did on QEMU
   (disk with deliberately bad CRC) — validate that the behavior observed on QEMU holds on
   the real board SD.
2. **`secure-boot-enforced`**: production keys (`tools/keys/prod_priv.bin`/`prod_pub.bin`)
   exist since May 2026, ignored by git, mode 600 — **no need to generate them, they
   already exist**. The success path (image signed with `prod_priv.bin`, verified against
   embedded `prod_pub.bin`) already validated on QEMU in batch 7 (D2): 3/3 clean boots.
   That same validation found and fixed a real and serious bug — `secure_boot.rs` used
   `/fat/`-prefixed paths resolved by direct FAT32 access (treats them as literal
   subdirectory), while the real `boot.cmd`/U-Boot convention and the kernel's own VFS
   layer expect these files at volume root; without the fix, `secure-boot-enforced` would
   have hung in `loop { wfi() }` on every real boot. See section 5 bis for detail. It
   remains to repeat the same validation on real hardware / real SD — QEMU's uses
   `-drive`+`virtio-blk-device`, not the real SD/eMMC path of the board.
3. **Slot R (recovery)**: `tools/boot.cmd` already branches by `active_slot=r` (E2) and
   the plain `/fat/BOOTMETA` that U-Boot reads already syncs at runtime (FIX A) — the
   "hardware" half of the A/B/R contract that Fable audit flagged as blocking is already
   resolved. No factory tool yet populates `image_size_r`/`image_crc_r`/`fw_version_r`, so R
   remains a non-real candidate until that is resolved — not part of this bring-up phase.

### What I Do NOT Recommend Testing Yet

- **Real flight or armed movement blindly trusting sensor/ina219 fixes without having
  confirmed them first on the bench** (Phase 2) — they are fixed by logic/formula, not yet
  confirmed against physical hardware.
- **The gripper** — deferred on purpose (dedicated timer design not started), will not move
  until that design exists.
- **Anything depending on the default assumed hart topology** (SMP with more than one hart,
  task rescue) without first confirming the real DTB (Phase -1) — the risk of non-contiguous
  harts is known and without FIX C.

### Note on Clock Constants Confidence (vf2)

- **`WDT_CLK_HZ` (24 MHz)**: high confidence. Confirmed via mainline Linux clock tree +
  JH7110's own watchdog driver — WDT's "core" clock is wired directly to 24 MHz crystal,
  bypassing the uncertain `APB_BUS` divisor.
- **`PWM_CLK_HZ`**: remains an unconfirmed assumption. Its only clock input goes through
  `APB_BUS`/`STG_AXIAHB`, supplied externally by boot firmware, and is not statically
  resolvable from the Linux driver — genuine research dead-end in this session, not a
  shortcut taken out of laziness.

### Note on `esp32c3`

Preexisting gap, unrelated to this session's work nor to VF2 bring-up — but
thoroughly investigated in a follow-up round after close (3 layers, first 2
fixed):

1. **`hart_start`/`send_ipi`/`set_timer` absent from `sbi` stub in
   `crates/arch-riscv64/src/lib.rs`** (used unconditionally by `api_impl.rs` on
   all riscv64 targets). **FIXED** — safe stubs added (single-core, no real SBI on
   esp32c3).
2. **`robot_os_ml`/`robot_os_camera` unconditionally called `robot_os_arch::vector`**,
   module that does not exist under `esp32c3`. **FIXED** — new `esp32c3` feature in
   each crate, portable scalar fallback.
3. **`crates/sched/src/process.rs` uses MMU/VMM unconditionally** (`robot_os_arch::mmu`,
   `robot_os_mm::{vmm, vdso}` — none exist under `esp32c3`). This is NOT a small fix
   like the previous two: it is deciding how "process" works on an MMU-less target, a
   real design decision. **Presented to owner, explicit decision: stop here and
   document** (esp32c3/HERMES paused as an active direction — VF2 is the hardware that
   arrived). **Full kernel build `--features esp32c3` STILL FAILS** for this reason —
   the first two layers were masking this third, deeper one.

Does not block anything in this VF2 bring-up plan.

---

## 10. One-Line Summary

**Kernel core: ready to power on.** **Sensor/actuator drivers: fixes applied in batch 7
(I2C0/UART1, GPS, barometer, IMU, ina219, motor PWM) and validated by
reading/primary source/compilation — none yet exercised against real hardware or
physical sensors, that is exactly what this plan covers.** Two gaps remain open on
purpose, non-blocking for the rest: **FIX C** (physical→logical hart_id mapping —
needs a real DTB as first deliverable, Phase -1) and **the gripper** (needs a
dedicated high-resolution timer design that has not started). Everything else is
known, documented, and should not surprise in the test bench.
