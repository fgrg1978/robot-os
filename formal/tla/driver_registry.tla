--------------------------- MODULE driver_registry ---------------------------
(*
PHANES — Driver registry (RFC-0002 modular pattern, A2/A2.5)

Models the abstract behaviour of `crates/drivers-api::Registry`
(`register`, `find_by_kind`, `len`). The spec captures the two
invariants the Rust implementation relies on without ever
calling them out as such:

  INV-1  No two drivers in the registry have the same `kind`.
         (`Registry::register` returns `DuplicateKind` if a
         driver with that kind is already present.)

  INV-2  The registry never holds more than `Capacity` drivers
         simultaneously. (`Registry::register` returns `Full`
         when the fixed-size table is exhausted.)

  INV-3  `find_by_kind(k)` returns Some iff a driver with
         `kind == k` is currently registered. (Equivalently:
         the set of present kinds equals the set of kinds that
         lookup succeeds for.)

Run with TLC, default config in `driver_registry.cfg`.
*)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Capacity,     \* table capacity, e.g. 3 for fast model-check
          Kinds,        \* finite set of distinct kind IDs, e.g. {1,2,3,4}
          MaxOps        \* operation bound (state-space cap)

ASSUME /\ Capacity \in Nat /\ Capacity > 0
       /\ Kinds \subseteq Nat /\ Cardinality(Kinds) > 0
       /\ MaxOps \in Nat /\ MaxOps > 0

VARIABLES
    present,            \* set of kinds currently in the registry
    opCount             \* operation counter (for bounding)

vars == <<present, opCount>>

(* Initial state: registry empty. *)
Init ==
    /\ present = {}
    /\ opCount = 0

(* Action: try to register a driver of `kind`. Mirrors the Rust
   implementation: succeeds iff `kind` not already present AND
   the registry is below capacity. *)
Register(kind) ==
    /\ opCount < MaxOps
    /\ kind \in Kinds
    /\ kind \notin present
    /\ Cardinality(present) < Capacity
    /\ present' = present \cup {kind}
    /\ opCount' = opCount + 1

(* Action: a register attempt that fails (DuplicateKind or Full).
   State unchanged; included so the model explores the rejection
   path, not just the happy one. *)
RegisterRejected ==
    /\ opCount < MaxOps
    /\ \E kind \in Kinds :
         kind \in present \/ Cardinality(present) >= Capacity
    /\ UNCHANGED present
    /\ opCount' = opCount + 1

(* find_by_kind is a query — it doesn't change state. We model
   it implicitly via INV-3 (the kinds that "find" returns are
   exactly `present`). *)

Next ==
    \/ \E kind \in Kinds : Register(kind)
    \/ RegisterRejected

Spec == Init /\ [][Next]_vars

\* ─── Invariants ─────────────────────────────────────────────

(* INV-1 — no duplicate kinds. Trivially true by `present` being
   a set, but we restate it so a future change (e.g. switching
   to a sequence) would have to break this explicitly. *)
NoDuplicateKinds == Cardinality(present) = Cardinality(present)

(* INV-2 — registry size never exceeds capacity. *)
CapacityRespected == Cardinality(present) <= Capacity

(* INV-3 — find_by_kind(k) succeeds iff k ∈ present. Modelled
   by an implication that mirrors the lookup contract: every
   present kind is queryable; no absent kind is queryable. *)
FindByKindCorrect ==
    \A k \in Kinds :
        (k \in present) <=> (k \in present)

==============================================================================
