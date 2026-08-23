# Kernel review notes — continuous session

> **Note (2026-08-18).** The build target `esp32c3` discussed in several
> dated sections of this document was removed from the tree that day —
> it never compiled, was never in CI. It remains parked in
> `newfeatures/esp32c3/`. The sections below are a historical record
> session by session and have not been rewritten.

> Manual review, step by step, code by code, starting with the RISC-V
> loader and progressing through the kernel in boot order. Each section is
> a file or block reviewed, with what drew attention — bugs, asymmetries,
> stale comments, decisions to confirm. Not everything listed is a bug; the
> status of each point is marked.
>
> Status convention: `[OPEN]` pending decision/fix, `[CLOSED]` investigated
> and discarded or confirmed correct, `[BUG]` confirmed incorrect.

## 2026-07-17 — RISC-V: `linker.ld` + `boot.S`

Files: `kernel/linker.ld`, `kernel/src/entry/riscv64/asm/boot.S`.

### `linker.ld`

- Fixed kernel window of 8M at `0x80200000` (VF2 total RAM is 8GB) —
  **[OPEN]** confirm it's intentional (remaining RAM for userspace/heap
  managed separately) when we reach PMM/VMM.
- Each section (`.text.boot`, `.text`, `.rodata`, `.data`, `.bss`)
  aligned to 4K — wastes pages but likely preparation for W^X permissions
  per page. **[OPEN]** confirm in MMU.
- `_bss_end` forced to align to 8 with explicit comment why (clear with `sd`
  in boot.S). **[CLOSED]** correct and well documented.
- `ASSERT(_kernel_end < _stack_start, ...)` guards against image overflow
  onto boot stack. **[CLOSED]** correct.

### `boot.S`

- `boot_lock` lives in `.section .data` (not `.bss`) with explicit `.word 0`
  — necessary because used in `amoswap` for boot hart selection *before* BSS
  is cleared; in `.bss` it would have garbage at that instant. **[CLOSED]**
  correct, good detail.
- `amoswap.w.aq` (acquire only) for boot lock — correct semantics, no release
  needed because losing harts don't read anything written by the winner
  through this lock. **[CLOSED]**.
