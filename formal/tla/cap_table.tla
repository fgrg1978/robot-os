--------------------------- MODULE cap_table ---------------------------
(*
PHANES — Per-task capability table

Models the abstract behaviour of `crates/ipc/src/cap.rs::CapTable`. The
spec is intentionally cap-shape-agnostic: it tracks slot occupancy,
generation, and outstanding handles. The point is to verify the
invariants of RFC-0003 §"Forgery resistance":

  INV-1  Generation values per slot are monotonic except for explicit
         wrap.
  INV-2  A handle is "valid" iff the slot's current generation matches
         the handle's generation **and** the slot is occupied.
  INV-3  Once revoked, a handle never becomes valid again without an
         explicit grant on the same slot incrementing the generation.

Run with TLC, default config in `cap_table.cfg` (next to this file).
*)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS NumSlots,    \* total slots, e.g. 4 for fast model-check
          MaxGen,      \* maximum representable generation, e.g. 3
          NumOps       \* bound on operations to explore, e.g. 8

ASSUME /\ NumSlots \in Nat /\ NumSlots > 0
       /\ MaxGen \in Nat /\ MaxGen > 0
       /\ NumOps \in Nat /\ NumOps > 0

VARIABLES
    slots,             \* function: slot -> [occupied: BOOL, gen: Nat]
    handles,           \* set of records {slot, gen} ever issued
    revoked,           \* set of records {slot, gen} that have been revoked
    opCount            \* operation counter, for bounding

vars == <<slots, handles, revoked, opCount>>

\* Initial state: every slot empty, generation 0, no handles, no revocations.
Init ==
    /\ slots = [s \in 1..NumSlots |-> [occupied |-> FALSE, gen |-> 0]]
    /\ handles = {}
    /\ revoked = {}
    /\ opCount = 0

\* "Pick a free slot" — any unoccupied slot; non-deterministic.
PickFree ==
    CHOOSE s \in 1..NumSlots : ~slots[s].occupied

CanGrant == \E s \in 1..NumSlots : ~slots[s].occupied

\* Action: grant a fresh cap on a free slot, bumping the generation.
Grant ==
    /\ opCount < NumOps
    /\ CanGrant
    /\ LET s == PickFree
           prev == slots[s].gen
           next == IF prev + 1 > MaxGen THEN 1 ELSE prev + 1
       IN /\ slots' = [slots EXCEPT ![s] = [occupied |-> TRUE, gen |-> next]]
          /\ handles' = handles \cup {[slot |-> s, gen |-> next]}
          /\ revoked' = revoked
          /\ opCount' = opCount + 1

\* Action: revoke any currently-occupied slot.
Revoke ==
    /\ opCount < NumOps
    /\ \E s \in 1..NumSlots : slots[s].occupied
    /\ \E s \in 1..NumSlots :
         /\ slots[s].occupied
         /\ slots' = [slots EXCEPT ![s] = [occupied |-> FALSE, gen |-> slots[s].gen]]
         /\ revoked' = revoked \cup {[slot |-> s, gen |-> slots[s].gen]}
         /\ handles' = handles
         /\ opCount' = opCount + 1

Next == Grant \/ Revoke

Spec == Init /\ [][Next]_vars

\* ───────────────────────────────────────────────────────────────────────
\* Invariants
\* ───────────────────────────────────────────────────────────────────────

\* INV-A: A handle is "valid" iff its generation matches the slot's
\* current generation AND the slot is occupied AND it has not been
\* explicitly revoked at this generation.
ValidHandle(h) ==
    /\ slots[h.slot].occupied
    /\ slots[h.slot].gen = h.gen
    /\ h \notin revoked

\* INV-B: At most one valid handle per slot (since granting bumps the gen
\* and we don't dup in this model).
AtMostOneValidPerSlot ==
    \A s \in 1..NumSlots :
        Cardinality({h \in handles : h.slot = s /\ ValidHandle(h)}) <= 1

\* INV-C: A revoked handle is never valid.
RevokedNeverValid == \A h \in revoked : ~ValidHandle(h)

\* INV-D: Generations are bounded by MaxGen.
GenInRange == \A s \in 1..NumSlots : slots[s].gen <= MaxGen

TypeOK ==
    /\ slots \in [1..NumSlots -> [occupied: BOOLEAN, gen: 0..MaxGen]]
    /\ handles \subseteq [slot: 1..NumSlots, gen: 1..MaxGen]
    /\ revoked \subseteq [slot: 1..NumSlots, gen: 1..MaxGen]
    /\ opCount \in 0..NumOps

\* ───────────────────────────────────────────────────────────────────────
\* The properties we want TLC to check
\* ───────────────────────────────────────────────────────────────────────

\* These are passed as INVARIANT in the .cfg file:
\*   - TypeOK
\*   - AtMostOneValidPerSlot
\*   - RevokedNeverValid
\*   - GenInRange

================================================================
