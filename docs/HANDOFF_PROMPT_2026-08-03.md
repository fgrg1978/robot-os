# HANDOFF PROMPT — Review + Audit of robot-os Kernel and Brain
# Generated: 2026-08-03 (v3). Source sessions: a879b51e (v1), f7d83d9a (v2/v3).
# Replaces HANDOFF_PROMPT_2026-08-02.md. Have the agent read this as the
# first message of the new Claude Code CLI session.

===============================================================================
0. WHAT THIS IS
===============================================================================

Long (days) code review of two repos. TWO parallel threads:

  (A) AUTOMATIC multi-agent audit (Workflow) searching for bugs.
      -> Last run (wf_1edef0a7-3dd) died by SESSION CREDIT LIMIT: completed
         only 1 of 17 finders (os-drivers, 15 new unverified candidates).
         Detail and relaunch plan in section 4.
  (B) MANUAL line-by-line review, with user present, on screen.
      -> IN PROGRESS. Goes through `kernel/src/main.rs`. Detail in section 5.

Persistent notes for ALL review live in the repo:
  /Users/azor/Library/Mobile Documents/com~apple~CloudDocs/Development/ia/robot-os/docs/KERNEL_REVIEW_NOTES.md
READ THAT ENTIRE FILE BEFORE DOING ANYTHING. It is the source of truth for
findings — it already includes the complete table of the 15 os-drivers
candidates (section "2026-08-02/03"). Every new finding is noted there, same
format (sections by date, [OPEN]/[CLOSED]/[BUG], emoji 🔴/🟡).

===============================================================================
1. REPOS AND PATHS (Always use absolute paths)
===============================================================================

robot-os (Rust kernel, RISC-V/aarch64/x86_64, ~80 kLoC, 50+ crates):
  /Users/azor/Library/Mobile Documents/com~apple~CloudDocs/Development/ia/robot-os

robot-brain (Python server, ~31 kLoC, 1279 pytest tests):
  /Users/azor/Library/Mobile Documents/com~apple~CloudDocs/Development/ia/robot-brain

Review notes (source of truth for findings):
  robot-os/docs/KERNEL_REVIEW_NOTES.md

Audit Workflow script (audited, with fix v2 + batch support):
  /Users/azor/.claude/projects/-Users-azor-Library-Mobile-Documents-com-apple-CloudDocs-Development-ia-robot-brain/a879b51e-43cc-4f3e-b2b8-77bfe71eb6d7/workflows/scripts/full-audit-os-brain-wf_c7cbb828-4ab.js

Journal of run wf_1edef0a7-3dd (real returns per agent; complete suggested fixes
for the 15 driver candidates):
  /Users/azor/.claude/projects/-Users-azor-Library-Mobile-Documents-com-apple-CloudDocs-Development-ia-robot-os/f7d83d9a-91da-44bf-b83d-dab426e753f6/subagents/workflows/wf_1edef0a7-3dd/journal.jsonl

===============================================================================
2. HARD RULES (Non-negotiable)
===============================================================================

- NEVER automatic `git commit` or `git push`. Only if the user explicitly asks
  with those words. If something deserves commit, ask.
- ZERO MAGIC NUMBERS. Every numeric value as named constant or config.
- Shell PATH is broken. On EVERY Bash command, prefix:
    export PATH="/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.cargo/bin:$PATH";
  or use absolute paths: /usr/bin/grep, /bin/cat, /usr/bin/find, /usr/bin/wc,
  /usr/bin/python3, /opt/homebrew/bin/qemu-system-riscv64
- ⚠️ The `timeout` binary does NOT EXIST on this machine (no /usr/bin/timeout,
  no gtimeout, no coreutils — verified, exit 127). To bound a command, use
  the Bash tool `timeout` parameter (milliseconds), never shell wrapper.
- `tail` and `head` are NOT reliable here. Use Read (offset/limit),
  /usr/bin/wc or /usr/bin/python3.
- Verify builds by EXIT CODE, not by grepping "error"/"warning".
- No writing RFCs while experimenting. Document only AFTER approving something.
- Do not invent progress. If something hasn't executed or been seen, say so.
- Workflow consumes LOTS of credits: do not relaunch without user asking,
  and relaunch BY BATCHES (see 4.4). If it fails massively: STOP and alert user.

===============================================================================
3. MANDATORY FORMAT FOR MANUAL REVIEW (thread B)
===============================================================================