- `fence.i` after clearing BSS (line 47), with comment "ensure I-cache sees
  zeroed BSS". BSS is *data*, not instructions — `fence.i` synchronizes
  instruction fetch, shouldn't be needed here unless BSS overlapped with code
  to execute (it doesn't). **[OPEN]** appears vestigial/misplaced boilerplate;
  harmless but comment doesn't describe what actually needs it.
- `_secondary_start` sets `SSTATUS_SPP` with `csrs` (lines 92-94) without
  nearby `sret` — SBI HSM `hart_start` already starts the hart directly in
  S-mode, so the bit has no immediate effect. **[OPEN]** confirm if it's
  preparation for the first scheduler `sret` or vestigial.
- No bounds-check on `secondary_stacks` index (`hart_id * stack_size`) in
  lines 79-84. An out-of-range `hart_id` would index memory outside the
  reserved array. **[OPEN]** confirm that number of harts is bounded by
  Kconfig/DTB before `hart_start` is invoked with that hart_id.

### `tp` (thread pointer) — crosses with documented historical bug

See memory `feedback_smp_tp_fix.md` (SMP tp corruption, ~70 days ago).
Verified in `kernel/src/main.rs`:

- `kernel_main` resets `tp = hart_id` just before `robot_os_sched::start()`
  (line ~1428), with explicit comment that Rust may have used `tp` as a
  caller-saved scratch register during boot.
- `smp_secondary_start` resets `tp` on entry and in **every iteration** of
  the WFI loop, before each `wfi` (lines ~1456, ~1474) — necessary because
  SBI ecalls (`set_next_tick`) and timer ISR can corrupt it again.
- Grep of `current_cpu_id()` in `main.rs`: all real uses are in worker tasks
  running **after** scheduler startup (lines 1576+), never before the reset
  at line 1428. **[CLOSED]** `tp` handling is solid and well reasoned in
  comments; the initial `mv tp` in boot.S is correctly treated as "boot
  value only", not as a guarantee.

**[MINOR BUG — stale comment]** `crates/sched/src/smp.rs:58`, doc-comment
for `current_cpu_id()`:

```rust
/// `tp` is set to `hart_id` in boot.S for all CPUs (both primary and secondary).
/// Rust does not use `tp` in `no_std` bare-metal builds.
```

This directly contradicts what was discovered and fixed in the
`feedback_smp_tp_fix.md` fix and what the comments in `main.rs` say (lines
~1425-1427, ~1469-1472): Rust **can** use `tp` as a caller-saved scratch
register, which is why the constant resets are needed. This comment in
`smp.rs` is misleading to anyone reading it without the history — could
suggest that `tp` is safe without reset discipline. **Candidate for one-line
fix** (update comment to explain actual discipline), not blocking.

---

## 2026-07-19 — Secondary hart startup (SMP bring-up)

Files: `kernel/src/main.rs`, `crates/sched/src/smp.rs`,
`crates/sched/src/scheduler.rs`.

### 🟡 `MAX_CPUS` duplicated in two crates — **[OPEN]**

- `kernel/src/main.rs:141` → `const MAX_CPUS: usize = 4;`
- `crates/sched/src/scheduler.rs:25` → `pub const MAX_CPUS: usize = 4;`

Same name, same value, **unrelated**. Kernel uses its own to cap `num_cpus`
from DTB; scheduler uses its own to dimension `PER_CPU` (line 131) and
`PREEMPT_COUNT` (1461). Today they match → no live bug. If someone bumps
`main.rs` to 8, `NUM_ONLINE_CPUS` will be 8 and `PER_CPU[i]` with `i` up to
7 indexes out of a 4-element array → UB.

Asymmetry to note: `MAX_HARTS` **is** single source (defined in `main.rs`,
exported to `boot.S` via `.quad`); `MAX_CPUS` is manually duplicated.
`scheduler.rs` has defensive `.min(MAX_CPUS - 1)` at 710, 1343, 1373, 1473,
1490, 1504 — but `find_least_loaded_cpu` (255-256) indexes `PER_CPU[i]`
without that clamp.

### 🟡 `wake_harts` assumes contiguous hart IDs from 0 — **[OPEN]**

`crates/sched/src/smp.rs:36-43`:

```rust
pub unsafe fn wake_harts(num_cpus: usize) {
    let boot = current_cpu_id();
    for hart_id in 0..num_cpus {          // <-- contiguous IDs from 0
        if hart_id != boot { wake_hart(hart_id); }
    }
}
```

`num_cpus` comes from DTB as a **count**, not a set of IDs. In QEMU `-smp N`
this holds (which is why tests never expose it). On VF2/JH7110 hart 0 is the
monitoring S7 core and U74s are 1–4: with `num_cpus=4` starting at hart 1,
this loop would try to wake S7 and never wake hart 4.

**Concrete pending question (requires looking at real DTB, not code): does
the VF2/K1 DTB enumerate hart IDs contiguously starting at 0?**

**Partially closes** the open item in `boot.S` (lines 50-54 of these notes,
bounds-check of `secondary_stacks[hart_id]`): no OOB in practice because
`wake_harts` only passes `0..num_cpus` with `num_cpus ≤ MAX_CPUS = 4 <
MAX_HARTS = 8` — but **conditional on the same contiguity assumption**. Same
concern viewed from two places; treat as one.

### 🔴 `hart_start` fails silently → ghost CPU consumes tasks — **[BUG]**

Three points to read together:

1. `crates/sched/src/smp.rs:31-32` — SBI return code is discarded:
   ```rust
   let ret = robot_os_arch::sbi::hart_start(hart_id, entry, hart_id);
   let _ = ret;
   ```
2. `kernel/src/main.rs:1040` — the count is published **before** waking
   anyone (`wake_harts` is 200 lines later, at 1241) and never corrected:
   ```rust
   robot_os_sched::smp::NUM_ONLINE_CPUS.store(num_cpus, Ordering::SeqCst);
   ```
3. `crates/sched/src/scheduler.rs:251-263` — `find_least_loaded_cpu`
   iterates `0..NUM_ONLINE_CPUS` and picks the one with lowest `ready_bitmap`.

If a `hart_start` fails (`SBI_ERR_INVALID_PARAM`, `SBI_ERR_ALREADY_AVAILABLE`,
hart not present…), no one finds out. That ghost CPU never runs anything →
its `ready_bitmap` stays at 0 **permanently** → it's always least loaded →
**all** load-balanced tasks queue there.

No safety net: `grep "steal\|work_steal\|rebalance\|migrat"` over
`crates/sched/src/scheduler.rs` returns **nothing**. No work-stealing or
rebalancing → tasks stranded forever, no recovery. Field symptom: robot
starts, prints everything correctly, then does nothing — hard to diagnose
precisely because the original failure was silently swallowed.

**Proposed fix (not applied):** let `wake_hart` return SBI `isize`; let
`wake_harts` count successes and do `NUM_ONLINE_CPUS` **store after** with
the real number. Also fixes the odd order of publishing the count 200 lines
before attempting startup.

---

## 2026-07-24/25 — Multi-agent automated audit (robot-os, partial coverage)

17 parallel fronts (13 robot-os + 4 robot-brain), each finding verified by 2
independent adversarial agents (confirmed only if 2/2 say it's real). Original
plan included fixing confirmed critical 🔴 bugs and validating with
build+QEMU, but after the first failure, this batch was limited to search
and report only, without touching code — fixes decided in manual review.

**Incomplete coverage — there was a long period of API instability
("Connection closed mid-response", hanging agents) during ~1 day of
cumulative execution.** 8 of 17 fronts failed completely and were omitted:
`os-fs-ota`, `os-syscall`, `os-flight-nav`, `os-config`, and **all 4
robot-brain** (`brain-protocol`, `brain-server`, `brain-perception`,
`brain-fleet`). **robot-brain ended with 0 findings — Python side has not
been audited yet.**

**Important on "confirmed" vs "discarded" in this batch:** the threshold
required 2/2 votes. The same instability knocked out many verifications
mid-stream, so of the 19 unconfirmed, **15 have only 0 or 1 vote** (not
refuted, the second verifier simply didn't complete) and that single vote,
when it exists, mostly says the bug **is real** with detailed reasoning.
Only 3 were genuinely refuted (2/2 against) and 1 ended in genuine tie (1
for, 1 against). Treat the 15 as "probably real, pending second verification",
not as discarded.

### 🔴 Confirmed (2/2 verifiers agree) — 11

| File:line | Category | Summary |
|---|---|---|
| `crates/sched/src/scheduler.rs:251` | correctness-load-balancing | `find_least_loaded_cpu()` counts bits of `ready_bitmap` (occupied priority levels), not task count — contradicts its own comment "fewest ready tasks" and can send new tasks to the most loaded CPU. |
| `crates/mm/src/vdso.rs:105` | race-condition | `vdso_update()` assumes single writer but called by timer ISR of **every** hart in SMP; seqlock uses non-atomic load+store → readers can see corrupted time snapshot (torn read) across cores. |
| `crates/ipc/src/port.rs:119` | race-condition | `port_queue_event()` (IRQ context) and `port_poll()/port_bind()` (syscall context) mutate `pending[]` without lock — two concurrent harts on same port can stomp and lose an event silently. |
| `crates/drivers/src/pwm.rs:187` | correctness | On real VF2/K1, `pwm_set_duty_pct` reuses same `PWMCMP` register for period and duty; `motor_init` never calls `pwm_set_period` → motor PWM stays miscalculated from first startup on real hardware. |
| `crates/drivers/src/gpio.rs:139` | race-condition | Real MMIO path on VF2/K1 does read-modify-write of `GPIOOUT0/OEN0` without lock (QEMU simulation does protect it) — motors, payload actuator and camera share bank 0 → concurrent writes to different pins stomp. |
| `crates/drivers/src/pwm.rs:177` | error-handling | `pwm_set_duty` on VF2/K1 is a stub always returning `-1` never touching hardware; only caller (payload gripper) discards result → gripper never moves on real hardware, no warning. |
| `crates/ota/src/secure_boot.rs:208` | auth-verification-bypass | Ed25519 verifier for secure-boot (F18) is implemented but **no one calls it** at boot, OTA, or shell — with `secure-boot-enforced` on, unsigned or tampered image boots anyway. |
| `crates/ota/src/secure_boot.rs:235` | stack-overflow | `secure_boot_verify_slot()` declares 2 MiB buffer **on stack**, but all kernel stacks are 64 KiB (boot) or 2-16 KiB (tasks) — the moment this function (currently no caller) is wired up, overflow guaranteed. |
| `kernel/src/panic.rs:133` | concurrency-deadlock | Crash-log writer in panic handler calls `vfs_open/write/close`, taking global `FS` lock — contradicts "lockless" panic handler design; with `panic=abort` (no unwind), panic while `FS` held hangs handler forever. |
| `crates/ota/src/secure_boot.rs:208` (dup) | unenforced-security-control | Same finding as above, verified independently by another finder — reinforces it's real. |
| `kernel/src/main.rs:544` | discarded-result | CRC-32 of active slot at boot only logged (OK/WARNING); mismatch **does not** trigger rollback to `last_good` — corrupted slot after valid OTA (flash bit-rot) still boots. |

### 🟡 Probably real — only 1 (or 0) of 2 verifiers completed, not refuted — 15

| File:line | Severity | Votes | Summary |
|---|---|---|---|
| `kernel/src/entry/riscv64/asm/context_switch.S:83` | 🔴 critical | 1/1 real | `tp` (hart identity) saved/restored as task context; EDF scheduler can migrate deadline task to different physical hart → restored `tp` doesn't match actual CPU, corrupting `current_cpu_id()` and per-CPU state. **Directly crosses with what we were investigating about `tp`** — review alongside [[feedback_smp_tp_fix]]. |
| `crates/sched/src/smp.rs:29` | 🔴 critical | 1/1 real | `wake_hart()` discards error code from `sbi::hart_start()` — hart failing to start stays treated as online forever. **Exactly the same bug we manually documented** in "Secondary hart startup" section above — confirmed independently by auto-audit. |
| `crates/sync/src/waitqueue.rs:91` | 🔴 critical | 1/1 real | `WaitQueue::wait()` checks condition, then enqueues without re-check after enqueue — classic lost-wakeup if wake arrives between check and enqueue. |
| `crates/sched/src/scheduler.rs:1373` | 🔴 critical | 1/1 real | `try_wake_task()`/`wq_wake_by_tid()` do `cpu_enqueue` on CPU different from caller, without taking `CPU_LOCKS[target_cpu]` — race with that CPU's `do_schedule()` on same queue. |
| `crates/sched/src/scheduler.rs:1202` | 🟡 warning | 1/1 real | `boost_ready_task()`/`restore_ready_task()` (lease priority inheritance) reposition on calling CPU's queue, not task's real owner — logic bug, latent today because feature gated off by default. |
| `crates/sched/src/scheduler.rs:617` | 🔴 critical | 1/1 real | `task_exit()` never frees `user_pt` or its physical pages — memory leak on every userspace task exit (repeated fork/exit, drivers restarting). Already seen in first auto-audit pass. |
| `crates/mm/src/vmm.rs:464` | 🟡 warning | 1/1 real | `enforce_wx()` only protects kernel image; heap, PMM pages and page tables stay `KERNEL_RWX` permanently — W^X hardening gap. |
| `crates/sched/src/process.rs:416` | 🟡 warning | 1/1 real | `sys_fork_impl()` discards `child_pt` from `fork_cow()` on failure without freeing, and `fork_cow` doesn't rollback COW pages already marked in parent — leak + orphan refcount. |
| `crates/ipc/src/shm.rs:78` | 🔴 critical | 0/2 (both verifiers crashed) | `shm_create()` mutates `static mut SHM_REGIONS` without lock, reachable from syscall on any hart — same race pattern as rest of `crates/ipc`. |
| `crates/ipc/src/irq_bind.rs:72` | 🔴 critical | 0/2 (both crashed) | `irq_bind()`/`irq_unbind_all()` (syscall context) mutate `static mut IRQ_BINDINGS` that `irq_dispatch()` (PLIC IRQ context) reads concurrently without lock. |
| `crates/ipc/src/handle.rs:89` | 🔴 critical | 1/1 real | `handle_grant/revoke/dup` on `static mut HANDLES` without lock, wired direct to syscalls — multi-hart race. |
| `crates/ipc/src/io_ring.rs:118` | 🟡 warning | 1/1 real | `io_ring_create()` scans/writes `static mut IO_RINGS` without lock, reachable from syscall on several harts. |
| `crates/ipc/src/rpc.rs:72` | 🟡 warning | 1/1 real | `rpc_register/reply/get_reply` on `static mut RPC_PENDING` without lock, reachable from `SYS_IPC_CALL/REPLY`. |
| `crates/net/src/tcp.rs:965` | 🔴 critical | 1/1 real | Incoming RST accepted and tears connection down **without checking sequence number** (RFC 793 §3.4 / RFC 5961) — unlike all other incoming segment paths in same file. Possible DoS from spoofed RST. |
| `crates/net/src/tcp.rs:721` | 🔴 critical | 1/1 real | MSS negotiation records peer MSS but never clamps to local capacity (`TCP_MSS`/`retx_buf`) — `send_data()` can exceed what retransmit buffer supports. |

**Note — `crates/ipc/*` as a block**: 5 of 6 findings in `os-ipc` dimension
are the same bug family (global `static mut` table without lock, mutated from
syscall and sometimes IRQ) in `shm.rs`, `irq_bind.rs`, `handle.rs`,
`io_ring.rs`, `rpc.rs`. Before fixing them one by one, worth checking if
`channel.rs`/`pubsub` (which do use `SpinLock` per suggested fix) have a
reusable pattern, or if we need a macro/helper to avoid repeating same lock
5 times.

### Reviewed and discarded / in genuine dispute (2/2 or split) — 4

- `crates/mm/src/vmm.rs:445` (`map_mmio_region`) — **[REFUTED 2/2]**:
  discards `Result` of each internal `map()` and always returns `Ok(())`,
  but both verifiers confirmed no call site in `kernel_main()` can
  realistically fail there (fixed MMIO range, already validated). Discarded
  as real bug, though code still throws away an error signal — candidate
  for style cleanup, not bug.
- `crates/drivers/src/ina219.rs:71` — **[REFUTED 2/2]**: discards I2C
  results in init/read, but both verifiers concluded no realistic production
  failure scenario documented.
- `crates/trace/src/lib.rs:169` — **[REFUTED 2/2]**: `record()` uses
  non-IRQ-safe `SpinLock` but real trace_irq never activates on path
  executing from IRQ today (feature-gated/not wired) — correct as-is, risk
  only if that path activates later.
- `kernel/src/main.rs:3853` (`wdt_kick` coverage) — **[IN GENUINE
  DISPUTE, 1-1]**: one verifier says "no detects scheduler hangs" framing
  doesn't hold scrutiny, other confirms the gap end-to-end including
  interrupt state during hypothetical hang. Needs third opinion or
  discussion in manual session.

---

## 2026-08-02 — Workflow script audit + full relaunch

Before relaunching automated audit, the script itself (`full-audit-os-brain-wf_c7cbb828-4ab.js`) and its anti-stall fix were audited:

- 🔴 **[FIX BUG — corrected]** `NO_SLOW_COMMANDS_NOTE` ordered wrapping commands
  in `/usr/bin/timeout 25`, but **the `timeout` binary doesn't exist on this
  machine** (no `gtimeout`, no Homebrew coreutils — verified with `ls`/exit
  127). Each attempt would have failed with exit 127. `SHELL_NOTE` also listed
  it as available. **Fix applied**: both notes now point to native Bash `timeout`
  parameter (ms) and warn explicitly the binary doesn't exist. `[CLOSED]`
- 🟡 **[CLOSED]** `resumeFromRunId: "wf_c7cbb828-4ab"` in continuation prompt
  was useless: resume is intra-session, that ID not in runs history, cache
  invalidated by prompt changes. Launched fresh with `scriptPath` only.
- ✅ Verified correct: interpolation of `NO_SLOW_COMMANDS_NOTE` in
  `finderPrompt()` and `verifyPrompt()`; fix for `null.findings` crash
  (`(result && result.findings) || []`); vote counting tolerant of crashed
  verifiers (0-1 votes ≠ refuted); explicit phases per agent (no global `phase()`
  race); **all 53 robot-os paths and 22 robot-brain paths referenced by 17
  dimensions exist**.
- 🟡 **[OPEN]** Large dimensions (e.g., `os-flight-nav`, 8 crates) use single
  finder — risk of silent truncation by context. Decided not to split yet; if
  this pass shows loose coverage there, split by subdirectory next.

**Relaunched full audit (17 fronts, 13 robot-os included)**: run `wf_1edef0a7-3dd`,
2026-08-02, in background. First real execution of anti-stall fix. Result pending.

---

## 2026-08-02/03 — Run `wf_1edef0a7-3dd`: credit limit crash; `os-drivers` harvest

Real result from relaunch on 2026-08-02 (17 fronts, anti-stall v2 fix):

- **Of 47 agents, only 1 completed**: the `os-drivers` finder (26 min, ~203k
  tokens) → **15 new candidates, 0 verified** (0/2 votes all). 
- Other 16 finders and 30 verifiers died. Dominant cause: **session credit
  limit** ("session limit · resets 2:20am Europe/Madrid"). Plus: 1 "Connection
  closed mid-response" (`find:os-ipc`).
- **Note — anti-stall v2 fix NOT validated**: stalls occurred again BEFORE
  credit exhaustion (`os-mm`, `os-safety`, `os-sched-smp`, `os-security`,
  `os-net`, `os-rt-wcet` — no progress 1100-4900 s, retries exhausted).
  Indistinguishable whether original problem or API degrading near limit.
  Hypothesis still unconfirmed.
- Cost of failed run: ~1.6M subagent tokens over 3h09m.
- Cumulative coverage after this run: `os-drivers` found-but-unverified; other
  12 robot-os fronts still depend only on old run `w8y011az8` (partial);
  **robot-brain still at ZERO**.
- Mitigation applied to script for next: batch support `args: {only:
  ['os-mm', ...]}` to relaunch in batches without burning whole session credit
  limit or re-paying `os-drivers`.

### 🔍 `os-drivers` candidates — 15, ALL UNVERIFIED (0/2 votes)

Found by single finder reading code; none yet confirmed by adversarial
verifiers. First 5 are `critical` per finder. Suggested fixes in run journal
(see path below).

| File:line | Sev. | Category | Summary |
|---|---|---|---|
| `crates/gps/src/lib.rs:394` | 🔴 | correctness | `parse_nmea_coord` inflates minute fraction ×10 (`min_frac as u64 * 10` when already at 10^7 scale). With `$GPGGA,...,4808.1062,N`: returns 48.1510° instead of 48.1351° (verified numerically by finder) → up to ~15 km position error in real navigation. |
| `crates/baro/src/lib.rs:163` | 🔴 | correctness | BMP280 compensation omits `+ (dig_p7 << 4)` term from datasheet before Q24.8→Pa conversion. With repo's simulated calibration: 99684 Pa vs correct 100653 Pa → ~969 Pa bias ≈ +80 m altitude feeding altitude-hold. |
| `crates/drivers/src/ina219.rs:127` | 🔴 | correctness | mAh integration sums integer `current_ma` per sample at 10 Hz instead of `current_ma/3600` (comment says so) → consumption inflated ×3600. With real 1 A: battery "depletes" in ~3.6 s → `ina219_failsafe_level()` = 4 (KILL) → cuts motors in flight seconds after arming. |
| `crates/drivers/src/platform.rs:96` | 🔴 | mmio-collision | On VF2, `I2C0_BASE` (line 79) and `UART1_BASE` (line 96) are both `0x1001_0000`: DW-I2C and UART1 bridge (ESP32/WiFi) program same MMIO region → neither sensor I2C nor WiFi link work. On real JH7110, 0x10010000 is uart1; i2c at 0x10030000+. |
| `crates/drivers/src/virtio/blk.rs:118` | 🔴 | race-condition | `blk_rw` uses global `BLK_DEV.vq`/`BLK_REQ_HDR`/`BLK_STATUS` without lock (virtio/net.rs does use SpinLock). fat32 calls blkdev outside its cache lock and msc_gadget enters directly → on SMP, two harts corrupt descriptor chain → wrong sector reads / silent FS corruption. |
| `crates/drivers/src/wdt.rs:113` | 🟡 | correctness | `hw_wdt_init` calculates ticks with `TIMER_FREQ` (4 MHz on VF2) when DW-WDT runs from APB at ~24 MHz (documented in same file) → real timeout ~167 ms instead of 1000 ms → with 500 ms kick, board resets in loop in production. On K1 coincidentally matches (TIMER_FREQ=24 MHz). |
| `crates/drivers/src/virtio/net.rs:193` | 🟡 | missing-timeout | `send()` waits for TX completion in unbounded `loop` holding SpinLock NET → dead device = deadlock of entire network stack. blk.rs has timeout for same pattern; net doesn't. |
| `crates/drivers/src/virtio/net.rs:230` | 🟡 | memory-safety | `poll_recv` uses used ring `len` without bounding to `RX_BUF_SIZE` before indexing `rx_bufs[slot][..]` → defective/malicious device with len>1526 = panic. Comment on `virtq_poll_with_len` says it validates, but only validates `id`. |
| `crates/drivers/src/i2c.rs:316` | 🟡 | hardware-protocol | dw_i2c (VF2): writes `IC_TAR` with controller enabled (DW ignores unless DYNAMIC_TAR_UPDATE), queues commands without checking TX FIFO (26 baro calibration reads overflow 8-16 FIFO) or TX_ABRT; `i2c_read` returns partial count (including 0) as success. |
| `crates/drivers/src/mmc.rs:345` | 🟡 | correctness | SD capacity invented (fixed 8 GiB SDHC / 2 GiB SDSC) instead of reading CSD (CMD9) → with 4 GB card, FAT32/OTA/MSC accept LBAs past end → intermittent errors or lost writes. |
| `crates/drivers/src/virtio/mod.rs:243` | 🟡 | fragile-assumption | `virtq_init` assumes N consecutive `pmm::alloc_page()` return physically contiguous pages — with freed hole before or concurrent alloc from another hart, `QUEUE_PFN` points to corrupt layout → device DMA writes to someone else's memory. |
| `crates/drivers/src/eth.rs:294` | 🟡 | race-condition | `eth_send`/`eth_recv` (MACB VF2) claim descriptors with separate `TX_HEAD`/`RX_HEAD` load/store without atomic RMW or lock → two harts grab same descriptor → frame lost/corrupt or delivered twice. |
| `crates/drivers/src/gpio.rs:154` | 🟡 | correctness | GPIO VF2: `BANK1_OFFSET=+0x08` applied to `GPIO_DIN0` too (0x50→0x58) but same map documents GPIOIN1 at 0x54 → `gpio_read` of pins 32-63 reads wrong register. Verify against JH7110 TRM. |
| `crates/imu/src/lib.rs:58` | 🟡 | silent-error | `imu_init` ignores return values of 5 `i2c_write` config calls and marks `IMU_READY=true` unconditionally after WHO_AM_I → NACK on PWR_MGMT_1 leaves MPU-6050 in SLEEP and AHRS consumes zeros as valid attitude. Same pattern in `baro_init` (baro/lib.rs:103-104). |
| `crates/baro/src/lib.rs:126` | 🟡 | hardware-protocol | `baro_read` triggers forced-mode and reads 0xF7 immediately without waiting for conversion (~40 ms with osrs x2/x16) or checking `measuring` bit (0xF3) → always one sample delayed, first read gets reset data → spurious altitude jump when altitude-hold starts. |

Full detail (failure_scenario + suggested_fix per finding) at:
`~/.claude/projects/...-ia-robot-os/f7d83d9a-91da-44bf-b83d-dab426e753f6/subagents/workflows/wf_1edef0a7-3dd/journal.jsonl`
and `/private/tmp/claude-501/...f7d83d9a.../tasks/w3cntq29d.output` (latter is
tmp — may disappear; journal and this table are persistent).

Cross-cutting note: `imu_init`/`baro_init`/`i2c_read` family with already
confirmed "error silently ignored" findings (pwm stub, wake_hart) — pattern
"driver init ignores bus errors and marks READY" repeats; consider pattern fix,
not patch-by-patch.

---

## 2026-08-07 — "AI OS" surface: what from that wave applies to this kernel

Conceptual section, not code review. Context: the 2026 "AI OS" wave (Operator,
NeuralOS, AIOS/Rutgers, Phind OS…) is **entirely application layer** — browser
agents, userspace runtimes, generative UIs. None touch a kernel. Question posed:
*what can a kernel contribute to that layer, and what makes sense here?*

Thesis: the agent layer has an **authority** problem, not an intelligence one.
Today its security model is "the model decides well". A kernel converts that to
"the model can't do anything else" — and prompt injection doesn't scale a
capability never granted.

Evaluated 6 candidate primitives. **2 apply, 4 don't.** And the 2 that apply
are *finishing what's already started*, not new architecture.

### ✅ Applies — deadline contract over the brain

The brain (Python + VLM/LLM over TCP) **already is** a non-deterministic AI
component giving orders to a deterministic kernel. Missing: check if kernel
*enforces* a time budget or just trusts.

Concrete questions to resolve (pending 10):
- What does kernel do if brain takes 2 s, replies garbage, or dies mid-maneuver?
  Hold last command? Wait indefinitely?
- Is there watchdog on the link with fallback to deterministic controller?
- **RFC-0037 (graceful degradation mode) still unwritten.**

Scoped, testable in QEMU without hardware, directly useful for HERMES (nodes
that degrade when link fails).

### ✅ Applies — authority envelope of brain (cap-IPC on the link)

Reframe: **brain is the AI layer and binary link is the syscall boundary.**
cap-IPC (RFC-0003) already built; missing check whether it's applied at the
boundary that matters.

Concrete questions to resolve (pending 11) — can the brain, sending well-formed
packets, do something it should never do?
- Can it initiate OTA without separate authority? (`PKT_OTA_BEGIN/CHUNK/END`)
- Can it raise a limit via `PKT_CONFIG` above the security envelope?
- Can it disable or downgrade L0-L3 layers?
- Is cap-IPC applied on link boundary or only intra-kernel?

If any answer is "yes", security model against the AI component is "trust
brain behaves" — same model criticized for the agent layer.

### ❌ Doesn't apply

- **Inference scheduling with deadlines/reserves**: inference runs on Mac;
  neither VF2 nor ESP32-C3 have NPU to schedule. Problem we don't have.
- **Attenuated delegation / revocation for sub-agents**: no sub-agents in robot.
  Forcing it would invent the problem.
- **Provenance at syscall level**: moderate value, real flash cost. Parked,
  not discarded.
- **IFC / taint tracking** (newest on the list): research project in itself.
  **Keep the idea**: in HERMES, a defective or compromised node emitting false
  location is real threat model, and "untrusted peer data can't trigger
  privileged action" is the narrow useful version. Not now.

### ⚠️ Prioritization note

This **isn't a new direction** — it's ammunition to prioritize the fix plan
(pending 8). The two things that apply are half-designed pieces waiting for
someone to wire them, exactly same pattern as `secure_boot.rs:208` (built
Ed25519 verifier **no one calls**). Adding new layer on top of 11 confirmed
bugs + 30 unverified candidates would repeat that error at scale. Wire first,
expand after.

---

## 2026-08-07 — Fix batch 1 (4 sonnet agents + validation)

First real fix batch on the 11 confirmed 2/2 bugs. Four sonnet agents editing
in parallel on disjoint files, with explicit ban on compiling (to avoid
serializing on `target/` lock, suspected in historical stalls). Compilation
and diff review done by strong model afterward. **No commit** — all in working
tree.

### Results per bug

| Bug | Status | Detail |
|---|---|---|
| `crates/drivers/src/gpio.rs:139` RMW no lock | ✅ **FIXED** | `GPIO_MMIO_LOCK: SpinLock<()>` in `mod mmio`, taken in `gpio_set_direction`/`gpio_write`/`gpio_toggle`. Mirrors pattern `mod sim` already used. |
| `crates/ipc/src/port.rs:119` IRQ/syscall race | ✅ **FIXED** | `static mut PORTS` → `static PORTS: SpinLock<[Port; MAX_PORTS]>`, 7 accessors to `lock_irqsave()`. **First use of `lock_irqsave()` in entire repo** (helper existed unused). |
| `crates/sched/src/scheduler.rs:251` count bits | ✅ **FIXED** | Sum `ready_queues[p].count` over `NUM_PRIORITIES` instead of `ready_bitmap.count_ones()`. |
| `crates/sched/src/smp.rs:31` SBI error discarded | ✅ **FIXED** | `wake_hart` returns `isize`; `wake_harts` returns live prefix and logs each failure. |
| `crates/drivers/src/pwm.rs:177` dead gripper | 🟡 **PARTIAL** | `-1` no longer swallowed: propagates and logged in `payload_init`/`payload_gripper`. Real MMIO path still unimplemented. |
| `crates/drivers/src/pwm.rs:187` PWMCMP reused | ❌ **NOT FIXED** | Documented with doc-comment. Requires JH7110 TRM to know if 2nd duty comparator exists and its offset. Agent correctly refused to invent registers. |

### Validation done

- **6 configs compile, exit 0**: default, vf2, k1, no-ml, no-mmu, qemu. Zero
  compiler warnings (only `warning:` in log is informational from `robot_os_ota`
  build script embedding pubkey, preexisting).
- **Boot in QEMU `-smp 4`, 45 s continuous**: no panic, fault, deadlock,
  assertion. Tasks spread across 4 harts (behavior→2, rt-motor→0, net-poll→3,
  sensor-ahrs→1, flight-ctrl→0). Scheduler starts, workers run, `timer_isr`
  fires 10.792 times with **0 WCET violations**. New IRQ-safe lock actually
  exercised (IRQs active throughout).
- No line `[SMP] WARNING: only x/y harts started` → the 3 secondary harts
  started and `NUM_ONLINE_CPUS` correction was no-op, correct for healthy QEMU.

### 🔴 PENDING DECISION — `NUM_ONLINE_CPUS` and boot tasks

Agreed fix said "move `store` after `wake_harts`". Agent **didn't move it**:
left original optimistic store and **added second corrective store** after.
Reason: ~15 unpinned `task_create` calls between store (1040) and `wake_harts`
(1246); if value stayed at initializer (`1`) during those, **all** boot tasks
would collapse to CPU 0.

Consequence to keep conscious: **original bug still lives for boot tasks.** If
hart fails, boot tasks assigned to it stay stranded forever. Correction only
protects what's created *after* `wake_harts` (basically `fork()`). I.e., the
original field symptom ("robot starts, prints fine, then does nothing") **isn't
fully closed**.

Options on table:
- (a) **As-is**: boot tasks load-balanced; if hart dies, some strand. Fast in
  healthy case, silent in failure.
- (b) **Actually move the store**: all boot tasks to CPU 0 (definitely alive).
  Never strand, but lose SMP distribution at startup — performance regression
  **certain and permanent** in healthy case, dangerous in kernel with WCET
  budget and documented starvation history on net-poll/hart-2.
- (c) **Rebalance post-`wake_harts`** (not implemented, probably correct): at
  line 1246 scheduler hasn't started yet and boot hart still owns all queues —
  only instant you can safely drain ready-queues of dead harts to live ones,
  no race. Closes gap without penalizing healthy case.

Agent chose (a) and declared it explicitly. **Pending user decision.**

### Agent wins beyond the charge

`wake_harts` returns **not** a success count but the **live prefix**: `find_least_loaded_cpu`
uses `NUM_ONLINE_CPUS` as bound (`for i in 0..num_online` indexing `PER_CPU[i]`),
not cardinal. With hole in middle (hart 2 dead, hart 3 alive), publishing "3"
would still route to dead hart 2 never reaching alive 3. So count stops growing
at first failure even though we keep trying to start all. Conservative (live
hart past hole sits idle) instead of unsafe. This improves the fix designed in
these notes.

### 🆕 New findings from fixes (unverified)

- 🔴 **K1 has no MMIO path in PWM.** `mod sim` is `cfg(not(vf2))` and `mod mmio`
  is `cfg(vf2)`: **K1 build falls into in-memory simulation** and never touches
  hardware, even though `pwm_driver.rs` announces MMIO range for `any(vf2, k1)`
  and `platform.rs` defines K1 `PWM_BASE`/`PWM_STRIDE`. Not fixed deliberately:
  K1 is `spacemit,k1x-pwm`, different IP from SiFive on JH7110 — widening the
  `cfg` would misprogrammed real hardware, worse than no-op. **The premise
  "real VF2/K1 path" is false for K1: no such path exists.**
- 🟡 **`platform.rs` has dead conflicting PWM constants.** `PWM_DUTY` (0x04) /
  `PWM_PERIOD` (0x08) / `PWM_ENABLE` (0x0C) with comment "Reference: JH7110
  TRM", never referenced, describe register map **incompatible** with
  `PWMCFG`/`PWMCMP`/`PWMSCALE` that `pwm.rs` uses (doesn't even have prescaler
  register). One of the two is wrong.
- 🟡 **Possible underflow in jitter report.** QEMU boot prints `[JITTER]
  timer_isr min_ns 0 max_ns 1844674407370`. That value suspiciously is
  `u64::MAX / 10^7` — smells like subtraction that wraps (`a - b` with `b > a`)
  in jitter calc. **We didn't touch it** (no file in this batch touches
  telemetry/jitter), so preexisting. Verify.
- 🟡 **`timer_isr` max 72.100 µs and 0 WCET violations.** 72 ms max for timer
  ISR, with budget approving it, suggests budget too lax — or QEMU TCG substrate
  makes measurement worthless (already documented in `project_bench_measurement_substrate`).

### Process note

One agent claimed in report that `payload_exec` **had no callers**. False:
two exist (`kernel/src/main.rs:2824` and `:3008`). Conclusion drawn (its
`true`→`false` change was harmless) holds anyway because both discard return
value, but by accident not by analysis. **Always verify agents' factual claims,
not just the diff.**

Another agent corrected the charge: the `pubsub` module these notes cited as
`SpinLock` precedent in `crates/ipc` **doesn't exist**. Real precedents are
`channel.rs` (`POOL`) and `signal.rs` (`SIG_TABLE`), both with plain `lock()`
because not reachable from IRQ. Corrected here.

---

## 2026-08-07 — Fix batch 2 + full validation

Three more sonnet agents on remaining 2/2 confirmed. Same protocol: agents edit
without compiling, strong model compiles and reviews. **No commit.** Full
working tree backup at `/tmp/fixes_tanda1_2.patch` (964 lines).

| Bug | Status | Detail |
|---|---|---|
| `crates/mm/src/vdso.rs:105` multi-writer seqlock | ✅ **FIXED** | Serializes writers with `compare_exchange` on the `seq` itself. No lock, no needing hart identity. |
| `crates/ota/src/secure_boot.rs:235` 2 MiB buffer on stack | ✅ **FIXED** | Buffer to `.bss` (shell's `ELF_BUF` pattern) + FAT32 read in 4 KiB chunks. ~2 MiB frame → ~121 bytes. |
| `kernel/src/panic.rs:133` deadlock from `FS` lock | ✅ **FIXED** | `write_crash_log` and `trace_dump` do *peek* with `try_lock` and skip dump if occupied. New `vfs_fs_lock_available()` / `fat32_locks_available()`. |

### Why vdso fix doesn't use hart identity

Offered two options (gate on hart 0, or IRQ-safe lock) and **refused both with
good reason**. Gating on `tp`/`hart_id()` would be unsafe **in this specific
kernel**: finding `context_switch.S:83` in these same notes documents `tp`
saved/restored as task context and EDF scheduler can migrate task to different
physical hart, leaving obsolete `tp`. Hart with inherited `tp == 0` would
believe it's sole writer and **silently reopen the race**. IRQ-safe lock
rejected for cost: runs in timer ISR, every tick, every hart, within WCET
budget.

Added on own a guard against sample regression: `uptime_ticks`/`uptime_ms`
sampled by *caller* before CAS resolves, from two sources with different
times, so lagging hart could publish older over newer sample and regress a
field declared monotonic. Now requires sample dominates **both** fields.

### 🆕 New bug CONFIRMED (verified by hand) — `crates/libsys/src/lib.rs:252`

`vdso_uptime_ticks()` does `vdso_read_u64(8)`, but layout comment in same file
says `+8 seq / +12 _pad / +16 uptime_ticks`. **Reading seqlock counter, not
uptime.** Since `seq` advances by 2 per publish, value *looks* like growing
counter — why never noticed. `vdso_uptime_ms()` (offset 24) is correct.
**One-line fix: `vdso_read_u64(16)`.** Not applied.

### ⚠️ CORRECTION to batch 1

Warned moving 2 MiB buffer to `.bss` would cost half kernel window. **Measured:
real cost today is ZERO.** `.bss` stays exactly 2.625.272 bytes before and
after. `IMG_BUF` is local `static` of function with no callers, so linker
eliminates it entirely. **2 MiB cost is latent: appears when
`secure_boot_verify_slot()` wired**, then margin would go from 3.67 MiB to
~1.67 MiB (still fits). So fix can stay as-is paying nothing while function
unwired.

### 🔴 REGRESSION WE INTRODUCED — panic handler on vf2

`panic()` calls `motor_stop(0)`/`motor_stop(1)`/`esc_disarm()` on lines 36-38,
**before first `uart::puts` at line 41**. Chain is:

```
motor_stop → motor_set → MOTORS.lock()      (blocking)
                       → gpio_write()        (blocking)
                       → pwm_set_duty_pct()  (blocking)
```

If another hart holds any of those locks when panic hits, **entire panic
message lost**. File comment falsely claimed `motor_stop` "only atomic writes";
agent corrected it.

**Our part**: before batch 1, `gpio_write` in vf2 MMIO path **had no lock**
(racy but not blocking). Our `GPIO_MMIO_LOCK` adds new blocking point in panic
handler. On QEMU nothing changes (sim already locked) → **regression exclusive
to real hardware**. Charge asked check task and IRQ context; no one looked at
third context, panic handler, where blocking forbidden. Options: (a) emergency
path without locks (`gpio_write_raw`/`motor_stop_panic`) — recommended; (b)
print before stopping actuators; (c) `try_lock` with fallback no lock —
reintroduces race at contention. **Pending decision.**

### Validation executed

- **6 configs compile, exit 0**: default, vf2, k1, no-ml, no-mmu, qemu. **Zero
  compiler warnings.**
- Kernel window: end of `.bss` at `0x80654ef8`, limit `0x80a00000` → **3.67 MiB
  margin**. Linker ASSERT comfortable.
- **8 boots in QEMU `-smp 4`** (1×45 s batch 1, 1×50 s, 3×25 s, 4×50 s): **7
  clean, 1 panic** (see below).

### 🔴 INTERMITTENT PANIC — `crates/sched/src/scheduler.rs:244`

One of eight boots ended in:

```
!!! KERNEL PANIC !!!
  at crates/sched/src/scheduler.rs:244
  hart=0 task=i3-spin
[PANIC] UART lock busy — skipping trace dump
```

Line 244 is `&mut TASKS[idx]` inside `task_mut()` → **index out of range in
`TASKS`**. NOT the new `find_least_loaded_cpu` loop (lines 265-267). `i3-spin`
created at `main.rs:2322` pinned to CPU0.

**Not reproducible on demand: 1 of 8.** With 4 more 50 s boots after observing
it, 0 repetitions. Can't attribute to our changes with available evidence;
distinguishing "introduced" from "preexisting" at ~1/8 rate would need dozens
of boots per branch.

**Suspicious mechanism number one**: garbage index in ready queue can only come
from someone corrupting that queue. That's exactly the already-logged 🟡 finding
**`crates/sched/src/scheduler.rs:1373`** — `try_wake_task()`/`wq_wake_by_tid()`
do `cpu_enqueue` on different CPU from caller **without taking
`CPU_LOCKS[target_cpu]`**. Our `find_least_loaded_cpu` change alters task
distribution across CPUs, so **changes exposure to that race without creating
it**.

If that reading correct, this is the **first observed execution symptom** of one
of 15 unverified findings — elevates from "probable" to "real trace exists".
Natural next step: fix `scheduler.rs:1373` (take `CPU_LOCKS[target]` on remote
enqueue) and soak again.

**Bright side**: panic handler fix from this same batch worked — line `[PANIC]
UART lock busy — skipping trace dump` proves it detected lock held and
continued instead of hanging. Before this fix, that boot would have spun silent
with no diagnosis.

---

## 2026-08-07 — Fix batch 3: cross-CPU scheduler race + panic without locks

Two more agents (one per domain, disjoint files) + one manually applied fix.
**No commit.** Working tree: 18 files, 803 insertions.

### ✅ `crates/sched/src/scheduler.rs` — cross-CPU access to ready queues

Charge was finding `:1373` (`try_wake_task`/`wq_wake_by_tid` do `cpu_enqueue`
on another CPU without `CPU_LOCKS[target]`). **Found five places, not one**, and
important one wasn't in the charge:

1. `try_wake_task()` — remote enqueue no lock.
2. `wq_wake_by_tid()` — same pattern.
3. **`do_schedule()` — its own `cpu_dequeue`/`cpu_enqueue` on its CPU's queue
   also didn't take `CPU_LOCKS[cpu]`**, running unprotected exactly against
   remote enqueues from (1) and (2) pointing at same queue. **This is route
   explaining `TASKS[idx]` out of range panic.**
4. `start()` — `cpu_dequeue` no lock, raceable against `task_create` on CPU
   not yet called `start()`.
5. `task_create_affinity()` — already did it right; just simplified wrapper.

Design: `CpuLockGuard` changes from simple spin to **irqsave**; wrappers
`cpu_dequeue_locked()` / `cpu_enqueue_locked()` acquire and release within
their own function, so lock **never** held entering `context_switch()` (which
may never return). Only one lock at a time in whole file → no AB-BA scenario,
no total order needed.

**Relevant side finding**: `block_current()` calls `do_schedule()` **without
disabling interrupts**, contradicting file's invariant comment ("must be called
with interrupts disabled"). Comment was **false** and corrected. Why fix uses
`irqsave` on all `CPU_LOCKS`, not just obviously-IRQ paths: survives regardless
whether that invariant holds per caller.

**Deliberately left untouched**: `boost_ready_task`/`restore_ready_task`
(:1202), now with `KNOWN ISSUE` comment. Real bug there is *targeting* (use
`current_cpu_id()`, caller's CPU, not task owner's); adding lock without fixing
that protects wrong queue. Gated off by default. Also deliberately unlocked
`cpu_peek_highest_prio()` — RT preemption heuristic, read single aligned `u32`,
staleness of one tick.

### ✅ Emergency path without locks in panic handler

Closes regression we introduced in batch 1. New functions, each in two platform
variants (`sim` = `cfg(not(feature="vf2"))`, `mmio` = `cfg(feature="vf2")`):

- `gpio::gpio_write_panic()` — skips `GPIO.lock()` (sim) / `GPIO_MMIO_LOCK` (mmio).
- `pwm::pwm_set_duty_pct_panic()` — skips `PWM.lock()` (sim). In mmio is alias:
  **that path already lock-free**, no `SpinLock` in pwm mmio module. Alias
  just avoids `cfg` at call site.
- `esc::esc_disarm_panic()` — same as `esc_disarm()` but without final
  `kprintln!`, which alone blocked (takes `UART_LOCK`).
- `motor::motor_stop_panic()` — reads `MOTORS` with `get_mut_unchecked` instead
  of `.lock()`, doesn't rewrite `direction`/`speed_pct` (unnecessary: machine
  stops right after).

Each documents that it **sacrifices mutual exclusion deliberately** and carries
explicit "don't fix this by adding lock", so no one reverts it in good faith
in six months.

**Verified, not assumed**: `uart::puts` → `putc` → `putc_raw` **doesn't touch
`UART_LOCK`** in either implementation (ns16550a / esp_uart); only waits for
transmitter-ready bit from hardware. `UART_LOCK` taken only by `kprint!`/`kprintln!`
via `uart::acquire()`.

### 🟡 Panic handler points that CAN STILL BLOCK (preexisting)

None before panic message, so guarantee "panic reason always reaches UART"
holds. Only affect what prints after:

- **`blk_rw()` in `crates/drivers/src/virtio/blk.rs`** calls `kprintln!` in
  four error branches (bad sector, read-only disk, timeout, bad state). If
  hart entering panic **already held `UART_LOCK`** (e.g., panicked inside own
  `kprintln!`), hitting one of those branches self-blocks. The peeks
  `vfs_fs_lock_available()`/`fat32_locks_available()` don't cover this. On
  vf2/k1 (backend `mmc.rs`) path is clean.
- `try_acquire()` in trace dump releases guard immediately, so `trace_dump`
  retries in fresh window and can still spin.
- `current_task_name()` reads `PER_CPU`/`TASKS` (`static mut`, no lock) — not
  hang risk, unsynchronized read yes.

### ✅ `crates/libsys/src/lib.rs:252` — vdso offset (manually applied)

`vdso_read_u64(8)` → `vdso_read_u64(16)`. See batch 2 section.

### Batch 3 validation

- **6 configs compile, exit 0** (default, vf2, k1, no-ml, no-mmu, qemu). **Zero
  compiler warnings.**
- **Soak: 16 consecutive QEMU `-smp 4` boots** (2 rounds of 8, 40 s each).
  **0 panics, 0 hangs, 0 WCET violations.** Verified each boot reaches "Starting
  scheduler" and keeps emitting periodic `timer_isr` reports (continuous
  life-sign, detect deadlock).

**Statistical honesty on panic**: prior observed rate was 1 of 8. Seeing 16
clean is *consistent* with it fixed, doesn't prove it: if rate stayed 1/8,
probability of 16 straight clean is ≈12%. Would need ~40-50 boots to affirm
confidently. What is proven: fix **introduced no deadlock or WCET regression**,
which was real risk of putting locks on hot scheduler path.

---

## 2026-08-11 — Fix batch 4: rescue stranded tasks + jitter

Two more agents. **No commit.** Working tree: 20 files.

### ✅ `rebalance_from_offline_cpus()` — closes boot task stranding gap

Implemented **option (c)** from decision left open in batch 1. New public
function in `crates/sched/src/scheduler.rs`, called from `kernel/src/main.rs`
right after corrective `NUM_ONLINE_CPUS` store and **before** `sched::start()`.
Only runs if `online != num_cpus` → zero cost in healthy case.

Drains ready queues from harts `online..total` and re-homes each task via
`find_least_loaded_cpu()`. **Can't loop-hang**: `find_least_loaded_cpu` iterates
`0..NUM_ONLINE_CPUS`, already equals `online` when called, so destination always
live hart, never re-enqueues queue being drained.

Two things agent did beyond charge:

- **Rewrites `task.context.tp`** in each moved task. `context_switch` loads that
  value direct into `tp` register on dispatch, and `current_cpu_id()` — thus
  **all** `PER_CPU[current_cpu_id()]` access inside task — relies on it. Without
  this, moved task would corrupt per-CPU state as soon as it ran. Same mechanism
  as finding `context_switch.S:83`.
- **Refuted charge premise.** Told it was race-free window; found
  `smp_secondary_start()` (`main.rs:1475`) enables its
own timer ISR immediately, so an **alive** hart can be scheduling before
boot hart reaches `start()`. Source side (dead harts) safe by definition —
never ran code — but destination isn't. Used `_locked` wrappers on both sides.

Tasks pinned to dead hart: move anyway, rewrite `cpu_affinity`, emit
`kprintln!` **per task** with name, tid, both harts. Breaking explicit affinity
must be visible to operator.

**⚠️ Not exercised at runtime**: function only runs if `hart_start` fails, never
happens in QEMU. Verified by compilation and reading, **never executed once**.
Pending test on real hardware or mechanism forcing hart failure.

### ✅ `crates/drivers/src/wcet.rs` — jitter report underflow

**Charge hypothesis FALSIFIED**: suggested `min` initialized to 0; code already
had correctly `u64::MAX`.

**Real root cause**: `JitterTable.last` was **single `AtomicU64` per series,
shared across 4 harts**, and `read_cycles()` reads `rdcycle`, a **per-hart**
counter. `jitter_record(JITTER_TIMER_ISR)` called from timer ISR of each hart,
so subtracted "now" from one hart against "before" from another:
- `now < prev` (step back across harts) → `wrapping_sub` → value near
  `u64::MAX` → after `saturating_mul(1e9)/freq` gave absurd `max_ns`.
- Two reads from different harts coincidentally close → delta ~0 → **latched
  `min_delta` at 0 forever**. Explained constant `min_ns = 0`.

**Exact number confirmed arithmetically**: `u64::MAX / 10_000_000 =
1844674407370`, where `10_000_000` is QEMU's `TIMER_FREQ` (`platform.rs:24`).
`saturating_mul(1e9)` caps `u64::MAX` for any wrapped delta → **all** corrupt
deltas saturate to same fixed value. Bug fingerprint, real but not big number.

**Fix**: `last` becomes `[[AtomicU64; JITTER_MAX_SERIES]; JITTER_MAX_HARTS]`
indexed by `hart_id()`. Explicitly discards with `if now < prev { return; }`
instead of `saturating_sub` — latter would return 0 and recreate same latching
at 0.

**Measured result on real boot**: before `min_ns=0 max_ns=1844674407370`; now
`min_ns` between 3.2 ms and 126 ms per window, `max_ns=6.33 s`. **Bug closed,
numbers unreliable** — 6.33 s jitter for 100 Hz timer is TCG artifact (QEMU
multiplexes 4 harts on one host thread and `rdcycle` counts cycles spent
emulating others). Substrate problem already documented in
`project_bench_measurement_substrate`, not code defect.

**Not touched**: WCET report. Its `wcet_end()` subtracts against local `start`
of same frame and hart, so cross-hart contamination structurally impossible.
Its `max=66400 µs` same TCG noise, already mitigated (`WCET_BOUND_TIMER_ISR_US =
0` under `feature="qemu"`, unenforced).

### 🟡 New risk flagged by jitter agent

`JITTER_MAX_HARTS = platform::hw::NUM_CPUS` and guard `if hart >=
JITTER_MAX_HARTS { return; }` **silently discard all samples** if hart IDs not
contiguous from 0. Fits with already-open finding that `wake_harts` assumes
contiguity, doubt whether JH7110's S7 appears as hart 0. **Same pending
question, now second consequence.** Verify against real DTB when board arrives.

### Manual cleanup

`.unwrap_or(31)` (magic literal, **preexisting** in `scheduler.rs:1256`,
duplicated by agent copying file idiom) → `TASK_NAME_MAX_LEN`, derived from new
`TASK_NAME_CAPACITY` in `task.rs`, now also size of `Task::name` field. Single
source.

### Batch 4 validation

- **6 configs compile, exit 0. Zero compiler warnings.**
- **8 QEMU `-smp 4` boots**: 0 panics, 0 hangs, 0 WCET violations.
- Session cumulative: **24 panic-free boots** since cross-CPU scheduler fix (16
  batch 3 + 8 here). Prior rate 1/8, probability 24 straight clean by chance
  ≈4%. Already reasonable evidence scheduler panic closed — still not formal
  proof.

---

## 2026-08-11 — Fix batch 5: secure boot wired + CRC fallback (owner decisions)

The two decisions held open from the start, closed by owner and executed. **No commit.**

### ✅ Secure boot wired — `secure_boot.rs:208`

**Owner decision**: policy set by compile feature; if activated and verification
fails, **doesn't boot**, with clear message; **identical in debug and production**,
no bypass.

Wired in `kernel/src/main.rs`, right after CRC block, at first point where kernel
has FAT32 mounted and concrete `active_slot` to trust, before network, tasks, or
brain link.

Implementation details that matter:
- Feature `secure-boot-enforced`, declared in `crates/ota/Cargo.toml` (already
  existed) and **new in `kernel/Cargo.toml`**, forwarded downward.
- **Deliberately rejected hanging on `BootTrust::is_bootable()`**: that function
  queries runtime atomic `secure_boot_require_signature()`, someone could change
  hot and relax policy — exactly bypass owner forbade. Gate is pure
  `#[cfg(feature=...)]` ignoring the atomic.
- **Rejected `panic!()`** as stopping method: `panic.rs:119` restarts after delay,
  restart re-enters `ota_boot_validate()`, which after `OTA_DEFAULT_MAX_BOOT_ATTEMPTS=3`
  rolls back to `last_good` — i.e., `panic!` would **wrap** persistent bad slot
  instead of stopping. Uses `loop { wfi() }`, stop idiom already present at
  `main.rs:257/1018`. Verified watchdog (`wdt_init`, `main.rs:852`) armed **after**,
  so nothing forces silent reset from that loop.

**🔴 Tooling bug caught incidentally**: `tools/kconfig_to_cargo.py` translated
`CONFIG_SECURE_BOOT_ENFORCED=y` to token `robot_os_ota/secure-boot-enforced`,
activates `ota` crate feature but **not `kernel` crate**, where `#[cfg]` doing
stop lives. Via `make menuconfig`, policy would compile out **silently**. Same
"control that controls nothing" bug reproduced one layer up. Fixed to bare token
`secure-boot-enforced`; `tools/test_kconfig_to_cargo.py` updated (10/10 pass).

**Validated in execution**:

| | With feature | Without feature |
|---|---|---|
| Message | `FATAL: slot A rejected — signature file absent — refusing to boot` | `WARNING: ... booting anyway` |
| Scheduler | **doesn't start** | starts |
| Shell | **doesn't reach** | reaches |
| Boot lines | 127 | 444 |

**Measured cost**: `IMG_BUF` stops being dead code. `.bss` 2.625.272 →
4.722.888 bytes (**+2.00 MiB exactly**). End of `.bss` at `0x808620c8`, window
to `0x80a00000` → **1.62 MiB margin**. `ASSERT(_kernel_end < _stack_start)` holds.
6 configs + enforced variant compile, exit 0, zero warnings.

**⚠️ Warnings**:
- Without `tools/keys/prod_pub.bin`, `build.rs` emits zero pubkey and with
  feature activated **always boots in FATAL** (`NoTrustedKey`). Correct fail-closed,
  but disconcerting if unknown.
- **`Kconfig.security` help text promises more than this does**: says "bootloader
  verifies signature before jumping to `_start`", which would be trust root in
  ROM/U-Boot. This verifies slot file **from inside already-running kernel**.
  Useful, but whoever can replace kernel the bootloader loads bypasses it
  entirely. **Fix that prose.**

### ✅ CRC fallback — `main.rs:544`

**Owner decision: option B** — refuse loading that slot, fall to `last_good`.

**Nuance conditioning everything**: check is **retrospective**. Runs inside
`kernel_main` validating `/fat/KERN_A.BIN` file on FAT, not already-running
code. Can't "not load" already-loaded; only this point can do is fix which slot
**next** boot points to.

Implemented chain: active CRC fails → `ERROR` (was `WARNING`) → if `last_good
!= active` **and its CRC verifies**, switch `active_slot`, persist via
`ota_write_boot_meta`+`ota_apply_meta`, continue → if nothing verifies, very
visible error and **continues booting**.

**Two decisions taken without consulting, declared to owner**:
1. **No forced restart.** Can run on moving robot; restarting over file CRC
   riskier than problem it fixes.
2. **Terminal case doesn't stop machine.** Unlike secure boot (bad signature ≈
   possible attack), bad CRC is accidental corruption; kernel running in RAM
   works. Stopping kills console when needed and turns bit-rot into brick.

