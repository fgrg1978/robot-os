# PHANES Safety Coding Standard (SC-1..SC-10)

> **Audience:** contributors to safety-critical crates  
> **Pre-requisites:** RFC-0013 (quality engineering), RFC-0015 (cert)  
> **Scope:** the **safety crates** —
> `crates/{ipc, sched, mm, ota, crypto, arch, abi, topology, behavior}`.
> Other crates (`drivers`, `fs`, `net`, `ml`, `camera`, etc.) are
> encouraged to follow these but it's not enforced by lint.

This document codifies the ten safety rules referenced throughout
the PHANES RFC corpus (see RFC-0013 §"Coding standards" and RFC-0015
§"Coding standards"). They derive from MISRA, IEC 61508 Annex C,
ISO 26262 Part 6, and the Rust Embedded Working Group's
safety-subset guidelines.

The standard is **enforced** today by:
- `clippy::pedantic` on the safety crates
- The `unexpected_cfgs` workspace lint
- Manual review during PR
- (W6 future) Custom rustc lints for SC-1 / SC-2 / SC-7

It will be enforced **automatically** by:
- (W6 future) cargo-mutants on safety crate test suites
- (Phase 2+) Custom `clippy::pedantic` rules
- (Phase 3) Ferrocene qualified compiler

---

## SC-1 — No dynamic allocation in safety paths

**Rule:** Safety crates may not use `alloc::*` types (`Box`, `Vec`,
`String`, `BTreeMap`, etc.) **at runtime**. Use fixed-size arrays,
`heapless` collections, or stack buffers.

**Why:** Allocator failures are non-recoverable in a kernel without
a paging-to-disk story. ISO 26262 requires deterministic memory
behaviour for ASIL-D.

**How to apply:** Each safety crate has `#![no_std]` and does **not**
add `extern crate alloc;`. Fixed pools (`CapTable`, `Topology`,
runqueue arrays) are sized at compile time.

**Tests / dev allowance:** `#[cfg(test)]` blocks may use `alloc`
because they run on the host.

---

## SC-2 — All loops are bounded

**Rule:** Every loop in a safety crate has a statically analysable
upper bound. Unbounded `loop { … }`, `while condition { … }` with
a non-monotonic condition, and `while let Some(…) = iter.next()`
without a known iter length are forbidden.

**Why:** Static analysis tools (Kani, MISRA checkers) need bounds
to prove termination. Cert auditors want termination proofs.

**How to apply:**

- Use `for _ in 0..N` with a `const` or `let bound = …;` expression.
- Use iterator combinators on bounded collections.
- For event-driven loops in the kernel main: those are at the
  *boundary* of safety code — kernel main itself isn't a safety
  crate and may use unbounded `loop`. Inside the safety crates,
  all loops are bounded.

**Tests / dev allowance:** None — applies even in `#[cfg(test)]`.

---

## SC-3 — No panics in safety code paths

**Rule:** Safety code must not invoke `panic!`, `unwrap()` on a
runtime-fallible value, `expect()` with a fallible source, indexing
that could overflow, or arithmetic that could panic.

**Why:** A panic in a kernel context is catastrophic. Cert auditors
demand panic-freedom in the safety path.

**How to apply:**

- Use `?` with explicit error types (`Result<_, Errno>`).
- Use `checked_*`, `saturating_*`, `wrapping_*` arithmetic.
- Use `get(idx)` instead of `[idx]` on slices when index source is
  untrusted.
