# ADR 0004: `@snapshot` Boundary Operator

**Status:** Accepted (2026-05-05)
**Date:** 2026-05-04 (proposed) → 2026-05-05 (accepted; architect sign-off "yes to all" on the propositions)
**Deciders:** Goose (architect)
**Spec impact:** §2 (Grammar — new `@snapshot` expression form), §4 (Type System — snapshot-typed values), §5 (Type Checker — boundary verification), §10 (Error codes — E0550 family)
**DECISIONS.md:** Decision #24 ✓ LOCKED (2026-05-05) per this ADR.
**Refines:** Decision #1 (sigil layering), Decision #21 (shared automata).
**Companion:** ADR 0003 (Haskell-clean `@fn` discipline) — the `Readable` trait that ADR 0003 introduces is what `@snapshot` discharges.
**Branch:** `adr/0003-0004-haskell-clean-fn-and-snapshot`.

---

## TL;DR

Today, an `@fn` body cannot reference automaton state directly —
`Counter.value` from inside `@fn analyze() { ... }` is `E0101
ImperativeInFunctional` (Slice 1 of `clifford-check`). This is correct
for the strict pure-side reading: `@fn` doesn't see mutable state.

But there's an *anti-pattern* hiding in plain sight: book Ch. 39's
SPSC ring buffer reads `Uart.head` and `Uart.tail` from inside `@fn`
analysis code (the lock-free derived-`count` calculation). It works
because the parser hasn't enforced the rule yet for that specific
read pattern, not because it's truly safe.

Decision #24 locked the direction: **introduce `@snapshot Auto.field`
as the only way to read mutable automaton state into pure-side
analysis**. The boundary crossing becomes syntactically visible —
every `@fn` that reads automaton state contains a `@snapshot`
expression at the read site, and the type checker verifies the
read is well-defined (the field is plain-data, the snapshot is
read-once, the calling `@fn` declares `$ [Readable]` per ADR 0003).

This ADR resolves the four open questions Decision #24 explicitly
flagged:

1. **Expression vs statement?** → **Expression.**
   `let v := @snapshot Counter.value;` is one expression; composes
   inside `let`/`return`/argument positions.
2. **Copy-by-value vs ref-to-snapshot?** → **Copy-by-value for `Copy`
   types; `@snapshot_ref` (separate operator) for borrow snapshots
   in v0.4+.**
3. **Interaction with `#shared` (Decision #21)?** → **`@snapshot` of a
   `#shared` field requires the lock to be held by the caller's
   thread-plane (or to be statically demonstrable per ADR 0005).**
4. **Backward compatibility with the snapshot-by-convention pattern in
   book Ch. 39?** → **Existing implicit-read patterns get a
   deprecation warning in v0.2; mandatory `@snapshot` in v0.4+.**

**Recommendation:** Lock Decision #24 with the proposed design once
the architect signs off on the four resolutions. Implementation
gated to **v0.2** alongside ADR 0003's effect-row work; mandatory
migration in v0.4+.

---

## Context

### The hole Decision #24 closes

Decision #1 forbids `#`-constructs in `@fn`. The current Slice 1
check enforces this for *statement-form* constructs (`#mutate`,
`#> proc()`, `#unchecked_*`) and for *expression-form* constructs
that the resolver flags as `BindingRef::AutomatonField`. But what
about a `let v := Counter.value;` line in `@fn`? Today's check
already rejects it (the resolver tags it as automaton-field-access).

So one might ask: *isn't the rule already enforced?*

The answer is yes, but the **enforcement is brittle**:

1. The current rule is "no automaton-field reads in `@fn`." The
   `Readable` trait (ADR 0003) wants to *permit* them under
   controlled circumstances. Without `@snapshot`, the only way to
   relax the rule is to weaken the check entirely.
2. Book Ch. 39's SPSC ring buffer *demonstrates* the hole: a `@fn`
   computes `(head - tail) mod 64` from automaton state, working
   only because the prose paper doesn't run through `clifford-check`.
   Once enforced strictly, that example breaks.
3. The semantics of "read mutable state from a pure context" needs
   to be **named**, not implicit. With concurrent writers
   (Decision #21's `#shared` fields, `#interrupt` updates), a
   "naked" read could observe a torn value or a transient state. A
   `@snapshot` operator names the issue and lets us specify the
   read-correctness rules.

### The four pieces of the design

A `@snapshot` operator must answer:

- **Syntactic position.** Where in the grammar does it sit?
- **Value semantics.** Does it copy, borrow, or alias?
- **Concurrency.** What ordering / lock guarantees does it carry?
- **Migration.** What happens to existing programs that read
  automaton state implicitly?

This ADR proposes one resolution per question.

---

## Proposed design

### 1. Surface syntax: `@snapshot` is an expression

```
expr := … | snapshot_expr
snapshot_expr := '@snapshot' field_path
field_path    := ident '.' ident   // Auto.field
                | 'Self' '.' ident // Self.field (inside #transition only — but
                                   // @snapshot inside #transition is unusual;
                                   // see Q2 in §6)
```

Examples:

```clifford
@fn current_used() -> u32 $ [Readable] {
  let head := @snapshot Uart.rx_head;        // expression in let RHS
  let tail := @snapshot Uart.rx_tail;
  return (head - tail) % 64u32;              // pure arithmetic on snapshots
}

@fn report() -> () $ [Readable, Observable] {
  @log("buffer used: {}", @snapshot Uart.rx_head - @snapshot Uart.rx_tail);
                          // ^ snapshot in argument position
}
```

`@snapshot` binds tighter than binary operators (so the multiplication
above parses as `(@snapshot a) - (@snapshot b)`). Precedence is the
same as a function call.

### 2. Value semantics: copy-by-value for `Copy`; ref-to-snapshot deferred

`@snapshot Auto.field` evaluates to a *copy* of the field's current
value, with two constraints:

- The field's type must satisfy a `Copy`-style trait (no heap
  ownership transfer; values are bit-copyable). Primitive types,
  fixed-size arrays of primitives, tuples of primitives all qualify;
  `&[u8]` slices, `Vec<T>`-equivalents, `access<T>` references do
  not.
- The copy is **atomic** at the granularity of the field's type — for
  primitive types up to the target's word size (typically 32 or 64
  bits), the read is a single load instruction; for larger types,
  the field must be `#shared` and the read must hold the relevant
  lock (see §3.3).

