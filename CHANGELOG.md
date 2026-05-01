# Changelog

All notable changes to Clifford and `cliffordc` are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/) — pre-1.0 minor versions
may include breaking changes.

## [Unreleased]

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
