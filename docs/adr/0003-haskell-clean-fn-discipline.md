# ADR 0003: Haskell-Clean `@fn` Discipline

**Status:** Proposed (2026-05-04)
**Date:** 2026-05-04
**Deciders:** Goose (architect)
**Spec impact:** §4 (Type System), §5 (Type Checker), §2 (Grammar — possibly), §10 (Error codes — E05xx range expansion)
**DECISIONS.md:** Decision #23 ✓ DESIGN-IN-PROGRESS — locks once this ADR closes.
**Refines:** Decision #1 (sigil layering), Refinement #1a (local-stack mutation in `@fn`).
**Branch:** `adr/0003-0004-haskell-clean-fn-and-snapshot`.

---

## TL;DR

Clifford's `@fn` is already a *better-than-Rust* pure function — no
shared-state mutation, no interrupts, no automaton-field reads. But
it's *worse than Haskell* in three concrete ways: it can loop forever
(no totality), it has no effect annotation in the signature beyond
the optional `$ [TraitList]` markers (no effect rows), and it has no
way to express "the argument must be in a particular subset of its
type" (no refinement types).

Decision #23 locked the direction: **make `@fn` as Haskell-clean as
practical without losing the systems-language target**. This ADR
specifies what "as practical" means by surveying the four nearest
candidate languages (Haskell, Idris, Liquid Haskell, Koka) on each
of three axes (totality, effect rows, refinement types) and proposes
a concrete design that picks one resolution per axis.

**The four candidate trade-offs to weigh:**