**Deferred to v0.4+:** `@snapshot_ref Auto.field` for borrow
snapshots. Returns `&T` valid for a scoped lifetime (the surrounding
`@fn` body). The trade is that the borrowed snapshot can't outlive
the `@fn` call, but it lets you snapshot large values without
copying. The ref form needs careful interaction with `#shared` lock
release; v0.2 punts.

### 3. Concurrency semantics

Three cases:

#### 3.1 Field is plain `Private` (the default v0.1 case)

The snapshot is a single load instruction (for word-size types).
On every architecture Clifford targets, single-word loads are
atomic. The snapshot value reflects the field's value at *some*
point during the snapshot's evaluation; concurrent writers may
update before or after.

This is the same memory model the §7.0.1 "Safety Pillars" already
rely on for the SPSC pattern's `head` / `tail` reads.

#### 3.2 Field is plain `Private`, but type is wider than word size

The compiler emits `E0551 SnapshotNotAtomic`: "field `Big.payload`
of type `[u8; 32]` is wider than the target's word size; cannot
snapshot atomically. Make `Big.payload` `#shared` (Decision #21) and
acquire the relevant lock, OR snapshot the individual atomic
sub-fields you need."

#### 3.3 Field is `#shared` (Decision #21)

The snapshot must be inside a context that statically demonstrates
the lock is held:

- Inside an `#effect` / `#interrupt` body whose `#mutates` declares
  the lock-protected automaton AND that body has acquired the lock
  via `#with_lock` (Decision #21).
- Inside an `@fn` whose caller chain demonstrates the lock is held
  (effect-row tracks a `Holds<L>` capability — v0.4+ feature TBD).

For v0.2, `@snapshot` of a `#shared` field is permitted **only**
from `#`-layer bodies that hold the lock; from `@fn` it is
`E0552 SnapshotNeedsLockProof`.

### 4. Type-checker rule

The static check (in `clifford-check`, alongside Slice 1's existing
`E0101 ImperativeInFunctional`):

- Inside `@fn`: a bare `Auto.field` reference (the existing
  `BindingRef::AutomatonField`-flagged read path) is `E0101`. A
  `@snapshot Auto.field` is *also* `E0101` **unless** the
  enclosing `@fn` declares `$ [Readable]` (per ADR 0003) or one of
  its supersets.
- Field-type check (atomicity): `@snapshot Auto.field` where
  `Auto.field`'s type isn't word-atomic → `E0551`.
- Lock-check (for `#shared` fields, v0.2 conservative): `@snapshot
  Auto.field` from `@fn` for a `#shared` field → `E0552`.

The two new error codes:

- **E0550 SnapshotInUnreadableFn.** "`@snapshot Auto.field` requires
  enclosing `@fn` to declare `$ [Readable]`; `f` declares only
  `$ [Pure]`."
- **E0551 SnapshotNotAtomic.** Field type wider than word size.
- **E0552 SnapshotNeedsLockProof.** `#shared` field snapshot from
  `@fn` without lock-holding proof.

### 5. Backward compatibility (book Ch. 39 and the like)

Existing programs that implicitly read automaton state from `@fn`
bodies are caught today by `clifford-check`'s Slice 1 with `E0101`
("automaton-field read"). So the migration is: **add `@snapshot`
to the read site, AND add `Readable` to the `@fn`'s trait list**.

```clifford
// Before (E0101):
@fn used() -> u32 {
  return Uart.rx_head - Uart.rx_tail;
}

// After (clean):
@fn used() -> u32 $ [Readable] {
  return @snapshot Uart.rx_head - @snapshot Uart.rx_tail;
}
```

Book Ch. 39's example will be updated as part of this ADR's
implementation. Existing code outside the book corpus: there is
none — the language is pre-v0.1.

---

## Trade-offs

| Choice                                | Win                                                                           | Cost                                                                                       |
|---------------------------------------|-------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------|
| Expression (not statement)            | Composes in `let`, argument, return positions                                 | Slightly fancier parse rule (precedence)                                                   |
| Copy-by-value for `Copy` types        | Simple, atomic, no lifetime gymnastics                                        | Big values can't be snapshotted in v0.2                                                    |
| Defer `@snapshot_ref` to v0.4         | Smaller v0.2 surface; gets `Copy` case shipped                                | Some patterns wait for v0.4                                                                |
| Atomicity check via field type width  | Caught at compile time, no runtime instrumentation                            | Some platforms may have wider atomic loads (XMM, NEON); we're conservative                 |
| `#shared` requires `#`-layer for v0.2 | Avoids designing capability-effect-row machinery prematurely                  | `@fn` can't snapshot `#shared` fields in v0.2; falls back to `#effect` reads               |
| Migration: hard E0550 break           | Forces explicit `@snapshot` everywhere                                        | Existing book examples need updating; small one-off cost                                   |

---

## Open questions

### Q1. Should `@snapshot` itself be markable for purity?

`@snapshot Auto.field` reads mutable state. Is that "pure" in any
sense? It's *referentially transparent within a single evaluation*
(two snapshots in the same expression observe the same value? — no,
not quite, since concurrent writers can intervene).

**Proposed resolution.** `@snapshot` is *not* pure — it's a
controlled effect. The `Readable` trait in the calling `@fn`'s
trait list is the marker. Within a single `@fn` body, two
`@snapshot`s of the same field MAY observe different values (this
is the SPSC `head` read in book Ch. 39: the producer writes between
the consumer's two reads).

If the user wants two reads to coincide, they bind the snapshot to a
local: `let h := @snapshot Uart.rx_head; … use h … use h …`. The
local capture is genuinely pure.

### Q2. `@snapshot Self.field` inside `#transition`?

`Self.field` from inside a `#transition` body is *not* a read across
the `@`/`#` boundary — it's an in-layer read. So `@snapshot` is
unnecessary there; ordinary `Self.field` reads suffice.

**Proposed resolution.** `@snapshot` is *only* meaningful in `@fn`
bodies. Using it in `#`-layer bodies is `E0553 SnapshotInImperative`
("you're already in the imperative layer; use the bare field
reference"). This keeps the language minimal — one canonical way per
context.

### Q3. What about complex composite reads?

`@snapshot Uart.rx_head` is one field. What about `@snapshot
Uart.rx_buffer[Uart.rx_tail]` — a *derived* read?

Two interpretations:
- (a) `@snapshot` only takes a single field path. Indexing must be
  done outside via the snapshotted indices.
- (b) `@snapshot` takes an arbitrary expression rooted in automaton
  state.

(a) is simple. (b) is more powerful but introduces ambiguity about
*what* gets snapshotted (the byte? the whole buffer? the index?).

**Proposed resolution.** (a) for v0.2. The user writes:

```clifford
let tail := @snapshot Uart.rx_tail;
let head := @snapshot Uart.rx_head;
// ... compute (head - tail) ... can't index into @snapshotted buffer in v0.2 ...
```

For "snapshot a byte from the buffer at a snapshotted index," v0.2
requires the read to happen in a `#effect` (which can do bare
`Uart.rx_buffer[i]` indexing). v0.4+ can extend `@snapshot` to
`@snapshot Auto.field[expr]` if the use case emerges.

### Q4. Migration timing

Plan:
- v0.2: introduce `@snapshot` + `Readable`. Implicit reads from `@fn`
  emit a deprecation warning (`W0001 ImplicitFieldRead`) instead of
  immediate `E0101`.
- v0.4+: deprecation becomes hard `E0101`. All `@fn` reads of
  automaton state must use `@snapshot`.

**Proposed resolution.** Adopt the two-phase migration. The hard
break is one minor version away, giving users time to migrate. (The
"users" today are the book examples and a handful of test fixtures;
real users won't appear until v0.2 ships.)

### Q5. Does `@snapshot` need an explicit ordering annotation?

For `#shared` fields, the snapshot might want to interact with
Decision #22's memory-ordering traits (`Acquire`, `Release`,
`SeqCst`). Should `@snapshot` carry an ordering parameter?

**Proposed resolution.** Defer. v0.2 `@snapshot` of a `#shared`
field implies `Acquire` ordering (the conservative default).
Explicit ordering control is v0.7+ material alongside the rest of
Decision #21.

---

## Implementation milestones

| Milestone | Feature                                                  | Crate(s)                          |
|-----------|----------------------------------------------------------|-----------------------------------|
| v0.2-α    | `@snapshot` token + AST node + parser                    | lexer, ast, parser                |
| v0.2-β    | Type-check rule (E0550 — requires Readable)              | check (depends on ADR 0003 β)     |
| v0.2-γ    | Atomicity check (E0551)                                  | types, check                      |
| v0.2-δ    | Implicit-read deprecation warning (W0001)                | check                             |
| v0.4+     | Implicit-read becomes hard E0101                         | check                             |
| v0.4+     | `@snapshot_ref` borrow form                              | (TBD)                             |
| v0.7+     | `#shared` field snapshot with explicit ordering          | (TBD)                             |

---

## Decision

**Status: Accepted (2026-05-05).** Architect signed off "yes to all"
on the four core resolutions (P1–P4) and all five sub-resolutions
(Q1–Q5).

**Locked resolutions:**

| # | Question | Locked resolution |
|---|---|---|
| P1 | Expression vs statement | **Expression.** `let v := @snapshot Counter.value;` composes in any expression position. |
| P2 | Copy-by-value vs ref-to-snapshot | **Copy-by-value** for `Copy` types in v0.2. `@snapshot_ref` borrow form deferred to v0.4+. |
| P3 | Interaction with `#shared` (Decision #21) | **Lock-holding proof required.** From `@fn` in v0.2: `E0552 SnapshotNeedsLockProof` (snapshot of `#shared` only from `#`-layer). Holds<L> capability row deferred to v0.4+. |
| P4 | Backward compat with implicit-read | **Two-phase migration**: v0.2 deprecation warning (`W0001 ImplicitFieldRead`); v0.4+ hard `E0101`. |
| Q1 | Is `@snapshot` itself "pure"? | **Not pure** — controlled effect. `Readable` trait is the marker. Two `@snapshot`s of the same field MAY observe different values; user binds to local for coherence. |
| Q2 | `@snapshot Self.field` inside `#transition` | **`E0553 SnapshotInImperative`** — use bare `Self.field` instead. One canonical way per context. |
| Q3 | Composite reads (`@snapshot Auto.field[expr]`) | **Single field path only** in v0.2. Indexing forms deferred to v0.4+. |
| Q4 | Migration timing | **v0.2 warn (W0001), v0.4+ hard E0101.** Matches P4. |
| Q5 | Explicit memory ordering on `@snapshot` | **Defer.** v0.2 implies `Acquire` for `#shared` field snapshots. Explicit `@snapshot Auto.field with_ordering(SeqCst)` deferred to v0.7+. |

Atomicity rule (locked): only word-size `Copy` fields snapshot
atomically; larger types → `E0551 SnapshotNotAtomic` (use `#shared`
+ lock). The `Readable` trait from ADR 0003 is the gate for
`@snapshot` from `@fn` (`E0550 SnapshotInUnreadableFn`).

**Action items (v0.2 implementation):**
1. Update DECISIONS.md Decision #24 from DESIGN-IN-PROGRESS to
   ✓ LOCKED with a one-paragraph summary referencing this ADR.
2. Reserve `@snapshot` token in the lexer.
3. Add `SnapshotExpr` to the AST.
4. Add E0550, E0551, E0552, E0553 to the spec §10 error-code table.
5. Add W0001 (deprecation warning) to the warning table.
6. Update book Ch. 24 (Decision #24 chapter) from stub to full
   chapter mirroring Ch. 25's quality bar.
7. Update book Ch. 43's (formerly Ch. 39) SPSC example to use
   `@snapshot` + `Readable`.

---

## Cross-references

- **DECISIONS.md Decision #24** — the locked direction this ADR
  formalises.
- **DECISIONS.md Decision #1** — the sigil layering whose pure-side
  read story this ADR completes.
- **DECISIONS.md Decision #21** — `#shared` fields whose snapshot
  semantics this ADR partially addresses.
- **ADR 0003** (Haskell-clean `@fn` discipline) — the `Readable`
  effect-row trait this ADR uses as the gate for `@snapshot` from
  `@fn`. ADRs 0003 and 0004 are complementary; they should land
  together.
- **Book Ch. 39 (firmware patterns)** — the SPSC example whose
  current implicit-read pattern motivates this ADR.
- **Book Ch. 24 (Decision #24 chapter)** — currently a stub awaiting
  this ADR.

---

*This ADR is Proposed. Locking requires architect sign-off on the
four core resolutions and the five §6 open-question resolutions.*
