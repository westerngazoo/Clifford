# Behaviour notes: `@sequential(A, B)` in the GA orthogonality engine

Status: **v0.2-γ** (shipped 2026-05-08)
Spec source: `docs/CLIFFORD_SPEC.md` §2.6, §7.0.1, §7.3
Decision source: `docs/DECISIONS.md` Decision #11
Crate: `clifford-ortho` (`crates/ortho/src/lib.rs`)

This document describes how the `@sequential(A, B);` top-level
attribute interacts with the `clifford-ortho` verifier in v0.2-γ
— what it does, what it doesn't do, and how to reason about
whether to add one to your program. Read alongside the spec
sections above.

## The one-line semantics

`@sequential(A, B);` is the user's *trusted assertion* that
**no callable touching automaton `A` ever runs concurrently
with any callable touching automaton `B`**. The orthogonality
engine takes this on faith and skips the wedge-product check
for the implied pairs.

## What "touches" means

A callable *touches* an automaton iff that automaton appears
in the callable's `actual_automata` set — which is computed by
`clifford-effect` from the callable's `actual_writes` (direct
+ transitive through `#> proc()` calls). Crucially:

| Action on automaton `A` | Counts as "touching `A`"? |
|---|---|
| `#mutate A { … }` (direct write) | ✅ |
| `Auto.field <op>= …` on `A` | ✅ |
| `#> proc()` where the callee writes a field of `A` | ✅ (transitive) |
| Reading `A.field` (e.g. in a `return` or `if`) | ❌ |
| `@snapshot A.field` | ❌ |

**Reads alone do not make a callable "touch" `A`.** This is
deliberate. The §6.2 `#mutates` declaration check only gates
writes, and `actual_automata` mirrors that contract. v0.2-β's
graded read-write check still fires on read-write races —
but `@sequential` cannot suppress them, because a read-only
callable doesn't touch the automaton in the v0.2-γ sense.