**🔴 Charge design was WRONG and agent refuted it.** Asked to fall to `SLOT_R` as
second fallback. Verified in `pure.rs`:
- `serialize_boot_meta`: `if meta.active_slot == SLOT_A { b"a" } else { b"b" }`
- `parse_boot_meta`: `if val == b"b" || val == b"B" { SLOT_B } else { SLOT_A }`
- `slot_crc`: `if slot == SLOT_A { image_crc_a } else { image_crc_b }`

**BOOTMETA can't represent "boot in R".** Setting `active_slot = SLOT_R` would
silently serialize as `"b"`, pointing to slot B **unverified** — worse than
original bug. And `ota_verify_slot(SLOT_R)` would compare R bytes against B's
expected CRC, nonsense. `SLOT_R` is **U-Boot** fallback when A and B fail to
load, not selectable from this pointer. Making it software-recoverable requires
extending BOOTMETA scheme (3 slots + CRC/size/version fields for R + who
populates on flash). **Not done, documented in code.**

**No overlap with loop detection**: `ota_boot_validate()` runs before (line 538),
fires on `boot_count` (consecutive boots) and does own rollback after 3 attempts.
New fix runs after, fires on different evidence (CRC of one boot's content) and
only touches `active_slot` — never `boot_count`, `last_good`, `ota_mark_boot_good()`.

### CRC branch validation IN EXECUTION (manual)

On normal QEMU this branch **never runs**: `BOOTMETA` generated by Makefile's
`build/disk.img` rule doesn't write `image_size_a`/`image_crc_a`, so `slot_size
== 0` block skipped ("no firmware recorded"). To test it really, built FAT32
disk by hand:

- `dd` + `mformat -F` + `mcopy` (mtools; `mkfs.fat` not on this machine).
- `KERN_A.BIN` and `KERN_B.BIN` 4096 bytes with different patterns.
- `BOOTMETA` with `image_size_a/b`, `last_good=b`, fabricated CRCs. Kernel CRC
  IEEE standard (poly `0xEDB88320` reflected, init `0xFFFF_FFFF`, XOR final) →
  identical to Python's `zlib.crc32`.
- Boot with `-drive file=...,if=none,format=raw,id=hd0 -device virtio-blk-device`.

**Scenario 1 — A corrupt, B healthy** (`image_crc_a` faked, `image_crc_b` correct):
```
[OTA] ERROR: Slot A CRC MISMATCH
[OTA] ERROR: switching NEXT boot to slot B (last_good, CRC verified) —
             slot A keeps running for the rest of this boot but will not be selected again
[OTA] Boot marked good (slot=B)
```
Detects, switches, persists, continues booting. No panic, scheduler up. ✅

**Scenario 2 — both corrupt** (terminal case):
```
[OTA] ERROR: Slot A CRC MISMATCH
[OTA] ERROR: no verified replacement slot available (last_good=B CRC also failed)
             — SLOT_R is a U-Boot-only recovery path, not selectable from BOOTMETA
             — continuing boot on unverified slot A; fix via OTA update or manual reflash
```
**Continues booting**: scheduler ✅, shell ✅, no panic. Decision not to turn
bit-rot into brick, demonstrated. ✅

### 📌 Answer to "why 2 MiB limit on OTA image?"

**No physical reason. Half-completed migration.**
- `Kconfig.ota:25` declares `OTA_MAX_IMAGE_SIZE_MB`, range 1-64, **default 8**.
- `phanes-config` emits it as `OTA_MAX_IMAGE_SIZE_BYTES = 8388608`.
- **No one references it** (grep users: empty).
- `crates/ota/src/pure.rs:32` hardcodes `2 * 1024 * 1024` and that's final.
- Kconfig help itself confesses: *"set to 8 MiB ... with the expectation C3
  migrates the constant"*. C3 never did.

Third instance of **built-configured-never-wired** pattern (other two: Ed25519
verifier and PWM constants in `platform.rs`).

**BUT raising to 8 MiB not possible alone**: `SECURE_BOOT_MAX_IMAGE_SIZE` does
two jobs — acceptance limit **and** verification buffer size. Real constraint
chain:

```
Pure Ed25519 (RFC 8032) → entire image contiguous in RAM
kernel window (linker.ld) → 8 MiB fixed
current kernel occupies → 4.33 MiB (6.33 with secure boot active)
                        ─────────────────────────
real buffer ceiling → ~3.6 MiB (only ~1.6 if secure boot active)
```

**Number 2 is arbitrary; ~3.6 MiB ceiling is structural.** Exits in order of
sense: (1) **expand kernel window** — VF2 has 8 GB RAM and `linker.ld` reserves
8 MiB; that question still `[OPEN]` since first review session and if arbitrary
other two exits unnecessary; (2) migrate to Ed25519ph for streaming verification
(touches crypto scheme and `tools/sign_ota.py`); (3) wire Kconfig with validation
preventing config beyond what fits.

Reminder: current image 1.915.996 bytes = **91.4% of 2 MiB limit**.

---

## 2026-08-11 — Fix batch 6: Kconfig OTA, BOOTMETA 3 slots, boot stack

### 📌 Answer to "what if 64 MB window?" — almost free

`LENGTH` in linker's `MEMORY` block **doesn't reserve RAM, it's a ceiling**:
linker only fails if sections overflow it. PMM doesn't start at fixed limit but
real symbol:

```rust
robot_os_mm::pmm::init(mem_start, mem_size, kernel_end_aligned);  // ← _kernel_end
```

Raising 8M to 64M **consumes no extra bytes**: kernel occupies what it occupies
and PMM still starts right after. Just removes artificial ceiling.

Real side effect: `_stack_end = ORIGIN + LENGTH` and `_stack_start = _stack_end
- BOOT_STACK_SIZE`, so **boot stack pinned to window end** and would move from
`0x809F0000` to `0x841F0000`.

QEMU caveat: `-machine virt` gives 128 MiB default (confirmed by boot DTB). 64
MiB window leaves 64 MiB for PMM/heap/user — works but system's half. On VF2
(8 GB) irrelevant.

**Not applied** — touches layout and owner decision.

### 🔴 NEW BUG FOUND AND FIXED — boot stack not reserved

Investigating above:

```rust
// pmm::init reserves ONLY from mem_start to kernel_end
let reserved_pages = (kernel_end - mem_start + PAGE_SIZE - 1) / PAGE_SIZE;
```

Boot stack lives **above** `_kernel_end`, so PMM marks it **free and can
distribute it**. Grep `_stack_start`/`_stack_end` across all Rust: **zero
references** — no one reserved it. `reserve_range()` existed and used for heap
(`main.rs:355`), not stack.

Consequence: `alloc_page()` can return page inside hart 0's stack and user
writes over it. Today gap between `_kernel_end` and `_stack_start` is ~1.6 MiB
(~400 pages), needs memory pressure to reach — but reachable. **With 64 MiB
window unreserved gap becomes ~58 MiB.**

**Fixed**: `_stack_start`/`_stack_end` declared external symbols and
`pmm::reserve_range(stack_start, stack_end - stack_start)` right after
`pmm::init`, with log `[MM] Boot stack reserved: ...`.

### ✅ Kconfig `OTA_MAX_IMAGE_SIZE_MB` wired (owner decision: "yes")

Hardcoded constant did **two jobs at once**. Separated:

- **`OTA_MAX_IMAGE_SIZE`** (*acceptance* limit) → now
  `robot_os_limits::OTA_MAX_IMAGE_SIZE_BYTES`, from Kconfig. Lives in
  `crates/ota/src/lib.rs`.
- **`SECURE_BOOT_MAX_IMAGE_SIZE`** (*verification buffer* size) → stays 2
  MiB, bounded by RAM because pure Ed25519 needs image contiguous.

With two guardrails against silent drift:
- `const _: () = assert!(SECURE_BOOT_MAX_IMAGE_SIZE <= OTA_MAX_IMAGE_SIZE, ...)`
  — fails at compile time, not runtime.
- New `BootTrustReason::ImageTooLargeToVerify`: image between both limits OTA
  accepts, secure boot rejects **stating exactly why**, instead of truncating
  and blaming signature.

### ⚠️ My error in this batch: broke documented invariant

`pure.rs` says in header: *"Keep this file dependency-free so the host test
crate (`crates/ota-tests/`) can `#[path]`-include it directly."* I added
`robot_os_limits::` and **broke `ota-tests` compilation** — whole test crate
stopped compiling. Kernel build didn't catch it because test crates compile
separately.

**Fixed correctly**: `ota_validate_header(h, platform, max_image_size)` receives
ceiling as parameter; Kconfig constant lives in `lib.rs` (can depend on
`robot_os_limits`); `pure.rs` dependency-free again. Updated `crates/shell`
caller and 7 `ota-tests` asserts (with local `TEST_MAX_IMAGE_SIZE`, because
those tests verify comparison, not configured number).

**Lesson**: kernel's 6 config build doesn't cover 22 test crates. Must execute
them.

### ✅ BOOTMETA 3 slots — `SLOT_R` representable (owner decision: "yes")

`BootMeta` gains `fw_version_r`/`image_size_r`/`image_crc_r`;
`slot_crc`/`slot_size`/`slot_version` shift from `if A {..a} else {..b}` to
three-branch `match`; `active_slot`/`last_good` serialize as `"a"`/`"b"`/`"r"`.
CRC fallback chain adds `SLOT_R` as last candidate, after `last_good`.

**Backward compatibility**: verified. Old BOOTMETA lacks `_r` keys → parser
leaves at 0 → `ota_verify_slot` cuts at `if expected_size == 0 { return false; }`
**before touching disk**, so R never offered as candidate. Covered by new test.

**Downgrade risk, no possible fix**: new BOOTMETA with `active_slot=r` read by
old kernel falls in `else` of `if val==b"b" {SLOT_B} else {SLOT_A}` →
**silently interpreted as `SLOT_A`**. Fixed in test and documented.

**🟡 GAP FEATURE LEAVES HALF-DONE**: `tools/boot.cmd` only distinguishes
`active_slot=="b"`; anything else (now including `"r"`) falls to A branch.
U-Boot only reaches R when `fatload` of A **and** B fails truly (file absent or
zero size) — file present but bad CRC loads and boots same. **So persisting
`active_slot=r` from kernel doesn't make U-Boot prefer R next boot until
`boot.cmd` learns to branch by `"r"`.** Scheme already supports it; real boot
doesn't. Not touched (would imply flash flow decisions).

**And no one populates R fields**: grep `tools/`, `crates/shell` (only place
writing `image_size_a/b`, always against `ota_inactive_slot()`, never returns
`SLOT_R`) and `docs/FLASH_PROCEDURE.md` → **nothing writes
`image_size_r`/`image_crc_r`/`fw_version_r`**. R representable but stays non-
candidate until some factory tool fills them.

### Batch 6 validation

- **7 builds exit 0** (default, vf2, k1, no-ml, no-mmu, qemu, qemu+enforced).
  **Zero compiler warnings.**
- **22 test crates executed: 21 pass clean.** Totals: abi 18, aead 6, arch-api
  17, arch-x86_64 13, cam-ring 10, cap 13, config 23, crypto 22, dfu 22,
  drivers-api 14, dtb 9, efi 16, encrypt-link 13, flight-math 19, gguf 16,
  msc 33, multi-stream 15, **ota 72**, sched-policy 44, tftp 28, topology 24.

### 🟡 `regression-tests` — 2 failures, PREEXISTENT AND FLAKY

`host_microbench::bench_crc8` and `bench_build_parse_packet_roundtrip` fail
intermittently. Are **wall clock time** asserts:

```rust
assert!(max < 5_000, "crc8(64B) max {}ns exceeds 5µs ceiling", max);
```

Re-run 3 times straight on unloaded machine: **fail, fail, pass**. Observed
values 5569 ns and 5100 ns against 5000 ceiling — 11% above, edge case.
Exercise `brain_protocol_src::crc8` and `build_packet`/`parse_packet`, **code
no batch touched**.

Diagnosis: performance gate miscalibrated ~1 in 3 fails on shared machine.
Not regression. But **broken gate**: either raise ceiling, mark `#[ignore]`
default, or move to bench harness with median of N runs (project already has
`tools/bench_compare.py`). Fits `project_bench_measurement_substrate`: timing
measurements this substrate doesn't give actionable results.

---

## 2026-08-13 — Independent audit (Fable) pre-hardware — closure verdict

External audit to fix session, explicit charge to attempt refuting
`CLOSURE_STATUS_2026-08-13.md`. Read-only + build/test verification. Sample
verified: vdso, panic handler, cross-CPU scheduler locking, BOOTMETA 3 slots,
boot stack reservation, CRC-fallback chain.

### Declared status verification

- Repo: commit `8919bc7` + exactly 9 uncommitted files. ✅
- `crates/ota-tests`: **72/72 pass** (executed, exit 0). ✅
- Default build: **real exit 0, zero compiler warnings** (only preexistent
  informational from ota build script embedding pubkey). Two road traps: (1)
  first pass gave false "exit 0" from pipe to `tail` — exactly false positive
  rules warn of; (2) repo `target/` left with sandboxed-build artifacts having
  restricted attributes, gives "Operation not permitted" on incremental builds
  — verified with isolated `CARGO_TARGET_DIR`; **may need `cargo clean` (or
  delete `target/release/build/`) before next repo build**. Deleted
  `target/release/build/compiler_builtins-04e1da94590d2a48` during diagnosis
  (regenerable artifact).
- `tools/keys/`: `prod_priv.bin`/`prod_pub.bin` EXIST since May (closure
  warning "without prod_pub.bin boots in FATAL" outdated), ignored by git,
  untracked, private 600 mode. ✅ correct hygiene.
- DTB parser (`crates/dtb/src/lib.rs:298`): counts all nodes
  `device_type=cpu`, **without checking `status`** — a disabled S7 in VF2's DTB
  counts equally toward `num_cpus`. Reinforces the hart discovery below.
- Fixes verified as correct: vdso (CAS claim + Release close, sound in
  RVWMO), panic handler (real emergency path, correct peeks; caveat: the
  trace-dump comment promises more than what peek guarantees — same
  TOCTOU as the FS one, documented there), BOOTMETA 3 slots (backward/forward
  compat as documented; without original SLOT_R trap), boot
  stack reserved (main.rs:258), `pure.rs` again dependency-free (no other
  `#[path]`-include in the repo violates its invariant; flight-math
  still has zero deps).

### 🔴 NEW — BLOCKING OTA: U-Boot reads `BOOTMETA`, kernel writes only `BOOTMETA.A/.B`

`tools/boot.cmd` decides slot reading `/fat/BOOTMETA` (flat, `env import`).
Since OT02.B, `ota_write_boot_meta()` writes ONLY records `BOOTMETA.A`/`BOOTMETA.B`
(`lib.rs:174-179`); flat read-only as migration (`lib.rs:276`) and **nothing
ever writes it runtime** (grep: single open, O_RDONLY). Real hardware consequence:

1. OTA to slot B → records say `active=b`; flat still says `a`.
2. U-Boot loads **KERN_A.BIN forever** (old firmware).
3. Kernel boots, reads records, thinks it's B, verifies KERN_B.BIN CRC (passes
   — file OK), marks boot-good B and raises `min_fw_version` to B's version…
   **while executing A**.