- Where panic is the only sane response (e.g., violated invariants
  in `task_create_affinity`'s pool lock), the panic site is
  documented in the safety case as "unreachable by construction"
  with a justification.

**Tests / dev allowance:** Tests may panic to indicate failure. The
runtime panic_handler is part of `kernel/`, not a safety crate.

---

## SC-4 — No recursion

**Rule:** Safety crates may not use recursive functions or
recursive types that recurse at runtime.

**Why:** Recursion makes stack-bound analysis intractable. Tail-call
optimization isn't guaranteed in stable Rust. WCET analysis is
much easier on iterative code.

**How to apply:**

- Use iteration instead of recursion.
- Tree traversal via explicit stack (`heapless::Vec<_, N>`).
- Recursive types (`enum List { Cons(_, Box<List>), Nil }`) are
  banned because they require allocator + boxed self-reference;
  see SC-1.

**Tests / dev allowance:** Host-side tests may use recursion (they
run on the host's stack).

---

## SC-5 — Every `unsafe` has a `// SAFETY:` comment

**Rule:** Every `unsafe { … }` block and every `unsafe fn` body
must have an immediately-preceding `// SAFETY:` comment that
justifies the soundness contract.

**Why:** `unsafe` is where Rust's guarantees end. Without a written
justification, the reviewer has no way to audit correctness.

**How to apply:**

```rust
// SAFETY: `idx` was returned by a successful `alloc_slot()` call;
// the slot is exclusively owned for the lifetime of this guard.
unsafe {
    let task = task_mut(idx);
    task.tid = NEXT_TID;
    NEXT_TID += 1;
}
```

**Enforced by:** `clippy::undocumented_unsafe_blocks` lint (enabled
in safety crates).

**Tests / dev allowance:** None — applies everywhere.

---

## SC-6 — All public APIs have type-state where possible

**Rule:** Where the type system can express a runtime invariant
(e.g., "this handle has been validated"), use a wrapper type to
encode it.

**Why:** Type-state moves invariant violations from runtime checks
to compile errors. Cert auditors love this.

**How to apply:**

- `Cap<T>` (RFC-0003) — the kind is in the type; you can't pass a
  `Cap<Sensor>` where `Cap<Channel>` is expected.
- `MaybeStr<'a>` (`crates/topology/src/types.rs`) — borrows from
  validated input, doesn't allocate.
- Future: `Validated<T>` newtype for parsed-and-checked inputs.

**Tests / dev allowance:** None.

---

## SC-7 — Numeric overflow is either explicit or panic-free

**Rule:** Arithmetic in safety code uses `checked_*`,
`saturating_*`, or `wrapping_*` explicitly. Bare `a + b` is allowed
**only** when one of:
1. The compiler can prove no overflow (constant inputs).
2. Overflow is acceptable and documented as `// OVERFLOW OK: …`.

**Why:** Default arithmetic panics on overflow in debug builds and
silently wraps in release — a divergence that can hide bugs. ISO
26262 requires deterministic overflow handling.

**How to apply:**

```rust
// Good — explicit:
let next = prev.checked_add(1).ok_or(Errno::EQUOTA)?;
let timeout = now.saturating_add(period);
let gen = current.wrapping_add(1);

// OVERFLOW OK: timestamp_us cannot exceed u64::MAX in
// the lifetime of the kernel (584 years at 1 µs resolution).
let elapsed = now_us - start_us;
```

**Workspace setting:** `overflow-checks = true` in release profile
already enabled (top-level `Cargo.toml`).

---

## SC-8 — No floating-point in the safety scheduler / deadline math

**Rule:** The safety scheduler classes (`SafetyCritical`, `HardRT`)
and any deadline / budget math may not use `f32` / `f64`.

**Why:**
- Many target CPUs (Cortex-R) lack hardware FP.
- FP rounding makes WCET analysis hard.
- ISO 26262 strongly discourages FP in safety-critical code.

**How to apply:**

- Budget percentages stored as `u8` (`min_pct: u8`).
- Time arithmetic in microseconds (`u32` or `u64`).
- Liu-Layland admission in per-mille:
  `Σ (C × 1000 / T) ≤ 1000`.

**Allowed:** FP is fine in AI / camera / non-safety crates
(`crates/ml`, `crates/camera`, `crates/flight` controller PID can
be FP if the platform supports it).

---

## SC-9 — Bounded recursion in formal proofs

**Rule:** Kani harnesses (RFC-0006) bound any recursion or loop so
the model-checker terminates. Use `#[kani::unwind(N)]` or explicit
loop-count assumptions.

**Why:** Kani is bounded model-checking; without bounds it can't
terminate. A harness that exhausts CBMC memory is worthless.

**How to apply:**

```rust
#[kani::proof]
#[kani::unwind(MAX_CAPS_PER_TASK + 1)]
fn cap_table_get_terminates() { … }
```

**Tests / dev allowance:** Only Kani harnesses; runtime loops use
SC-2 (bounded).

---

## SC-10 — Traceability: every safety requirement maps REQ→CODE→TEST

**Rule:** Every entry in `safety/SAFETY_REQS.md` (RFC-0015) maps
to:
1. The RFC that establishes it.
2. The source file:line that implements it.
3. The test or formal proof that verifies it.

**Why:** Cert audit requires bidirectional traceability. An
unverified requirement is a finding.

**How to apply:** The matrix lives in `safety/traceability.csv`
(W6 W7 / Phase 2 deliverable). CI gate verifies every `SR-*` ID has
a non-empty `code_path` and `test_path`.

**Tests / dev allowance:** None.

---

## Status today (Phase 1 close)

| Rule | Enforced now | Method | Gaps |
|------|--------------|--------|------|
| SC-1 | ✅ Partial | `#![no_std]` + no `extern crate alloc;` in safety crates | No automated lint yet |
| SC-2 | ⚠️ Manual | PR review | No clippy rule yet |
| SC-3 | ⚠️ Manual | PR review | No `panic_path` lint yet |
| SC-4 | ⚠️ Manual | PR review | No recursion lint yet |
| SC-5 | ✅ | `clippy::undocumented_unsafe_blocks` | — |
| SC-6 | ✅ | `Cap<T>`, `MaybeStr<'a>` patterns established | — |
| SC-7 | ✅ Partial | `overflow-checks = true` release profile | No `checked_*` enforcement lint |
| SC-8 | ✅ | No FP in sched / budgets / cap-table code | — |
| SC-9 | ✅ | All current Kani harnesses bound their loops | — |
| SC-10 | ⏳ | `safety/traceability.csv` is Phase 2 work | RFC corpus tracks invariants |

**Phase 2 work** brings SC-1, SC-2, SC-3, SC-4 to automated enforcement
via custom `clippy::pedantic` rules + Ferrocene's qualified subset.

## References

- RFC-0013 Quality Engineering
- RFC-0015 Compliance & Certification
- ISO 26262-6 Software unit design and implementation
- IEC 61508 Annex C — Recommended techniques and measures
- MISRA C 2012 (where applicable to Rust analogue)
- Rust Embedded Working Group safety-subset guidelines
- Ferrocene Language Specification subset
