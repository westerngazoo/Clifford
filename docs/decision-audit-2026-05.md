# Decision Audit — May 2026

> **Status:** working document / proposal. Drafted 2026-05-15 as the second
> artifact of the post-GA-narrative pivot (after `docs/foundations.md`).
> Grades every locked decision against the project's three first principles
> and the post-pivot grounding. **This document does not enact anything** —
> per CLAUDE.md §5.5, changes to locked decisions need maintainer sign-off,
> and each lands as its own PR. This is the map that sequences that work.

---

## 0. Why this audit exists

Decisions #1–#27 were locked between 2026-04-28 and 2026-05-05. A hostile
review (2026-05-15) found the unifying "Geometric Algebra orthogonality
engine" claim was decorative — the engine is a bitmask set-disjointness
check, standard since Lucassen & Gifford (1988). The project pivoted:
drop the GA-novelty narrative, keep the language, ground both layers in
the literature (`docs/foundations.md`). A research advisory on functional-
layer purity (2026-05-15) added that `@`-purity is best framed as an
information-flow non-interference property.

Several locked decisions were written *because the GA framing demanded
them*, not because a v0.1/v0.2 use case required them. Others are sound
but carry GA vocabulary that the pivot makes stale. This audit separates
the two and grades each decision.

## 1. Grading legend

| Grade | Meaning |
|---|---|
| **KEEP** | Sound as locked. May need a vocabulary sweep (stray GA terms) but the design stands. |
| **NARROW** | The core idea is sound; scope or surface has accreted past the third principle. Tighten it. |
| **CUT** | The decision is GA-framing with no surviving independent value, or its kernel folds into another decision. |
| **DEFER-TO-RESEARCH** | Addresses a real problem, but via GA machinery that is unimplemented, speculative, and better served by established literature. Move to `docs/research/`; cut its scaffolding from the live tree. |

The three first principles, restated (they are the grading rubric):

1. **GA-driven engine** to check orthogonality/correctness even in parallel scenarios — now honestly named: a read/write footprint disjointness check.
2. Code split into **functional logic** (`@`) and **automaton state mutators** (`#`).
3. **As simple as possible given the restrictions.**

## 2. Summary table

