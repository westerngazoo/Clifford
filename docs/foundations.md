# Clifford: Theoretical Foundations

> **Status:** working document. Drafted 2026-05-15 as the first artifact of
> the post-GA-narrative pivot. Feeds directly into a planned rewrite of
> `CLIFFORD_SPEC.md` §7 and a decision audit of #21, #22, #23, #26, #27.
> Not normative; the spec remains the contract. This document explains
> *what tradition Clifford is in* so future spec changes can cite the
> literature instead of inventing.

---

## 0. Purpose

Until 2026-05-15 the project framed itself around a "novel Geometric
Algebra orthogonality engine." On hostile review the framing was
identified as decorative: the `outer_product` function in `crates/ortho`
reduces to `if a & b != 0 { None } else { Some(a | b) }` — a set
disjointness test on bitmask-encoded read/write footprints, in the
direct lineage of Lucassen & Gifford (1988) and Reynolds (2002). The
GA story was added value as marketing, but invited immediate technical
pushback from anyone who knew the field, and pulled the design toward
three large speculative commitments (Decisions #21, #26, #27) that
locked architectural surface for a foundation no v0.1 use case
demanded.

The pivot: **drop the GA-novelty claim; keep the language; ground both
remaining layers in the existing literature**. This document does that
grounding. For each tradition Clifford touches, it identifies:

- (a) what Clifford already takes from this tradition;
- (b) where Clifford is strictly weaker than the tradition's strongest
  offerings;
- (c) what specific extensions Clifford could add by reaching further
  into that tradition.

The intent is to feed the spec rewrite, not to replace it.

---

## 1. The two layers, restated

The README and CLAUDE.md describe a "three-layer Safety Triangle":
functional `@`, imperative `#`, hardware `access<T>`. The hostile
review pointed out, correctly, that `access<T>` is doing too little for
its weight: it is a nominal-pointer typing discipline (Decision #19),
not a layer with its own semantics. Treat it as a typing discipline
*within* the imperative layer.

What remains is **two layers**:

1. **Functional `@`** — pure transformations. Default-immutable.
   Locally-mutating per Refinement #1a. Cannot call `#`-layer code.
   Effect rows tracked via `$ [TraitList]` per Decisions #2 and #23.
   Source-grep-able by the leading `@`.

2. **Imperative `#`** — state and effects in time. State is owned by
   `#automaton`s. Mutation is a typed effect (`#effect`, `#interrupt`,
   `#transition`, `#mutate`) carrying a `#mutates: [Auto1, Auto2, ...]`
   profile. Body-scoped references (Decision #13) eliminate lifetime
   annotations at the cost of cross-call borrow patterns. Narrow unsafe
   primitives (Decision #17) provide per-occurrence audit. Source-grep-able
   by the leading `#`.

The `access<T>` typing discipline lives inside the imperative layer:
register-block automatons (Decision #6) provide the structured surface,
and `#unchecked_load` / `#volatile_store` / `#unchecked_cast` /
`#unchecked_offset` (Decision #17) are the narrow primitives. None of
this is a third layer; it's how the imperative layer touches hardware.

Reframing the README and the spec to reflect this is part of the
follow-up work.

---

## 2. The imperative layer's lineage: effect systems and separation logic

Two traditions converge here.

**Effect systems** trace from Lucassen & Gifford (1988, *Polymorphic
Effect Systems*). A function's type is `T1 -> T2 ! e` where `e` is a
set of effects (typically read/write footprints on regions). Two
computations are safely parallel iff their effect sets satisfy a
disjointness condition. Modern descendants: Koka's row-polymorphic
effects (Leijen 2014), Eff and Frank's algebraic effects with handlers,
F*'s indexed effect monads, OCaml 5's effect handlers.

**Separation logic** traces from Reynolds (2002) and O'Hearn (2007).
The frame rule `{P} C {Q} ⊢ {P * R} C {Q * R}` says that if `C`
satisfies `{P} C {Q}` and `R` is disjoint from anything `C` touches
(`*` = separating conjunction = disjoint heaps), then `R` is preserved.
The disjointness condition is structurally identical to the
effect-systems disjointness condition; the difference is that
separation logic talks about heap predicates and effect systems talk
about region/footprint sets. Mechanizations: Iris (Jung et al.),
Verified Software Toolchain, RustBelt's λ-Rust.

**(a) What Clifford takes.** The orthogonality engine implements an
effect-system disjointness check on the bitmask-encoded
`(automaton, field, read|write)` footprints of effects. Spec §7's
"behavior multivector wedge non-zero" is operationally exactly the
Lucassen-Gifford effect-set disjointness check (read-write, write-write,
write-read), with bitmask XOR as the implementation strategy. Decision
#22's `#mutates: [...]` clause is the explicit footprint declaration
the literature requires. Decision #25's `#hidden` fields correspond to
the existential quantification used in separation logic to hide local
state from the frame rule.

**(b) Where Clifford is weaker.** Modern separation-logic systems
(Iris, RustBelt) handle:

- *Aliasing through references.* Clifford's body-scoped references
  (Decision #13) avoid the problem by forbidding cross-call borrow
  patterns. Iris/RustBelt prove the same safety property *with*
  cross-call references via lifetimes. Clifford trades expressivity
  for annotation-freedom; the trade is defensible but the literature
  result is strictly stronger on the borrowing axis.
- *Shared mutable state with disciplined access.* Clifford rejects
  shared mutable state at the type system level. Pony's reference
  capabilities (next section) and Iris's concurrent invariants
  represent this safely. Clifford defers the question to v0.7 (#21,
  #26).
- *Resource accounting.* Verus and Prusti discharge richer
  obligations (e.g. "this heap region has total size N"). Clifford
  has no resource-accounting story.

**(c) What Clifford could add.**

- **Cite the lineage explicitly.** Spec §7's reframe should open with:
  "Clifford's concurrency safety check is an instance of the
  effect-system disjointness condition (Lucassen & Gifford 1988),
  with the bitmask intersection as the implementation strategy
  (Reynolds 2002 separation-logic frame rule and modern AliasSetTracker
  representations both use the same encoding)." This is the honest
  positioning. It loses nothing technical.
- **Frame-rule semantics for effect composition.** Currently the engine
  computes per-callable footprints and rejects pairs whose footprints
  intersect. The literature's frame rule lets you compose effects
  monoidally when disjoint: `eff(C1; C2) = eff(C1) ⊎ eff(C2)` when
  `eff(C1) ∩ eff(C2) = ∅`. Clifford's bitmask-OR already does this
  operationally; spelling it out as a documented invariant gives the
  ortho crate a published-literature foundation.
- **Per-call effect inference.** Currently effects are declared per
  callable. The literature supports inferring effects bottom-up from
  expression structure. This would let users write `#effect foo() { ... }`
  without `#mutates: [...]` and have the engine infer the profile.
  Cost: the inference is straightforward; the question is whether
  losing the explicit declaration hurts auditability. Recommend keeping
  the declaration but inferring + diffing as a sanity check (E04xx
  warning when declared profile is wider than inferred).

---

## 3. Reference capabilities (Pony) — the upgrade path for the imperative layer

Pony's six reference capabilities (`iso`, `val`, `trn`, `ref`, `box`,
`tag`) form a per-reference lattice that statically eliminates data
races *including across actor boundaries*. The key insight: each
reference carries a capability that determines how it can be aliased
and whether it can be sent. `iso` references are sendable because they
guarantee no other reference exists; `val` references are immutable so
sharing is safe; `tag` references are identity-only so reads and writes
are forbidden through them.

This is the mature design Clifford should be measured against on the
race-freedom axis.

**(a) What Clifford takes.** Effectively nothing yet. Clifford's
`#mutates` profiles play a similar role to Pony's per-call aliasing
analysis — both reduce to "what does this code touch?" — but Clifford
operates at the per-effect granularity, not per-reference. Decision #13
(body-scoped references) prevents the cross-call aliasing patterns that
require Pony's capability annotations to disambiguate.

**(b) Where Clifford is weaker.** Pony's `iso` send-once semantics
allows shared mutable state *without* races (the receiver gets exclusive
access; the sender loses its reference). Clifford has no equivalent.
This is exactly the gap that the reviewer flagged: Pony catches a class
of race-free designs that Clifford cannot express. Decisions #21 and
#26 are the planned long-term answer (rotor-as-acquisition-primitive
for locks), but the implementation is gated to v0.7 and the design
locks more surface than necessary.

**(c) What Clifford could add.**

- **A minimal `#owned` qualifier on `#mutates` entries**, semantically
  equivalent to Pony `iso` for the duration of the effect. `#mutates:
  [#owned RxBuffer, AuditLog]` would mean "this effect has exclusive
  access to RxBuffer for its duration; safe to schedule against any
  other effect regardless of `RxBuffer`'s footprint." Implementation:
  the orthogonality check skips `#owned` entries when computing
  inter-effect disjointness. Cost: ~100 LoC in `crates/ortho`. Benefit:
  expresses the SPSC-ring-buffer pattern without mixed-metric algebra.
- **An explicit `#sendable` field marker**. Combined with `#owned`,
  this lets the language express the producer-consumer pattern
  (producer constructs into a `#sendable` slot; consumer takes
  exclusive access) without lifetime annotations. Pony achieves this
  via the `iso` capability + `consume` keyword.
- **Defer #21/#26 indefinitely.** If `#owned` + `#sendable` covers
  the embedded patterns Clifford targets, the rotor-plane-locks
  apparatus is unnecessary. The decision audit should test this
  hypothesis against the five comparison programs (slice 47 in the
  review).

---

## 4. Stack Resource Policy — the priority story Clifford imitates

Baker (1991, *Stack-Based Scheduling of Realtime Processes*) introduced
SRP: assign each task a static priority; assign each shared resource a
ceiling = max priority of any task using it; while a resource is held,
the holder's effective priority raises to its ceiling. SRP guarantees:

1. **Single shared stack** is sufficient for any task set.
2. **Deadlock-free** under fixed-priority scheduling.
3. **Bounded priority inversion** (at most one critical section).
4. **Race freedom** for shared resources accessed via the protocol.

RTIC implements SRP for Cortex-M Rust at zero runtime overhead and
proves the above properties at compile time. This is the headline
mechanism Clifford competes with on embedded.

**(a) What Clifford takes.** Decision #11's `@sequential(A, B)` is
SRP-flavored: it is a user-asserted priority/sequencing constraint
that the engine consumes. Decision #25 (the `$ [Acquire/Release/SeqCst]`
trait family in #22) imitates the memory-ordering side of the protocol.
The `#priority: HIGH` interrupt annotation (slice 22 evidence) plus
the `#priority`-aware ortho inference (`v0.2-η`) move toward
SRP-compatible static scheduling.

**(b) Where Clifford is weaker.** SRP handles *shared resources*;
Clifford's engine handles *disjoint resources*. The whole point of SRP
is to safely schedule tasks that access the same data — exactly the
case Clifford rejects today and defers to v0.7 (#21). Concretely:
RTIC lets two tasks share a `Mutex<Counter>` and proves no race + no
unbounded inversion. Clifford forbids the construct.

**(c) What Clifford could add.**

- **Adopt SRP wholesale for the `#interrupt` + `#priority` subset.**
  Decision #21 is trying to reinvent the protocol via mixed-metric
  algebra; the algebra is unimplemented and may not survive its own
  correctness proof. Adopting SRP directly (cite Baker 1991, RTIC's
  proof) gives Clifford the same race-freedom and deadlock-freedom
  guarantees with zero algebraic apparatus. Cost: refactor #21 as
  "Clifford schedules interrupts via SRP; effects on shared resources
  acquire their ceiling." Benefit: the v0.7 work is replaced by a
  v0.2 work item with established literature backing.
- **Document the priority + atomicity story in spec §6.6 with the
  SRP citation.** The current `#atomic: interrupt_critical` (slices
  v0.2-δ/ε) is operationally a critical section; framing it as SRP's
  ceiling-priority promotion makes the literature connection explicit.
- **Drop the rotor-as-acquisition framing.** The rotor framing
  obscures what is, operationally, a priority-ceiling promotion. SRP
  uses simpler vocabulary and has 35 years of mechanized
  verification behind it.

---

## 5. The functional layer's lineage: HM → effect rows → totality

The functional `@` layer is in the tradition that runs from
Hindley-Milner (1978) through ML's value restriction, Haskell's IO/ST
monads, Koka's row-polymorphic effects, and Idris's totality checking.
F*, Liquid Haskell, and Verus extend the same tradition with refinement
types and SMT discharge.

**(a) What Clifford takes.** Decision #2 introduced `$ [TraitList]` for
purity tracking on `@fn`. Decision #23 ADR-0003 commits to:

- Total-by-default (Idris-style structural recursion + sigma-bound
  termination).
- Effect rows (`Pure`, `Readable`, `Observable`, `Opaque`) on `@fn`
  signatures with row-composition checking.
- Refinement types in argument positions (`§5.8` sigma-bound extension);
  no SMT in v0.2.
- Local mutation per Refinement #1a (ST-monad-equivalent).

This is a real commitment to the literature. Each subfeature traces
to a specific tradition:

- Totality → Idris (Brady 2013).
- Effect rows → Koka (Leijen 2014).
- Refinement types → Liquid Haskell (Vazou et al.) / F* (Swamy et al.).
- ST-style local mutation → ML's `ref` + Haskell's `ST`.

**(b) Where Clifford is realistic vs aspirational.** The reviewer's
critique here is the most actionable: Decision #23 bundles three
multi-year research projects into one locked decision, with v0.2
implementation slated. Honest assessment per subfeature:

- *Totality with structural-recursion termination* is a real
  Phase-1 implementation effort but tractable. Idris has a working
  totality checker; the algorithm is publishable but not novel. v0.2
  realistic.
- *Effect rows* with `Readable`/`Observable`/`Pure`/`Opaque` are a
  fixed-set extension, not row-polymorphic in the Koka sense. Decision
  #23 P2 admits this. Implementation is ~ Koka-without-polymorphism,
  i.e. tractable. v0.2 realistic.
- *Refinement types* limited to sigma-bound extension are tractable
  precisely because they avoid SMT. The `RefinementNotDischarged`
  E0542 fail-closed posture is the right v0.2 stance. SMT-backed
  refinement is correctly deferred to v1.0+.

**(c) What Clifford could add.**

- **Cite Koka explicitly as the closest match for the effect-row
  subset.** The fixed-set `Readable`/`Observable`/`Pure`/`Opaque`
  design is row-polymorphism's poor cousin, but the pedagogy is the
  same and the literature is rich. Spec §4.5 should reference Leijen's
  Koka papers when introducing the row syntax.
- **Cite Idris for the totality story** in spec §5 (type-checking) and
  in the `clifford-check` crate's docstrings. Brady's totality
  algorithm has a well-known shape; reproducing it without citation
  is needless reinvention.
- **Keep refinements deliberately small.** The temptation to add
  Liquid-Haskell-style predicates beyond sigma bounds is real and
  should be resisted until v0.4+. Sigma-bound refinements alone cover
  the embedded case (array indexing, ring buffer offsets, etc.).
- **Resolve the trait-list confusion between #22 and #23.** Both
  decisions extend `$ [...]`. #22's traits are documentary tags on
  `#effect` (Hardware, Realtime(deadline), LockingDiscipline, etc.);
  #23's traits are type-checked effect rows on `@fn` (Pure, Readable,
  Observable, Opaque). Same syntax, different layer, different
  semantics. Spec §4.5 should partition them explicitly: "the
  trait-list on `@fn` is verified by the type system; the trait-list
  on `#effect` is documentary and consumed by downstream tooling."
  Or — better — give them different sigils.

---

## 6. Narrow unsafe primitives: Ada/SPARK precedent (Decision #17)

Ada and SPARK have used per-operation unsafe primitives
(`Unchecked_Conversion`, `Unchecked_Deallocation`, `Unchecked_Access`)
for decades in DO-178C avionics and IEC 62304 medical software. Each
operation is an individually-audit-loggable, individually-grep-able
construct. Rust's `unsafe` block aggregates: a 30-line block can be
doing many things, each line's audit cost is hidden behind block-level
decoration.

Decision #17 adopts the Ada approach: `#unchecked_load`,
`#unchecked_store`, `#volatile_load`, `#volatile_store`,
`#unchecked_cast`, `#unchecked_offset`, `#asm`. Each carries its own
sigil; each is its own grep target.

**This is unambiguously a Clifford strength.** The hostile review
identified it as "legitimately better than Rust." It deserves
prominent positioning in the post-pivot README.

The literature support is strong: Ada Reference Manual §13.9, SPARK
Reference Manual §13, RTCA DO-178C §6 (verification), and a long line
of certification authority guidance. Clifford's framing as "narrow
unsafe primitives in the Ada tradition" is honest, defensible, and
unique-among-systems-languages-that-aren't-Ada.

**Action:** the README's two-paragraph pitch should lead with this,
not with the (now-dropped) GA claim. Something like:

> Clifford is a systems language designed around two principles that
> are well-established in safety-critical software but absent from
> mainstream systems languages: (1) effect-system disjointness as the
> concurrency-safety primitive (Lucassen-Gifford 1988, mechanized by
> separation-logic descendants); and (2) per-occurrence narrow unsafe
> primitives in the Ada/SPARK tradition (DO-178C §13.9). Three sigils
> separate pure code (`@`), state-mutating effects on named automata
> (`#`), and per-occurrence unsafe operations.

---

## 7. What is genuinely Clifford

After mapping the borrowed pieces:

- Effect-system disjointness on `#mutates` profiles → Lucassen-Gifford
- Frame-rule-style composition → separation logic
- Body-scoped references → Clifford's own simplification of Rust
  lifetimes (defensible but not novel)
- Sigma loops with bounds → Liquid Haskell's poor cousin
- Totality / effect rows / sigma-refinements → Idris / Koka / Liquid
- Narrow unsafe primitives → Ada/SPARK

What remains as Clifford's own contribution:

1. **The two-sigil partition as a pre-attentive feature.** No other
   language commits to single-character grep-ability of the
   pure/imperative boundary at the source level. Haskell's IO is in
   types; Pony's capabilities are at reference sites; Koka's effects
   are in signatures. Clifford makes the partition visible at every
   call site. This is real and worth keeping.
2. **Automaton-as-state-owner as the only legal home for mutable
   state.** Rust permits free-standing mutable globals (with `static
   mut`) and stack mutation freely. Pony has actors, but actor
   boundaries are scheduling boundaries. Clifford insists every byte
   of mutable state is owned by a named `#automaton` whose mutation
   profile is statically declared. This is more restrictive than
   anything in the literature and arguably the design's strongest
   organizing principle.
3. **The combination.** Sigil + automaton-state-owner + narrow unsafe
   + sigma loops + body-scoped references is a coherent point in the
   design space that no existing language occupies. The closest analog
   the reviewer found was "Pony for Cortex-M with Ada-style unsafe
   discipline." That's a defensible niche.

These are the things to lead with after the pivot.

---

## 8. Where the design needs sharpening

Drawing from sections 2–6 plus the hostile review:

- **`access<T>` is overweighted.** Demote from "third layer" to
  "typing discipline within the imperative layer." README + CLAUDE.md
  + spec §1.

- **`$ [TraitList]` carries two unrelated jobs.** Decision #22 traits
  are documentary tags on `#effect`; Decision #23 traits are
  type-checked rows on `@fn`. Same syntax, different semantics. Either
  partition cleanly in §4.5 or split the syntax (`$@ [...]` for `@fn`,
  `$# [...]` for `#effect`?). The current overlap is a footgun.

- **Decision #21 (rotor-plane locks via mixed-metric GA), #26
  (refinement of #21), #27 (GA across scales).** The hostile review's
  central evidence that the GA framing was a complexity attractor.
  All three should move to `docs/research/` with a note that the
  problems they address (shared mutable resources, distributed race
  detection) are real but Clifford's chosen mechanisms (Pony refcaps
  / SRP for shared resources; OCC literature for distributed) are
  better-established.

- **Decision #22's "engine ignores these traits" is fine in
  isolation** (documentary tags are a known pattern), **but the
  category overlap with #23 needs cleaning up** as above.

- **Decision #23's three-subfeature bundling is the strongest
  remaining hazard.** Totality, effect rows, and refinements are
  separate research areas. v0.2 should land totality + the fixed-set
  effect-row subset only; the sigma-bound refinement extension can
  ride along because it's a Decision #14 generalization, not new
  machinery. Liquid Haskell-style refinements beyond sigma bounds
  should be explicitly carved out as v0.4+.

- **The "general-purpose" positioning is dilutive.** README claims
  Clifford is not embedded-only; this is aspirational without an
  ecosystem. Reposition as "designed for safety-critical systems
  programming, with embedded firmware as the canonical first target,"
  and stop apologizing for the embedded specialization.

---

## 9. Implications for the spec and decisions

This document recommends, but does not enact, the following follow-up
work. Each is a separate slice or PR.

1. **Rewrite spec §7 (orthogonality engine).** Open with the
   Lucassen-Gifford / Reynolds citation. Frame the wedge-product
   computation as bitmask-encoded effect-set disjointness. Remove
   Cl(0,0,n) framing from the surface; keep the bitmask-XOR as the
   implementation strategy under the heading "Implementation:
   bitmask intersection." Move Appendix B's category-theoretic
   apparatus to `docs/research/categorical-foundation.md` for anyone
   who finds it pedagogically useful but does not want it normative.

2. **Rewrite README and CLAUDE.md §0–§1.** Lead with the two
   distinguishing features (sigil-partition; automaton-as-state-owner)
   plus the Ada/SPARK narrow-unsafe story. Drop the "novel GA" claim.
   Reframe the three-layer triangle as two-layers-plus-typing-discipline.

3. **Decision audit (target slice 48).** Walk Decisions #1–#27 with
   the three first principles in hand. Specific recommendations from
   this document:
   - #21, #26, #27 → `docs/research/`. Cut lexer reservations and
     AST scaffolding.
   - #22 → keep, but partition cleanly from #23 in §4.5.
   - #23 → narrow: ship totality + fixed-set effect rows + sigma-bound
     refinements in v0.2. Defer everything else explicitly to v0.4+.
   - All others → grade KEEP / NARROW / CUT in a new
     `docs/decision-audit-2026-05.md`.

4. **Rename `crates/ortho`** to `crates/disjoint` or `crates/effects`.
   The `ortho` name is GA-flavored; the new name should reflect the
   effect-system / separation-logic lineage. (Note: this is a
   workspace-wide rename and should land as its own slice with no
   semantic changes.)

5. **Add SRP-flavored handling of shared resources.** Sketch
   `#owned` and `#sendable` per §3 above; if the design holds, this
   *replaces* Decision #21's mixed-metric machinery with established
   literature. Validate against the comparison programs in slice 47
   before locking.

6. **Eventually: the comparison artifact (slice 47 in the hostile
   review).** Five small programs, three implementations each
   (Clifford / Rust+RTIC / C with separation-logic proof). Identify
   the bug class each tool catches. This is the artifact that lets
   the project make honest claims.

---

## 10. Direction summary

Clifford is in the effect-system / separation-logic / Ada-narrow-unsafe
tradition, with a distinctive sigil-partition and automaton-as-state-owner
overlay. The GA framing was decorative and is being dropped. The two
remaining layers can be theoretically sharpened by:

- Citing the lineage explicitly in spec §7 (effect systems +
  separation logic for the imperative layer; HM/Koka/Idris for the
  functional layer; Ada/SPARK for the unsafe primitives).
- Adopting the SRP literature wholesale for the priority + shared-
  resource story, in place of Decision #21's mixed-metric GA.
- Adding a minimal `#owned` / `#sendable` qualifier inspired by
  Pony's `iso` to express send-once shared mutable state, again in
  place of Decision #21.
- Narrowing Decision #23 to the v0.2-realistic subset (totality +
  fixed-set rows + sigma-bound refinements).
- Resolving the #22 vs #23 trait-list overlap with explicit
  partitioning or sigil split.
- Cutting Decisions #21, #26, #27 from the v1.0 surface; moving to
  research-future folder.
- Demoting `access<T>` from third-layer to typing-discipline.

Each is a tractable next step, grounded in named literature, that
respects the three first principles: GA-driven engine *is* now
honestly named (effect-set disjointness check); the functional/automaton
split *is* the load-bearing design; and the resulting language *is*
simpler than what was being built before.

---

*Document version 0.1.0. Drafted 2026-05-15 after the post-review
pivot. To be revised as the spec rewrite, decision audit, and
comparison-artifact work proceed.*