Explicitly requested by user; follow strictly:

  1. ALWAYS show code on screen. Never describe without pasting.
  2. SMALL chunks: 5-15 lines at a time. Never a whole file at once.
     Deliberately slow pace.
  3. Mark the commented line with `->` at the beginning.
  4. Tag findings with emoji (terminal doesn't render color):
       🔴 CRITICAL  = real bug or real risk
       🟡 REVIEW    = odd, needs confirmation, non-blocking
  5. At end of each chunk, dump findings to
     robot-os/docs/KERNEL_REVIEW_NOTES.md in its existing format.
  6. Wait for user to say "continue" before next chunk.

===============================================================================
4. STATUS — THREAD A: AUTOMATIC AUDIT
===============================================================================

### 4.1 What It Is
Workflow of 17 fronts (13 robot-os + 4 robot-brain). Per front: 1 finder ->
each finding verified by 2 adversarial agents -> confirmed only 2/2. 100%
READ / STATIC ANALYSIS. No compile, no test execution, no code changes.

robot-os fronts (13): os-boot-entry, os-sched-smp, os-mm, os-ipc, os-net,
os-drivers, os-security, os-safety, os-fs-ota, os-rt-wcet, os-syscall,
os-flight-nav, os-config.
robot-brain fronts (4): brain-protocol, brain-server, brain-perception,
brain-fleet.

### 4.2 Run History
  wfogux638       -> stopped by hand
  wjyjo518c       -> crashed (null.findings) [script bug ALREADY FIXED]
  w8y011az8       -> PARTIAL complete: 30 candidates -> 11 confirmed 2/2,
                     15 with 0-1 votes, 4 resolved. Dumped in notes
                     (section 2026-07-24/25).
  wanwmai9b       -> stopped on its own
  wb8dswn40       -> stopped by hand
  wf_1edef0a7-3dd -> 2026-08-02/03, first execution of anti-stall fix v2.
                     DIED BY SESSION CREDIT LIMIT (reset 2:20am):
                     46 of 47 agents failed. Only completed find:os-drivers
                     -> 15 candidates, 0 verified. ~1.6M tokens burned.
                     Dumped in notes (section 2026-08-02/03).

### 4.3 ⚠️ ANTI-STALL FIX v2: STILL UNVALIDATED
History: old runs died by hung agents >3 min (hypothesis: unbounded
builds/tests). Fix v1: ban builds/tests + require /usr/bin/timeout 25. Script
audit (2026-08-02) discovered v1 was BROKEN (/usr/bin/timeout doesn't exist ->
exit 127) and applied v2: use Bash tool `timeout` parameter and warn that the
binary doesn't exist. In run wf_1edef0a7-3dd THERE WERE STALLS AGAIN despite v2
(os-mm, os-safety, os-sched-smp, os-security, os-net, os-rt-wcet — no progress
1100-4900 s, retries exhausted) BEFORE credit limit killed the rest. Not
distinguishable whether original problem or API degrading near limit. => Diagnosis
hypothesis NOT confirmed nor refuted. Stalls occurred with agents doing normal
Read/Grep, pointing more to API/infra than hung commands. Watch in next run.

### 4.4 HOW TO RELAUNCH (only when user asks) — BY BATCHES
Script now supports batches: `args: {only: ['key', ...]}` (no args runs all 17).
Previous run burned entire session limit at once; don't repeat. Suggested plan
(adjust with user):

  Batch 1 (robot-brain, ZERO historical coverage — priority):
    Workflow({ scriptPath: "<script, section 1>",
      args: { only: ['brain-protocol','brain-server','brain-perception','brain-fleet'] } })
  Batch 2 (robot-os core):
    args: { only: ['os-sched-smp','os-mm','os-ipc','os-boot-entry'] }
  Batch 3 (robot-os rest):
    args: { only: ['os-net','os-security','os-safety','os-fs-ota'] }
  Batch 4 (robot-os tail):
    args: { only: ['os-rt-wcet','os-syscall','os-flight-nav','os-config'] }

DO NOT include 'os-drivers' in any batch: its finder already completed and its
15 candidates are in notes. Verifying them is separate task (4.5). Resume
(`resumeFromRunId`) does NOT work across sessions — always launch fresh. Each
batch runs in background and takes time. Report real result, no padding. If
batch fails massively: STOP and tell user.

### 4.5 PENDING VERIFICATION (two blocks)
  (a) The 15 NEW os-drivers candidates (table in notes, 0/2 votes).
      Options: small ad-hoc workflow that only verifies (2 adversarial
      verifiers per finding, prompt like verifyPrompt from script), or
      manual one-by-one verification in thread B. The 4 critical ones
      (gps:394, baro:163, ina219:127, platform:96) first.
  (b) The 15 OLD findings with 0-1 votes from run w8y011az8 (table in
      notes, section 2026-07-24/25). Not refuted; missing 2nd verification.

### 4.6 MODEL STRATEGY / COST
  CHEAP MODEL (haiku; sonnet if haiku falls short) for mechanical work:
    execute tests and parse results, inventory, massive grep/find,
    collect context, mechanical checks (constant drift, protocol sync,
    broken imports, TODO/FIXME), and WRITE/DUMP extensive documentation
    (notes, findings tables, handoffs) — explicit user request (2026-08-03):
    "use sonnet or cheap agents for writing". Common-sense exception: a
    small edit is done directly (delegating costs more than doing).
  STRONG MODEL (session's model, don't lower) for:
    concurrency/races/ordering/lifetimes, adversarial verifiers
    (don't save there), security/crypto/safety-critical/WCET, synthesis.
  In Workflow: agent(prompt, { model: 'haiku', effort: 'low', ... }) for
  mechanical; no `model` to inherit strong.
  >>> Cheap does NOT mean shallow: EXHAUSTIVE coverage. If a front doesn't
      fit one agent, split and report in log. Save on AGENT TYPE, not
      COVERAGE. Never truncate silently.

### 4.7 TESTS: NONE EXECUTED
Everything "confirmed" is by AGENT CONSENSUS READING CODE. Not one QEMU test
nor pytest in entire audit. Current pass is static ON PURPOSE (script's
NO_SLOW_COMMANDS_NOTE forbids it — don't override by putting tests in
finders/verifiers). If empirical validation decided: SEPARATE PHASE, cheap
agents run, strong analyzes.

===============================================================================
5. STATUS — THREAD B: MANUAL REVIEW
===============================================================================

### 5.1 Already Reviewed (Closed)
  - kernel/linker.ld (complete)
  - kernel/src/entry/riscv64/asm/boot.S (complete)
  - Thread `tp`: Rust can clobber `tp` as scratch caller-saved; hence
    reasserts in main.rs before sched::start(), in smp_secondary_start
    and in each WFI loop iteration. Verified correct.
  - SMP bring-up: 3 findings, see 5.3.

### 5.2 Where To Resume EXACTLY
`kernel/src/main.rs`, function `kernel_main`. Already seen: trap_init, final
section of tp/scheduler start, SMP bring-up (~1040 and ~1237-1252).
MISSING, in order:
  1. Phase 1 — UART + DTB parse          (lines ~166-224)
  2. Phase 2 — memory PMM/VMM/heap       (lines ~226-350)
  3. Subsystem init up to ~1040          (long section)
(Optional before: close `_secondary_start` section of boot.S —
stvec / sscratch / SSTATUS_SPP.)

### 5.3 Open Manual Findings (detail in notes)
  🟡 Duplicate MAX_CPUS: kernel/src/main.rs:141 vs
     crates/sched/src/scheduler.rs:25                              [OPEN]
  🟡 wake_harts assumes contiguous hart IDs from 0 (smp.rs:36-43).
     Pending: does real VF2/K1 DTB enumerate harts contiguous from 0?
     (S7 usually is hart 0)                                        [OPEN]
  🔴 hart_start fails silently -> phantom CPU swallows tasks:
     smp.rs:31-32, main.rs:1040 vs 1241, scheduler.rs:251-263      [BUG]
     Proposed fix (not applied): propagate SBI isize, count successes,
     store NUM_ONLINE_CPUS afterwards with real number.
  🟡 Obsolete comment in crates/sched/src/smp.rs:58 ("Rust does not
     use tp" — false)                                              [MINOR BUG]
  Pending when reaching MMU/PMM/VMM: fixed 8M window, 4K aligned,
  vestigial fence.i post-bss, SSTATUS_SPP without nearby sret, bounds-check
  of secondary_stacks.

===============================================================================
6. SUMMARY OF ACCUMULATED FINDINGS (complete detail in notes)
===============================================================================

  - 11 CONFIRMED 2/2 (run w8y011az8) — none fixed, none validated in
    execution. Highlights: find_least_loaded_cpu counts wrong
    (scheduler.rs:251), broken seqlock vdso (vdso.rs:105), race port
    pending[] (port.rs:119), PWM/gripper dead in real HW (pwm.rs:187/177),
    GPIO RMW without lock (gpio.rs:139), Ed25519 secure-boot NOBODY CALLS
    (secure_boot.rs:208), 2 MiB buffer on stack (secure_boot.rs:235), panic
    handler takes FS lock (panic.rs:133), CRC mismatch no rollback
    (main.rs:544).
  - 15 OLD with 0-1 votes — NOT refuted, pending 2nd verification. Include:
    tp as task context + EDF migration (context_switch.S:83 — CROSSES with
    manual tp thread), wake_hart discards SBI error (smp.rs:29 — same bug as
    manual), lost-wakeup (waitqueue.rs:91), remote enqueue without CPU_LOCKS
    (scheduler.rs:1373), leak user_pt (scheduler.rs:617), 5 `static mut`
    without lock in crates/ipc/* (unique family — pattern fix), RST without
    sequence check (tcp.rs:965), MSS without clamp (tcp.rs:721).
  - 15 NEW from os-drivers (run wf_1edef0a7-3dd) — 0/2 votes, complete table
    in notes section 2026-08-02/03. Critical: NMEA parser ×10 (gps:394),
    BMP280 missing dig_p7 ≈ +80 m (baro:163), mAh ×3600 -> failsafe KILL in
    ~3.6 s (ina219:127), MMIO collision I2C0==UART1 on VF2 (platform.rs:96),
    blk_rw without lock (virtio/blk.rs:118). Cross pattern: driver inits
    ignoring bus errors and marking READY.
  - 4 CLOSED: 3 refuted 2/2 (vmm.rs:445, ina219.rs:71, trace lib.rs:169)
    and 1 tie 1-1 (main.rs:3853, wdt_kick) needing third opinion. NOTE:
    ina219.rs:71 (refuted, on I2C init) is NOT the same finding as
    ina219.rs:127 (new, mAh integration) — don't confuse them.

===============================================================================
7. PRIORITIZED PENDING LIST
===============================================================================

  [ ] 1. When user asks and credits available: relaunch audit BY BATCHES (4.4),
         starting with robot-brain (ZERO historical coverage).
  [ ] 2. Verify 15 new os-drivers candidates (4.5a) — the 4 critical ones first
         (gps/baro/ina219/platform are "robot doesn't work or falls out of the
         sky in real HW").
  [ ] 3. Complete 2nd verification of 15 old with 0-1 votes (4.5b).
  [ ] 4. Resume manual review in kernel/src/main.rs Phase 1 (5.2),
         strictly follow section 3 format.
  [ ] 5. Cross context_switch.S:83 and smp.rs:29 with manual tp/SMP thread.
  [ ] 6. Resolve 1-1 tie at main.rs:3853 (wdt_kick).
  [ ] 7. Decide empirical validation (QEMU/pytest) of confirmed — today all
         static. Cheap execute, strong analyzes.
  [ ] 8. Decide FIX plan. Nothing is fixed yet. Start with crates/ipc/*
         block with common pattern; assess same for "driver init ignores
         errors" pattern (imu/baro/i2c).
  [ ] 9. When kernel reviewed, move to robot-brain by hand.
  [ ] 10. Deadline contract on brain: check if kernel imposes time bound on
         link (what if brain takes 2 s / returns garbage / dies mid-maneuver?)
         and if there is fallback to deterministic controller. Write RFC-0037
         (graduated degraded mode) once and for all. Scoped and testable on
         QEMU. Useful for HERMES. Reasoning in notes, section 2026-08-07.
  [ ] 11. Brain authority envelope: the binary link IS the syscall boundary
         of the AI component. Can the brain, with well-formed packets, start
         an OTA without separate authority, raise a limit via PKT_CONFIG above
         the security envelope, or degrade L0-L3 layers? Does cap-IPC
         (RFC-0003) apply at that boundary or only intra-kernel? Reasoning in
         notes, section 2026-08-07.

===============================================================================
8. HOW TO START
===============================================================================

1. Read robot-os/docs/KERNEL_REVIEW_NOTES.md ENTIRELY. Don't launch anything.
2. Say in 5-8 lines the understood status (include: what happened with last
   run and what's left uncovered).
3. Ask user which pending from section 7 to tackle.
4. If manual review: strictly follow section 3 format — show code on screen,
   5-15 lines, `->`, 🔴/🟡, and wait for their "continue".
5. If relaunch audit: confirm with them WHICH BATCH, then only launch (4.4).