I.e.: **no slot switch kernel makes (OTA, boot_count rollback, CRC fallback)
ever reaches U-Boot.** Batch 5 QEMU experiment validated only kernel half: QEMU
boots with `-kernel`, no U-Boot, so other half of contract never executed.
`FLASH_PROCEDURE.md` (line 185) edits flat by hand — manual flow works;
autonomous doesn't. **[BUG — blocking for Phase 5 / real OTA]** Obvious fix:
`ota_write_boot_meta` must rewrite flat too (with corruption window it reopens
— think: maybe flat = only min `active_slot=` for U-Boot, records = kernel
ground truth).

### 🔴 NEW — BLOCKING PHASE 0: hardware WDT armed mid-boot, fed only at scheduler

Verified chain: `main.rs:949` calls `wdt_init(CFG_WATCHDOG_MS=500)` Phase D →
`hw_wdt_init` (vf2/k1) arms DW-WDT, **once armed can't disarm** (`wdt.rs:137-143`).
Only kernel `wdt_kick()` lives in **timer ISR** (`main.rs:4035`) — doesn't fire
until scheduler startup, hundreds UART lines at 115200 later (≈3.5 ms/line).
Plus `hw_wdt_init` calculates ticks with `TIMER_FREQ` (VF2: **4 MHz**,
`platform.rs:45`) when file itself documents DW-WDT runs from APB ~24 MHz →
for 500 ms requested: TOP=5 → 2^21/24e6 ≈ **87 ms actual**. Even with correct
clock (TOP=8 → ~699 ms), Phase D→first tick window likely exceeds it.
**Forecast: loop reset on first Phase 0, before prompt** — hardware plan
mentions WDT nowhere (`wdt.rs:113` finding 0/2 votes stayed backlog, didn't
cross plan). Minimum pre-board mitigation: first image WDT unarmed (or explicit
kicks between boot phases) + own `WDT_CLK_HZ` constant. **[BUG in plan + reading-confirmed wdt.rs:113 candidate]**

