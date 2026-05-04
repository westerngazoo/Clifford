# Changelog

All notable changes to Clifford and `cliffordc` are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/) — pre-1.0 minor versions
may include breaking changes.

## [Unreleased]

### Proposed — ADR 0005: Rotor-based plane-confined locks (2026-05-04)

New ADR formalising a sharper interpretation of the rotor machinery
already locked in Decision #21 / ADR 0002. **Status: Proposed.** The
ADR reframes rotors from a same-priority *tiebreak* mechanism to the
*acquisition primitive itself*: a `#rotor_lock L` is conceptually a
multivector cell that gets rotated into the holder's signature plane
on acquire, and the runtime check "is acquire possible?" reduces to
the wedge-product the orthogonality engine already computes
(`caller.thread_plane ∧ lock.plane`).

Three properties fall out of the algebra:
- **Mutual exclusion.** Cross-plane acquire produces a non-rotor
  multivector (odd-grade components) → reject.
- **Wrong-thread release detection.** `R̃_t' · R_t ≠ 1` for `t' ≠ t`
  → reject.
- **Re-entrancy.** Same-plane re-entry produces `R_t(2θ)`, still a
  rotor in the holder's plane → succeed (with optional depth
  counter — Q2 in §6).

**Crucial: `exp` cost is zero at runtime.** The lowered code is a
standard CAS-based spinlock with an integer owner-ID field; the GA
formulation lives entirely in the *static analyzer*. This is the
same pattern Decision #21 established: GA is the proof system, not
the runtime.

**Status remains Proposed (not Accepted)** until the five open
questions in ADR §6 close:
1. Thread-plane assignment (embedded vs RTOS — proposed: pool-based
   for v0.7).
2. Re-entrancy semantics (free / counted / forbidden — proposed:
   counted to match POSIX expectations).
3. Same-plane uniqueness enforcement (proposed: hard error
   `E0539 DuplicateThreadPlane`).
4. Who carries `θ` for release symmetry (proposed: lock owns its
   full state).
5. Relation to Decision #21's priority-ordering proof (proposed:
   rotor-as-acquisition supersedes; priority becomes a derived
   total order on planes).

If accepted, this becomes a *refinement* of Decision #21 (not a
separate decision), implementation gated to v0.7+ alongside the
rest of the mixed-metric machinery.

Diagnostic family proposed: E0535 PlaneeMismatch, E0536 NoThreadPlane,
E0537 SharedFieldOutsideLock, E0538 ReEntryViolation, E0539
DuplicateThreadPlane.

This is a pure documentation ADR — no code changes, no spec changes
yet. Spec amendments and `crates/ortho` extensions land per ADR
acceptance and per Decision #21's v0.7 milestone.

### Type Checker — Slice T4a: nominal types from Path-position type expressions (2026-05-01)

First semantic resolution of `Path`-form type expressions in the type
checker. `clifford-types` previously translated `TypeKind::Path` to
`Type::Unknown`; T4a introduces a new `Type::Nominal { path, args }`
variant and translates path-position types into it verbatim.

- New `Type::Nominal { path: Vec<String>, args: Vec<Type> }` variant
  on `crates/types/src/lib.rs::Type`. Path is recorded as the source-
  order segments (e.g. `["clifford", "core", "Option"]`); generic
  arguments translate recursively. `display()` renders as `Foo`,
  `Result<u32, bool>`, `clifford::core::Option<u8>`.
- `type_from_type_expr()` now translates `TypeKind::Path(pt)` to
  `Type::Nominal { path: pt.segments.clone(), args: pt.generic_args
  .iter().map(type_from_type_expr).collect() }`.
- Two `Type::Nominal` values with different paths are distinct (per
  Decision #19's nominal-access identity rule, extended to all top-
  level type-bearing declarations).

What slice T4a deliberately does **not** do, kept for T4b+: verifying
the path resolves to an actual top-level declaration; following `@type`
aliases to the underlying type for equality / unification (so today
`let _x: MyAlias = 0u32;` where `@type MyAlias = u32;` emits E0512 —
the `Nominal MyAlias` ≠ `Primitive u32` mismatch is correct under
T4a's assumptions); ADT-variant resolution for multi-segment paths
like `Result::Ok`.

Tests: 10 new unit tests exercising display (simple / multi-segment /
generic / nested-generic), distinct identity, parameter-position type
carry-through into expression typing, let-annotation E0512 with the
nominal name in the diagnostic, generic-arg recursive translation,
empty-args verbatim translation. Workspace remains green (all 502+
tests passing across 19 crates).

### Spec — Decisions #22-#25: cleaner pure/imperative boundary (2026-05-03)

A coordinated set of four design decisions sharpening Clifford's pure /
imperative split. **Decisions #22 and #25 lock now** (designs are
mechanical); **Decisions #23 and #24 record the direction with ADRs
forthcoming**.

- **Decision #22 — Kinds of Imperative.** Extend `$ [TraitList]` markers
  from `@fn` to `#effect` / `#interrupt` / `#transition` declarations.
  Predeclared traits classify mutation kind: `Hardware`, `Realtime`,
  `Acquire` / `Release` / `SeqCst` (memory ordering), `LockingDiscipline`,
  `PureState`, `Encapsulated`. The orthogonality engine ignores them;
  codegen / `cliffordc audit` / certification consume them. Locked.