| # | Title | Grade | One-line rationale |
|---|---|---|---|
| 1 | Syntactic layering via sigils | **KEEP** | The sigil partition is genuinely Clifford's; it is the surface syntax of the IFC lattice. |
| 1a | Local-stack mutation permitted | **KEEP** | The `ST`-monad carve-out. Add the citation. |
| 2 | Hybrid trait system `$ [TraitList]` | **NARROW** | Sound idea; cut the basis-vector language; resolve the 3-way `$ [...]` overload. |
| 3 | Named effect procedures `#>` | **KEEP** | Solid. Stray `#hardware` reference is spec rot. |
| 4 | Auto-assign GA basis vectors | **CUT** | Almost entirely GA surface; kernel ("invisible by default, auditable") folds into the §7 rewrite. |
| 5 | Automaton-as-Category | **NARROW** | Automaton-as-state-owner is core; the category-theory apparatus is novelty-bait. |
| 6 | Memory-mapped registers as automata | **KEEP** | Unifying, no novelty-bait. One GA phrase to reword. |
| 7 | `#test "name" { … }` | **KEEP** | Smallest viable testing primitive. |
| 8 | `:=` short binding | **KEEP** | Harmless sugar. Discretionary simplification candidate only. |
| 9 | Drop `#visible`/`#hidden` | **KEEP** | A deletion decision; already done. (#25 reintroduces a narrower `#hidden`.) |
| 10 | Interrupt vector naming via linker symbol | **KEEP** | Pragmatic; matches Cortex-M/RISC-V conventions. |
| 11 | `@sequential(A, B)` | **KEEP** | Sound trusted-assertion escape hatch; SRP-adjacent. |
| 12 | `#staged` automata | **KEEP** | Implemented, sound, no GA dependency. |
| 13 | Body-scoped references | **KEEP** | Clifford's own lifetime-free simplification; closes the `ST` escape channel structurally. |
| 14 | Sigma loops | **KEEP** | Sound, simple, honest "refinement-lite" framing. |
| 15 | Single-field mutation sugar | **KEEP** | Ergonomic, no semantic divergence. |
| 16 | `#interface` / `#impl` plugin mutators | **KEEP** | Sound; explicitly Pony-interfaces-analog. One GA phrase to reword. |
| 17 | Ada-style narrow unsafe primitives | **KEEP** | Clifford's strongest unique feature. Elevate it. |
| 18 | `#audit` runtime auditing | **KEEP** | Implemented through slice 44. Status text is stale. |
| 19 | Nominal access types | **KEEP** | Sound. `access<T>` demotion (layer → typing discipline) is a §1 framing change, not a change to #19. |
| 20 | First-class bitfields | **KEEP** | Big spec surface, but firmware-first justifies it; the decision argues its own cost honestly. |
| ER1 | Emergent Rule 1 — trait basis vectors global | **CUT** | Pure GA framing. |
| ER2–5 | Emergent Rules 2–5 | **KEEP** | Sound consequences of the layer design. |
| ER6 | Emergent Rule 6 — GA = product-category existence | **CUT** | The exact novelty-bait the review demolished. |
| 21 | Shared automata via mutator multivectors | **DEFER-TO-RESEARCH** | Unimplemented mixed-metric GA; the real need (shared resources) is better served by SRP. |
| 22 | `$ [TraitList]` on effects | **NARROW** | Documentary tags are fine; the syntax collision with #23 must be resolved. |
| 23 | Tighten `@fn` toward Haskell-clean | **NARROW** | Bundles three research areas; ship the v0.2-realistic subset, defer the rest. |
| 24 | `@snapshot` boundary crossing | **KEEP** | Sound; it is exactly the explicit Property-A/B boundary. One sub-item depends on #21. |
| 25 | `#hidden` encapsulation | **KEEP** (reframe) | The feature is sound scoping; drop the "algebraic trivial orthogonality" rationale. |
| 26 | Rotor-based plane-confined locks | **DEFER-TO-RESEARCH** | Refines #21; rotor-as-acquisition is novelty-bait. |
| 27 | GA across scales — distributed race detection | **DEFER-TO-RESEARCH** | "Publish read/write sets to a coordinator and intersect" — known OCC work, GA-costumed. |

**Tally:** 20 KEEP (incl. #1a, ER2–5), 4 NARROW (#2, #5, #22, #23), 3 CUT (#4, ER1, ER6), 3 DEFER-TO-RESEARCH (#21, #26, #27).

The headline: **the language survives the audit almost intact.** Everything that makes Clifford itself — sigils, automaton-as-state-owner, narrow unsafe primitives, body-scoped references, sigma loops, register-block automata, `#staged`, `#audit`, `#interface` — is KEEP. What gets cut or deferred is, with near-total consistency, *the GA apparatus and the three decisions written to feed it*. That is the cleanest possible confirmation that the pivot is correct: the decorative layer and the load-bearing layer separate at a clean seam.

---

## 3. NARROW — four decisions to tighten

### Decision #2 — Hybrid trait system `$ [TraitList]`

**Keep:** the hybrid structural-trait scheme (auto-inference + optional annotation), the `$ [Pure]` default for unmarked `@fn`, the `$ [Opaque]` default for C FFI. These are sound and serve principle 2.

**Cut:** Rule 4 ("Trait Basis Vectors" — "compiler auto-assigns basis vectors to each trait globally... required for GA orthogonality"). Traits are not algebraic objects; they are effect-row labels. The basis-vector language goes. The line "Power users (GA enthusiasts) can annotate explicitly" identifies exactly the audience `foundations.md` §8 says not to design for — cut it.

**Resolve:** `$ [TraitList]` now carries *three* jobs across #2, #22, #23 — purity rows on `@fn`, documentary tags on `#effect`, type-checked effect rows on `@fn`. See §6 below; the resolution is shared with #22 and #23.

### Decision #5 — Automaton-as-Category

**Keep:** automaton-as-state-owner (the single most important organizing principle — `foundations.md` §7), `#transition`-only state changes, the FSM model, the optional-`#states` monoid-degenerate case as an *ergonomic* fact, and **all of Refinements #5a–#5e** — those are concrete, sound, and already implemented (transition atomicity #5e is live codegen).

**Narrow:** the *category-theoretic framing*. "Every `#automaton` is a small category," the one-object-monoid vocabulary, the product category `C_A × C_B`, `Appendix B: Categorical Semantics`, `cliffordc inspect --as-category`, and Emergent Rule 6 are the same novelty-bait pattern as the GA engine — intimidating mathematics whose technical content (the actual codegen, the actual reachability analysis) is plain FSM graph work. The review flagged Appendix B explicitly. **Action:** keep the FSM; call it a finite-state machine; move Appendix B to `docs/research/categorical-semantics.md` for anyone who finds it pedagogically pleasant. The decision's own Rule 6 ("Category-theoretic terminology is internal") already concedes the framing earns nothing on the surface — finish that thought and drop it from the foundation too.

### Decision #22 — `$ [TraitList]` on effects

**Keep:** the idea — documentary kind-tags on `#effect`/`#interrupt`/`#transition` (`Hardware`, `PureState`, `LockingDiscipline`, `Acquire`/`Release`/`SeqCst`, `Encapsulated`). Documentary tags consumed by codegen/audit/certification are a known, principled pattern. The memory-ordering tags in particular do real work (instruction selection).

**Narrow:** `Realtime(deadline)` is described as "refinement-typed" — that is aspirational and should be marked as such or dropped from the v0.2 set. And `LockingDiscipline` references Decision #21's §5.5, which is being deferred — drop or rename it.

**Resolve:** the syntax collision with #23 — see §6.

### Decision #23 — Tighten `@fn` toward Haskell-clean discipline

**Keep:** the *direction* — `@fn` should be semantically, not just syntactically, pure. Total-by-default and effect rows both belong.

**Narrow:** the decision bundles three independent research areas — totality checking (Idris-grade), effect rows (Koka-grade), refinement types (F\*/Liquid-grade) — into one locked commitment with v0.2 implementation slated. The purity advisory's honest split:

- **v0.2-realistic:** totality via structural recursion + sigma bounds (Idris's algorithm, tractable); the *fixed-set* effect rows `Pure`/`Readable`/`Observable`/`Opaque` (Koka-without-polymorphism, tractable); sigma-bound refinements on function arguments (a Decision #14 generalization, not new machinery).
- **Defer explicitly to v0.4+:** anything SMT-backed; Liquid-Haskell-style refinement predicates beyond sigma bounds; `@throw`/`try`. ADR 0003's P3 already fails-closed on SMT (`E0542`) — good; make the deferral a visible part of the decision, not a footnote.

**Recast:** per the purity advisory (R1/R2), the effect-row subset *is* Clifford's functional-layer purity mechanism. It should be recast as a two-point information-flow lattice (`@ ⊑ #`) with a non-interference reading, citing Volpano-Smith (1996) and the Dependency Core Calculus (Abadi et al. 1999). And the layer check should move *into type formation* (advisory R2) so an impure `@fn` is unrepresentable rather than rejected-by-walk. This is the single most important follow-on from the purity research and #23 is where it lands.

---

## 4. CUT — three items with no surviving independent value

### Decision #4 — Auto-assign GA basis vectors

The decision is, end to end, the GA surface: "auto-assign basis vectors `e1, e2, e3`," "behavior multivector," "outer product `A ∧ B == 0`," grade, `#basis: {field: e1}` explicit override, `--verbose-basis`, IDE hover showing "behavior multivector: e1 ∧ e2 (grade 2, bivector)." None of it survives the pivot.

**The kernel that does survive** is one sentence: *the disjointness engine assigns each field/trait an internal identifier automatically; the check is invisible in normal use and auditable via a flag.* That sentence belongs in the rewritten §7, not in a standalone decision. The `#basis` override syntax is genuinely cut — `foundations.md` §8 says so directly. `--verbose-basis` becomes `--explain-disjointness` (or similar) in the §7 rewrite.

**Action:** mark #4 superseded; fold its one surviving sentence into the §7 rewrite; remove `#basis` from the grammar.

### Emergent Rule 1 — Trait basis vectors are global

Pure GA framing ("assigns it a single, consistent basis vector globally... ensures orthogonality checks remain sound"). The real content — "a trait name resolves to the same effect-row label everywhere" — is just name resolution and needs no rule. **Action:** delete; absorb the naming-consistency point into the trait-resolution spec text.

### Emergent Rule 6 — GA orthogonality = product-category existence

This is the precise claim the hostile review demolished: "you constructed a category whose composition is defined exactly when writes are disjoint, then proved composition is defined exactly when writes are disjoint." The bitmask check is not "grounded in a categorical theorem"; it is a set-intersection test. **Action:** delete the rule; its honest replacement is the Lucassen-Gifford / separation-logic citation in the §7 rewrite (`foundations.md` §2).

---

## 5. DEFER-TO-RESEARCH — the GA-across-scales arc (#21, #26, #27)

These three are the strongest evidence that the GA framing was a complexity attractor. All three are locked, none are implemented, all are gated to v0.7+ or v0.4+, and all exist to extend the GA story rather than to serve a v0.1/v0.2 use case.

### Decision #21 — Shared automata via mutator multivectors (mixed-metric GA)

The problem is real: Clifford cannot model shared mutable resources (run-queues, capability tables, allocators), and real kernels need them. The chosen mechanism — a mixed-metric Cl(p,0,n) algebra where shared fields contribute non-null basis vectors, locks are multivectors `lock(L) = pri(L) + e_L`, deadlock-freedom falls out of a "sketched" theorem — is unimplemented, gated to v0.7, and assumes a categorical correctness proof that does not exist in writing.

`foundations.md` §3–§4 argues the established answer: **Stack Resource Policy** (Baker 1991, mechanized by RTIC, 35 years of certification use) for the priority + shared-resource story, plus a minimal **`#owned`/`#sendable` qualifier** inspired by Pony's `iso` reference capability for send-once shared mutable state. Both have published soundness results; neither needs an algebra.

**Action:** move Decision #21 to `docs/research/ga-shared-automata.md` as a research bet. **Cut its Phase-1 scaffolding from the live tree** — the `crates/lexer` reservations of `#shared`/`#lock`/`#with_lock`/`#reads`/`#rotor`, and the `FieldKind` `#[non_exhaustive]` enum in `crates/ast`. That scaffolding's stated justification was "the cost of not doing it is weeks of refactoring once v0.7 begins" — but if v0.7 adopts SRP + `#owned` instead, the scaffolding is for a design that won't be built. Open a fresh decision for SRP-based shared resources when the need is real (validated against the comparison artifact).

### Decision #26 — Rotor-based plane-confined locks

Refines #21; same verdict. "To acquire `L`, a thread rotates the cell `M ← R_t · M` where `R_t = exp(-θ_t · B_t / 2)`" — and then the decision admits "`exp` does not appear in generated code; runtime cost is a normal CAS-based spinlock with an integer owner-ID." That admission *is* the diagnosis: the rotor algebra is decoration over a CAS spinlock with an owner-ID. **Action:** move to `docs/research/`; cut the `#rotor_lock`/`#thread_plane`/`#guarded_by` token reservations.

### Decision #27 — GA across scales — distributed runtime race detection

The decision's own table says the distributed runtime "carries" `Behaviour { (resource, slice) bits }` and the implementation is "RPC publish + central coordinator + RPC retract; `&` op on coordinator." That is optimistic-concurrency-control: publish a read/write set, intersect at a coordinator, retract. Sinfonia (2007), Calvin (2012), and every modern OCC system already do exactly this. The "GA is the unifying algebra" claim adds nothing operational. **Action:** move to `docs/research/`; cut the `#dist_shared`/`#dist_phase`/`#on_dist_race` reservations. If distributed checking is ever built, ground it in OCC literature under its own decision.

**Note on "but we lose the unifying claim":** yes — deliberately. The unifying claim ("GA scales from single-IRQ to multi-machine") was the marketing thesis the pivot exists to retire. Decisions #21/#26/#27 are where that thesis was load-bearing. Deferring them *is* the pivot.

---

## 6. Cross-cutting findings

### 6.1 The `$ [TraitList]` triple-overload

One syntax, three jobs:

- **#2** — purity/readability rows on `@fn`, structurally inferred.
- **#22** — documentary kind-tags on `#effect`/`#interrupt`/`#transition`, engine-ignored, tooling-consumed.
- **#23** — type-checked effect rows on `@fn` (`Pure`/`Readable`/`Observable`/`Opaque`), verified.

A reader seeing `$ [Hardware, Realtime(100us), Acquire]` cannot tell from syntax which entries are type-checked and which are documentary. Two clean resolutions:

- **(a) Partition by layer in the spec:** state in §4.5 that `$ [...]` on `@fn` is type-system-verified (an effect row) and `$ [...]` on `#effect` is documentary (a tag set). The layer is already visible from the sigil, so the partition is unambiguous — it just needs to be *written down*.
- **(b) Split the syntax:** different brackets/sigils for the verified vs documentary lists.

Recommend **(a)** — it is zero grammar change and the sigil already disambiguates. Decide this in the #22/#23 follow-on PRs.

### 6.2 GA-vocabulary sweep

Many KEEP decisions carry stray GA vocabulary that the pivot makes stale: "basis vector" (#6, #16, #20), "behavior multivector" (#16), "wedge" / "outer product" (passim). These are not per-decision grades — they are one mechanical sweep across `DECISIONS.md`, `CLIFFORD_SPEC.md`, and crate docstrings, replacing GA terms with the effect-system vocabulary (`foundations.md` §2). Best done as a single dedicated slice *after* the §7 rewrite settles the canonical replacement terms.

### 6.3 `access<T>` is a typing discipline, not a third layer

`foundations.md` §1: Decision #19's `access<T>` is sound *as a decision*, but the README/CLAUDE.md/spec-§1 "three-layer Safety Triangle" oversells it. Demote to "a typing discipline within the imperative layer." This is a §1/README framing change, not a change to #19 — #19 stays KEEP.

### 6.4 Stale status text

`DECISIONS.md`'s header status line and Decision #18's body still say the runtime `PointerAuditor` pass is "deferred to a later milestone." It landed in slices 37–44. The doc-sync is overdue (last sync was slice 28). Fold into the GA-vocabulary sweep slice or do it first.

---

## 7. Recommended follow-on sequencing

Each item is a separate PR per CLAUDE.md §5.5. Ordered by dependency:

1. **Decision-change PRs for #21/#26/#27** — move to `docs/research/`, cut lexer reservations + `FieldKind` scaffolding. Mechanical, unblocks the spec rewrite. *(One PR, or three small ones.)*
2. **§7 rewrite** — cite Lucassen-Gifford + Reynolds; replace the GA surface; fold in Decision #4's surviving kernel; move Appendix B to `docs/research/`. Settles the canonical replacement vocabulary.
3. **GA-vocabulary sweep + status-text sync** — once §7 fixes the terms, sweep `DECISIONS.md` + spec + docstrings; delete ER1 and ER6; fix the stale #18 status.
4. **#22/#23 resolution PR** — partition the `$ [TraitList]` semantics per §6.1; narrow #23 to the v0.2-realistic subset with explicit v0.4+ deferrals.
5. **Purity ADR (advisory R1)** — recast `@`/`#` as a two-point IFC lattice; adopt the non-interference framing. Small, high-value; can run in parallel with 2–4.
6. **README + CLAUDE.md §0–§1 reframe** — drop the GA claim, demote `access<T>`, lead with the real distinguishing features.
7. **`crates/ortho` rename** — to `crates/disjoint` or `crates/effects`; semantically empty, its own slice.
8. **New decision: SRP-based shared resources** — `#owned`/`#sendable`, grounded in Baker 1991 + Pony `iso`; validate against the comparison artifact before locking. Replaces the deferred #21.

Items 2–7 are doc/spec work with little or no compiler-code risk. Item 8 is the one genuinely new design effort, and it should not be locked until there is evidence (the comparison artifact) that the embedded patterns Clifford targets actually need it.

---

*Document version 0.1.0. Drafted 2026-05-15. Companion to `docs/foundations.md`.
Non-normative; a proposal for maintainer sign-off, not an enactment.*