### 🔴 NEW — PHASE 0 UNPLANNED: noncontiguous VF2 harts break `wake_harts`+`rebalance` chain

`wake_harts` documents its ASSUMPTION (contiguous harts from 0, boot hart=0).
On VF2 hart 0 is S7 (no S-mode/MMU — can't run this kernel), so boot hart
ALWAYS 1..4. Expected chain with boot=1: `wake_hart(0)` (S7) fails →
`prefix_intact=false` at i=0 → `online=1` even if harts 2-3 boot well →
`NUM_ONLINE=1` → `rebalance_from_offline_cpus(1,4)` **drains harts 1..3
queues — boot hart itself and live secondaries —** moves all to
`find_least_loaded_cpu()` = range `0..1` = **PER_CPU[0], dead S7's queue**,
rewriting `context.tp=0`. Result: original symptom ("boots, prints all,
does nothing") would reappear VIA rescue mechanism. `smp.rs:31` "✅ FIXED"
correct for QEMU topology; its prefix semantics assume boot∈{0} and on VF2
premise false by default. **Plan Phase 0 ("wait Online CPUs: 4") doesn't
contemplate this**, despite notes marking contiguity 3 times. First datum
to capture with board: real DTB's cpu nodes list. **[OPEN — plan blocking,
not necessarily code: may suffice gate "if boot_hart != 0, no rebalance and
NUM_ONLINE=1" until DTB]**

### 🟡 NEW — task lifecycle race outside CPU_LOCKS

Batch 3 fix protects queue STRUCTURE well. But state transitions outside lock:
`block_current` sets `state=Blocked` → `do_schedule` → `context_switch` (saves
context), remote waker (timer ISR another hart) sees `Blocked`, enqueues task
different CPU, dispatches **before source hart saved context** → same task
running two harts at once, stale context. Narrow window but reachable (short
sleep + other hart's tick). Plus: (a) concurrent wakers both pass `state !=
Blocked` check (no lock) → double enqueue same idx; (b) `do_schedule`
doesn't re-verify `state == Ready` after dequeue; (c) `find_earliest_deadline`
accepts `Running` from any CPU — latent now (zero `task_set_deadline` callers),
but exactly `context_switch.S:83`'s mine. Consistent with panic 1/8 "closed"
with 24 boots being reasonable evidence not proof. **[OPEN — closure's "✅
FIXED" for :1373 true for what charged and overstates as total guarantee]**

### 🟡 New minor items

- `ota_mark_boot_good` logs `slot=` with `if SLOT_A {'A'} else {'B'}` —
  `active_slot=R` prints as "B". Cosmetic, 1 line.
- Secure boot: SUCCESS path (feature active + real key + valid signed image
  → boots) **never executed** — only fail-closed with zero key. Generate key
  and test before Phase 5.
- `crates/topology-tests/Cargo.lock` (+223 lines) traveling in batch 6:
  regenerated lockfile from excluded crate — harmless but doesn't belong;
  review before commit.

### Verdict

**Session work solid and notes honest** (declared validations reproduce; no
fabrication signs). But **wouldn't close as-is**: (1) BOOTMETA/U-Boot gap
invalidates half the A/B system hardware — if OTA matters for closure,
blocking; (2) plan Phase 0 has two known unincorporated mines (WDT and
noncontiguous harts) predicting first real boot failure would lose days
attributing wrongly. Plan's I2C-first and ina219-before-actuators order right;
add "Phase -1": initial image without `secure-boot-enforced` and **WDT unarmed**,
dump real DTB (cpu nodes) as first deliverable, expect `only X/4 harts` without
trusting rebalance.

---

## Next step (updated 2026-08-03)

Two threads open in parallel:

1. **Automated audit**: pending relaunch IN BATCHES (script already supports
   `args: {only: [...]}`) when credits available: (a) 16 uncovered fronts this
   pass — robot-brain still at ZERO —, (b) verify 15 `os-drivers` candidates
   (0/2 votes), (c) re-verify old 15 🟡 with 0-1 votes. Anti-stall v2 fix still
   UNVALIDATED (stalls occurred again before credit limit).
2. **Line-by-line manual review**: `kernel_main` in `kernel/src/main.rs` — seen
   trap_init, end stretch of tp/scheduler start, SMP bring-up (1040 /
   1237-1252). Missing detailed review: Phase 1 UART + DTB parse (166-224),
   Phase 2 memory PMM/VMM/heap (226-~350), long subsystem init stretch to 1040.
   **Bug `context_switch.S:83` (tp + EDF migration) and `smp.rs:29` (wake_hart
   silent) fit directly in this thread**. 4 new critical driver bugs
   (gps/baro/ina219/platform-MMIO) candidates for manual verification one-by-one
   if automation keeps failing.

## 2026-08-13 — Fix batch 7, G: task lifecycle race (Fable)

First fix of post-audit closure round by Fable, executed in total isolation (nothing
else touched in parallel) by explicit instruction: only change in whole batch
capable of producing silent intermittent failure, mixing with other 4 batch 7
fronts would make impossible attributing flake to real cause.

**Real bug (two parts, confirmed reading code, not just Fable's hunch):**

1. `block_current()` sets `state = Blocked` and `do_schedule()` (eviction branch)
   sets `old.state = Ready` + enqueues — both BEFORE `context_switch.S`
   finishes saving that task's registers. Remote waker (timer ISR another hart)
   sees new state, re-dispatches task before its saved context valid: same task
   running two harts at once, same stack.
2. `find_earliest_deadline()` (EDF picker) had no lock and accepted `Running` tasks
   ANY cpu, not just own. For deadline task without fixed affinity not even a race:
   second hart programs, it "steals" or duplicates deterministically. (Currently
   zero `task_set_deadline` callers in tree, so latent — but subsystem real and
   should be correct before someone uses it.)

**Design (mine, line-by-line verified against real `.S` before delegating):**

- New `Task::context_saving: AtomicBool` field. Zero-init-safe by design: `TASKS`
  initialized with `core::mem::zeroed()`, so `false` (="safe to dispatch") must
  be default — deliberately inverted vs. naive `context_saved: bool`.
- `context_switch.S`: between saving last callee-saved register (`sd ra,
  CTX_PC(a0)`) and `restore_new_task:`, insert call to Rust function
  `mark_context_saved(old)` doing `context_saving.store(false, Release)` —
  same pattern `context_switch_rvv.S` already uses for `rvv_ctx_save`/`restore`,
  not new technique in file. Avoids splitting function in two (tried that
  mentally first; splits break `old`'s return point — resumes mid-`do_schedule`
  instead of end — so scrapped). `do_schedule()` gains spin-gate: before
  activating `next`, waits `context_saving == false` (`Ordering::Acquire`).
  Normal window: handful instructions; bounded by IRQ duration worst case, same
  as rest of kernel's WCET budget.
- `find_earliest_deadline()`: only accepts `Running` if it's THIS cpu's current
  task (self-continuation); any other `Running` excluded.
- New `DeadlinePickGuard` (global, IRQ-safe, exact same pattern as `CpuLockGuard`:
  saves/disables `sstatus.SIE`, spins, restores on drop) serializes "scan +
  claim" in `do_schedule()`'s EDF branch so two harts never both scan and pick
  same task before either writes `Running`.

**Scope — 3 `context_switch` variants, explicit per-architecture decision:**

- `context_switch.S` (vf2/k1/qemu, July's real target): complete fix, applied
  and validated.
- `context_switch_esp32c3.S`: single-core, race structurally impossible — but
  Rust side (spin-gate, both `store(true, ...)`) unconditional across targets,
  so without `mark_context_saved` call this file, `context_saving` stayed `true`
  forever after first block/evict, hanging entire kernel on that target first
  time real scheduling exercised. Detected BEFORE compile (reviewing own design,
  not agent) — applied same 3-line patch there too.
- `context_switch_rvv.S`: deferred deliberately. `--features rvv` is vector
  extension benchmark QEMU-only (explicit comment `kernel/Cargo.toml:8`);
  JH7110/vf2 has no RVV (confirmed `defconfigs/vf2.config`); `k1` (only real
  target with RVV 1.0) doesn't activate this kernel feature (only forwards
  `robot_os_arch/rvv` etc., not kernel's own `rvv` selecting this `.S`) — so
  file never assembles in real hardware build. Mechanism explicitly gated
  `#[cfg(not(feature = "rvv"))]` in `scheduler.rs` not to hang that build too
  (same reason esp32c3: Rust side unconditional default). Also required
  declaring `rvv` as real `crates/sched` feature and forwarding from
  `kernel/Cargo.toml` (`robot_os_sched/rvv`) — without it `cfg` lived wrong
  crate did nothing (caught by compiler's own `unexpected_cfgs` warning, not
  blind).

**Execution:** full design mine; mechanical implementation delegated sonnet agent
exact line-by-line diff specified (no redesign delegated); diff reviewed
character-by-character vs. charge before compile (only 3 files touched, match).
esp32c3/rvv gap bug found myself reviewing design before building, not by agent
or build.

**Validation:**
- 7 configs rebuilt after fix (`qemu`, `qemu+rvv`, `vf2`, `k1`, `no-mmu`, `no-ml`,
  `esp32c3`) — 6/7 exit 0 clean. `esp32c3` fails but from absent `hart_start` in
  `sbi` stub `crates/arch-riscv64/src/lib.rs:25-26` — preexistent, untouched
  file by this fix or session; unrelated to G. Pending as known gap, doesn't
  block G closure.
- Soak QEMU `-smp 4`, 20 boots 18s: **20/20 clean**, 0 panics/fatals/deadlocks, 0
  WCET violations `timer_isr` across 37 collected report rows, real workers
  completing 2000 iterations crossing CPU 2/3 each time (directly exercises
  block_current + eviction + new spin-gate). Matches prior baseline 24 clean
  boots with more real scheduling load.

## 2026-08-13 — Fix batch 7 (rest): BOOTMETA, WDT, sensors, memory/Kconfig, E1, PWM

Rest of Fable's post-audit closure, executed in parallel (disjoint files,
verified before launch) once G was isolated and validated. Pattern: I design and
verify against real code, sonnet agents implement the exact diff mechanically,
I review character by character before compiling. All findings below compile
6/7 configs clean (`qemu`, `qemu+rvv`, `vf2`, `k1`, `no-mmu`, `no-ml` —
`esp32c3` still broken by the preexisting `hart_start` gap documented in
section G, untouched by anything in this batch) and pass QEMU `-smp 4` soak
(20/20 and 15/15 clean in two rounds, 0 WCET violations).

**FIX A (flat BOOTMETA)** — `ota_write_boot_meta()` now also
rewrites `/fat/BOOTMETA` (flat format that `boot.cmd`/U-Boot reads via
`env import -t`) in addition to the `.A`/`.B` records, via new helper
`fs_write_plain_boot_meta()`. Torn write on flat file accepted
(`.A`/`.B` records remain the ground truth and self-recover). D1 (bonus,
same file): `ota_mark_boot_good()` now prints 'R' correctly instead of
aliasing to 'B'.

**FIX B (WDT)** — two independent bugs, same file
(`platform.rs`): (1) New `WDT_CLK_HZ` (24MHz, provisional, already
corroborated by preexisting comments in vf2/k1's `WDT_BASE` saying
"APB clock ≈ 24 MHz") replaces `TIMER_FREQ` (4MHz, the RISC-V timer
clock, not the WDT clock) in `wdt.rs`. (2) The WDT arming block moved
from Phase D (early boot) to just before `sched::start()` — the only point
that feeds it is the timer ISR, which doesn't start until the scheduler;
arming earlier left a long unfed window that could reset the board mid-boot.
Placed BEFORE `tp` re-establishment (explicit code invariant: "no Rust
functions after this point"), not after.

**I2C0/UART1** — `I2C0_BASE` in vf2's block in `platform.rs` was aliased
to `UART1_BASE` (`0x1001_0000`). Fixed to `0x1003_0000` (real JH7110,
mainline `jh7110.dtsi`).

**Sensors (4 bugs + 2 siblings)** — all verified with real numbers
before delegating, not just description:
- GPS (`parse_nmea_coord`): `min_frac * 10` was redundant — `min_frac`
  was already scaled to 7 digits; applying the extra `*10` introduced ~5%
  error in the minutes term (~6 km position error).
  Verified with "4807.038" → expected 70,380,000, code gave
  73,800,000.
- Barometer (`baro_read`): missing `dig_p7` term in BMP280 pressure
  compensation (official 64-bit Bosch formula adds it at
  the end, `+ (dig_P7 << 4)`, before `/256`) — fixed pressure bias →
  altitude bias.
- Barometer (`baro_read`): triggers forced measurement and reads immediately,
  without waiting for conversion (~40ms typical) — stale or garbage data.
  Now polls "measuring" bit (STATUS 0xF3, bit 3) bounded.
- `imu_init`/`baro_init`: I2C config writes unchecked
  — if they fail, sensor still marked READY. Now each write is
  verified (`i2c_write` returns i32, 0=success/-1=fail — agent
  detected that my original spec assumed byte count like `i2c_read`,
  self-corrected after reading real signature).
- INA219 (`ina219_poll`): mAh counter summed raw `current_ma`
  per poll without dividing by 3600 — over-counted ~3600x, would trigger
  battery-low failsafe in ~3.6 s real time. Fix: raw accumulator
  (`MA_POLL_SUM`) with deferred division on read (`ina219_mah_used`),
  not per-poll division (which would truncate to 0 for any realistic
  current). **Note**: `ina219_poll` has 0 callers in tree today —
  latent bug, not yet reachable, but fixed anyway because it will
  be wired at bring-up.

**Memory/Kconfig (4 gaps)**:
- `TcpConn::new()`: 5 non-zero fields moved static `TCP` (~1.02
  MiB) from `.bss` to `.data`. Zeroed; `reset_conn_state()` already
  restored 4 of the 5 on each real activation; `remote_window` at 0
  already had fallback. Verified in binary: `TCP` now in
  `.bss`, 1,069,720 bytes.
- `sched::task::STACK_SIZE` now comes from
  `robot_os_limits::KERNEL_STACK_SIZE_BYTES` (Kconfig
  `KERNEL_STACK_SIZE_KB`, lives in `Kconfig.limits` not `Kconfig.platform`
  as I said initially — agent self-corrected). Verified
  in binary: `TASK_STACKS` still exactly 1,048,576 bytes
  in `.bss` (16 KiB × 64 tasks) — no regression in default. New
  invariant in `build.rs`: `KERNEL_STACK_SIZE_KB` cannot leave 0
  usable bytes after the real 4 KiB guard page (`setup_stack_guard_pages`).
- `mm::pmm::MAX_PAGES` now derived from `robot_os_limits::RAM_SIZE`
  (real MiB per board) instead of a flat 16 GiB ceiling. Verified:
  PMM bitmap no longer appears among the 15 largest `.bss` symbols in
  QEMU binary (dropped from 512 KiB to ~8 KiB with `RAM_SIZE=256`).
- `net::tcp::TCP_BUF_SIZE` — Kconfig symbol ALREADY EXISTED
  (`Kconfig.network`, default 131072, identical to hardcoded value);
  only needed wiring (not creating new one, as my original note said —
  corrected before delegating). New
  power-of-two invariant in `build.rs` (value used as ring mask).

**E1 (kernel window)** — only `kernel/linker.ld` (QEMU/default)
was stuck at 8 MiB; vf2 (128M), k1 (256M), and fleet (1022M) already
had plenty of margin. Expanded to 32M. `_stack_start`/`_stack_end`
are pinned to window top by design, so they move automatically.
Verified in QEMU: boot stack relocated exactly where it should be
(`0x821f0000-0x82200000` = 32M - 64K), clean boot.

**PWM — design pivot mid-batch.** Original plan ("PWM by
software on GPIO+timer ISR for gripper", agreed before this
batch) proved unfeasible: scheduler tick is 10ms
(`SCHED_TICK_US`), and an RC servo needs to represent 1-2ms pulses
within a 20ms frame — at 10ms resolution the pulse can't be
represented. Discovered reviewing real hook before delegating, not
at runtime. Consulted with user: **motors stay on real PWM8
(hardware), gripper deferred to hardware bring-up (needs
dedicated high-resolution timer, not scheduler tick)** —
also confirmed that with only 2 real motors (`motor_init(0,0,...)`,
`motor_init(1,1,...)`, channels 0-1), channel 2 IS free on the
real 4-channel chip, but there's a second unseen problem:
`PWMCFG` (which sets period) is a SINGLE register shared by all 4
channels — motors (~1kHz) and servo (~50Hz) can't coexist in the
same instance with a shared period. Confirmed against primary source
(Linux `drivers/pwm/pwm-sifive.c`, exact `PWMCFG` bits cited literally).
With that decision, corrected the real register model for motors:
`PWMCFG`@0x0 shared (scale=bits [3:0], previously modeled as separate
`PWMSCALE` register that doesn't exist), `PWMCMP(i)`@`0x20+4*i` i=0..3
the only genuinely per-channel register (previously aliased with period —
the root cause of bug already documented in the file). Only 4 real channels,
not 8 (new local constant `PWM_MMIO_CHANNELS`, global `PWM_MAX_CHANNELS=8`
untouched for `sim`/QEMU/K1 compatibility). Correct side effect:
gripper's `payload_init()` now fails with log instead of silently writing
outside real hardware range (channel 4 previously passed 8's bound-check
and wrote to invented MMIO address with no one noticing). Validated only by
compilation + line-by-line review — QEMU uses `sim` module, not `mmio`,
so this fix can't be boot-tested without real hardware; that's exactly
what's missing for the bring-up phase.

## 2026-08-13 — D2: secure boot success path — real bug found and fixed

Executed directly (Bash/mtools/QEMU, no agents) because it was most
delicate: signing a real image with production key and verifying
`secure-boot-enforced` allows boot had never been tested in the entire
project history — only the FAIL path was tested (zero key → halt).

**Procedure**: build with `--features qemu,secure-boot-enforced`,
`objcopy -O binary` to raw binary, signed with `tools/sign_ota.py
--priv tools/keys/prod_priv.bin` (real production key, not dev key),
FAT32 disk hand-crafted with mtools containing
`KERN_A.BIN`+`KERN_A.SIG`+`BOOTMETA`, booted in QEMU with
`-drive ...,format=raw -device virtio-blk-device`.

**First attempt — deceptively partial result.** I placed files in
subdirectory `fat/` (mimicking `/fat/...` path prefix). Log showed
`[SECURE-BOOT] Slot A signature: verified` — but also `[OTA] Slot A —
no firmware recorded`, a signal something was off. Investigating that
discrepancy instead of taking it as good revealed a much more serious
problem than what D2 asked to verify:

**The real bug**: `ota_verify_slot`/`ota_read_boot_meta` (CRC check +
metadata) go through VFS layer, which mounts `/fat` as MOUNT POINT
and strips it from path before searching — i.e., they search files at
FAT32 volume ROOT. `secure_boot_verify_slot_detailed`
(`read_sig_file`/`read_slot_image`) uses DIRECT access to FAT32 driver
(`fat32_open`→`resolve_parent`), which walks the literal path —
`/fat/KERN_A.SIG` there means "find a real subdirectory called
`fat`". Two incompatible interpretations of same path constant.
`tools/boot.cmd` (ground truth — real U-Boot script) uses
`fatload mmc 0:1 ... KERN_A.BIN` without subdirectory: real convention
is volume root. With that layout (the only one U-Boot can produce),
`secure_boot_verify_slot_detailed` would NEVER have found the `.SIG` —
**`secure-boot-enforced` would have hung on `loop { wfi() }` on every
real boot**, the moment anyone enabled that feature.

**Fix** (`crates/ota/src/secure_boot.rs`, consulted with user
before touching code given impact): `SECURE_BOOT_SIG_PATH_A/B/R`
move from `/fat/KERN_{A,B,R}.SIG` to `/KERN_{A,B,R}.SIG` (root,
root-relative). New constant `SECURE_BOOT_BIN_PATH_A/B/R` +
`secure_boot_bin_path()` so `read_slot_image` stops reusing
`crate::ota_slot_path()` (which should stay `/fat/`-prefixed —
correct for its VFS callers) and uses its own root-relative path.

**Validation**: disk rebuilt with EVERYTHING at root (like
`boot.cmd`) — `[OTA] Slot A CRC OK (fw=1, size=907804)` +
`[SECURE-BOOT] Slot A signature: verified` together, no FATAL, 3/3
repeated boots clean. 6/6 configs (`qemu`, `qemu+secure-boot-enforced`,
`vf2`, `k1`, `no-mmu`, `no-ml`) compile clean after fix
(`secure_boot.rs` compiles in all, not just feature). Exact
counter-example for why "test the success path" matters:
code review by inspection never would have found this — code on each
side (VFS vs. direct) is internally consistent and "looks correct"
read in isolation; only running it with real data exposed it.

## 2026-08-13 — E2: `boot.cmd` `active_slot=r` branch

Added symmetric `elif test "${active_slot}" = "r"` branch to existing A/B
branches (tries `KERN_R.BIN`, falls to `KERN_A.BIN` on fail) — before, any
`active_slot` value not exactly `"b"` silently fell to branch A, including
`"r"`. If/elif/else/fi structure manually reviewed (9 `fi` for 9 blocks needing
them, balance correct). **Not executed**: `mkimage` (u-boot-tools) unavailable
on this machine, no real U-Boot environment in QEMU this session (this project's
boots use `-kernel` direct over OpenSBI, bypass U-Boot entirely) — same
confidence level as rest of script, never executed before this session. Pending
verification when real U-Boot available (hardware or QEMU with full U-Boot).

**Batch 7 closure**: FIX C (physical→logical hart_id map) remains
deferred to real hardware, untouched — it's the only one of the 12
original tasks intentionally left open until we have a real VF2
DTB.

## 2026-08-13 — Post-closure: real clocks, esp32c3 gap (3 layers), boot.cmd validation

Short follow-up round on 3 pending items from batch 7 closure, explicitly
requested by user ("2 -> do it, 3 -> fix it, 5 -> validate it").

**WDT_CLK_HZ / PWM_CLK_HZ — investigation with real primary source
(Linux mainline), not repetition of prior assumption.**
- `crates/watchdog/starfive-wdt.c` confirms timeout calculated
  with `wdt->core_clk` (`count = timeout * clk_get_rate(core_clk)`), NOT
  the "apb" clock (that only serves register access). Clock tree
  (`clk-starfive-jh7110-sys.c`) confirms `wdt_core` is tied DIRECTLY to
  `JH7110_SYSCLK_OSC` (24MHz crystal), without going through uncertain
  chain `APB_BUS = STG_AXIAHB / 8`. **Result:
  `WDT_CLK_HZ = 24_000_000` for vf2 confirmed with solid technical reason**
  (was "APB clock ≈ 24 MHz", assumption without source) — comment updated
  in `platform.rs`. K1 block (SpacemiT, different chip) accidentally got
  same comment via careless `replace_all` — corrected to own comment making
  clear JH7110 investigation DOES NOT apply to K1 and that value remains
  provisional there.
- `pwm_apb` (only PWM8 clock, no separate "core" clock)
  DOES go through `APB_BUS = STG_AXIAHB / 8`, and `STG_AXIAHB` is an
  EXTERNAL clock supplied by boot firmware — has no fixed rate definable
  in Linux source. Real dead end, not a shortcut: traced as far as public
  source allows. `PWM_CLK_HZ` stays at 24MHz, now as explicitly
  documented assumption with reason why it couldn't be confirmed, instead
  of generic "verify in DTS" note.

**esp32c3 — 3 layers of breakage, first 2 fixed, 3rd
documented and stopped by explicit user decision.**
1. `crates/arch-riscv64/src/lib.rs` — esp32c3's `sbi` stub only
   had `shutdown`/`reboot`; missing `hart_start`, `send_ipi`,
   `set_timer` (the 3 used unconditionally by `api_impl.rs` on
   all riscv64 targets). Added as safe stubs: `hart_start`/
   `send_ipi` return error (mono-core, no other harts to start/signal),
   `set_timer` is no-op (esp32c3 uses its own TIMG registers, not SBI).
   FIXED.
2. `crates/ml` and `crates/camera` — both unconditionally called
   `robot_os_arch::vector::dot_f32_best`, a module that doesn't exist
   under `esp32c3`. Same pattern, same fix in both: new `esp32c3` feature
   in each Cargo.toml, `dot()` dispatcher bifurcated
   (`#[cfg(not(esp32c3))]` → vector accelerated, `#[cfg(esp32c3)]` →
   portable scalar product — no real regression, esp32c3 has no
   SIMD/RVV hardware to lose). Forwarded from `esp32c3` feature in
   `kernel/Cargo.toml`. FIXED, both crates compile clean standalone.
3. `crates/sched/src/process.rs` uses MMU/VMM (`robot_os_arch::mmu`,
   `robot_os_mm::{vmm, vdso}`) with no conditions — none of those
   modules exist under `esp32c3` (no-MMU). This is NOT a small fix
   like the previous two: it's deciding how "process" works on an
   MMU-less target, a real design decision. Presented to user with 3
   options (stop and document / investigate scope without touching code /
   design and fix it now) — **decision: stop here**.
   esp32c3/HERMES paused as active direction (substrate, not
   current goal — VF2 is the hardware that arrived). Full kernel build
   `--features esp32c3` STILL FAILS for this reason — layers 1 and 2
   were masking this deeper 3rd layer.

**`tools/boot.cmd` — real validation, not just manual reading.**
Installed `u-boot-tools` (Homebrew) specifically for this —
`mkimage -C none -A riscv -T script -d tools/boot.cmd build/boot.scr`
compiles clean (exit 0), confirming the hush-shell syntax of the
`active_slot=r` branch added in E2 is valid per U-Boot's own
tool, not just my manual `if`/`fi` count. Still never executes
for real (would need to compile real U-Boot from source for QEMU
target binary — considerably more work, not undertaken without
explicit request).

**This round's validation**: 5/5 configs (`qemu`, `vf2`, `k1`,
`no-mmu`, `no-ml`) still compile clean after touching `sbi.rs`
(affects all riscv64 targets, not just esp32c3) — verified again,
not assumed. Sanity QEMU boot clean after change.

## 2026-08-13 — `PWM_BASE` (vf2) wrong since before this session — found investigating HDMI, not searching

Starting framebuffer/HDMI work (see next section), adding `DC8200_NOC_BASE`
to `platform.rs` with the real address of the third DC8200 register block
(`0x1703_0000`, confirmed against mainline `jh7110.dtsi`/`jh7110-common.dtsi`),
I noticed it matched EXACTLY with existing `PWM_BASE` in same file. Two real
peripherals never share physical address — investigated immediately instead
of ignoring.

**Confirmed**: `PWM_BASE` in vf2 block was `0x1703_0000` since the initial
repo import commit — never corrected. Real PWM8 address (`pwm@120d0000`)
was already confirmed against primary source **in this same session**,
during PWM register model fix (batch 7) — but that fix worked only on
OFFSETS within `pwm.rs`'s `mmio` module, importing `PWM_BASE` from
`platform.rs` without re-verifying the value itself. Real consequence:
with register model corrected but base uncorrected, PWM code on real
hardware would have kept writing to wrong address — which happens to be
the DC8200 video controller's own NOC/clock block, not an unmapped zone.
Potentially the most dangerous bug of entire session if it had executed
on silicon: not "PWM doesn't work" but "PWM writes to another
peripheral's clock config".

**FIXED**: `PWM_BASE` corrected to `0x120D_0000` (confirmed, same source
as rest of session). Cleaned `PWM_STRIDE`/ `PWM_DUTY`/`PWM_PERIOD`/`PWM_ENABLE`
— confirmed by grep nothing uses them since register model fix, just
dead comments describing old model. Validated: 5/5 configs compile clean
after change.

**Lesson from the process itself**: a "register model" fix doesn't replace
re-verifying the base address that model assumes — it stayed silent debt
until unrelated investigation (HDMI) stumbled on collision by chance. Worth
remembering for any future MMIO review: ALWAYS verify the base, not just
relative offsets.

## 2026-08-13 — Framebuffer/HDMI (VF2): research complete, implementation starting

New direction explicitly requested by user, outside original review scope —
robot carries display in one of the models. Same pattern as rest of session:
primary source before code.

**Real hardware confirmed** (Linux mainline + starfive-tech fork,
`JH7110_VisionFive2_devel` branch for nodes not yet upstream):
- **DC8200** (display controller, Verisilicon) — 3 register blocks:
  `0x2940_0000` (top/config), `0x2940_0800` (main block, to offset `0x2544`),
  `0x1703_0000` (NOC/clock) — IRQ 95. Only HDMI path used; DSI/D-PHY exist
  in SoC but not wired.
- **HDMI TX** (Innosilicon, NOT DesignWare — corrected wrong initial assumption)
  — `0x2959_0000`, IRQ 99. 8-bit registers (byte-addressed), unlike DC8200
  which is word-aligned.
- DC8200 timing registers confirmed: `VSDC_DISP_HSIZE/VSIZE/
  HSYNC/VSYNC(n) = 0x1430/0x1440/0x1438/0x1448 + 4n` (relative to
  main block).
- Framebuffer registers confirmed: `VSDC_FB_ADDRESS(n)=0x1400+4n`,
  `VSDC_FB_STRIDE(n)=0x1408+4n`, `VSDC_FB_CONFIG(n)=0x1518+4n`,
  `VSDC_FB_SIZE(n)=0x1810+4n`, `VSDC_FB_CONFIG_EX(n)=0x1CC0+4n` (bit
  `FB_EN`=13, bit `COMMIT`=12).
- HDMI TX registers confirmed (extended timing + PHY):
  `HDMI_VIDEO_TIMING_CTL=0x08`, `HDMI_VIDEO_EXT_H*`/`_V*` at
  0x09-0x15, PHY block at 0xCE/0xE0-0xED (`SYNC`/`SYS_CTL`/
  `CHG_PWR`/`DRIVER`/`PRE_EMPHASIS`/`FEEDBACK_DIV_RATIO_*`/
  `PRE_DIV_RATIO`), mute/enable at `HDMI_AV_MUTE=0x05` bit
  `VIDEO_BLACK`=0.

**Two genuine gaps, not solved by design, marked explicitly
in code instead of guessed**:
1. Pixel format enum for `VSDC_FB_CONFIG_FMT` field (6 bits) —
   what numeric value corresponds to XRGB8888/RGB565 didn't appear
   in any consulted source.
2. HDMI PHY PLL calibration table (`FEEDBACK_DIV_RATIO`/
   `PRE_DIV_RATIO` per target pixel frequency) — table by known
   frequency in reference driver, not simple formula; specific
   values for JH7110 not found.

**Simplifications agreed with user** before writing code:
fixed resolution without EDID negotiation, fixed pixel format, single
plane, initial goal = solid color (no text/font).

**License discipline, explicitly recalled by user
mid-investigation**: data extracted from Linux drivers (MMIO addresses,
register offsets, what bit does what) are facts about hardware, not
copyright expression — used as you'd use a datasheet. Rust code in
`crates/display` written from scratch, without transliterating
structure/abstractions from Linux driver (GPL) — project is Apache 2.0.

**Status**: `crates/display` crate created and complete for milestone 1
(solid color) — `Cargo.toml`, `lib.rs` (framebuffer + orchestration),
`dc8200.rs`, `hdmi.rs`. New `hdmi` feature in `kernel/Cargo.toml`
(opt-in, not part of `vf2` bundle), Kconfig `HDMI_WIDTH`/
`HDMI_HEIGHT` in `Kconfig.platform` (default 640/480, with
compile-time `assert!` in `lib.rs` forcing match with fixed VESA mode
if someone changes Kconfig without updating timing).
`robot_os_display::display_init()` call wired in `kernel_main`,
just after FAT32 mount, gated behind `#[cfg(feature = "hdmi")]`.

**Real incidental bug found, unrelated to HDMI**: adding
`DC8200_NOC_BASE` to `platform.rs` matched exactly existing `PWM_BASE`
— led to discovery and fix of `PWM_BASE` wrong since before this session
(see own section above, "`PWM_BASE` (vf2) wrong since before this
session").

**Second real problem found during validation, unrelated
to HDMI**: `crates/display` had no own feature — since `cargo
build --features X` (no `-p`) always compiles ALL workspace members,
crate tried to compile unconditionally, expecting `DC8200_MAIN_BASE`/
`HDMI_TX_BASE` to exist regardless of active board. With `.config` on
`BOARD_VF2` but `--features qemu`, this crashed with "cannot find value".
**Fixed**: new own `vf2` feature in crate's `Cargo.toml`, all real
`lib.rs` body behind `#[cfg(feature = "vf2")]` with no-op `display_init()`
when inactive — same pattern preventing any `cargo build --features <whatever>`
from trying to compile board-specific code for a board that isn't that one.

**Validated**: 6/6 configs compile clean (`qemu`, `vf2,hdmi`, `vf2`,
`k1`, `no-mmu`, `no-ml`) — including new combination. **Not validated
on real hardware** — QEMU simulates neither peripheral, no test path
before physical bring-up phase. Two known gaps marked explicitly in own
code (see above): DC8200 pixel format enum, and especially HDMI PHY PLL
calibration — without latter, expect no monitor signal even if everything
else is correct. Treat as same confidence level as rest of "fixed by
primary source, never executed" from this session.

**Note on `.config`**: left in `vf2` profile (`make defconfig-vf2`,
run to regenerate new Kconfig symbols) — gitignored/ local, deliberately
not restored to prior state, since vf2 is active hardware for this phase
of project.

## `ramfb` (QEMU): two real bugs found and fixed, validated visually

QEMU-only path (`crates/display/src/ramfb.rs`, feature `ramfb`) — unrelated
to real VF2 driver. Two real failures found booting for real in QEMU, not
hypothetical:

**Bug 1 — selector register requires real 16-bit access.** `select()`
did two separate 8-bit writes to selector register (offset
`FW_CFG_BASE+8`). QEMU rejected it with `Store/AMO access fault`
(`scause: 0x7`) on first real boot. Fixed: single volatile 16-bit write
(`key.to_be()` — fw_cfg MMIO region is `DEVICE_BIG_ENDIAN`, confirmed
same pattern applied to bug 2's DMA register).

**Bug 2 — classic byte-at-a-time DATA register write is no-op since QEMU v2.4.**
With bug 1 fixed, kernel booted clean and `[RAMFB] configured: ...` printed
error-free — but screendump showed uniform black, byte-identical across runs
(hint not time race but data simply never arrived). Confirmed against QEMU
source (`hw/nvram/fw_cfg.c`): `fw_cfg_write()` — classic DATA register write
handler — literally `/* nothing, write support removed in QEMU v2.4+ */`. Entire
`write_data()`/`write_be32`/`write_be64` loop this driver wrote to register QEMU
silently discards; `ramfb_fw_cfg_write()` (callback really installs surface)
never fired. Confirmed with `xp` over QMP that RAM framebuffer had correct color
(`0x004080ff` repeating) — ruling out fill/address bug, isolating to DATA
register itself.

**Real fix**: fw_cfg DMA protocol (`REG_DMA` at `FW_CFG_BASE+16`, offset
confirmed against `hw/riscv/virt.c` → `fw_cfg_init_mem_dma` →
`fw_cfg_init_mem_internal(base+8, base, 8, base+16, ...)`). Build 16-byte
`fw_cfg_dma_access` descriptor (`control`, `length`, `address`, all big-endian,
field-by-field verified against `include/standard-headers/linux/qemu_fw_cfg.h`
and full `fw_cfg_dma_transfer()` body in `hw/nvram/fw_cfg.c`) in guest RAM, write
its physical address single volatile 64-bit write to `REG_DMA` — QEMU fires
transfer synchronously within that write (confirmed: `size==8 && addr==0` →
`fw_cfg_dma_transfer(s)` direct, no polling). 28-byte payload (`RAMFBCfg`:
`addr`+`fourcc`+`flags`+`width`+`height`+`stride`, field order confirmed against
`hw/display/ramfb.c`) transfers in one DMA call instead of 28 useless 1-byte writes.

**Actually validated, not just "compiles"**: clean rebuild, boot QEMU, `screendump`
via QMP (manual JSON-RPC handshake — first try `nc`+telnet gave no clear failure
signal, switched to real QMP) read with Python: **307200/307200 pixels = exactly
`(0x40, 0x80, 0xFF)`**, expected solid color, uniform across screen. First visual
framebuffer milestone in this kernel, end-to-end.

**Real VF2 driver relevance (`hdmi`, not QEMU)**: none direct — DC8200/HDMI TX
use no fw_cfg, real JH7110 hardware with own MMIO register protocol. But sets
useful methodological precedent: success log (`[RAMFB] configured: ...`) without
access fault not proof data reached device — must verify final effect (here,
real pixel content), not just absence of exception.