- **Decision #25 — `#hidden` Encapsulation.** Re-introduce `#hidden` as
  a per-field modifier on automaton fields, with the algebraic
  interpretation: a hidden field's basis vector cannot appear in any
  callable's `actual_writes` outside the owning automaton's surface.
  Encapsulation is "the bit isn't there for outsiders to refer to" —
  trivial orthogonality by construction. No engine machinery; ~50 LoC
  parser + resolver. Locked.
- **Decision #23 — Tighten `@fn` toward Haskell-clean.** Direction
  agreed: total by default, effect rows in signatures, refinement types
  in argument positions, local mutation per Refinement #1a remains
  permitted (ST-monad-equivalent). DESIGN-IN-PROGRESS — needs an ADR
  surveying Idris totality, Liquid Haskell refinements, Koka effect rows.
  Targeted ADR: `docs/adr/0003-haskell-clean-fn-discipline.md`.
- **Decision #24 — `@snapshot` Boundary Operator.** Direction agreed:
  introduce `@snapshot Auto.field` as the only way to read mutable
  automaton state into pure-side analysis. The boundary crossing
  becomes syntactically visible. DESIGN-IN-PROGRESS — needs an ADR
  resolving the expression-vs-statement question, copy-by-value vs
  ref-to-snapshot, interaction with `#shared` (Decision #21), and
  backward compatibility with the existing snapshot-by-convention
  pattern in book Ch. 39. Targeted ADR:
  `docs/adr/0004-snapshot-boundary-operator.md`.

The four taken together commit Clifford to the framing the architect
articulated: pure side becomes Haskell-clean (Decisions #23 + #1a);
imperative side becomes a legible "dark side" with explicit kinds
(Decision #22), explicit encapsulation (Decision #25), and an explicit
boundary-crossing operator (Decision #24).

This PR is pure documentation — `DECISIONS.md` updated with the four
entries, the matrix table extended, and the date footer rewritten.
No code changes; no spec amendments yet (those land per-decision as
ADRs lock and implementation begins).

### Spec — §7.0.1 Safety Pillars + book Ch. 39 SPSC ring buffer (2026-05-03)

Pins the v0.1 GA orthogonality engine's contract — what's guaranteed,
what's deliberately not — and grounds it in the canonical embedded
worked example.

**Spec:**

- New `docs/CLIFFORD_SPEC.md` §7.0.1 "Safety Pillars" subsection.
  Two normative statements about what the v0.1 engine guarantees
  (procedural mutation safety; parallel verification by exhaustive
  pairwise check) and three explicit limits (narrow-unsafe writes
  outside the proof boundary, read-write races deferred to v0.2,
  `@sequential` user-asserted-not-verified). Sets the precise boundary
  of v0.1 safety so users designing systems know what they can and
  cannot rely on.

**Book:**

- `book/src/part5/39-firmware.md` — first real Part-V chapter.
  Producer/consumer SPSC ring-buffer worked example end-to-end. Two
  versions: the naive design (with a `count` field both sides update,
  which the engine rejects with E0520 on `count`) and the lock-free
  SPSC (no `count`, derived from head/tail, which the engine accepts).
  Each version traced through every compiler phase showing what the
  engine sees. Closes with explicit cross-references to §7.0.1's two
  pillars and the read-write deferral. ~5,000 words.

Both items are pure documentation — no code touched. PRs against the
ortho engine and the effect crate land in their own branches.

### Added — Ortho slice O1: GA orthogonality engine (Cl(0,0,n) bitmask check) (2026-05-03)

The headline slice. After this lands, Clifford does the thing it claims
to do: compile-time race detection via geometric algebra, on real `.cl`
source, with diagnostics in source identifiers (not basis indices).

End-to-end pipeline driven by `check_orthogonality(&Program,
&MutationProfiles)`:

1. **Basis assignment** (§7.1): every distinct `(automaton, field)`
   pair appearing in any callable's `actual_writes` set gets a unique
   bit position in the blade. Sorted by `(automaton, field)` for
   reproducibility.
2. **Behavior multivector construction** (§7.2): per callable, one
   `Blade { bits }` whose set bits = the basis vectors of fields the
   callable writes (direct + transitive per slice E2).
3. **Concurrency inference** (§7.3): every pair of `#effect`s,
   `#interrupt`s, and effect-interrupt combinations is treated as
   concurrent. `@sequential(A, B)` excludes pairs *only* when each
   side touches exactly one of `{A, B}` (strict v0.1 rule — prevents
   the attribute from masking races through third automata).
4. **Pairwise check** (§7.4): for every concurrent pair,
   `outer_product(blade_a, blade_b)`. `None` (collapse) → race
   detected.
5. **Diagnostic** (§7.5): shared fields decoded back to source
   `(automaton.field)` notation per Emergent Rule 1; never raw `e_n`
   indices.

Public surface: `check_orthogonality`, `assign_basis`,
`build_behaviors`, `build_concurrency_matrix`, `outer_product`,
`BasisAssignment`, `Blade`, `CallableBehavior`, `ConcurrencyMatrix`,
`OrthoReport`. `MAX_BASIS_VECTORS_V1 = 64` (with `E0530` when
exceeded). `outer_product`'s foundational invariant
(`is_some() ⟺ a & b == 0`) is property-tested.

Errors: `E0520 OrthogonalityViolation` (callable pair + shared
`(automaton.field)` pairs by source name), `E0530 TooManyBasisVectors`.

PR #5; built atop slice E2 (mutation profiles, PR #10).

### Added — Phase 2 effect slice E4: Refinement #5e interrupt-overlap set R(A) (2026-05-02)

Computes the `R(A)` set per Refinement #5e: for each automaton `A`,
the set of interrupts whose `actual_writes` overlap `A`'s field set.
Downstream consumers (atomicity check, `cliffordc audit`) use `R(A)`
to determine which critical sections need interrupt-disabling.

- Public entry: `compute_interrupt_overlap(&Program, &MutationProfiles)
  -> InterruptOverlap`. Returns a `HashMap<AutomatonName, HashSet<
  InterruptName>>`.
- `InterruptOverlap::interrupts_for(&str)` lookup; returns a static
  empty set via `OnceLock` for the no-overlap path (no allocation).
- Validates that every `#mutates` entry on `#interrupt` declarations
  references a real automaton; emits `E0440 UnknownMutatedAutomaton`
  otherwise.
- Tests cover: empty programs, single interrupt + single overlap,
  multi-interrupt overlap, transitive overlap through `#>` calls,
  no-overlap silence, unknown-automaton diagnostic, and shared-set
  static-empty optimization.

PR #9.

### Added — Phase 2 effect slice E3: §6.3 proc-call graph + cycle detection (2026-05-02)

Builds the procedure-call graph per §6.3 and detects strongly-connected
components (cycles) via Tarjan's algorithm. The graph is the substrate
for slice E2's transitive `actual_writes` closure and for
`@sequential` constraint propagation.

- Public entry: `build_call_graph(&Program) -> Result<ProcCallGraph,
  Vec<EffectError>>`.
- `ProcCallGraph` is a hand-rolled `HashMap<CallableId,
  HashSet<CallableId>>` (no `petgraph` dep — keeps deps minimal per
  CLAUDE.md §3.1; algorithms are textbook ~30 lines).
- `CallableId` covers `@fn`, `#effect`, `#interrupt`, `#transition`,
  and `#proc` (Decision #3); cycle reporting canonicalizes by
  rotating to the lex-smallest member so the same cycle isn't
  reported twice from different DFS entry points.
- Errors: `E0441 CycleInProcCalls` (lists the cycle in canonical
  order), `E0442 UnknownProcReference`.

PR #8.

### Added — Phase 2 effect slice E2: §6.2 mutation profile extraction (2026-05-02)

Computes per-callable `actual_writes` sets per §6.2 (the heart of the
GA engine's input). Transitively closes through `#> proc()` calls
using slice E3's `ProcCallGraph` (delivered together).

- Public entry: `extract_mutation_profiles(&Program) ->
  Result<MutationProfiles, Vec<EffectError>>`. Returns
  `MutationProfiles { actual_writes: HashMap<CallableId,
  HashSet<(AutomatonName, FieldName)>> }`.
- Walks every `#effect`, `#interrupt`, `#transition`, and `#proc`
  body. Records direct writes (`Auto.field = …`, `Auto.field +=
  …`, etc. — the §15 sugars from Decision #15).
- Transitively unions `actual_writes` of every `#>` callee, using
  the call graph from slice E3. Resolves before slice O1's wedge
  check sees the input.
- Validates that every `#mutates` declaration matches the body's
  actual writes (no over-promising or under-promising); emits
  `E0445 MutationProfileMismatch` with both sets named by source
  identifier.

PR #10.

### Added — Phase 2 effect slice E1: §6.1 category construction (2026-05-02)

First piece of the GA-engine bridge. After this slice, the compiler
produces a per-automaton categorical structure (the `C_A` of Appendix B)
that downstream phases (`crates/ortho`, `crates/codegen`) consume.

- `clifford-effect`: public entry point `extract_categories(&Program)
  -> Result<Categories, Vec<EffectError>>`. Walks every `#automaton` and
  produces an `AutomatonCategory` per declaration.
- New types: `Categories` (the artifact), `AutomatonCategory` (per-automaton
  state set + transitions + initial state), `StateInfo`, `TransitionInfo`,
  `EffectError` (reserves E04xx and E06xx ranges per the spec).
- For monoid automata (no `#states` clause per Decision #5 Rule 4), gets a
  synthetic `[Ready]` state automatically.
- For multi-state automata, validates every `#transition T -> Target`'s
  `Target` is in the declared `#states` (`E0430 UnknownState`). Monoid
  automata reject any transition with an explicit destination
  (`E0431 MonoidTransitionWithDestination`).
- Detects duplicate state names (`E0433`) and duplicate transition names
  (`E0432`) within the same automaton; first-wins for the table.
- Errors accumulate (not fail-fast); a single pass surfaces every
  validation failure.
- 13 unit tests + 1 doctest covering: empty programs, monoid automata
  (with and without destinationless transitions), monoid + destination
  rejection, multi-state state recording, valid destinations, unknown
  destination rejection, duplicate-transition rejection, multi-error
  collection, multi-automaton extraction, item_index correctness, and
  a realistic 3-state Counter automaton.
- What's deferred to slice E2+: §6.2 mutation profile extraction
  (per-effect `actual_writes` set, transitive through `#> proc()` calls),
  §6.3 proc-call resolution and CallContext propagation, §6.4 state-tag
  update points, §6.5 invariant verification, §6.6 atomic-annotation
  lowering hints, and the Refinement #5e interrupt-overlap set.

### Added — Phase 1 check slice 1: §5.5 sigil-layer boundary checking (2026-05-01)

The first language invariant Clifford actually enforces. After this PR,
the sigil layering that's been the language's signature property is no
longer a convention — the compiler rejects layer-crossing programs.

- `clifford-check`: public entry point `check(&Program, &Resolution) -> Result<(), Vec<CheckError>>`.
  Walks every `@fn` body and rejects any `#`-construct it finds.
- New `CheckError` variants:
  - `E0101 ImperativeInFunctional` — fired for `#mutate`, `Auto.field <op>= …`,
    `#> proc()`, `#unchecked_store`, `#volatile_store`, `#unchecked_load`,
    `#volatile_load`, `#unchecked_cast`, `#unchecked_offset`, `Auto@state`,
    automaton-field reads (`Counter.value`), and bare automaton references
    (`let _c := Counter`).
  - `E0102 CrossBoundaryCall` — fired when an `@fn` body calls a top-level
    `#effect` or `#interrupt` via regular call syntax. Carries the callee
    name and kind for the diagnostic.
- `#`-layer items (`#effect`, `#interrupt`, `#automaton.transitions`) are
  not walked by §5.5 — imperative constructs are legal there. §5.4
  mutability checking, §5.6 trait-list verification, §5.7 reference
  provenance, and §5.8 sigma bounds will walk them in subsequent slices.
- Errors accumulate (not fail-fast) so a single pass surfaces every
  layer violation in a body.
- Forward-compat: walker uses `_ => {}` arms over `Stmt`/`ExprKind` so new
  variants default to "no rule" behavior. New `#`-constructs added to
  the language need an explicit arm here.
- 25 new unit tests + 1 doctest covering: empty/clean programs, `@fn → @fn`
  calls (allowed), `#`-layer items (not walked), every statement-form
  `#`-construct (Mutate / MutateShort / ProcCall / unsafe stores) in `@fn`,
  every expression-form `#`-construct (unsafe loads, casts, offsets,
  StateRead, automaton-field reads, bare automaton refs) in `@fn`,
  cross-boundary calls to `#effect` and `#interrupt`, multiple-violation
  collection, nested `#`-form inside arithmetic, and a realistic clean
  program. **Total clifford-check: 25 unit + 1 doctest.**

### Added — Phase 1 type checker slice 3: structured-type expressions (2026-05-01)

- `clifford-types`: extends `Type` with `Array { element, size }`,
  `Slice { element }`, `Tuple(Vec<Type>)`, and `Range { element, inclusive }`.
  `Type::display` renders all four (`[u8; 64]`, `[u8]`, `(u32, bool)`,
  `u32..u32` / `u32..=u32`).
- `Expr::Tuple` types to `Type::Tuple`. `Expr::Array` types to
  `Type::Array { element: <first-elem>, size: <count> }`. Empty arrays
  produce `[?; 0]`. Mixed-type arrays propagate Unknown (T4 may add a
  dedicated mismatch error).
- `Expr::ArrayRepeat` types to `Type::Array { element: <value>, size: <count_text> }`.
  Const-evaluating the count is deferred; the raw text is preserved.
- `Expr::Index` types via auto-deref: indexing into `[T; N]`, `[T]`,
  `&[T; N]`, `&[T]`, or the `&[u8]` shorthand all return the element
  type. Non-integer index → `E0517 IndexNotInteger`. Non-indexable
  receiver → `E0516 IndexNonIndexable`.
- `Expr::Range` types to `Type::Range`. Bounds must be the same integer
  type; mismatches reuse `E0510 BinaryTypeMismatch` with op `..` / `..=`.
- `type_from_type_expr` now translates `TypeKind::Array`, `TypeKind::Slice`,
  and `TypeKind::Tuple` to their semantic counterparts. Parameters
  declared as `&[u8; 64]` / `&[u8]` / `(u32, bool)` carry through correctly.
- 16 new tests + every prior slice green: display formatting for all four
  new variants; tuple expressions; array literals; array-repeat;
  indexing into arrays / refs to arrays / slices / refs to slices;
  index-with-non-integer (E0517); index-into-non-indexable (E0516);
  half-open / inclusive ranges; range bound mismatch (E0510);
  array-typed automaton field via `Counter.flags[0]`. **Total
  clifford-types: 78 unit tests + 1 doctest.**
- What remains for slice T4: method-call typing (needs nominal/trait
  registry), `Path([X, Y])` for ADT constructors and module paths,
  generic instantiation with HM unification, trait satisfaction (§5.3),
  access-type modeling.

### Added — Phase 1 type checker slice 2: function calls, automaton fields, references (2026-05-01)

- `clifford-types`: extends `Type` with `Ref { mutable, inner }` for borrow
  expressions and parameter types like `&[u8]` / `&mut T`. `Type::display`
  renders these as `&u32` / `&mut u32`.
- New `SignatureRegistry` (built once at the start of `infer`) maps every
  top-level `@fn` / `#effect` / `#interrupt` name to its `(params, return_type)`.
  Per-call-site lookup is O(1).
- `Expr::Call` typing: when the callee resolves to a top-level callable,
  arguments are checked against the registry's signature. Arity mismatches
  emit `E0514 CallArityMismatch`; per-position type mismatches emit
  `E0513 CallArgMismatch`. The call expression's own type is the callee's
  declared return type (or `Type::Unit` if absent).
- `Expr::FieldAccess` typing: when the resolver tagged the access as an
  `AutomatonField`, the typer fetches the field's declared type from a
  per-automaton field-type registry. Supports both `Auto.field` reads in
  effects and `Self.field` reads in transition bodies.
- `Expr::Ref` typing: yields `Type::Ref { mutable, inner }` where `inner`
  is the operand's type.
- `*r` deref typing: unwraps `Type::Ref` to the referenced type. Applying
  `*` to a non-reference (e.g. `*42i32`) emits `E0515 DerefNonReference`.
- `type_from_type_expr` recursively translates `TypeKind::Ref` so parameters
  declared as `&T` carry their reference structure into the body's typing.
- 17 new tests + every prior slice-1 test still green: borrow / mut-borrow
  yield correct ref types; ref param + deref returns inner type; deref of
  non-reference is E0515; call returns callee's return type; arity mismatch
  is E0514; arg type mismatch is E0513; call to local (shadowed top-level)
  silently returns Unknown; auto-field reads yield the declared field type;
  Self.field reads in transitions work; field type drives let-annotation
  matching (mismatch is E0512); realistic 3-item program with calls and
  fields. **Total clifford-types: 62 unit tests + 1 doctest.**
- What's still deferred (slice T3): index typing (needs Array/Slice
  full modeling), tuple/range/method-call typing, `Path([X, Y])` typing
  for nominal types and ADT constructors, generic instantiation with HM
  unification, trait satisfaction (§5.3).

### Added — Phase 1 type checker slice 1: literal-type inference + primitive expression typing (2026-05-01)

- `clifford-types`: first real implementation. Public entry point
  `infer(&Program, &Resolution) -> Result<Typing, Vec<TypeError>>`.
  Walks every `@fn` / `#effect` / `#interrupt` / `#transition` body and
  assigns a `Type` to each expression node, recording the result in
  `Typing.types: HashMap<Span, Type>`.
- New types: `Type` (`Unit` / `Primitive(PrimitiveType)` / `StringSlice` /
  `Unknown(reason)`), `Typing`, `TypeError`. `Type` carries display,
  numeric-classification, and unknown-detection helpers.
- Literal typing with suffix recognition: integer literals default to `i32`
  but honor `u8` / `u16` / `u32` / `u64` / `usize` / `i8` / `i16` / `i32` /
  `i64` / `isize` suffixes; hex/binary literals share the integer suffix
  rules; float literals default to `f64`, honor `f32`. Char/byte/string/
  bool/null literals get their canonical types.
- Path resolution to primitive types via the resolver's local-binding info:
  parameters carry their declared types; `let`-bindings use the annotation
  if present, otherwise the initializer's inferred type; `let :=` short
  bindings use the initializer's type.
- Unary operator typing per §4: `-` on numeric, `!` on bool, `~` on integer,
  `*` deferred to slice T2 (needs reference types). Type-mismatches emit
  `E0511`.
- Binary operator typing per §4: arithmetic (`+ - * / %`) on same numeric
  type, comparison (`== != < <= > >=`) returning bool with broad operand
  set, logical (`&& ||`) on bool, bitwise (`& | ^`) on same integer type,
  shift (`<< >>`) returning lhs type. Mismatches emit `E0510`.
- `let name: T = expr;` annotation/initializer compatibility checking
  (E0512); `Unknown` types treated as compatible with anything to avoid
  cascading errors when an upstream type isn't yet computable.
- `as` cast trusts the user-asserted target type (validity check is
  `clifford-check`'s slice 2 work).
- Narrow unsafe primitives type to their type-argument: `#unchecked_load<T>`,
  `#volatile_load<T>`, `#unchecked_cast<S, T>` all return `T`.
- Forward-compat: `Type` enum is not `#[non_exhaustive]` (small closed
  set), but `Type::Unknown(&'static str)` carries deferred-reason strings
  so consumers can produce specific diagnostics about *why* a type is
  unknown rather than treating Unknown as a generic failure.
- 45 unit tests + 1 doctest covering: every literal kind with default and
  suffixed forms, path-via-local typing, all unary forms (positive +
  mismatch), all binary categories (positive + mismatch), cast,
  let-annotation match/mismatch, unknown-initializer-doesn't-spuriously-
  error, narrow unsafe primitive return types, multiple-error collection,
  realistic 2-item program, and Type::display formatting.

### Added — Phase 1 resolver slice 3: transitions, Self, ProcCall, field validation (2026-05-01)

- `clifford-resolve`: walks `#automaton.transitions[].body` with the
  enclosing automaton in context. `Self` resolves to a new
  `BindingRef::SelfRef { automaton }` variant; `Self.field` validates
  against the automaton's declared fields and records a
  `BindingRef::AutomatonField { automaton, field_name }` binding.
- `Auto.field` field-access in expression position validates the field
  against the automaton's declared fields when the receiver resolves to
  an `#automaton` symbol. Same `BindingRef::AutomatonField` shape.
- `#mutate Auto { field = … }` and `Auto.field <op>= …` mutation sugar
  validate the field name against the automaton's fields. Field-validation
  is suppressed when the automaton itself is undefined (avoids redundant
  `E0405 + E0403` noise).
- `#> proc(args)` callee resolution with `CallContext` tagging per
  Refinement #5b: `Identity` (callee is a top-level `#effect`) /
  `Transition` (callee is a `#transition` of an automaton in `#mutates`
  scope, or a sibling transition of the enclosing transition's automaton).
  Records a `BindingRef::Proc { name, target_span, ctx }`.
- New errors: `E0404 UnknownProc` (proc name not an effect or transition
  in scope), `E0405 UnknownField` (field name not on the named automaton).
- `Symbol` gains `name: String` so consumers holding a `Symbol` (e.g. inside
  `BindingRef::SelfRef` or `BindingRef::AutomatonField`) can recover the
  original identifier without reverse-iterating the symbol table.
- `BindingRef` is now `#[non_exhaustive]` (forward-compat for
  Generic-context proc calls / impl method bodies / module paths).
- 22 new tests covering: Self in transitions, Self outside transitions,
  Self.field validation (positive and unknown-field), Auto.field reads
  (positive and unknown-field), field-access on non-automatons silently
  no-ops, `#mutate` / `MutateShort` field-name validation,
  field-check suppression on undefined automatons, all four ProcCall
  shapes (top-level effect → Identity, transition in mutates scope →
  Transition, sibling transition inside a transition body → Transition,
  unknown proc → E0404, function-as-proc → E0404, transition outside
  mutates scope → E0404), Proc target_span correctness, transition body
  let-bindings, AutomatonField cross-automaton correctness, and a
  realistic 3-item program exercising every slice-3 feature together.
  Total resolver test count: **68 unit + 2 doctests**.

### Added — Phase 1 resolver slice 2: body name resolution (2026-05-01)

- `clifford-resolve`: public entry point `resolve(&Program) -> Result<Resolution, Vec<ResolveError>>`.
  Walks every `@fn` / `#effect` / `#interrupt` body, building a scope chain
  (parameters at the bottom; `let` and `let :=` bindings stacked above), and
  resolves every single-segment `Path([X])` expression to a `BindingRef` —
  either a top-level `Symbol` or a `LocalBinding`.
- New types: `Resolution` (carries `SymbolTable` + `bindings: HashMap<Span, BindingRef>`),
  `BindingRef::{TopLevel, Local}`, `LocalBinding`, `LocalKind::{Param, Let, LetShort}`.
- `Auto@state` reads, `#mutate Auto { … }`, and `Auto.field <op>= …` mutation
  sugar verify their automaton-name component resolves to an `#automaton`
  symbol; mismatches surface as the new `E0403 NotAnAutomaton` error
  (carries the actual kind found, e.g. "function", or `"undefined"`).
- New `E0402 UndefinedName` error for unresolved single-segment names in
  expression position.
- Locals shadow top-level symbols (a `let helper := …` inside a function
  hides the global `@fn helper` for the rest of the block). `let x = x + 1`
  references the *outer* `x` on the RHS — initializer is walked before the
  binding is declared.
- `#> proc(args)` walks its arguments but does not resolve the proc name
  itself (that's slice 3 work alongside CallContext tagging per Refinement #5b).
- 25 new tests + 1 new doctest covering: param/let/let-short resolution,
  mutability + type-annotation tracking, outer-binding-on-let-RHS semantics,
  shadowing, undefined-name errors collected (not fail-fast),
  `#mutate` / `Auto.field <op>=` / `Auto@state` automaton verification
  including wrong-kind diagnostics, scope-chain depth (3-let chain),
  recursion through Binary/Index/Call/ArrayRepeat/Unsafe-load expressions,
  proc-call argument walking, mixed slice-1+slice-2 error reporting, and a
  realistic 3-item program. Total resolver test count: **46 unit + 2 doctests**.

### Added — Phase 1 resolver slice 1: top-level SymbolTable (2026-05-01)

- `clifford-resolve`: first real implementation. `SymbolTable::build(&Program)`
  walks every top-level item and produces a global namespace mapping
  identifier → `Symbol { kind, item_index, layer, span }`. Detects duplicate
  declarations (E0401), collecting all errors rather than failing at the
  first.
- New types: `SymbolKind` (`Fn` / `Type` / `Trait` / `Automaton` / `Effect` /
  `Interrupt` / `Interface`), `Symbol`, `SymbolTable`, `ResolveError`.
- `SymbolTable::build_partial` returns a (possibly partial) table alongside
  any errors so IDE-style consumers can keep resolving past a duplicate-name
  conflict. First-declaration wins for resolution purposes.
- `@sequential`, `#impl`, and `#test` declarations do not populate the table
  (no resolvable name; impl coherence and test discovery happen in later
  slices).
- 21 unit tests + 1 doctest covering: empty programs, every named item kind,
  item-index correspondence to source order, layer derivation, exclusion of
  nameless items, single duplicates, cross-kind duplicates (single global
  namespace per Decision #1), three-way duplicates (N-1 errors), partial
  table reconstruction past errors, multi-impl / multi-test / multi-sequential
  coexistence, and a realistic 10-item program end-to-end.

### Added — Phase 0 parser slice 8: automaton members (2026-05-01)

- `clifford-ast`: `AutomatonDecl` extended with `address: Option<AddressClause>`
  (Decision #6 register-block annotation), `basis: Option<BasisClause>`
  (Decision #4 explicit GA basis assignment), `states: Option<Vec<StateName>>`
  (Decision #5; `None` = monoid), `fields: Vec<AutomatonField>`,
  `transitions: Vec<TransitionDecl>`.
- New AST types: `AddressClause`, `BasisClause`, `StateName` (each with
  per-element span tracking), `AutomatonField` with optional `#offset` /
  `#access` field-meta clauses, `AccessMode` (`Read` / `Write` /
  `ReadWrite`), `TransitionDecl` with optional destination state and a
  full `Block` body (Refinement #5b).
- `clifford-parser`: full automaton body parsing — dispatch on the leading
  token of each member (`#address` / `#basis` / `#states` / `#transition` /
  identifier-for-field), with members allowed in any order. `#offset`
  and `#access` field-meta clauses likewise allowed in either order.
- New parser errors: `E0210 DuplicateClause` (rejects double `#address` /
  `#basis` / `#states` / `#offset` / `#access`) and `E0211 EmptyStatesList`
  (rejects `#states: []` since a multi-state automaton with zero states
  is nonsense; use no `#states` clause for a monoid).
- `clifford-parser`: 30 new tests covering every member kind, field metadata
  in both orders, all three access modes, mixed-member ordering, duplicate-
  clause rejection, hex-literal validation, plus realistic register-block
  and multi-state state-machine fixtures. All up: **205 parser+AST tests
  passing**.
- The realistic test fixture `realistic_register_block_automaton` parses a
  three-register UART peripheral with `#address`, `#basis`, three fields
  with full `#offset` + `#access` metadata and three distinct access modes.
  `realistic_multistate_automaton` parses a Counter with three states and
  three named transitions exercising both same-state (`#transition tick`)
  and cross-state (`#transition start -> Counting`) forms.

### Added — Phase 0 parser slice 7: function/effect/interrupt bodies (2026-05-01)

- `clifford-ast`: full `Expr` / `ExprKind` covering §2.6 — literals (int/hex/bin/
  float/char/byte/string/bool/null), paths, `Auto@state` reads, parenthesised
  expressions, tuples, array literals, array-repeat literals, postfix
  `.field` / `.method(args)` / `[index]` / `(args)`, prefix unary
  (`-`, `!`, `~`, `*`), borrows (`&`, `&mut`), full binary operator set,
  `as` casts, `..` / `..=` ranges, and the narrow unsafe expressions
  (`#unchecked_load`, `#volatile_load`, `#unchecked_cast`, `#unchecked_offset`).
- `clifford-ast`: `Stmt` / `StmtKind` for `let` / `let mut` / `let x := …`,
  `return`, `#mutate Auto { … }`, `Auto.field <op>= …` (Decision #15 sugar
  with all 11 compound-assignment operators), `#> proc(args)`, and the
  unsafe-store primitives.
- `clifford-ast`: `Block { stmts, span }` wired into `FnDecl`, `EffectDecl`,
  and `InterruptDecl`.
- `clifford-parser`: Pratt-style expression parser with binding-power table
  (range 1, `||` 3/4, `&&` 5/6, comparisons 7/8, bitwise `|` 9/10 / `^` 11/12 /
  `&` 13/14, shifts 15/16, +/- 17/18, */// 19/20, `as` 23, unary 25);
  recursive-descent statement parser with multi-token lookahead for the
  `Auto.field <op>= …` sugar; public `parse_expression` entry point;
  `parse_block` wired into all three declaration parsers.
- `clifford-parser`: 72 new tests covering atoms, postfix chains,
  precedence (mul-over-add, left-associative, comparison-below-arith,
  bitwise hierarchy, shift-vs-add, paren overrides), unary, borrows,
  cast, ranges, narrow unsafe primitives (including the non-empty-reason
  rejection per Refinement #19a), every statement form including all 11
  compound-assignment operators, body wiring through `@fn` / `#effect` /
  `#interrupt`, and a realistic 11-item program exercising every Phase-0
  surface end-to-end.

### Added — Phase 0 bootstrap (2026-04-30)

- Cargo workspace skeleton at the project root; rust-toolchain pinned to 1.76.0.
- Empty crates for the full pipeline: `lexer`, `parser`, `ast`, `resolve`,
  `types`, `check`, `effect`, `ortho`, `codegen`, `stdlib`, `cli`.
- Project meta: `README.md`, `CHANGELOG.md`, `LICENSE-MIT`, `LICENSE-APACHE`,
  `.gitignore`, basic `.github/` workflows and templates.
- Documentation supplementary files: `docs/architecture.md`,
  `docs/adr/0001-rust-as-implementation-language.md`.

## Spec Changes

### v0.6.0-draft (2026-05-01) — Decision #21: shared automata via mutator multivectors ✓ LOCKED

ADR 0002 accepted. The orthogonality engine's algebra is documented as the
*restricted form* Cl(0,0,n) for v0.1–v0.6, with the *full form* mixed-metric
Cl(p,0,n) extension reserved for v0.7+. New §7.0 prologue and §7.9
extension sketch added to the spec.

- **Algebra:** v0.7+ extends to mixed-metric Cl(p,0,n). Private fields contribute null basis vectors (current behavior); shared fields contribute non-null basis vectors that don't collapse the wedge product. Overlap on a shared basis vector generates a separate proof obligation: lock coverage.
- **Lock as multivector:** each lock `L` is a mixed-grade multivector `lock(L) = pri(L) + e_L` (scalar priority + identity basis vector). The lock-context multivector held by an executing automaton is the wedge of every held lock.
- **Acquisition validity is algebraic:** ascending priority is canonical wedge; descending is Koszul-flippable; equal-priority falls through to a deterministic GA *rotor* parameterised by a canonical structural attribute (MMIO `#address` for register-block locks; `#rotor:` clause / link-section position / source-location hash for software locks).
- **Theorem (sketched):** lock-context multivector never collapses to zero ⟺ execution is deadlock-free. Lock-ordering safety falls out of the algebra; no separate procedural checker.
- **Interrupts and locks unify:** a `#interrupt #priority: N { … }` is a priority-ordered acquisition; the algebra handles both interrupt and lock concurrency with the same machinery.
- **Phase-1 scaffolding (lands now):** `crates/ast` adds `FieldKind` enum on `AutomatonField` (one variant `Private`, marked `#[non_exhaustive]`); `crates/lexer` reserves `#shared`, `#lock`, `#with_lock`, `#reads`, `#rotor` tokens; `docs/DECISIONS.md` adds Decision #21 LOCKED entry; `docs/CLIFFORD_SPEC.md` adds §7.0 prologue and §7.9 extension sketch. No engine changes; v0.7 implementation work is gated on Phase 0–4 closing.

### v0.5.0-draft (2026-04-30) — Decision #19: nominal access types

- `*const T` / `*mut T` retired in favor of `access<T>` / `access const<T>`.
- Each `@type` declaration of an access type produces a distinct nominal type.
- Cross-type pointer use requires explicit `#unchecked_cast<S, T>`.
- New narrow primitive `#unchecked_offset<T>(p, n)` for pointer arithmetic.

### v0.4.0-draft (2026-04-30) — Decisions #6–#18

- **#6**: register blocks as `#automaton` with `#address`/`#offset`/`#access`;
  `#hardware` retired.
- **#7**: `#test "name" { … }` testing primitive.
- **#8**: `:=` short binding for type-inferred immutable locals.
- **#9**: dropped `#visible` / `#hidden` (subsumed into `#mutates`/`#cannot_mutate`).
- **#10**: `#interrupt` resolves by linker symbol.
- **#11**: `@sequential(A, B)` non-concurrency assertion attribute.
- **#12 (deferred to v0.2)**: `#staged` automata for deferred mutation.
- **#13**: body-scoped references with provenance tracking + Rule 0
  (no `&mut` to automaton fields). Catches UAF cases 1–5 without lifetime
  annotations.
- **#14**: sigma loops with bounds tracking as primary iteration construct.
- **#15**: `Auto.field <op>= expr` sugar for single-field `#mutate`.
- **#16**: `#interface` + `#impl` + monomorphization for plugin mutators.
- **#17**: Ada-style narrow unsafe primitives; `#unsafe { … }` block retired.
- **#18 (deferred to v0.2)**: `#audit` runtime auditing of unsafe primitives.

### v0.3.0-draft (2026-04-30) — Decision #5: automaton-as-category

- Every `#automaton` is a small category; state changes happen exclusively
  inside named `#transition` blocks; effects are top-level (Refinement #5a).
- New §5.7 reference provenance, §5.8 sigma bounds tracking, Appendix B
  categorical semantics.

### v0.2.0-draft (2026-04-30) — Decisions #1–#4 reconciliation

- Reconciliation between earlier drafts and `DECISIONS.md` Decisions #1–#4.
- Sigil layering (`#`, `@`, `$`, `#>`) becomes structural.
- Hybrid `$ [TraitList]` markers; named effect procedures with `#>`;
  auto-assigned GA basis vectors.

### v0.1.0-draft (2026-04-29)

- Initial draft of the spec under the former name (Ferrum); renamed to Clifford
  alongside the move to GA orthogonality.