| Axis              | Haskell                | Idris                       | Liquid Haskell             | Koka                              | Proposed for Clifford                                              |
|-------------------|------------------------|-----------------------------|----------------------------|-----------------------------------|--------------------------------------------------------------------|
| **Totality**      | Optional (`-Wpartial`) | Required by default         | N/A (separate axis)        | Optional (`fun` is total, not partial) | **Required by default**, `@partial @fn` opt-out                    |
| **Effect rows**   | None (monads)          | Effect rows (Idris 2)       | None                       | First-class effect rows (`<exn,io>`) | **First-class** as extension of `$ [TraitList]` per Decision #2 + #22 |
| **Refinement types** | None                | Limited (dependent types)   | First-class (SMT-backed)   | None                              | **Limited refinements** (sigma-bound style per Decision #14, no SMT) |
| **Local mutation** | None (monadic ST)     | Limited (linear types)      | Inherits Haskell           | Allowed in `fun`                  | **Allowed** per Refinement #1a (already locked)                    |

The headline trade is **totality + effect rows are real wins; full
refinement types via SMT are not (yet).** Clifford's target audience
ships firmware on devices; an SMT solver in the compile pipeline is a
deal-breaker for v0.1–v0.6. We can pick up Liquid-style refinements
*without* a solver by limiting them to the patterns the §5.8 sigma-
bound machinery already proves (Decision #14 — bounded loops); that
gives us 80% of the value at 5% of the cost, and leaves the door open
for v1.0+ to add a solver if a use case justifies it.

**Recommendation:** Lock Decision #23 with the proposed design once
the architect signs off on the four-axis trade table. Implementation
gated to **v0.2** (totality + effect rows) and **v0.4+** (refinements
beyond the sigma-bound carve-out). Local mutation (Refinement #1a)
already shipped.

---

## Context

### Where Decision #1 left `@fn`

Decision #1 (sigil layering) established that `@fn` is the functional
layer. Concretely today:

- A `@fn` body **cannot** contain `#`-constructs (Slice 1 of
  `clifford-check` enforces this — §5.5).
- A `@fn` body **can** contain local-stack mutation per Refinement #1a
  — `let mut x = …; x = …;` is fine as long as `x`'s type doesn't
  contain a reference into shared state.
- A `@fn` carries an optional `$ [TraitList]` (Decision #2) — markers
  like `[Pure]`, `[Readable]`, `[Observable]`, `[Opaque]`. The
  default is `$ [Pure]`. Decision #22 extended the trait-list
  mechanism to `#`-layer callables for *kind* classification; this
  ADR's trait-list use is for the original `@fn` *purity* purpose.

### What this ADR is and is not

This ADR is the **operational specification** of Decision #23. It
takes the locked direction ("Haskell-clean") and turns it into:
1. A specific set of language features (totality, effect rows,
   refinement types — each with a concrete syntax sketch).
2. A specific set of static checks the type checker / `clifford-check`
   must implement.
3. A specific set of error codes (E05xx range, see §6).
4. A milestone schedule (which features land in v0.2 vs v0.4+).

It is **not** a proof system specification — totality and
refinements interact with proof obligations that go beyond v0.1's
type-checker shape. The ADR proposes the *interface* (what users
write); the proof discharge for refinements is delegated to §5.8's
sigma-bound machinery (Decision #14) for v0.2 scope, with the SMT-
backed extension as a v1.0+ open question.

---

## Survey: the four reference languages

### Haskell (the namesake)

- **Totality.** Not enforced. `head []` is a runtime error, not a
  compile error. GHC has `-Wincomplete-patterns` and `-Wpartial`
  but they're warnings, not errors.
- **Effect rows.** None as a language feature. Effect tracking is
  done with monads (`IO a`, `State s a`, etc.) which are great
  abstractions but require monadic plumbing for every effect-using
  function. The transformer stack pattern (`StateT s IO a`) is the
  standard way to combine effects.
- **Refinement types.** None natively. Liquid Haskell is a separate
  tool that re-checks Haskell programs with SMT-backed refinements.
- **Local mutation.** Available via the `ST s` monad and `IORef` /
  `STRef`; the `runST` family lets you run `ST` computations from
  pure code, and the `s` parameter prevents reference leakage. **This
  is the model Refinement #1a echoes.**

**Take-away.** Haskell-clean isn't really Haskell; it's
"Haskell-modulo-the-bits-that-don't-suit-systems-code." We want the
purity discipline without the monad-transformer ceremony.

### Idris (totality as a language property)

- **Totality.** Required by default. The Idris 2 type checker proves
  a function total via structural induction on its arguments. If it
  cannot, the function is marked `partial` and callers must mark
  themselves `partial` to call it.
- **Effect rows.** Idris 2 has effect rows in the type system
  (extending Idris 1's monolithic `IO`). Effects are tracked
  per-function in the signature.
- **Refinement types.** Limited via dependent types — not the same
  thing as Liquid-style refinements but covers many of the same use
  cases (e.g., `Vec n a` for "vector of length n").
- **Local mutation.** Limited; uniqueness types and linear types
  provide controlled mutation without breaking purity.

**Take-away.** Idris's totality model is what we want. The check is:
"every recursive call is on a strictly smaller argument" (structural
recursion) or "every loop has a strictly decreasing measure" (sigma-
bound style — Decision #14). Idris's effect rows are also what we
want, modulo their interaction with our `$ [TraitList]`.

### Liquid Haskell (refinement types via SMT)

- **Totality.** Inherits Haskell — same story.
- **Effect rows.** None.
- **Refinement types.** First-class, SMT-backed. Annotations like
  `@-{ x:Int | x > 0 }-` express subset constraints; the type
  checker discharges them via Z3 / CVC4 / similar.
- **Local mutation.** Inherits Haskell.

**Take-away.** Liquid's refinement types are the holy grail for the
"my array index is in bounds" pattern, but they require an SMT
solver in the compile pipeline. For Clifford's v0.1–v0.6 firmware
target, that's a deal-breaker:
- Solver dependency adds 50+ MB to the toolchain footprint.
- Solver runs are non-deterministic (timeouts, heuristic backends).
- Solver evolution affects compile-output reproducibility.

So we want refinement-style *expression* without SMT discharge for
v0.2 — limited to patterns the §5.8 sigma-bound check already
handles syntactically. Add a solver in v1.0+ if a use case justifies
the cost.

### Koka (effect rows as the language's distinguishing feature)

- **Totality.** Optional. `fun` is total by default, `ctl` is for
  control-flow effects, `bif` is for built-ins. Mixing is explicit.
- **Effect rows.** First-class. Signatures like `read-line : () ->
  <exn, io> string` carry the effect set directly. Effects compose
  via row union.
- **Refinement types.** None.
- **Local mutation.** Allowed inside `fun` via the `<local>` effect.
  The locality is *scoped*; references can't leak out.

**Take-away.** Koka's effect rows are the model we want syntactically.
Their integration with `$ [TraitList]` is the design question (see §3.2).
Koka's `<local>` effect is essentially Refinement #1a wrapped in
syntax — same idea, different surface.

---

## Proposed design

### 1. Totality required by default; `@partial` opt-out

Every `@fn` is total by default. The check is structural:
- Every recursive call is on a `sigma`-bounded loop variable
  (Decision #14) OR a structurally-smaller argument (every recursive
  argument is one of: a destructured field of a parameter, an index
  into a parameter that is bounded by the parameter's length, a
  pattern-matched constructor's argument).
- Every match expression is exhaustive (no `_` fall-through unless
  explicitly written).
- Every conditional branch reaches a `return` (no falling-off-the-end
  panics).

The opt-out:

```clifford
@partial
@fn parse_input(s: &[u8]) -> Result<Tree, ParseError> {
  // ... a parser that may not terminate on pathological input ...
}
```

`@partial @fn` is allowed only when:
- Called from another `@partial @fn`.
- Called from a `#`-layer callable (which is partial-by-construction).
- Wrapped in a runtime budget (`@with_budget(n) { ... }` — v0.4+
  feature TBD).

The default-total stance changes the user contract: a stable
non-terminating loop is a *compile error*, not a runtime hang.

**New diagnostic:** `E0540 NotTotal` — name the suspect call and the
totality argument that failed (e.g., "recursive call to `f` on
argument `xs`, but `xs` is not structurally smaller than the parameter
`xs`").

### 2. Effect rows as extension of `$ [TraitList]`

Today: `@fn read() $ [Pure]` declares the function pure. Decision #22
extended `$ [TraitList]` to `#`-layer callables for *kind*
classification; this proposal extends `$ [TraitList]` to encode
**effect-row membership** for `@fn`.

Predeclared `@fn` traits become:
- `Pure` — no observable side effects (the default if no list).
- `Readable` — may read declared automaton state (via `@snapshot`,
  see ADR 0004).
- `Observable` — may produce diagnostic output (logs, traces) without
  modifying program state.
- `Opaque` — implementation may use unsafe (escape hatch).
- `Diverges` — may not terminate (counterpart to `@partial`).
- `Throws<E>` — may produce a value of type `Result<_, E>` via
  early return; static check ensures `E` is the function's declared
  error type.

Composition: `$ [Readable, Observable]` means *both*. The trait list
is **subtractive** — every effect a body uses must appear in the
list, OR be one of the defaults `Pure` provides (which is "none").

```clifford
@fn analyze_uart() -> u32 $ [Readable] {
  // Allowed: @snapshot of automaton state.
  let head := @snapshot Uart.rx_head;
  let tail := @snapshot Uart.rx_tail;
  return head - tail;
}

@fn dump_debug() -> () $ [Readable, Observable] {
  let head := @snapshot Uart.rx_head;     // Readable
  @log("rx_head = {}", head);             // Observable
}
```

A `@fn`'s *callers* must declare a superset of the callee's effect
row (per the standard effect-row composition rule). The type checker
verifies this at every call site.

**New diagnostic:** `E0541 EffectRowMismatch` — name the callee, the
caller, and the missing trait(s) that the caller's row doesn't
satisfy.

### 3. Limited refinement types via the sigma-bound carve-out

Decision #14 already proves that bounded loops have predictable
iteration counts. Extend the same machinery to function arguments:

```clifford
@fn safe_index<T>(arr: &[T; N], i: usize { i < N }) -> &T {
  // The compiler can elide the bounds check on `arr[i]` because
  // `i < N` is in scope.
  return &arr[i];
}
```

The refinement `i: usize { i < N }` is checked *only* at call sites
where the bound can be discharged using:
- Sigma-loop-index machinery (Decision #14).
- Constant evaluation.
- Pattern matching on `Option<…> { Some(i) if i < N => … }`.

If the bound *can't* be discharged, the call is `E0542
RefinementNotDischarged`. The user can either:
- Make the bound discharge syntactically obvious (use a sigma loop or
  a literal).
- Cast through the `#unchecked_cast` narrow primitive (Decision #17)
  with an audit-tracked justification.

**Crucial deferral.** This is *not* general refinement-type checking.
It's "the sigma-bound machinery already in §5.8, generalised from
loop variables to function arguments." It catches the common
"index in bounds" case without dragging in an SMT solver. Full
SMT-backed refinement types would be a v1.0+ design that requires
its own ADR.

### 4. Local mutation: keep Refinement #1a

Already locked. `let mut x = …; x = …;` inside `@fn` is permitted as
long as `x`'s type doesn't reach into shared state. No change.

---

## Trade-offs

| Choice                        | Win                                                                      | Cost                                                                                                            |
|-------------------------------|--------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------|
| Total by default              | Compile-time termination guarantee for the bulk of code                  | `@partial` annotations on parsers, REPLs, certain dispatch loops; users must learn the structural-recursion rule|
| Effect rows in `$ [TraitList]` | Effects visible in signature; composition statically checked            | One more trait-list element to remember; row composition requires a bit of explanation                          |
| Refinements via sigma-bound  | Captures 80% of "index in bounds" without solver dependency              | Doesn't catch "index is bounded by some non-loop expression"; user falls back to `#unchecked_cast` for those    |
| Refinements WITHOUT SMT      | No 50 MB solver in toolchain; deterministic compile                      | Some patterns Liquid Haskell catches will be `E0542` here                                                       |
| Keep Refinement #1a          | Local accumulators in `@fn` work as users expect                         | Minor — no real cost                                                                                            |

---

## Open questions

### Q1. What counts as "structurally smaller" for totality?

Idris's exact rule is sophisticated. For Clifford v0.2's first cut:
- Pattern-matched constructor arguments (the `xs` in `Some(xs)` is
  smaller than `Some(xs)`).
- Indexing into a parameter at a sigma-bounded index.
- Recursive call after `return` makes the call tail-recursive (allowed
  unconditionally — it's a loop).

What about non-structural recursion (Ackermann-style)? Reject for
v0.2. Users mark the function `@partial`. v0.4+ might add a
"well-founded relation" annotation.

**Proposed resolution.** Adopt the three-rule cut for v0.2; defer
sophistication to v0.4+.

### Q2. How do effect rows interact with `#`-layer callers?

A `@fn` declares `$ [Readable]`. A `#effect` calls it. Does the
`#effect` need to declare `Readable` too? Or are `#`-layer callables
"all effects already" and the trait-list check is one-directional?

**Proposed resolution.** `#`-layer callables are "all effects" — the
trait-list check is one-directional (`@fn` → `@fn` only). A
`#effect` may freely call any `@fn` regardless of effect row. This
matches Decision #1's "downward call always permitted" rule and
keeps the imperative side simple.

### Q3. Should `Throws<E>` exist, or use `Result<_, E>` directly?

Two patterns for partial computation:
- (a) Function returns `Result<T, E>`; caller pattern-matches.
- (b) Function declares `$ [Throws<E>]` and uses an explicit `@throw`
  expression that early-returns `Err(_)`; caller can `try` to
  unwrap.

(a) is what Rust does (and Idris). (b) is what Koka does.

**Proposed resolution.** Start with (a) only — `Result<T, E>` and
explicit pattern match. Defer (b) to v0.4+ if the ergonomic case is
strong. (b) introduces effect-row complexity (`Throws<E>` in row,
discharge by `try` block); not worth it in v0.2.

### Q4. How does `Diverges` interact with `@partial`?

These are nearly the same concept: "may not terminate." Should they be
the same trait? Or are they different (`@partial` = "may not
terminate AND may have other partial behaviour"; `Diverges` = "may
specifically loop forever")?

**Proposed resolution.** `@partial` is the broader marker (covers
non-termination, exceptions, partial pattern matches); `Diverges` is
unnecessary as a separate trait. Drop `Diverges` from the proposed
trait list.

### Q5. SMT-backed refinements — when, if ever?

The `i: usize { i < N }` refinement is discharged syntactically. What
about `i: usize { i % 4 == 0 }` (DMA-aligned index) or `s: &str { len
s > 0 }` (non-empty string)?

**Proposed resolution.** Defer to v1.0+ ADR. Until then, those cases
require either a wrapping safe-API (`AlignedIndex`, `NonEmptyStr`)
or `#unchecked_cast` with audit. The carve-out catches enough of the
firmware case to justify the no-solver decision.

---

## Implementation milestones

| Milestone | Feature                                                  | Crate(s)                          |
|-----------|----------------------------------------------------------|-----------------------------------|
| v0.2-α    | `@partial` keyword + AST flag; default-total walker      | lexer, ast, parser, check         |
| v0.2-β    | Effect rows in `$ [TraitList]`: Readable, Observable     | types, check                      |
| v0.2-γ    | Effect-row composition + caller verification             | check                             |
| v0.2-δ    | Sigma-bound refinements on function arguments            | types, check                      |
| v0.4+     | Throws<E> if ergonomic case justifies                    | (TBD)                             |
| v1.0+     | SMT-backed refinements (separate ADR)                    | (TBD)                             |

---

## Decision

**Status: Proposed.** The four-axis trade table in §"TL;DR" is the
core question; the proposed-resolutions in §6 close out the
implementation details once the core is locked.

**Action items if accepted:**
1. Update DECISIONS.md Decision #23 from DESIGN-IN-PROGRESS to
   ✓ LOCKED with a one-paragraph summary referencing this ADR.
2. Add the totality-check skeleton to `clifford-check` (parser
   accepts `@partial`; check walks the AST).
3. Extend `$ [TraitList]` semantics in `clifford-types` to include
   `Readable`, `Observable`, `Diverges` removed.
4. Introduce E0540, E0541, E0542 to spec §10 error-code table.
5. Update book Ch. 23 (Decision #23 chapter) from stub to full
   chapter mirroring the structure of book Ch. 25 (Decision #25 — the
   reference quality bar).

---

## Cross-references

- **DECISIONS.md Decision #23** — the locked direction this ADR
  formalises.
- **DECISIONS.md Decision #1** — the sigil layering this ADR refines.
- **Refinement #1a** — local-stack mutation in `@fn`, already locked.
- **Decision #14** — sigma-bound machinery this ADR extends to
  argument refinements.
- **ADR 0004** (`@snapshot` boundary operator) — the *other* ADR
  needed to make `Readable` effect-row work; the two ADRs are
  complementary and should land together.
- **Book Ch. 23 (Decision #23 — Tighten `@fn` toward Haskell-clean)**
  — currently a stub awaiting this ADR.

---

*This ADR is Proposed. Locking requires architect sign-off on the
four-axis trade table and the five open-question proposed
resolutions.*