To suppress a read-write race with the consumer side
read-only, you need `#atomic` (§6.6) or `@snapshot` (Decision
#24 / ADR 0004). Both are deferred to subsequent slices.

## The matrix of effects

Given `@sequential(A, B);` declared at top level, the engine
suppresses pairs `(X, Y)` exactly when:

> there exists `α ∈ touches(X)` and `β ∈ touches(Y)`
> such that `(α, β)` matches `(A, B)` (or symmetrically `(B, A)`).

Concrete cases:

| `X` writes | `Y` writes | `@sequential(A, B)` declared? | Engine flags violation if disjoint? |
|---|---|---|---|
| `A.x` | `B.y` (different basis bit) | yes | (no violation either way; disjoint) |
| `A.x` | `B.y` (different basis bit) | no  | (no violation; disjoint) |
| `A.x` | `B.x` (same field name, different automatons; different basis bit) | yes | suppressed |
| `A.x` | `B.x` (same field name, different automatons; different basis bit) | no  | flagged if conflicting |
| `A.x` | `A.x` (same automaton, same field) | yes / no | always **flagged** — same-automaton pairs are checked regardless |
| reads `A.x`, writes `A.y` | writes `A.x` | yes / no | always **flagged** — read-write race within same automaton |
| reads `A.x` (no writes) | writes `A.x` | yes — but X doesn't touch `A` for §6.2 | **flagged** — `@sequential` doesn't help |

## Symmetry

`@sequential(A, B)` and `@sequential(B, A)` carry the same
meaning per spec §2.6. The verifier canonicalises the pair to
`(lo, hi)` (alphabetical) so duplicate declarations are
de-duplicated. Multiple `@sequential` declarations on the
same pair produce one suppression entry.

## What `@sequential` does not cover

Per spec §7.0.1's safety pillars, the engine deliberately does
**not** verify these claims:

1. **The user's assertion is true.** If the program actually
   does run callables of `A` concurrently with callables of
   `B` despite `@sequential(A, B);`, the resulting race is a
   *user-introduced soundness bug*. The engine accepts the
   assertion as gospel.

2. **`@sequential` is not transitive.** `@sequential(A, B);
   @sequential(B, C);` does NOT imply `@sequential(A, C);`.
   The user must declare each pair they assert.

3. **`@sequential` is not contagious.** A callable that touches
   both `A` and `B` (say, an effect with `#mutates: [A, B]`)
   is paired with EVERY OTHER callable per the standard rules.
   The `@sequential(A, B)` clause only matches pairs where the
   two callables touch `A` and `B` *separately* — one each.

4. **Same-automaton pairs are not affected.** Two callables
   that both touch the same automaton `A` can never be the
   target of `@sequential` suppression. `@sequential(A, A);`
   is meaningless — Decision #5 already says automaton `A`'s
   transitions are inherently sequential within `A`.

5. **Read-only callables sit outside the override.** If
   callable `X` only READS from `A` (no writes; `A ∉
   actual_automata(X)`), then `X` does not "touch `A`" and no
   `@sequential` clause can suppress a pair involving `X`.
   This is the SPSC consumer-side case `examples/dual_uart_telemetry.cl`
   ran into; the routes to safety are `#atomic` or `@snapshot`,
   not `@sequential`.

## Worked example

```clifford
#automaton A { x: u32; }
#automaton B { y: u32; }

@sequential(A, B);

#effect set_a() #mutates: [A] { A.x = 1u32; }
#interrupt IRQ_B() #mutates: [B] #priority: HIGH { B.y = 1u32; }
```

Without `@sequential`, the engine pairs `set_a` (an effect on
foreground) with `IRQ_B` (an interrupt). They touch disjoint
basis bits, so even without the override the wedge is
non-zero — orthogonal.

With `@sequential(A, B);`, the engine *also* skips the pair
explicitly. Either way the program compiles. The
`@sequential` is "documentary" here — it captures the user's
intent but doesn't change the outcome.

The override matters when `set_a` and `IRQ_B` would otherwise
share a basis bit. In v0.1's restricted Cl(0,0,n) algebra,
each `(automaton, field)` gets its own bit — so two
*different* automatons can never share a field-basis bit.
The override therefore only matters today for **trait-basis
conflicts** (a user-defined trait shared between two callables
on different automatons). When `#shared` fields land in
v0.7+ per Decision #21, `@sequential` will become more
operationally relevant — shared basis vectors will no longer
be inherently disjoint.

## When you should use `@sequential`

Use `@sequential(A, B);` when **all** of these are true:

1. The verifier rejects a pair `(X, Y)` where `X` touches
   `A` and `Y` touches `B` and `A ≠ B`.
2. You can guarantee — by external mechanism (NVIC priority
   masks, scheduler ordering, manual interrupt disable) —
   that the two callables truly never overlap.
3. The alternative (restructuring state ownership so the
   conflict goes away) is impractical for your design.

Do **not** use `@sequential` to silence:

- Same-automaton write-write races. These are real; the
  fix is restructuring or `#atomic`.
- Read-write races where the reader is a foreground `@fn`
  or `#effect`. Use `@snapshot` or `#atomic`.
- Imagined races between declared-as-pure callables. Spec
  §7.3 already handles these correctly.

## Implementation notes

The relevant code lives in `crates/ortho/src/lib.rs`:

- `collect_sequential_pairs(program)` — walks
  `Item::Sequential(_)` items and builds a
  `HashSet<(String, String)>` of canonicalised pairs.
- `node_touches(node, profiles)` — returns the
  `actual_automata` set for a node, treating `@fn`s as
  touching nothing (they have no mutation profile).
- `is_pair_sequential(a, b, profiles, pairs)` — checks the
  cross-product of touch sets against the declared pairs.
  Skips entries where `α == β` (no same-automaton sequential
  meaning).
- `verify` — calls `is_pair_sequential` after `can_concur` and
  before the wedge check; matching pairs are skipped.

The relevant tests:

- `sequential_attr_*` — end-to-end verifier tests covering
  the matrix above.
- `is_pair_sequential_*` — direct unit tests on the helper.
- `collect_sequential_pairs_*` — symmetry and dedup tests.

## Forward references

When the following slices land, this document needs a
revision:

- **`#atomic: interrupt_critical`** (§6.6). Will become the
  canonical fix for the read-write race case `@sequential`
  cannot help with.
- **`@snapshot Auto.field` codegen** (Decision #24 / ADR
  0004). Will let foreground readers copy state into a
  private local before reading, sidestepping the race
  altogether.
- **`#shared` fields + locks** (Decision #21, v0.7+). Will
  introduce mixed-metric Cl(p,0,n) basis vectors that don't
  collapse on overlap; `@sequential` will become operationally
  meaningful in cases where today it's documentary.
- **`#priority`-aware concurrency inference**. Currently the
  verifier conservatively pairs every interrupt with every
  other interrupt regardless of priority. A future slice could
  use NVIC priority semantics (same priority on Cortex-M ⇒
  no preemption) to refine the matrix, reducing the need for
  `@sequential` in many real programs.
