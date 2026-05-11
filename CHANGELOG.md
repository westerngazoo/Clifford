# Changelog

All notable changes to Clifford and `cliffordc` are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/) — pre-1.0 minor versions
may include breaking changes.

## [Unreleased]

### Added — Slice 22: audit markers extended to effects + interrupts (2026-05-09)

Closes the soundness gap explicitly noted in slice 21: an
`#effect` or `#interrupt` whose `#mutates: [...]` clause
names an audited automaton now emits the same
`; audit-wrap site for <Owner> (<primitive>) ; Decision #18`
IR markers at every unsafe-primitive site in its body. The
slice-21 transition-only marker placement is retained
unchanged; slice 22 just expands the set of callable kinds
that participate.

The marker text names the **first** audited automaton in
source order from the `#mutates: [...]` list. The marker is
a wiring signpost — the future instrumentation pass that
turns markers into real `PointerAuditor` dispatch will
consult the AST's full `#mutates: [...]` list to determine
which Sanitizer instances to call into. The marker only
needs to be present at the wrap site.

**Codegen changes (`crates/codegen/src/lib.rs`):**

- New `first_audited_in_mutates(mutates: &[String]) -> Option<String>`
  helper iterates a `#mutates: [...]` list in source order
  and returns the first automaton whose `AutomatonInfo` has
  `is_audited == true`.
- `emit_effect` and `emit_interrupt` now set
  `current_audited_owner = self.first_audited_in_mutates(&decl.mutates)`
  immediately after `reset_per_function_state`. Cleared by
  the next callable's reset, so non-audit effects /
  interrupts that follow an audit one in source order do
  not inherit the marker.

**Tests:**

- The slice-21 negative test
  `s21_audit_marker_does_not_appear_in_effects` documented
  the future-slice extension and is now obsolete; replaced
  by **6 new positive slice-22 tests**:
  - `s22_audit_marker_appears_in_effect_targeting_audit_automaton`
  - `s22_effect_without_audited_mutates_emits_no_marker`
  - `s22_effect_with_empty_mutates_emits_no_marker`
  - `s22_effect_with_mixed_mutates_uses_first_audited_owner`
  - `s22_audit_marker_appears_in_interrupt_targeting_audit_automaton`
  - `s22_audit_context_clears_between_effects`

**Sample:** `examples/audit_marker_demo.cl` extended with
two effects and one interrupt — `poke_audited_via_effect`
(2 markers), `poke_plain_via_effect` (zero), and
`SysTick` (2 markers) — verifying the propagation rule
end-to-end through the cliffordc CLI.

**Deliberately NOT in slice 22:**

- **Multiple audited owners.** When an effect mutates two
  audited automata, only the first appears in the marker
  text. The actual wrap pass (future slice) consults the
  AST for the full list; the marker is just a stable
  signpost at the IR-instruction site.
- **Still no actual `PointerAuditor` calls.** Slices 18,
  20, 21, 22 stand up the surface, AST, codegen markers,
  and propagation rules. The stdlib interface, default
  `ShadowSanitizer` impl, and call-emitting pass remain
  in subsequent slices.

### Added — Slice 21: `#audit` codegen markers at unsafe-primitive sites (2026-05-09)

Wires the slice-20 `#audit` modifier through to codegen.
Every unsafe-primitive emission site
(`#unchecked_load` / `#unchecked_store` /
`#unchecked_offset` / `#unchecked_cast` /
`#volatile_load` / `#volatile_store`) inside an
`#audit`-marked automaton's transition body now emits a
`; audit-wrap site for <Owner> (<primitive>) ; Decision #18`
IR comment immediately before the instruction. The marker
gives the future `PointerAuditor` instrumentation pass a
stable injection point without requiring further emitter
changes; release builds elide the marker trivially (LLVM
strips IR comments during parsing).

**Slice 21 is the marker only.** No actual `PointerAuditor`
calls are emitted — the marker is purely a wiring signpost.
The `PointerAuditor` interface, the default
`ShadowSanitizer` impl, and the wrap-emitting pass land in
subsequent slices once the stdlib has the runtime helpers.

**Codegen changes (`crates/codegen/src/lib.rs`):**

- `AutomatonInfo` gains `is_audited: bool` (mirrors
  slice-18's `is_staged`). Populated in pass 1 from
  `decl.audited`.
- `Emitter` gains `current_audited_owner: Option<String>`,
  set at the top of `emit_transition` iff the owning
  automaton's info has `is_audited == true`. Cleared by
  `reset_per_function_state` so effects, interrupts, and
  `@fn`s start fresh.
- New `emit_audit_marker_if_needed(kind: &str)` helper —
  the only behavioural change in the per-primitive emit
  functions. Each of `emit_unchecked_load`,
  `emit_unchecked_store`, `emit_unchecked_cast`,
  `emit_unchecked_offset` calls it with the appropriate
  category string. Same-IR-type `#unchecked_cast` is a
  no-op and intentionally skips the marker (nothing to
  wrap).

**Tests added (9 codegen):**

- `s21_unaudited_transition_emits_no_audit_marker`
- `s21_audited_transition_unchecked_store_emits_marker`
- `s21_audited_transition_unchecked_cast_emits_marker`
- `s21_audited_transition_volatile_store_emits_marker`
- `s21_audited_transition_unchecked_offset_emits_marker`
- `s21_unchecked_load_emits_marker`
- `s21_audit_marker_does_not_leak_across_transitions`
- `s21_audit_marker_does_not_appear_in_effects`
- `s21_audit_marker_composes_with_staged_writes`

**Sample:** `examples/audit_marker_demo.cl` — three
automata side by side: `AuditedRing` (4 markers, one per
primitive), `PlainRing` (zero markers, byte-identical
slice-20 IR), and `AuditedStaged` (markers + shadow global,
demonstrating slices 18 and 21 compose cleanly).

**Deliberately NOT in slice 21:**

- **Effects / interrupts that target audited automatons.**
  Slice 21 marks only the audited automaton's *transition*
  bodies. An effect with `#mutates: [P]` where `P` is
  audited does **not** get markers in its body. Extending
  to effects requires per-primitive lookup of every
  mutated automaton's audit flag — straightforward but
  defers to a follow-up slice once the transition-body
  case is exercised in real firmware.
- **No `PointerAuditor` interface.** Lives in stdlib
  (Decision #16 substrate); requires the `.cl` source tree
  scaffold to grow first.
- **No `ShadowSanitizer` default impl.** Same dependency.
- **No actual call emission.** The marker is the wiring
  signpost; turning it into a real call is a separate
  pass once the stdlib interface lands.

### Added — Slice 20: `#audit` automaton modifier surface (Decision #18) (2026-05-09)

Lands the AST + parser surface for **Decision #18** (runtime
auditing of unsafe primitives), previously listed as
"designed; deferred to v0.2." User code can now opt an
automaton into runtime instrumentation by writing
`#audit #automaton Foo { … }`. The instrumentation itself
(wrapping every `#unchecked_*` / `#volatile_*` /
`#unchecked_cast` with `PointerAuditor` calls in debug
builds) is deliberately not in scope for slice 20 — the
codegen pass and the `clifford::audit::ShadowSanitizer`
default impl land in subsequent slices once the stdlib has
the runtime helpers.

The two prefix modifiers `#staged` (slice 18) and `#audit`
(slice 20) compose in **any order** — `#staged #audit
#automaton …` and `#audit #staged #automaton …` produce
identical ASTs. Each modifier may appear at most once per
declaration; duplicates are a parse error.

**Surface syntax:**

```clifford
#audit #automaton Buf { storage: [u8; 64]; }
#audit #staged #automaton Pose { x: i32; y: i32; }
#staged #audit #automaton Pose2 { x: i32; }   // identical to above
```

**Pipeline changes:**

- **AST (`crates/ast/src/lib.rs`):** new `audited: bool`
  field on `AutomatonDecl`. Documented to indicate the
  surface-only scope of slice 20 (no codegen wrapping yet).
- **Lexer:** already had `KwHashAudit` reserved (per the
  spec's "reserved keywords for v0.2" list); slice 20 just
  consumes it.
- **Parser (`crates/parser/src/lib.rs`):**
  - `KwHashStaged` and `KwHashAudit` arms in `parse_item`
    both dispatch to a new `parse_prefixed_automaton`
    helper that runs a small state machine consuming any
    permutation of the two modifiers, rejecting
    duplicates, then handing off to `parse_automaton_decl`
    with both flags resolved.
  - `parse_automaton_decl` now takes `audited: bool`
    alongside `staged: bool` and threads it onto the AST.
  - The slice-18 `#staged @fn …` rejection still works
    (and now uses the unified diagnostic pointing at both
    Decisions #12 and #18).
- **No resolver, codegen, or sample changes.** Programs
  with `#audit` automata compile through to the same IR
  as their non-`#audit` siblings; the AST round-trips the
  flag for downstream consumption when the codegen pass
  lands.

**Tests added (7 parser):**

- `unprefixed_automaton_is_not_audited`
- `audited_automaton_sets_flag`
- `audit_then_staged_composes`
- `staged_then_audit_composes_identically`
- `duplicate_audit_modifier_errors`
- `duplicate_staged_modifier_errors`
- `audit_without_automaton_errors`

**Deliberately NOT in slice 20:**

- **No codegen wrapping.** Future slice 21+ will (a)
  define the `PointerAuditor` interface in the stdlib,
  (b) ship a default `ShadowSanitizer` impl, and (c)
  wire codegen to wrap unsafe primitives in
  `#audit`-marked automata's bodies during debug builds.
- **No instrumentation profile.** The §6.2 mutation-
  profile check sees `#audit` automata identically to
  their non-`#audit` siblings — runtime instrumentation
  doesn't change which fields are read or written
  semantically.

### Added — Slice 19: `#flush` flows into the mutation profile (2026-05-09)

Closes the deliberately-deferred soundness gap from slice 18:
the §6.2 mutation-profile check now treats `#flush A;` as a
write to **every declared field of `A`** so the existing
`#mutates: [...]` validator fires uniformly. The §7
orthogonality engine sees the same expanded write set, so a
flush + a field-write to the same staged automaton is now
detected as a race (previously the flush was invisible to the
verifier and could quietly conflict with concurrent writers).

**Effect crate (`crates/effect/src/lib.rs`):**

- New `build_automaton_fields(program)` indexes every
  `#automaton`'s declared field names. Built once per
  `extract_mutation_profiles` invocation and threaded
  through `walk_body_for_direct` /
  `walk_transition_for_direct` / `walk_stmts`.
- New `Flush { automaton }` arm in `walk_stmts` looks up the
  target's field set and inserts one `FieldRef { automaton,
  field }` per declared field into `out.writes`. If the
  target doesn't resolve to any automaton (resolver-side
  E0413), the lookup misses and nothing is recorded — the
  resolver's error is the user's signal.
- The downstream `actual_automata` derivation
  (`actual_writes → automaton names`) and
  `validate_declared_mutates` (E0410 / E0411) work
  unchanged: routing flush writes through `actual_writes`
  is enough to make the existing checks fire correctly.

**Tests added (4):**

- `flush_records_one_write_per_field` — confirms a flush
  records exactly one direct write per declared field of
  the target staged automaton.
- `flush_outside_mutates_clause_is_e0410` — surfaces E0410
  `EffectMutatesUndeclaredAutomaton` on a flush whose
  enclosing callable omits the target from
  `#mutates: [...]`.
- `flush_inside_mutates_clause_is_well_formed` — happy
  path; no errors.
- `flush_transitively_propagates_through_proc_call` —
  confirms the transitive-closure pass picks up flushes
  from `#>`-called callees.

**No surface, codegen, or sample changes.** Slice 18's IR
output is byte-identical; the only observable difference is
that programs that previously *quietly* compiled an
unsound `#flush` outside `#mutates: [...]` now correctly
surface E0410.

### Added — Slice 18: `#staged` automaton + `#flush` (Decision #12) (2026-05-09)

Implements **Decision #12** (deferred-mutation automata),
previously listed as "designed; deferred to v0.2." A
`#staged #automaton Foo { … }` modifier allocates an
additional shadow global of identical type; every `#mutate
Foo { … }` and `Foo.field <op>= …` write inside any
callable is redirected to the shadow; an explicit
`#flush Foo;` statement commits the shadow into live state
via a single `llvm.memcpy`. Reads (`Foo.field`,
`@snapshot Foo.field`) continue to come from live state, so
no consumer ever observes a half-built update.

The orthogonality engine is **untouched**: `#staged` only
changes *when* writes become observable, not *which fields*
a callable touches. Per Decision #12, "the GA engine treats
`#staged` automata identically to non-staged ones for
orthogonality (timing doesn't change field overlap)."

**Surface syntax:**

```clifford
#staged #automaton Pose { x: i32; y: i32; theta: i32; }

#interrupt EncoderTick() #mutates: [Pose] #priority: HIGH
  #atomic: interrupt_critical;
{
  #mutate Pose { x = 100i32, y = 200i32, theta = 45i32 };
  #flush Pose;          // commit shadow → live atomically
  return;
}

@fn read_x() -> i32 $ [Readable] {
  return @snapshot Pose.x;   // always sees the most recent commit
}
```

**Pipeline changes:**

- **AST (`crates/ast/src/lib.rs`):** new `staged: bool`
  field on `AutomatonDecl`; new
  `StmtKind::Flush { automaton: String }` variant.
- **Lexer:** already had `KwHashStaged` and `KwHashFlush`
  reserved (the spec called them "reserved keywords for
  v0.2"); slice 18 just consumes them.
- **Parser (`crates/parser/src/lib.rs`):** new
  `KwHashStaged` arm in `parse_item` that consumes the
  modifier and dispatches to `parse_automaton_decl(start,
  /*staged=*/ true)`. The `#staged` keyword is reserved
  for `#automaton` only — any other follower is a parse
  error citing Decision #12. New `parse_flush_stmt`
  consumes `#flush <Ident> ;` from `parse_stmt`. Six new
  parser tests cover staged-on / staged-off, the
  `#staged` + non-`#automaton` rejection, and the three
  `#flush` shapes.
- **Resolver (`crates/resolve/src/lib.rs`):** new
  `staged: HashMap<String, bool>` side table on
  `AutomatonMeta`; new `lookup_automaton_staged` helper.
  `StmtKind::Flush` resolution emits **E0412
  `FlushOnNonStaged`** if the target exists but isn't
  `#staged`, **E0413 `FlushOfUnknownAutomaton`** if the
  target doesn't resolve to any automaton (distinct from
  E0412 so users can disambiguate "I forgot the modifier"
  from "I typoed the name"). Five new resolver tests.
- **Codegen (`crates/codegen/src/lib.rs`):**
  - `AutomatonInfo` gains an `is_staged: bool` field and
    a `write_global()` helper that returns either
    `@<Name>.shadow` (staged) or `@<Name>.state` (default).
  - `emit_automaton_state_structs` emits the second
    `@<Name>.shadow = global %struct.<Name>
    zeroinitializer` global for staged automata only.
  - The three write paths (`emit_field_store`,
    `emit_indexed_field_store`, the transition-exit
    state-tag write in `emit_exit_fence_if_pending`)
    consult `write_global()` so writes are correctly
    routed to the shadow. Reads continue to use
    `@<Name>.state` unconditionally — the shadow is for
    pending writes only.
  - New `emit_staged_intrinsics_if_needed` emits
    `declare void @llvm.memcpy.p0.p0.i64(i8*, i8*, i64,
    i1)` once at module scope iff the program contains at
    least one `#staged` automaton (so non-staged programs
    are byte-identical to pre-slice-18 output).
  - New `emit_flush(automaton)` lowers `#flush Name;` to
    bitcasts of both globals to `i8*`, computes the
    struct size via the GEP-on-null idiom (target-pointer-
    width-agnostic), then issues the `llvm.memcpy` call.
    Eight new codegen tests cover shadow-emission,
    write-redirection, read-from-live preservation,
    flush-memcpy shape, multi-state tag-write redirection,
    and the no-staged → no-intrinsic invariant.

**Sample:** `examples/staged_pose_handoff.cl` — the
canonical "ISR builds up a complete pose update under
`#atomic: interrupt_critical;`, then flushes" pattern. The
generated IR shows two ISRs writing into `@Pose.shadow`,
the `cpsid i` / `cpsie i` mask wrapping the bodies, the
`llvm.memcpy` flush call, and three `@fn`-side
`@snapshot Pose.{x,y,theta}` readers reading from
`@Pose.state`. A non-staged sibling automaton (`Counter`)
in the same file demonstrates that the codegen leaves
unstaged automata's IR unchanged.

**What's deliberately NOT in this slice:**

- **No `#flush` profile check.** The resolver doesn't yet
  enforce that `#flush A;` requires `A` in the enclosing
  callable's `#mutates: [...]` profile. The existing
  effect-checker enforces this for `#mutate`; extending
  the same check to `#flush` is a one-line follow-up
  slice once the helper is reused.
- **No reads-from-shadow.** v0.2 always reads from live.
  A `@shadow Pose.x` operator that explicitly reads the
  pending value (useful for "did the ISR start an
  update?" patterns) is a future slice if a real firmware
  pattern demands it.
- **No partial flushes.** `#flush Pose;` commits the
  whole struct. A `#flush Pose { x, y };` per-field form
  is a possible future surface but the v0.2 firmware
  patterns don't seem to need it.
- **Register-block automata can't be `#staged`** at
  codegen time — the combination falls back to direct
  MMIO (no shadow makes sense for memory-mapped
  hardware). A future parse-time error can lift this to
  an explicit rejection if firmware patterns prove the
  combination is always wrong.

### Added — Slice 17: `break` and `continue` for `sigma` loops (2026-05-09)

Two new statement keywords let a sigma-loop body short-circuit
or skip an iteration, closing the firmware ergonomics gap of
"early-exit when a buffer position is reached" patterns
without forcing a sentinel-encoded re-roll. Both keywords are
unit statements (`break;`, `continue;`) and apply to the
**innermost enclosing** sigma loop.

**Surface syntax:**

```clifford
@fn first_index_past(n: u32, threshold: u32) -> u32 {
  let mut found: u32 = n;
  sigma i in 0u32..n {
    if i > threshold { found = i; break; }
  }
  return found;
}

@fn count_skipping_low(n: u32, low: u32) -> u32 {
  let mut kept: u32 = 0u32;
  sigma i in 0u32..n {
    if i <= low { continue; }
    kept = kept + 1u32;
  }
  return kept;
}
```

**Pipeline changes:**

- **AST (`crates/ast/src/lib.rs`):** new
  `StmtKind::Break` and `StmtKind::Continue` unit variants
  alongside `Sigma`. Span is the keyword's source location;
  the parser does not store a target label (no labelled
  loops in v0.2).
- **Parser (`crates/parser/src/lib.rs`):**
  `parse_break_stmt` / `parse_continue_stmt` consume the
  keyword + `;`. Dispatched from `parse_stmt` on `KwBreak` /
  `KwContinue`. Four parser tests cover happy-path, sigma
  body, and the missing-semicolon error.
- **Resolver (`crates/resolve/src/lib.rs`):** new
  `loop_depth: u32` Walker field. `StmtKind::Sigma` walking
  increments the depth around the body and decrements after.
  `Break` / `Continue` arms reject `loop_depth == 0` with
  **E0411 `KeywordOutsideLoop`**, citing the keyword by name
  in the error message.
- **Codegen (`crates/codegen/src/lib.rs`):** sigma's emitter
  now produces a **four-block CFG** — `header / body /
  continue / exit` — with the iteration-variable increment
  living in the new `sigma.continue.<id>` block. The phi's
  body-incoming label is `%sigma.continue.<id>` (was
  `%sigma.body.<id>`), giving `continue;` a clean back-edge
  target without re-emitting the increment. A new
  `sigma_loop_stack: Vec<(continue_label, exit_label)>`
  field on `Emitter` is pushed on sigma entry / popped on
  exit; `Break` / `Continue` arms read the top of the stack
  and emit `br label %sigma.exit.<id>` or `br label
  %sigma.continue.<id>` respectively, then mark
  `current_block_terminated = true`. Body-statement
  iteration switched from a `Return(_)`-only check to
  reading `current_block_terminated` so any terminator
  (return / break / continue) suppresses dead-code
  emission. Same fix applied pre-emptively to `emit_if`'s
  then-block and else-block loops.

**Tests added (8 codegen + 4 parser + 1 resolver):**

- `break_emits_branch_to_sigma_exit_label`
- `continue_emits_branch_to_sigma_continue_label`
- `break_in_nested_sigma_targets_innermost_loop`
- `break_inside_if_inside_sigma_targets_loop_not_if`
- `break_outside_sigma_rejected_by_resolver` (E0411)
- `continue_outside_sigma_rejected_by_resolver` (E0411)
- `break_with_local_mut_acc_can_early_exit`
- `body_after_break_is_dead_code`
- Parser: `break_stmt_parses_as_unit`,
  `continue_stmt_parses_as_unit`,
  `break_inside_sigma_body_parses`,
  `break_without_semicolon_errors`
- The existing
  `s11_sigma_basic_half_open_emits_loop_cfg` was updated to
  expect the slice-17 four-block CFG (phi reads from
  `%sigma.continue.<id>`, new `sigma.continue.<id>:` label).

**Sample:** `examples/buffer_init_sigma.cl` extended with
two `@fn`s — `first_index_past` (break) and
`count_skipping_low` (continue) — that compile through to
LLVM IR with the expected branches.

**What's deliberately NOT in this slice:**

- **No labelled loops.** Both keywords always target the
  innermost loop. A labelled syntax (`'outer: sigma …`,
  `break 'outer;`) is a future slice if the firmware
  patterns warrant it.
- **No `break <expr>;`.** Sigma loops are pure iteration in
  v0.2; they have no value. A loop-as-expression form
  belongs to a different design conversation.
- **No effect-side checks.** Break/continue are local
  control-flow inside one effect/transition body; they
  don't change the function's mutation profile or interact
  with the ortho engine.

### Added — `clifford-ortho` v0.2-θ: transition-side `#atomic` inheritance (delegated-ISR pattern) (2026-05-08)

The verifier now recognises the canonical "delegated ISR"
shape: an interrupt (or effect) whose body is exactly one
`#>` call to a `#atomic: interrupt_critical;` callee
**inherits atomicity** from that callee. The pair-check
treats the inheriting caller as if it were directly
`#atomic`, suppressing pairs against other interrupts.

Closes the v0.2-δ documented gap that transition-side
`#atomic` was parsed but not consumed by the verifier. The
runtime side (codegen v0.2-ε) was already correct — calling
into the atomic body emits `cpsid i` / `cpsie i`. v0.2-θ makes
the verifier acknowledge that.

**The pattern that now works:**

```clifford
#automaton C { v: u32; w: u32;
  #transition handle #atomic: interrupt_critical; {
    C.v = 1u32;
    C.w = 2u32;
  }
}

#interrupt SysTick() #mutates: [C] #priority: HIGH {
  #> handle();   // body = exactly one #> call → inherits atomic
}

#effect drain() -> u32 #mutates: [C] {
  let _x: u32 = C.v + C.w;  // would race pre-v0.2-θ; safe now
  return _x;
}
```

**The inheritance rule (deliberately strict):**

- The caller's body must consist of **exactly one** statement.
- That statement must be a `#> name();` proc-call to a callee
  declared `#atomic: interrupt_critical;`.
- A trailing `return;` (no value) is permitted and filtered
  out before the count.

Anything else — direct mutation, non-atomic call,
let-binding that reads automaton state, `if`/`sigma` block —
prevents inheritance. Those statements could expose racy
operations BEFORE the atomic callee enters its masked region.
The strict rule keeps the soundness claim tight; richer
inheritance (multi-statement bodies, atomic-on-call-site
rather than per-callable) is a future slice with its own
analysis.

**Implementation (`crates/ortho/src/lib.rs`):**

- New `collect_atomic_callable_names(program) -> HashSet<String>`
  walks every `Item::Effect` / `Item::Interrupt` /
  `Item::Automaton.transitions` and indexes each callable
  declared `#atomic: interrupt_critical;` by name. Effects,
  interrupts, and transitions are recorded uniformly so
  inheritance works whichever kind the callee is.
- New `body_inherits_atomic_from_proc_call(body,
  &atomic_callables) -> bool` filters trailing `return;` and
  asserts the remaining single statement is a `ProcCall`
  whose name is in the atomic-callable set.
- `verify`'s node-collection now ORs the inheritance check
  into the per-node `is_atomic` flag for `Effect` and
  `Interrupt` nodes (only when not already directly atomic;
  no double-counting). Downstream pair-check is unchanged —
  the existing v0.2-δ "if either side is atomic AND the
  other is interrupt → suppress" logic now also fires for
  inherited-atomic.

**Companion behaviour-doc update.**
`docs/ortho-atomic-attribute.md` gains a "Delegated-ISR
inheritance" section explaining the rule, why it's strict,
the worked example, and the implementation refs. The matrix
row for `#atomic transition × #interrupt` flips from `❌
(today)` to `✅ via inheritance (v0.2-θ)`.

**Tests added: 10 new in ortho (64 → 74).**

End-to-end via `verify`:
- `delegated_isr_inherits_atomic_from_transition_callee` —
  the canonical pattern: interrupt body = single `#>` call
  to atomic transition; foreground reader doesn't race.
- `delegated_isr_with_explicit_return_still_inherits` —
  trailing `return;` filtered out.
- `body_with_multiple_statements_does_not_inherit` —
  conservatism: any extra statement kills inheritance.
- `body_calling_non_atomic_transition_does_not_inherit` —
  the callee must itself be `#atomic`.
- `already_atomic_callable_unaffected_by_inheritance_check`
  — direct atomic still wins regardless of body shape.

Direct unit tests on the helpers:
- `body_inherits_atomic_helper_smoke_one_stmt_call`.
- `body_inherits_atomic_helper_returns_false_on_empty_body`.
- `body_inherits_atomic_helper_returns_false_when_callee_not_in_set`.
- `collect_atomic_callable_names_finds_all_three_decl_kinds`
  (effect + interrupt + transition).
- `collect_atomic_callable_names_excludes_non_atomic`.

**Pipeline regression checked.** All 7 examples pass through
v0.2-θ unchanged — none currently use the delegated-ISR
shape, so the inheritance check returns `false` for every
existing callable. The slice is forward-looking: it accepts
patterns that pre-v0.2-θ users would have had to refactor
into directly-`#atomic` callables.

**Deferred:**

- **Multi-statement atomic-block inheritance**: e.g. a body
  like `#> step_one(); #> step_two();` where both callees
  are atomic. Today this fails the one-statement rule even
  though atomicity could plausibly compose. A future slice
  could lift this with careful analysis (the gap between the
  two atomic calls is technically interruptible).
- **Atomic-on-call-site**: a syntax like
  `#> proc() #atomic;` to mark individual call sites
  atomic without changing the callee declaration. Not in
  the current spec.
- **Iterative closure**: `#> foo()` where `foo()` is
  inheritance-atomic from `#> bar()` etc. The strict
  one-hop rule doesn't iterate; richer chains need explicit
  `#atomic` on the leaf.

Total ortho tests: **74** (64 + 10). Workspace clean; clippy
clean. Behaviour doc updated.

### Added — `clifford-ortho` v0.2-η: `#priority`-aware concurrency inference (2026-05-08)

The §7.3 inference now consults each `#interrupt`'s declared
`#priority: …` clause. Two interrupts at the **same** priority
cannot preempt each other on Cortex-M's NVIC (they process via
tail-chaining, no nested vectoring) — so the verifier suppresses
the pair. Different-priority interrupts still take the standard
wedge check.

This **reduces the conservative false-positive rate** on
realistic firmware: many drivers put related ISRs on the same
priority specifically so they don't preempt each other; v0.2-η
respects that.

**Concrete change.** Pre-v0.2-η:

```clifford
// Two ISRs at the SAME priority writing the same field.
#interrupt USART1_IRQ() #mutates: [C] #priority: HIGH { C.v += 1u32; }
#interrupt USART2_IRQ() #mutates: [C] #priority: HIGH { C.v += 1u32; }

// → error[ortho]: E0520 (false positive — NVIC processes them sequentially)
```

Post-v0.2-η: same source compiles cleanly. The pair-check
matrix consults priorities and skips matches.

**Why this is sound.** ARM v7-M ARM B1.5: when an exception
handler at priority N is running, only an exception with
strictly higher priority (numerically lower) can preempt it.
Same-priority interrupts remain pending until the running
handler returns. Therefore the two handlers' bodies execute
**strictly sequentially** — no concurrency, no race. This holds
on every Cortex-M variant and on RISC-V's PLIC under the
standard level-priority configuration.

**Implementation (`crates/ortho/src/lib.rs`):**

- New `collect_interrupt_priorities(program) -> HashMap<String, PriorityLevel>`
  walks `Item::Interrupt` items and indexes each by name.
- New `priorities_indicate_no_preemption(a, b) -> bool`:
  - `Low/Low`, `Medium/Medium`, `High/High` → `true`.
  - `Numeric(s1)/Numeric(s2)` → canonicalised string equality
    (whitespace + `_` separators stripped).
  - **Mixed kinds (e.g. `High` vs `Numeric("0")`) → `false`
    conservatively.** The target's priority encoding isn't
    known at this layer; a future target-aware slice can
    refine.
- `verify` consults the map only when both sides of a pair
  are `ConcurrencyNode::Interrupt`. The check runs after the
  `@sequential` and `#atomic` overrides and before the wedge
  check.

**Companion behaviour doc:** `docs/ortho-priority-aware-inference.md`
(~190 lines) covers the matrix, the soundness argument, the
concrete before/after, the conservative mixed-kind rule, and
the choice-of-attribute decision flow.

**The full safety-attribute deck for the SPSC pattern:**

| Pattern | When |
|---|---|
| `#priority: …` (matched) | Two interrupts on the same NVIC priority — automatic, no annotation needed beyond the existing `#priority` clauses |
| `@sequential(A, B);` | Different priorities + scheduler-guaranteed non-concurrency |
| `#atomic: interrupt_critical;` | Multi-field consistency under preemption |
| `@snapshot Auto.field` | Single primitive field read under preemption |

The four together cover the realistic firmware safety surface.

**Tests added: 8 in ortho (56 → 64).**

- `same_priority_interrupts_do_not_concur` — the canonical
  win (HIGH × HIGH on same field).
- `different_priority_interrupts_still_violate` — sanity that
  the rule is priority-conditional.
- `medium_vs_medium_also_suppressed` — not HIGH-specific.
- `numeric_priorities_compare_by_canonical_text` — canonical
  numeric matching.
- `different_numeric_priorities_still_violate`.
- `mixed_kinds_conservatively_treated_as_concurrent` — the
  HIGH vs Numeric("0") corner case.
- `priority_suppression_does_not_apply_to_effect_interrupt_pair`
  — interrupt-only.
- `priorities_indicate_no_preemption_helper_smoke` — direct
  unit tests on every variant pair.

**Pipeline regression checked.** All 7 examples pass through
v0.2-η unchanged (no example currently has two same-priority
ISRs touching shared fields, so the suppression doesn't fire
on existing samples — but it's there when needed).

**Deferred:**

- **Target-aware priority normalisation**: parse `--target`
  and map `HIGH/MEDIUM/LOW` to the target's numeric encoding
  so mixed-kind comparisons can match. v0.2-η stays
  conservative.
- **Priority-band asymmetric inference**: even different-
  priority pairs could be analysed (only the lower-priority
  handler can be preempted; the higher one is never the
  preemptee). The spec's §7.3 model is symmetric; refining
  would let us suppress more pairs at the cost of more
  careful analysis.
- **NMI handling**: non-maskable interrupts preempt
  everything, including same-priority handlers. v0.2-η
  treats every declared `#interrupt` uniformly; NMI is
  currently outside the proof boundary.

Total ortho tests: **64** (56 + 8). Workspace clean; clippy
clean.

### Added — `@snapshot Auto.field` codegen + verifier exclusion (v0.2-ζ) (2026-05-08)

End-to-end implementation of Decision #24 / ADR 0004's
`@snapshot Auto.field` boundary-crossing read. Codegen lowers
the construct to a single `load` (same shape as `Auto.field`);
the verifier excludes snapshot reads from `actual_reads` so the
v0.2-β graded check doesn't pair them against concurrent writes.
The snapshot-and-decide pattern (spec §7.2 closing note 3) is
the lighter-weight alternative to `#atomic: interrupt_critical;`
for SPSC consumer-side reads.

**The headline win**: `dual_uart_telemetry.cl`'s drain effects
are now race-free WITHOUT `#atomic` (no interrupt-mask cost).
The IR for `drain_total` shrinks from ~3164 bytes (with `#atomic`
wrapping) to ~2573 bytes (with `@snapshot`) — six fewer
inline-asm instructions across the three drain effects, and no
runtime interrupt-latency hit.

```clifford
#effect drain_total() -> u32 #mutates: [Telemetry] $ [Acquire] {
  return @snapshot Telemetry.bytes_uart1 + @snapshot Telemetry.bytes_uart2;
}
```

**Why this is sound** (per the new
`docs/snapshot-attribute.md`):
1. `@snapshot Auto.field` lowers to a single `load` instruction.
2. On every supported target, single-word loads of aligned data
   are atomic at the instruction level — the racer either reads
   the pre-write or the post-write value, never torn.
3. After the snapshot, the SSA value is owned and immutable;
   subsequent operations don't re-read the field.
4. Therefore the read race v0.2-β would have flagged is benign
   at the hardware level — the user took responsibility for the
   single-load atomicity by writing `@snapshot`.

The verifier trusts the annotation; codegen enforces the
type-level precondition (primitive single-word fields only) so
the trust is sound.

**Pipeline changes (2 crates):**

- **`clifford-effect`**: the `Snapshot` arm in
  `walk_expr_for_reads` is now **deliberately empty** — it
  walks past the snapshot without recording a read in
  `actual_reads`. The arm carries a long comment explaining
  the spec basis. This single arm change is what makes the
  verifier accept the snapshot pattern.
- **`clifford-codegen`**:
  - New `Emitter::emit_snapshot(automaton, field)` resolves
    `Self` to the enclosing automaton (mirroring
    `emit_field_access`), validates the field's IR type is
    primitive via `is_primitive_ir_ty_for_snapshot`, then
    delegates to the new `emit_field_access_by_name` for the
    actual lowering.
  - `emit_field_access` refactored to delegate to a new
    `emit_field_access_by_name(auto_name, field)` so both the
    `FieldAccess` and `Snapshot` paths share lowering. No
    behaviour change to `Auto.field` reads.
  - New `is_primitive_ir_ty_for_snapshot(ir_ty)` free helper:
    `i1`/`i8`/`i16`/`i32`/`i64` are primitive; everything else
    rejects with a structured `NotYetImplemented`. This is
    what keeps the soundness claim tight — compound types
    would tear at the load level.
  - New `Snapshot` arm in `emit_expr` dispatches to
    `emit_snapshot`.

**`docs/snapshot-attribute.md`** ships as the behaviour
reference (~210 lines). Covers:

- The "why this works" reasoning chain.
- The choice matrix between `@snapshot` /
  `#atomic: interrupt_critical` / `@sequential`.
- What `@snapshot` covers (foreground × interrupt reads,
  `Self.field` inside transitions, composition in arithmetic).
- What it doesn't (compound fields, multi-field consistency,
  writes, NMI).
- Implementation references + test list.
- Worked example (the dual_uart_telemetry pattern).
- Forward references (compound `@snapshot`, `@fn` snapshots
  with row-typed `Readable`).

**Pipeline regression checked.** All 7 examples pass:

| Sample | Status |
|---|---|
| `examples/dual_uart_telemetry.cl` | ✅ (drain side now uses `@snapshot`) |
| `examples/buffer_init_sigma.cl` | ✅ |
| `examples/uart_fsm.cl` | ✅ |
| `examples/traffic_classifier.cl` | ✅ |
| `examples/crc32.cl` | ✅ |
| `examples/sequential_attribute_demo.cl` | ✅ |
| `tests/qemu/firmware_smoke.cl` | ✅ |

**Tests added: 13 new total.**

*Codegen (8) — `crates/codegen/src/lib.rs`:*
- `snapshot_lowers_to_same_load_as_field_access`
- `snapshot_on_register_block_field_emits_volatile_load`
- `snapshot_self_inside_transition_resolves_owner`
- `snapshot_compound_field_returns_e0810`
- `snapshot_on_unknown_automaton_rejected_by_resolver`
- `snapshot_inside_arithmetic_composes`
- (refactor regression) two pre-existing `snapshot_canonical_*`
  tests still pass through the new `emit_field_access_by_name`
  path.

*Ortho (5) — `crates/ortho/src/lib.rs`:*
- `snapshot_read_does_not_trigger_race_with_concurrent_write`
  — the canonical SPSC consumer-side fix.
- `snapshot_in_arithmetic_remains_race_free` — composition.
- `plain_read_still_triggers_race_with_snapshot_alternative`
  — negative control (snapshot is the difference-maker).
- `snapshot_does_not_protect_writes` — sanity that writes
  still race (it would be very wrong to silently exempt
  writes).
- `snapshot_self_inside_transition_excluded_from_reads` —
  `Self` resolution path consistency.

**Deferred:**

- **Compound `@snapshot`** (memcpy-style snapshot inside a
  `cpsid i` / `cpsie i` scope). Would lift the primitive
  restriction at the cost of becoming equivalent to `#atomic`
  — deferred until a use case surfaces.
- **`@snapshot` inside `@fn`** with full row-typed `Readable`
  enforcement (ADR 0003). v0.2-ζ supports `@fn` snapshots
  syntactically; the layer-aware `Readable` row-gating lands
  with a separate `clifford-types` slice.
- **Target-aware `#atomic` codegen** (RISC-V `csrrci/csrrsi`,
  x86 `cli/sti`). Filed as known follow-up; doesn't affect
  `@snapshot` since it lowers to a portable `load`.

Total ortho tests: **61** (56 + 5). Total codegen tests:
**178** (170 + 8 net of 0 regressions). Workspace clean;
clippy clean.

### Added — `clifford-codegen` v0.2-ε: `#atomic: interrupt_critical` runtime wrapping (Cortex-M) (2026-05-08)

Closes the soundness gap v0.2-δ deliberately documented. Codegen
now emits the actual interrupt-mask / unmask instructions for
`#atomic: interrupt_critical` bodies — `cpsid i` at body entry,
`cpsie i` before every `ret` exit. The verifier's safety claim
finally holds at runtime on Cortex-M.

**The IR shape** (every `#atomic: interrupt_critical` callable):

```llvm
define i32 @drain_total() {
entry:
  fence acquire
  call void asm sideeffect "cpsid i", ""() ; #atomic: interrupt_critical entry (mask all maskable interrupts)
  ; ... body ...
  ; (slice 9 tag write, if any)
  ; (Decision #22 release fence, if any)
  call void asm sideeffect "cpsie i", ""() ; #atomic: interrupt_critical exit (unmask)
  ret i32 ...
}
```

**Order at exit** (enforced by `emit_exit_fence_if_pending`):

1. State-tag write (slice 9).
2. Release / SeqCst fence (Decision #22) — publishes prior writes.
3. **`cpsie i`** (this slice) — re-enables interrupts.
4. `ret`.

Reversing 2 and 3 would let an interrupt fire mid-publication and
observe partial state. The order is tested explicitly by
`atomic_interacts_correctly_with_release_fence`.

**Implementation (`crates/codegen/src/lib.rs`):**

- New `Emitter::pending_atomic_exit_unmask: bool` flag, reset
  per function. Set by `emit_atomic_entry_mask` when an
  `#atomic: interrupt_critical` body opens; consumed by
  `emit_atomic_exit_unmask_if_pending` at every `ret` site.
- New `Emitter::emit_atomic_entry_mask(atomic)` method emits
  `call void asm sideeffect "cpsid i", ""()` for
  `InterruptCritical` and queues the unmask. Other kinds
  (`MulticoreCritical`, `Custom(_)`) surface a structured
  `NotYetImplemented` instead of silently producing wrong
  code.
- New `Emitter::emit_atomic_exit_unmask_if_pending` method
  emits `call void asm sideeffect "cpsie i", ""()`. Called
  from `emit_exit_fence_if_pending` AFTER the release fence.
- Three call sites (`emit_effect`, `emit_interrupt`,
  `emit_transition`) migrated from the v0.2-δ
  `emit_atomic_marker_if_any` free helper to the new
  method-based `emit_atomic_entry_mask`. The v0.2-δ helper is
  kept as `#[allow(dead_code)]` for one slice as a transition
  marker; will be deleted in a follow-up cleanup.

**Target portability.** v0.2-ε MVP wires only Cortex-M
(`cpsid i` / `cpsie i`). Other targets need different
sequences (`cli`/`sti` for x86, `csrrci`/`csrrsi` for RISC-V).
The IR emitted today targets thumbv7m-none-eabi unconditionally;
a future `cliffordc compile --target` slice will switch on the
requested triple. Programs built for non-ARM targets without
the future flag will produce IR that clang rejects on link.

**Codegen ↔ verifier soundness contract — now tight.**

- v0.2-δ: verifier trusts `#atomic` for race-freedom
  reasoning.
- v0.2-ε: codegen makes that trust valid at runtime.

Together: a program that the v0.2-δ verifier proves race-free
under `#atomic: interrupt_critical` AND that v0.2-ε codegen
accepts (i.e. uses `interrupt_critical`, not the deferred
`multicore_critical` / `custom` kinds) will, when built for a
Cortex-M target, actually mask interrupts at runtime as
asserted. The verifier-runtime contract holds end-to-end.

**Tests added: 7 new in codegen.**

- `atomic_interrupt_critical_emits_cpsid_at_body_start` — entry
  mask emitted in the right place.
- `atomic_interrupt_critical_emits_cpsie_before_ret` — exit
  unmask before the `ret`.
- `atomic_emits_balanced_pair_per_function` — exactly one
  cpsid + one cpsie per atomic body (and zero for non-atomic
  siblings).
- `atomic_interacts_correctly_with_release_fence` — exit order
  fence < cpsie < ret.
- `non_atomic_effect_emits_no_cpsid_or_cpsie` — sanity that
  non-atomic bodies aren't wrapped.
- `atomic_on_interrupt_emits_wrapping_too` — `#interrupt`
  with `#atomic` gets wrapped.
- `atomic_multicore_critical_is_not_yet_implemented` — v0.7+
  reserved kind surfaces structured error.
- `atomic_custom_kind_is_not_yet_implemented` — user-defined
  kinds rejected; codegen has no semantics to emit.

**Behaviour doc updated.** `docs/ortho-atomic-attribute.md`
now documents the v0.2-ε runtime contract instead of the
v0.2-δ gap. The "runtime gap" section is replaced with the
"runtime contract" section showing the actual IR sequence,
the exit-order rationale, and target-portability notes.

**Pipeline regression checked.** All 7 examples compile
cleanly through the v0.2-ε pipeline. The `dual_uart_telemetry`
sample's drain effects now emit real wrapping (3164-byte IR,
up from 2852 in v0.2-δ — the difference is the new asm
instructions).

**Deferred:**

- **Target-aware emission**: switch on `--target` to emit
  Cortex-M / x86 / RISC-V variants.
- **Transition-side `#atomic` consumption** (verifier still
  doesn't propagate transition atomicity through `#>` chains;
  codegen wraps the transition body if the attribute is set,
  but the verifier doesn't yet trust that).
- **Multi-exit safety audit**: a body with multiple `ret`
  paths emits multiple cpsie's. v0.2-ε handles this via the
  existing `emit_exit_fence_if_pending` infrastructure;
  worth a future smoke test once `match` / `break` / multi-
  return shapes land.
- **NMI handling** documentation in `clifford-check`.

Total codegen tests: **170** (163 pre-v0.2-ε + 7 new).
Workspace clean; clippy clean.

### Added — `clifford-ortho` v0.2-δ: `#atomic: interrupt_critical` (§6.6) — verifier side (2026-05-08)

End-to-end plumbing for `#atomic: <kind>;` clauses on `#effect`,
`#interrupt`, and `#transition` declarations. The verifier
consumes the attribute to suppress orthogonality pairs against
`#interrupt` callables — the canonical fix for the SPSC
consumer-side race v0.2-β rejected on `dual_uart_telemetry.cl`.

**The drain effects are back.** The sample's foreground readers
(`drain_total`, `drain_last_uart1`, `drain_last_uart2`) now
carry `#atomic: interrupt_critical;` and pass the v0.2-δ
verifier cleanly:

```clifford
#effect drain_total() -> u32
    #mutates: [Telemetry]
    #atomic: interrupt_critical;
    $ [Acquire]
{
  return Telemetry.bytes_uart1 + Telemetry.bytes_uart2;
}
```

**⚠ Documented soundness gap.** v0.2-δ is **verifier only**.
Codegen emits a comment marker in the IR but does NOT yet emit
the runtime masking instructions (`cpsid i` / `cpsie i` on
Cortex-M). A binary built today with `#atomic` will *not*
actually mask interrupts at runtime. The verifier's safety
proof is valid for the program as written; the runtime gap is
on the emission side. The closing slice (v0.2-ε, planned)
wires the inline-asm sequences for at least Cortex-M.

This is a deliberate trade-off: shipping the verifier value
NOW (so users can write idiomatic `#atomic` code and have it
type-checked) and deferring the runtime to a focused
follow-up. The CHANGELOG and the `; #atomic:` IR comment
both call out the gap.

**Pipeline (5 crates touched):**

- **`clifford-ast`**: new `AtomicKind` enum
  (`InterruptCritical`, `MulticoreCritical` (reserved for
  v0.7+), `Custom(String)`) marked `#[non_exhaustive]`. New
  `atomic: Option<AtomicKind>` field on `EffectDecl`,
  `InterruptDecl`, and `TransitionDecl`.
- **`clifford-parser`**: new `parse_optional_atomic_clause`
  recognises `#atomic: <ident>;` after the `#mutates` /
  `#priority` / `#cannot_mutate` clauses, before the
  `$ [TraitList]` (or before the body if no trait list). Wired
  into all three decl parsers.
- **`clifford-resolve`** / **`clifford-types`**: no changes.
  The attribute is data-only; no name resolution or typing
  obligations.
- **`clifford-codegen`**: new `emit_atomic_marker_if_any`
  helper writes `; #atomic: <kind> (runtime wrapping deferred
  to a future slice; see CHANGELOG)` as a comment line at the
  start of every `#atomic` callable's `entry:` block. The
  comment is stripped by clang on parse — purely documentary
  for human readers and post-processing tools.
- **`clifford-ortho`**: each `ConcurrencyNode` now carries an
  `is_atomic_critical: bool` flag collected during the
  node-collection phase. `verify` checks the flag before the
  wedge-product check: if either side is atomic AND the other
  side is an `#interrupt`, the pair is suppressed. Effect ×
  effect and effect × @fn pairs are unaffected (they're
  already non-concurrent per §7.3).

**Suppression matrix (per `docs/ortho-atomic-attribute.md`):**

| Pair | Atomic suppresses? |
|---|---|
| `#atomic effect` × `#interrupt` | ✅ |
| `#atomic interrupt` × `#interrupt` | ✅ |
| `#atomic effect` × `#effect` | ❌ (foreground serialises) |
| `#atomic effect` × `@fn` | ❌ (foreground serialises) |
| `#atomic transition` × `#interrupt` | ❌ today (transitions aren't direct concurrency nodes per §7.3; the attribute is parsed but transition-side suppression needs call-site-aware tracking — separate slice) |

**`docs/ortho-atomic-attribute.md`** ships alongside this
slice as the behaviour reference. Covers the verifier
semantics, the runtime gap, the closing slice's plan, what
`#atomic` doesn't cover (NMI, multi-core, foreground-vs-
foreground races), worked examples, and cross-references to
the implementation + tests.

**Tests added: 6 new in ortho (45 → 51).**

- `atomic_effect_suppresses_pair_with_interrupt` — the
  canonical SPSC consumer-side fix.
- `atomic_interrupt_suppresses_pair_with_other_interrupt` —
  IRQ-vs-IRQ pair with one side atomic.
- `atomic_does_not_suppress_pair_with_non_interrupt` — atomic
  is interrupt-specific; foreground serialisation handles the
  rest.
- `no_atomic_means_no_suppression` — sanity that the attribute
  is what's making the first test pass.
- `atomic_with_multiple_field_writes_suppresses_all` — §7.2's
  motivation (multi-field consistency).
- `atomic_transition_is_recognised` — confirms transition-side
  parsing (verifier consumption deferred).

**Pipeline regression checked.** All 7 examples (the 6 from
prior slices plus this slice's restored
`dual_uart_telemetry.cl` drain effects) compile cleanly:

| Sample | Status |
|---|---|
| `examples/dual_uart_telemetry.cl` | ✅ (drain effects restored with `#atomic`) |
| `examples/buffer_init_sigma.cl` | ✅ |
| `examples/uart_fsm.cl` | ✅ |
| `examples/traffic_classifier.cl` | ✅ |
| `examples/crc32.cl` | ✅ |
| `examples/sequential_attribute_demo.cl` | ✅ |
| `tests/qemu/firmware_smoke.cl` | ✅ |

**Deferred:**

- **v0.2-ε: runtime wrapping for `#atomic: interrupt_critical`.**
  Emit `cpsid i` / `cpsie i` (or target equivalents) around the
  body. Cortex-M only for the MVP; other targets surface a
  structured `NotYetImplemented`.
- **Transition-side `#atomic` consumption in the verifier.**
  Today the parser plumbs the field on transitions but the
  verifier doesn't consume it (transitions aren't direct
  concurrency nodes). Call-site-aware atomicity tracking
  through `#> proc()` chains is its own slice.
- **`#atomic: multicore_critical`** for Decision #21 (v0.7+)
  shared-field locking.
- **NMI handling**: documenting which interrupts are NMI on
  each target so `#atomic` can warn or escalate.

Total ortho tests: **51** (45 pre-v0.2-δ + 6 new). Workspace
clean; clippy clean.

### Added — `clifford-ortho` v0.2-γ: `@sequential(A, B)` consumption (Decision #11) (2026-05-08)

The verifier now consumes `@sequential(A, B);` top-level
attributes per Decision #11. When the user declares two
automatons sequential, the engine suppresses the orthogonality
check for any pair where one callable touches `A` and the other
touches `B`. Per spec §7.0.1 the assertion is *trusted* — the
compiler does not verify that A and B truly never run
concurrently.

**The behaviour doc.** Because `@sequential` has subtle
semantics — what counts as "touching" an automaton, what cases
it doesn't help with, the relationship to v0.2-β's read-write
detection — this slice ships
**`docs/ortho-sequential-attribute.md`**: a focused behaviour
note explaining what the attribute does, what it doesn't, the
trust model, and worked examples. The doc is grounded in the
actual implementation (every claim cross-references the helper
in `crates/ortho/src/lib.rs`) and the test corpus
(`sequential_attr_*` and `is_pair_sequential_*`).

**Implementation:**

- New `collect_sequential_pairs(program) -> HashSet<(String,
  String)>` walks `Item::Sequential(_)` items and
  canonicalises pairs to `(lo, hi)` alphabetical order so
  `@sequential(A, B)` and `@sequential(B, A)` produce the same
  entry per spec §2.6's symmetry.
- New `node_touches(node, profiles)` returns the
  `actual_automata` set for an effect or interrupt, treating
  `@fn`s as touching nothing (they have no mutation profile).
- New `is_pair_sequential(a, b, profiles, pairs)` walks the
  cross-product of the two callables' touch sets against the
  declared sequential pairs. Skips entries where the
  automaton names match (same-automaton sequentiality is
  meaningless per Decision #5 — automatons are inherently
  self-sequential).
- `verify` calls `is_pair_sequential` after `can_concur` and
  before the wedge check; matching pairs are skipped silently.

**What `@sequential` covers (per the behaviour doc):**

- Two callables on different automatons that share a basis bit
  (currently only possible for trait-basis bits in v0.2-α; will
  matter for `#shared` fields in v0.7+ per Decision #21).
- Documentary use: capturing the user's design intent that two
  automatons run on disjoint scheduler tasks, even when the
  basis bits are already disjoint.

**What `@sequential` deliberately does NOT cover:**

- Same-automaton concurrency. `@sequential(A, A)` is ignored
  because automaton `A`'s transitions are inherently sequential
  within `A` per Decision #5.
- Read-only callables. A callable that only READS from `A`
  doesn't have `A` in its `actual_automata` set, so
  `@sequential` clauses involving `A` won't apply to it. The
  SPSC consumer-side race that v0.2-β rejected on
  `dual_uart_telemetry.cl` is exactly this case — the routes
  to safety remain `#atomic` (§6.6) or `@snapshot` (Decision
  #24 / ADR 0004), both of which are deferred.
- Transitive sequentiality. `@sequential(A, B)` plus
  `@sequential(B, C)` does not imply `@sequential(A, C)` —
  each pair must be declared explicitly.

**Files added:**

- `docs/ortho-sequential-attribute.md` — the behaviour doc.
- `examples/sequential_attribute_demo.cl` — small program
  exercising the attribute. Compiles cleanly via the standard
  CLI pipeline.

**Tests added: 9 in ortho (36 → 45).**

- `sequential_attr_suppresses_violation_between_two_automatons`
  — disjoint + `@sequential` → still passes (no regression).
- `sequential_attr_silences_violation_when_automatons_share_state`
  — two effects on same automaton → already orthogonal via
  Decision #5; `@sequential` is a no-op here.
- `sequential_attr_suppresses_real_cross_automaton_interrupt_pair`
  — effect on `A` + interrupt on `B` with `@sequential(A, B)` —
  passes as expected.
- `is_pair_sequential_returns_true_when_attribute_present` —
  direct unit test on the helper.
- `is_pair_sequential_handles_reverse_order_in_attribute` —
  symmetry: `@sequential(B, A)` canonicalises to `(A, B)`.
- `is_pair_sequential_returns_false_without_attribute`.
- `sequential_attr_does_not_suppress_same_automaton_pair` —
  `@sequential(C, C)` doesn't suppress real same-automaton
  write-write races.
- `collect_sequential_pairs_deduplicates_symmetric_declarations`.
- `collect_sequential_pairs_handles_multiple_distinct_pairs`.

**Pipeline regression checked.** All 6 prior examples plus the
new `sequential_attribute_demo.cl` compile cleanly through
the v0.2-γ verifier.

**Deferred to subsequent ortho slices:**

- **`#atomic: interrupt_critical` annotation** (§6.6).
  Wraps a body in CLI/STI; turns the body's reads into safe
  atomic ones for orthogonality purposes. Would unblock the
  SPSC consumer-side pattern.
- **`@snapshot Auto.field` codegen** (Decision #24 / ADR
  0004). Lets foreground readers copy state into a private
  local before reading.
- **`#priority`-aware concurrency inference**. Currently the
  verifier conservatively pairs every interrupt with every
  other interrupt regardless of priority. A future slice could
  use NVIC priority semantics to refine the matrix, reducing
  the need for `@sequential` in many real programs.
- **`#basis` override clauses** (Decision #4 rule 2).
- **Property tests via `proptest`** per CLAUDE.md §4.1
  (toolchain pin still blocks).

Total ortho tests: **45** (36 pre-v0.2-γ + 9 new). Workspace
clean; clippy clean.

### Added — `clifford-ortho` v0.2-β: read-write race detection (§7.2 graded algebra) (2026-05-08)

Extends the §7 verifier from "write-write only" (v0.2-α) to the
full graded read-write algebra per spec §7.2. Each callable now
carries TWO blades — `writes` and `reads` — and the orthogonality
check covers all three race classes:

```text
safe(A, B) ⟺ (writes_A ∧ writes_B == 0)    [v0.1: write-write]
           ∧ (reads_A  ∧ writes_B == 0)    [v0.2-β: read-write]
           ∧ (writes_A ∧ reads_B  == 0)    [v0.2-β: write-read]
```

Read-read overlap is never a conflict — two reads of the same
field don't race. The cost is roughly 2× engine work per pair, as
the spec predicted.

**The verifier caught two more real bugs in `dual_uart_telemetry.cl`.**
The slice-α version split the producer-side counters into per-source
fields (which fixed the v0.1 write-write race). v0.2-β then flagged
the *consumer* side: `drain_total` reads `bytes_uartN` while the
ISRs write them.

```text
error[[ortho]]: E0520: orthogonality violation between
  `effect drain_total` and `interrupt USART1_IRQ`:
  shared field(s) `Telemetry.bytes_uart1`
error[[ortho]]: E0520: orthogonality violation between
  `effect drain_total` and `interrupt USART2_IRQ`:
  shared field(s) `Telemetry.bytes_uart2`
```

This is the spec's exact §7.2 promise: "the engine catches read-
write races at field granularity." On aligned 32-bit hardware the
race is benign at the instruction level (single-word loads are
atomic), but at the abstract memory-cell level it's a real race
that the v0.2-β engine correctly rejects. The two paths to make
the pattern safe — `#atomic: interrupt_critical` (CLI/STI wrapper)
or `@snapshot Auto.field` (Decision #24 / ADR 0004 copy-then-read)
— are deferred to subsequent slices. For v0.2-β shipping, the
sample drops the drain effects with a comment documenting the
race and the routes to safety.

**Pipeline changes:**

- `clifford-effect`:
  - `MutationProfile` gains `actual_reads: HashSet<FieldRef>`
    alongside `actual_writes`.
  - The body walker now traverses every expression-bearing
    statement (`Let` / `LetShort` / `Return(Some(_))` / `Expr` /
    `Mutate` value / `MutateShort` value / `ProcCall` args /
    `UncheckedStore` ptr+value / `VolatileStore` ptr+value /
    `Assign` value / `If` cond+branches / `Sigma` source+body)
    via the new `walk_expr_for_reads` helper.
  - `walk_expr_for_reads` recursively descends every expression
    tree and records `FieldAccess { obj: Path([X]), field }` as
    a read iff `X` resolves to an automaton (via the resolver
    symbol table) or `X == "Self"` inside a transition body. Also
    records `@snapshot Auto.field` as a read.
  - Compound `MutateShort` (`Auto.field <op>= expr` with
    `op != Eq`) now records the LHS as a read in addition to a
    write — load-modify-store is implicit.
  - Plain `MutateShort` (`Auto.field = expr`) records ONLY a
    write; the field is overwritten without being read.
  - The transitive-closure pass propagates reads through
    `#> proc()` calls symmetrically with writes.
  - `actual_automata` continues to track WRITES only — §6.2's
    `#mutates` / `#cannot_mutate` declaration check gates writes,
    not reads.

- `clifford-ortho`:
  - Internal `Behaviour` is now `{ writes: u64, reads: u64 }`
    instead of a single `u64` blade.
  - `behaviour_from_profile_and_traits` populates both blades:
    field writes + in-basis traits → writes; field reads → reads.
  - `behaviour_from_fn_traits` populates both blades: traits →
    writes; field reads (from `actual_reads`, if profile is
    available) → reads.
  - `verify` performs the three-class graded check and unions
    the conflict bits across all three for the diagnostic.

**Tests added:** 9 new in ortho (27 → 36). 0 new in effect (the
existing 51 still pass — no regression).

*v0.2-β tests:*
- `effect_reads_field_that_interrupt_writes_violates` — the
  canonical SPSC consumer-side race.
- `interrupt_reads_field_that_effect_writes_violates` — mirror
  case; check is symmetric.
- `read_read_does_not_violate` — two reads of the same field is
  never a conflict.
- `compound_assign_implies_read` — `Auto.field += expr` is a
  load-modify-store, so the LHS counts as a read.
- `pure_write_without_read_dependency_passes` — plain `=` is a
  pure store, no implicit read.
- `read_in_let_initializer_counts` — `let _x = Auto.field`
  captured.
- `read_in_if_condition_counts` — `if Auto.field > 0` captured.
- `read_in_sigma_range_bound_counts` — `sigma i in 0..Auto.field`
  captured.
- `read_propagates_through_proc_call` — transitive read
  propagation symmetric to writes.

**Pipeline regression checked against every committed example:**

| Sample | v0.2-α | v0.2-β |
|---|---|---|
| `examples/dual_uart_telemetry.cl` | ✓ | ✗ → fixed (drain side removed) |
| `examples/buffer_init_sigma.cl` | ✓ | ✓ |
| `examples/uart_fsm.cl` | ✓ | ✓ |
| `examples/traffic_classifier.cl` | ✓ | ✓ |
| `examples/crc32.cl` | ✓ | ✓ |
| `tests/qemu/firmware_smoke.cl` | ✓ | ✓ |

Five of six passed unchanged; the sixth — the multi-producer
telemetry — got its consumer side legitimately flagged by the
new check. The fix preserved the producer-side demonstration and
documented the consumer-side race + the routes to safety.

**API changes:**

- `clifford_effect::MutationProfile` gained `actual_reads`
  field — a public addition. Existing readers that destructure
  the struct need an explicit `actual_reads: _` ignore arm; the
  workspace's only such reader (`crates/ortho`) was updated.
- No changes to `clifford-codegen`, `clifford-check`,
  `clifford-resolve`, or `clifford-types`.

**Deferred to subsequent ortho slices:**

- **`@sequential(A, B)` consumption** (Decision #11). The
  dual_uart_telemetry pattern is exactly the case `@sequential`
  was designed for — let users assert "drain runs only when
  ISRs are masked" so the verifier skips the pair.
- **`#atomic: interrupt_critical` annotation** (§6.6). Wraps a
  body in CLI/STI; turns the body's reads into safe atomic ones
  for orthogonality purposes.
- **`@snapshot Auto.field` codegen** (Decision #24 / ADR 0004).
  The parser ships in v0.2-α; codegen lowering would let users
  copy-then-read without a race.
- **`#basis` override clauses** (Decision #4 rule 2).
- **Property tests via `proptest`** per CLAUDE.md §4.1 mandate
  (still blocked on the toolchain pin).

Total ortho tests: **36** (27 pre-v0.2-β + 9 new). Total effect
tests: **51** (unchanged). Workspace clean; clippy clean.

### Added — `clifford-ortho` v0.2-α: trait basis + `@fn` concurrency nodes (2026-05-08)

Extends the §7 verifier from "field basis only" (v0.1) to the
full §7.1 basis: predeclared §4.5 pure-side traits + user-defined
`@trait`s now contribute basis vectors alongside automaton fields.
`@fn` callables become concurrency nodes whose behaviour blade is
the wedge of their declared traits — letting the engine reason
about the §7.1 prose example: a `$ [Readable]` `@fn` running
concurrent with a mutating `#interrupt` is provably orthogonal.

**What changed in the basis:**

- **§4.5 predeclared pure-side traits** (`Pure`, `Readable`,
  `Observable`, `Opaque`) get the lowest trait-basis indices in
  table order, even if unreferenced. This keeps diagnostic bit
  indices stable across edits that add or remove `@fn`s.
- **User-defined traits** get bits in first-appearance order
  after the predeclared four.
- **Decision #22 imperative-side traits** (`Hardware`, `Realtime`,
  `Acquire`, `Release`, `SeqCst`, `LockingDiscipline`,
  `PureState`, `Encapsulated`) are deliberately EXCLUDED from
  the basis per spec §2.6's note "the orthogonality engine
  ignores `trait_list` entirely" with respect to those traits.
  They serve different purposes — codegen fences, audit trail,
  certification — and including them would create false
  positives between interrupts that legitimately share an
  ordering or hardware tag.

**What changed in concurrency inference:**

- `@fn` callables are now concurrency-checked against
  `#interrupt`s. The §7.3 rationale is the spec's own: "A `@fn`
  invoked from a `#`-context can_concur with anything its
  caller can, inheriting the caller's concurrency class." v0.2-α
  applies that conservatively — every `@fn` is a node, paired
  against every interrupt.
- `@fn × @fn`, `@fn × #effect` — never concurrent (single
  foreground thread).
- `@fn × #interrupt` — concurrent (interrupt preempts the
  foreground while the `@fn` is running).
- v0.1's existing rules unchanged: `#effect × #effect` →
  never; `#interrupt × #interrupt` → concurrent;
  `#transition × *` → never (transitions propagate via
  `actual_writes` closure).

**What changed in the behaviour blade:**

- For `#effect` / `#interrupt`: the v0.1 field-write blade is
  now OR'd with the basis bits for any §7-basis traits in the
  declared `trait_list`.
- For `@fn`: the blade is the wedge of in-basis traits in its
  declared `trait_list`. Empty trait_list defaults to
  `[Pure]` per Emergent Rule 2 — the bare `@fn` carries the
  `Pure` bit.

**Headline new test cases:**

- `pure_fn_concurrent_with_mutating_interrupt_is_orthogonal` —
  the §7.1 prose example. The `@fn`'s `Pure` bit and the
  interrupt's field bits live in disjoint basis ranges → wedge
  is non-zero → orthogonal.
- `imperative_traits_dont_create_violations_between_interrupts`
  — two interrupts both carrying `$ [Hardware, Release]` on
  disjoint fields. Without §2.6's exclusion, the shared
  `Hardware` and `Release` would falsely violate; with the
  exclusion they pass cleanly.
- `fn_and_interrupt_sharing_a_user_trait_violates` — verifies
  the corner case where a user-defined trait DOES land on both
  sides (since `@trait` is layer-universal in v0.2-β). The
  shared user trait shows up as an E0520 with the trait name
  in the diagnostic.

**API surface added:**

- `BasisAssignment::trait_bit_index(name)` — look up a trait's
  bit index.
- `BasisAssignment::field_count()` / `trait_count()` — split the
  dimension for diagnostics + IDE integration.
- `BasisAssignment::decode_mask(mask)` now returns
  `(Vec<FieldRef>, Vec<String>)` so diagnostics can split
  shared fields and shared traits.
- `OrthoError::OrthogonalityViolation` gained a
  `shared_traits: Vec<String>` field alongside the existing
  `shared_fields: Vec<FieldRef>`. The display message renders as
  `` shared field(s) `Counter.value`; shared trait(s) `Shared` ``
  when both are present.

**Tests added: 8 new in `crates/ortho/src/lib.rs`** (19 → 27).

- `basis_user_traits_get_bits_after_predeclared`
- `basis_imperative_traits_excluded_per_section_2_6`
- `pure_fn_concurrent_with_mutating_interrupt_is_orthogonal`
- `fn_with_user_trait_concurrent_with_unrelated_interrupt_is_orthogonal`
- `fn_and_interrupt_sharing_a_user_trait_violates`
- `two_pure_fns_are_skipped_by_concurrency_inference`
- `imperative_traits_dont_create_violations_between_interrupts`
- `can_concur_fn_fn_returns_false` /
  `can_concur_fn_effect_returns_false` /
  `can_concur_fn_interrupt_returns_true`

Plus existing tests updated for the renamed shape:
- `basis_is_just_predeclared_traits_for_program_with_no_automatons`
  (renamed from `basis_is_empty_for_program_with_no_automatons`).
- `basis_assigns_fields_first_then_traits` (renamed from
  `basis_assigns_in_declaration_order`).
- `basis_decode_mask_returns_named_fields_and_traits` (returns
  the two-vec tuple now).
- `basis_exhausted_when_more_than_64_basis_vectors` (now counts
  fields + 4 predeclared traits).
- `interrupt_and_effect_writing_same_field_violates` /
  `transition_writes_propagate_into_caller_blade` (assertions
  on the new `shared_fields` + `shared_traits` fields).
- `multiple_violations_all_reported` unchanged.

**Pipeline regression checked against every committed example:**

| Sample | Status |
|---|---|
| `examples/dual_uart_telemetry.cl` | ✓ |
| `examples/buffer_init_sigma.cl` | ✓ |
| `examples/uart_fsm.cl` | ✓ |
| `examples/traffic_classifier.cl` | ✓ |
| `examples/crc32.cl` | ✓ |
| `tests/qemu/firmware_smoke.cl` | ✓ |

All pass through the trait-basis verifier. The trait-basis
extension caught zero new false positives on real code — the
§2.6 exclusion of imperative traits was the right call.

**Deferred to subsequent ortho slices:**

- **Read-write race detection** (§7.2 graded algebra). Track
  reads on a separate read-blade.
- **`@sequential(A, B)` consumption** (Decision #11). Suppress
  pairs the user has asserted as serialised.
- **`#basis` override clauses** (Decision #4 rule 2). Honour
  user-supplied basis assignments.
- **Property tests via `proptest`** per CLAUDE.md §4.1. Still
  blocked on the toolchain pin.
- **Transitive `@fn` trait inheritance** (§7.3 footnote): when
  an `#effect` calls an `@fn`, the `@fn`'s trait bits should
  enter the calling effect's blade. v0.2-α adds `@fn`s as
  separate concurrency nodes instead, which is conservative but
  not transitive; transitive would be a wider check at lower
  cost per pair.

Total ortho tests: **27** (19 pre-trait-basis + 8 new). Workspace
clean; clippy clean.

### Added — `clifford-ortho` §7 verifier (2026-05-08)

The first post-v0.1.0 slice. Implements the GA orthogonality
engine end-to-end: basis assignment, behaviour-multivector
construction, concurrency inference, pairwise wedge-product
check, and source-identifier diagnostics per spec §7. Wired into
`cliffordc compile` between the `effect` gate and `codegen`, so
programs with concurrent write-write races on the same automaton
field are now rejected with `E0520`.

**The verifier caught its first real bug.** While integrating
into the CLI, the verifier rejected `examples/dual_uart_telemetry.cl`
with:

```text
error[[ortho]]: E0520: orthogonality violation between
  `interrupt USART1_IRQ` and `interrupt USART2_IRQ`:
  shared field(s) `Telemetry.bytes_total`, `Telemetry.last_byte`
```

The original sample claimed "the two UART ISRs touch disjoint
counters" but actually shared `bytes_total` and `last_byte`. The
v0.1 codegen happily compiled the racy program; the §7 verifier
proved it unsafe at the wedge-product level. Fixed by splitting
the shared fields into per-source `bytes_uartN` / `last_byte_uartN`
counters. **The verifier did its job on the first real program.**

**Spec mapping:**

- §7.1 basis assignment → `BasisAssignment::build(program)` —
  fields in declaration order, automaton-major / field-minor;
  `BasisExhausted` when count > 64 (the `u64` blade width).
  Trait basis (§7.1 step 2) deferred — v0.1 firmware code uses
  fields, not trait-only callables.
- §7.2 behaviour multivector → `behaviour_for(profile, basis)` —
  union of basis bits for fields in `MutationProfile.actual_writes`
  (the transitive closure already computed by `clifford-effect`).
  v0.1 collapses to a single blade per callable since field-only
  basis means every write contributes to one grade.
- §7.3 concurrency inference → `can_concur(a, b)` — sound-
  conservative:
    - effect × effect → `false` (single foreground thread)
    - interrupt × anything → `true` (preemption)
    - transition × anything → `false` (transitions are leaves of
      `#>` chains; their writes propagate via the transitive
      closure of the calling effect/interrupt's blade)
- §7.4 wedge product → `outer_product(a, b)` — the algorithmic
  core. `Some(a | b)` if disjoint, `None` if overlap.
- §7.5 error reporting → `OrthoError::OrthogonalityViolation`
  decodes the conflict mask back to `(automaton, field)` pairs
  via `BasisAssignment::decode_mask`. Per Emergent Rule 1, the
  diagnostic NEVER exposes raw `e_n` indices — always source
  identifiers.

**v0.1 scope (matches spec §7's "restricted form"):**

- Cl(0,0,n) algebra: every basis vector squares to zero.
- Field basis only (trait basis deferred).
- Write-write races at field granularity. Read-write races are
  v0.2 graded-algebra work per §7.2.
- No `@sequential(A, B)` overrides yet (Decision #11 is parsed
  but not consumed by the verifier; conservative).
- No explicit `#basis` override clauses (Decision #4 rule 2
  parsed but not consumed; auto-assignment is canonical).

**Spec §7.0.1 pillars deliberately not guaranteed:**

- Mutations through `#unchecked_store` / `#volatile_store` /
  `#asm` — outside the proof boundary by design.
- Read-write races — v0.2.
- Concurrency excluded by `@sequential` — user-trusted.

**Implementation (`crates/ortho/src/lib.rs`, ~420 lines + tests):**

- Replaced the slice-3-era stub (`outer_product` + a placeholder
  enum) with the full verifier.
- New deps in `crates/ortho/Cargo.toml`: `clifford-ast` (to walk
  the program for basis assignment); dev-deps: `clifford-lexer`,
  `clifford-parser`, `clifford-resolve` (for integration tests
  that parse + resolve real Clifford source).
- Public API: `verify(program, profiles) -> Result<(), Vec<OrthoError>>`
  is the top-level entry. `outer_product` and `BasisAssignment`
  remain public for external use (the spec uses them in the
  Appendix B proof and in `--verbose-basis` IDE integration).
- The CLI's `compile_source` wires `verify_ortho` between
  `extract_call_graph` and `lower`. The `error[ortho]:` phase
  prefix is added to the taxonomy in the doc-comment.

**Tests added: 19 in the ortho crate.**

*Primitive (3):*
- `disjoint_bitmasks_wedge_to_union` / `sharing_any_bit_yields_none`
  / `outer_product_invariant` (the mandatory CLAUDE.md §4.1
  invariant: `outer_product(a, b).is_some() ⟺ a & b == 0`).

*BasisAssignment (4):*
- `basis_is_empty_for_program_with_no_automatons`.
- `basis_assigns_in_declaration_order` —
  `(A.x, A.y, B.z) → (0, 1, 2)`.
- `basis_decode_mask_returns_named_fields` — round-trip from
  bitmask back to source identifiers.
- `basis_exhausted_when_more_than_64_fields` — the v0.1 width
  limit.

*Verifier (7):*
- `orthogonal_program_passes` — disjoint ISRs.
- `two_effects_never_concurrent_so_no_violation` — §7.3 says
  effects share the foreground thread.
- `empty_program_passes` — degenerate baseline.
- `interrupt_and_effect_writing_same_field_violates` — the
  canonical `E0520` shape.
- `two_interrupts_writing_same_field_violates` —
  preemption-driven race.
- `diagnostic_names_source_identifiers_not_basis_indices` —
  Emergent Rule 1: messages must say `Counter.value`, never
  `e_2`.
- `multiple_violations_all_reported` — N violations → N
  `E0520`s, not just the first.
- `transition_writes_propagate_into_caller_blade` — §6.2
  transitive closure: an interrupt that calls a transition
  inherits the transition's writes in its behaviour blade.

*can_concur (4):*
- `can_concur_effect_effect_returns_false`.
- `can_concur_interrupt_effect_returns_true`.
- `can_concur_interrupt_interrupt_returns_true`.
- `can_concur_skips_transitions`.

Plus the `dual_uart_telemetry.cl` race fix (4 fields renamed
to be per-source, `bytes_total` / `last_byte` removed). The
sample's intro comment now documents that the §7 verifier
proved the original design unsafe.

Total tests: ortho **19**, CLI **29** (unchanged), workspace
~770. All green; clippy clean across the workspace.

**What's next for ortho (v0.2):**

- **Trait basis** (§7.1 step 2). Adds `Pure` / `Readable` /
  `Observable` / `Opaque` + user `@trait`s to the basis. Lets
  the engine catch "pure callable concurrent with mutating
  one" patterns at the trait level.
- **Read-write race detection** (§7.2 graded algebra). Track
  reads on a separate read-blade; the check becomes
  `(write_A ∧ write_B) ⊕ (read_A ∧ write_B) ⊕ (write_A ∧ read_B)`.
- **`@sequential(A, B)` consumption** (Decision #11). Suppress
  pairs the user has asserted as serialised.
- **`#basis` override clauses** (Decision #4 rule 2). Honour
  user-supplied basis assignments instead of auto-assigning.
- **Property tests via `proptest`** per CLAUDE.md §4.1 mandate.
  Currently blocked on the toolchain pin (proptest's transitive
  deps want newer rustc); land alongside a toolchain bump.

## [0.1.0] — 2026-05-08

The first tagged release. v0.1 cuts the language and toolchain at
the point where:

- The full firmware language surface lowers to LLVM IR
  (slices 1–13: `@fn`, `#automaton` mono- and multi-state,
  `#effect`, `#interrupt`, `#transition`, register-block MMIO with
  volatile loads/stores, indexed-field operations, aggregate
  literals, integer casts, Decision #17/19 unsafe primitives,
  `Auto@state`, sigma loops, local `let mut` re-assignment,
  `if`/`else`, comparison + bitwise + shift ops, Decision #22
  Acquire/Release/SeqCst fence ordering).
- `cliffordc compile foo.cl` produces a `.ll` file end-to-end
  (slice 10), with the four upstream semantic gates wired in
  (slice 15: sigil-layer + mutation-auth + totality + categorical
  + mutation-profile + call-graph), and source-line diagnostics
  rendered via `codespan-reporting` (slice 16).
- The bare-metal Cortex-M3 path is verified by a CI job running
  `qemu-system-arm -M lm3s6965evb` against a checked-in firmware
  smoke test (slice 14 + post-merge defensive hardening).
- A non-firmware pure-functional example (`examples/crc32.cl`)
  proves Clifford targets the same problem space as host-side
  systems languages — not just embedded.

**Test surface:**

| Crate | Tests |
|---|---|
| `clifford-codegen` | 163 |
| `clifford-parser` | 242 |
| `clifford-cli` | 29 |
| `clifford-types` | 176 |
| `clifford-resolve` | 80 |
| `clifford-check` | 51 |
| `clifford-effect` | 48 |
| `clifford-ast` | 70 |
| `clifford-lexer` | 3 |
| (other small crates) | ≈10 |
| **Total** | **~870** |

All green; clippy clean across the workspace.

**What v0.1 deliberately leaves for v0.2+:**

- The `clifford-ortho` GA orthogonality engine itself — the
  crate exposes only the `outer_product` primitive today; the
  full §7 verifier (with behaviour-multivector lifting + race
  detection) is the next slice.
- Compound assignment on locals (`x += 1u32` works on automaton
  fields but not on `let mut` locals — workaround:
  `x = x + 1u32`).
- `break` / `continue` inside sigma loops.
- `match` expressions / statements.
- Method calls + trait dispatch.
- String literals as values.
- `@snapshot` boundary-crossing reads (Decision #24 / ADR 0004).
- The `test`, `lint`, `audit`, `inspect` CLI subcommands.

None of these block firmware shipping; v0.1 is the proof that
the language compiles and runs on real hardware.

**Spec snapshot at v0.1.0:**

- `docs/CLIFFORD_SPEC.md` v0.6.0-draft
- `docs/DECISIONS.md` decisions #1–#22 + #25 locked

### Added — Non-firmware example: pure-functional CRC-32 (2026-05-08)

Per CLAUDE.md §10 v0.1 criteria: "Also: a non-firmware example
(e.g., a small CLI tool or a numerical kernel) to demonstrate
the language is not embedded-only." `examples/crc32.cl` is that
example.

**Zero `#`-layer constructs.** No `#automaton`, no `#effect`, no
`#interrupt`, no `#transition`, no register-block MMIO. Just
four pure `@fn`s that link against any host C / Rust / Zig
harness. The IR is target-agnostic — `clang -c crc32.ll` works
on x86_64-linux, aarch64-darwin, thumbv7m-none-eabi, riscv32imc,
or anywhere else LLVM has a backend.

**The four entry points:**

```clifford
@fn crc32_init() -> u32                    // 0xFFFFFFFF seed
@fn crc32_byte(crc: u32, byte: u8) -> u32  // fold one byte
@fn crc32_finalize(crc: u32) -> u32        // XOR with all-ones
@fn crc32_test_vector() -> u32             // crc of "123456789"
```

Algorithm: CRC-32/ISO-HDLC. Reflected polynomial `0xEDB88320`,
all-ones init, final XOR with all-ones. The variant used by
gzip, zip, png, Ethernet FCS, and a hundred other host-side
formats — proves Clifford targets the same problem space, not
just embedded.

**What it exercises across slices:**

- `@fn` purity                       (slice 1)
- integer arithmetic + `as u32` cast (slices 1, 7)
- `let mut` + assignment             (slice 12)
- sigma loops                        (slice 11)
- `if` / `else`                      (slice 13)
- comparison + bitwise + shift       (slice 13)

**Files added:**

- `examples/crc32.cl` — the four `@fn`s, ~50 lines.
- `examples/crc32_host.c` — host C harness with three test vectors
  (empty string, single byte 'a', canonical "123456789"). Verifies
  exit code 0 on all passes.

**How to run manually:**

```bash
cliffordc compile examples/crc32.cl
clang examples/crc32.ll examples/crc32_host.c -o crc32
./crc32
# crc32 of "123456789" = 0xcbf43926  (PASS)
# crc32 of "a"         = 0xe8b7be43  (PASS)
# crc32 of "" (empty)  = 0x00000000  (PASS)
```

**Tests added:** 1 new CLI integration test
(`non_firmware_example_crc32_compiles_cleanly`) verifies that
cliffordc compiles `examples/crc32.cl` and the resulting IR
exports all four expected entry points without leaking any
`#`-layer artefacts (no `%struct.`, no `.state =`, no
`section ".interrupts"`). Runs on every CI; doesn't require
clang or qemu.

Total CLI tests: **29**.

### Fixed — QEMU CI test defensive hardening (2026-05-08)

Three pre-emptive fixes for likely failure modes that the local
toolchain couldn't reproduce (Windows dev machine doesn't have
clang / qemu / lld installed):

1. **`tests/qemu/link.ld`**: switched `.data` placement from a
   hand-computed `AT (LOADADDR(.text) + SIZEOF(.text) +
   SIZEOF(.rodata))` to the canonical `> SRAM AT > FLASH` form.
   The arithmetic version assumed lld places `.rodata`
   contiguously after `.text`, which lld is free to reorder; the
   canonical form lets the linker pick the load address itself.

2. **`tests/qemu/run.sh` + `qemu-firmware.yml`**: switched to
   lld via `-fuse-ld=lld` and added `lld` to the apt install.
   Ubuntu's default linker (ld.bfd) doesn't understand thumbv7m
   ELF without a cross-binutils sysroot; lld is target-aware
   out of the box.

3. **`tests/qemu/run.sh`**: prepend `target triple =
   "thumbv7m-none-eabi"` and the matching `target datalayout`
   to the `.ll` before clang processes it. cliffordc doesn't
   yet emit these in the IR header (deferred to a future slice
   with a CLI `--target` flag); without them, clang substitutes
   the host's datalayout, which would break struct layouts and
   pointer arithmetic on Cortex-M.

Plus toolchain pre-flight checks in `run.sh` so missing tools
produce a clean "install via apt" error instead of a cryptic
"command not found" mid-pipeline.

These are educated guesses based on known QEMU + Cortex-M +
clang gotchas; the actual proof comes from CI.

### Added — Slice 16 (3/3 of v0.1 release prep): source-line diagnostics via `codespan-reporting` (2026-05-08)

Errors now render with line / column position, the actual
offending source line, and a caret pointing at the exact byte —
not just `at byte 1234`. The polish lift that makes
`cliffordc`'s diagnostics usable for real users.

**Before (slice 15):**

```text
error[parse]: E0204: expected expression, found Semi at byte 51
```

**After (slice 16):**

```text
error[[parse]]: E0204: expected expression, found Semi at byte 51
  ┌─ examples/bad.cl:3:14
  │
3 │   return x + ;  // syntax error
  │              ^ here
```

ANSI-coloured on capable terminals; falls back to monochrome on
pipes / redirects via `ColorChoice::Auto`.

**Implementation (`crates/cli/src/main.rs`):**

- `CompileError::Phase(String)` was replaced with
  `CompileError::Phase { name: &'static str, diags: Vec<PhaseDiag> }`.
  `PhaseDiag` carries the original error message plus an optional
  byte offset extracted at construction time.
- New `byte_offset_from_msg(msg) -> Option<usize>` helper. The
  `clifford-*` error catalogues universally embed `at byte N`,
  `(at byte N)`, or `referenced at byte N` in their `Display`
  output (per `thiserror` formatters). The helper does a literal
  `find("byte ")` followed by an ASCII-digit run scan — no regex
  dependency, ~20 lines, handles all current catalogue shapes.
- New `PhaseDiag::from_error(e)` builds a single diagnostic from
  any `Display`-able error.
- New `render_phase_error(file_name, source, name, diags)` builds
  one `codespan_reporting::Diagnostic::error()` per `PhaseDiag`
  with the phase name as the diagnostic code (`[parse]`, etc.)
  and a primary `Label` at the byte offset (1-byte point span;
  full start..end ranges await per-error-variant span plumbing
  in a future slice).
- The renderer is invoked from `run_compile`, which has the
  source text in scope. `main()` just maps the error variant to
  the appropriate exit code; rendering happens earlier so the
  source string stays alive across the codespan-reporting borrow.

**Why Display-string extraction (not per-variant pattern
matching):** modifying every error enum across the seven
`clifford-*` crates to expose a `primary_offset()` method would
touch 50+ variants. The Display-string approach captures ~95% of
cases (everything that includes `byte N` in its message) with
~20 lines in one file. Errors without an offset (`E0205 unexpected
end of input`, `E0500 ortho not yet implemented`,
`E0810 codegen not yet implemented`, `E0422 proc-call cycle`)
render as plain `error[phase]: …` banners with no source snippet
— acceptable for v0.1.

**Plumbing the actual `Span` (start..end) into each error variant
is the natural slice 17.** It would let the caret span the whole
offending token instead of just one byte, and would also let
errors with multiple positions (e.g. E0401's `duplicate item …
first declared at byte X`) render two labels — primary at the
duplicate, secondary at the original.

**Tests added:** 8 new tests, +1 retired (the dead
`format_phase_errors_joins_multiple_with_newlines` that tested
the now-deleted helper).

- `byte_offset_from_msg_extracts_basic_form` — `at byte 42` →
  `Some(42)`.
- `byte_offset_from_msg_handles_parenthesized_form` — `(at byte 7)`.
- `byte_offset_from_msg_handles_referenced_form` — `referenced at byte 123`.
- `byte_offset_from_msg_returns_first_offset_when_multiple` —
  E0401's two-position message returns the duplicate's offset.
- `byte_offset_from_msg_returns_none_for_no_offset` — empty,
  no-offset, plain message.
- `byte_offset_from_msg_skips_byte_without_digits` — defensive
  case where the literal word "byte" appears in non-offset
  context.
- `phase_diag_from_error_extracts_offset_when_present` — direct
  unit on the `PhaseDiag` constructor.
- `phase_diag_from_error_offset_none_when_absent` — same, no-offset.
- `compile_source_phase_error_carries_offsets_for_renderable_diagnostics`
  — end-to-end: real parse errors carry offsets that the CLI can
  render.

Plus the three slice-15 gate tests
(`compile_source_surfaces_*`) updated to match the new
`Phase { name, diags }` shape.

Total CLI tests: **28** (20 pre-slice-16 + 9 net new — 8 added,
1 retired). All green; clippy clean across the workspace.

**v0.1 release prep complete.** Slice 14 added the QEMU CI
firmware proof; slice 15 wired the semantic gates; slice 16
delivered usable diagnostics. The remaining items (compound
assign on locals, `break`/`continue`, `match`, methods, strings,
the §7 GA orthogonality verifier) are nice-to-haves for v0.2+.

### Added — Slice 15 (2/3 of v0.1 release prep): semantic gates wired into `cliffordc compile` (2026-05-08)

The CLI now runs the four upstream semantic gates between `infer`
and `lower`:

```text
tokenize → parse → resolve → infer
                            → check                    (sigil-layer + mut-auth + totality)
                            → extract_categories       (Decision #5 categorical structure)
                            → extract_mutation_profiles (§6 #mutates / #cannot_mutate)
                            → extract_call_graph       (proc-call cycle detection)
                            → lower                    (codegen)
```

Programs that violate any gate are now rejected with a
phase-prefixed structured diagnostic instead of compiling to
incorrect IR. The gate-prefix taxonomy is:

| Prefix | Source |
|---|---|
| `error[lex]:` | `clifford-lexer` |
| `error[parse]:` | `clifford-parser` |
| `error[resolve]:` | `clifford-resolve` |
| `error[types]:` | `clifford-types` |
| `error[check]:` | `clifford-check` (§5.5 boundary, §5.4 mut-auth, Decision #23 totality) |
| `error[effect]:` | `clifford-effect` (categories, mutation profiles, call-graph) |
| `error[codegen]:` | `clifford-codegen` |

**`clifford-ortho` deliberately deferred.** The crate is still
scaffolding — only the `outer_product` primitive exists; no
top-level `verify_orthogonality(program)` function lives there
yet. The CLI carries an explicit comment marking where the
`error[ortho]:` arm goes when §7 lands. v0.1 release ships with
the gates that exist today.

**Pipeline regression checked against every committed example:**

| Sample | Status |
|---|---|
| `examples/dual_uart_telemetry.cl` | passes all gates ✓ |
| `examples/buffer_init_sigma.cl` | passes all gates ✓ |
| `examples/uart_fsm.cl` | passes all gates ✓ |
| `examples/traffic_classifier.cl` | passes all gates ✓ |
| `tests/qemu/firmware_smoke.cl` | passes all gates ✓ |

So none of the slices 5–14 had latent semantic bugs that codegen
was happily compiling around. The gates are now an active
correctness check on every PR.

**Tests added (`crates/cli/src/main.rs::tests`):** 3 new tests.

- `compile_source_surfaces_check_phase_error` — `#> bump()` from
  inside an `@fn` (sigil-layer violation per Emergent Rule 4)
  is rejected with an upstream gate prefix.
- `compile_source_surfaces_effect_phase_error_for_undeclared_mutates`
  — `#effect bump() #mutates: [] { Counter.v += 1u32; }` is
  rejected by the mutation-profile validator.
- `compile_source_passes_all_gates_for_real_firmware` — positive
  integration: a multi-state automaton with effects, transitions,
  state-tagged data, and an `if`-conditional all pass through
  the gated pipeline and produce IR.

Total CLI tests: **20** (17 pre-slice-15 + 3 new). Workspace
clean, clippy clean across the workspace.

### Added — Slice 14 (1/3 of v0.1 release prep): QEMU firmware smoke test (2026-05-08)

End-to-end proof that `cliffordc`-generated LLVM IR compiles to a
working Cortex-M3 binary and runs correctly under QEMU. This is
the **v0.1 release-blocker test** — every codegen slice
contributes a function to `tests/qemu/firmware_smoke.cl`, the C
harness verifies each returns the expected value, and a
regression anywhere in the pipeline fails the test.

**The test surface (`firmware_smoke.cl`):**

| Function | Slice exercised | Verified |
|---|---|---|
| `answer()` | 1 — basic integer fn | returns 42 |
| `clamp(x)` | 13 — `if` + comparison | clamp(50)=50, clamp(200)=100 |
| `sum_to(n)` | 11+12 — sigma + `let mut` accumulator | sum_to(10)=45 |
| `bit_count(x)` | 13 — `if` + sigma + bitwise + shift | bit_count(0xFF)=8 |

Eight functional checks total. The harness exits via ARM
semihosting `SYS_EXIT_EXTENDED` with code 0 on success or `1..=8`
indicating which check failed; QEMU propagates that as the
process exit code.

**Pipeline (per `tests/qemu/run.sh`):**

```
firmware_smoke.cl
  -> cliffordc compile           -> firmware_smoke.ll
  -> clang --target=thumbv7m-none-eabi -c
                                  -> firmware_smoke.o
  -> link with startup.o + harness.o + link.ld
                                  -> app.elf
  -> qemu-system-arm -M lm3s6965evb -kernel app.elf
                                  -> exit code (functional check result)
```

**Why bare-metal Cortex-M3 (not Linux user-mode ARM):** the v0.1
target IS firmware. A user-mode test would prove the codegen
produces valid ARM machine code, but it wouldn't exercise the
vector table layout (Decision #10), the fixed memory map
(Decision #6 register-block `#address`), the no-runtime
discipline (no libc, no allocator, no `_start`), or the
semihosting exit primitive. The `lm3s6965evb` board is the
embedded community's standard QEMU smoke target precisely because
it's minimal and supports semihosting out of the box.

**Files added:**

- `tests/qemu/firmware_smoke.cl` — the Clifford program with four
  functions exercising slices 1–13.
- `tests/qemu/harness.c` — extern declarations + 8 functional
  checks + semihosting exit propagation.
- `tests/qemu/startup.c` — minimal Cortex-M3 boot: vector table
  at `.vectors`, `Reset_Handler` that zeros `.bss` and copies
  `.data` from flash to SRAM, `semihost_exit(code)` primitive
  using the ARM semihosting `SYS_EXIT_EXTENDED` (0x20) call with
  `ADP_Stopped_ApplicationExit` reason code.
- `tests/qemu/link.ld` — Cortex-M3 linker script matching the
  Stellaris LM3S6965 memory map: 256 KB flash at 0x00000000, 64
  KB SRAM at 0x20000000. Vector table KEEP'd at the start of
  flash; stack grows down from the top of SRAM.
- `tests/qemu/run.sh` — end-to-end driver script (cargo + clang +
  qemu-system-arm).
- `tests/qemu/README.md` — manual + CI flow documentation.
- `.github/workflows/qemu-firmware.yml` — runs on every PR via
  Ubuntu, installs `clang`, `llvm`, `qemu-system-arm` via apt,
  builds cliffordc release, runs `tests/qemu/run.sh`.

**`tests/qemu/build/`** is gitignored — intermediate `.ll`,
`.o`, `.elf` artefacts.

**Adding a new slice's smoke check** requires only:
1. Append a function to `firmware_smoke.cl`.
2. Append an `extern` decl + check to `harness.c`.

No CMake, no Cargo target glue, no embedded HAL — the harness
stays small on purpose so a regression points at exactly one
slice.

**Local toolchain unavailable on the development machine** — the
Windows environment `cliffordc` was developed in doesn't have
`clang --target=thumbv7m-none-eabi` or `qemu-system-arm`
installed. The CI job IS the proof. Ubuntu PRs will catch any
break in the bare-metal Cortex-M path on every change.

### Added — Slice 13: `if` / `else` statement form (2026-05-08)

The single most-felt remaining v0.1 ergonomic gap: conditional
statements. Adds `if cond { … }`, `if cond { … } else { … }`,
and `else if` chains end-to-end across the pipeline. Unlocks the
"branch on `Auto@state`", "guarded mutation", and "early return"
patterns that the dual-UART telemetry sample had to leave as
TODO comments.

The slice also closes a half-dozen parallel codegen gaps that
fell out naturally from needing real boolean conditions:
**comparison operators** (`==`, `!=`, `<`, `<=`, `>`, `>=`) and
**bitwise / shift / logical operators** (`&`, `|`, `^`, `<<`,
`>>`, `&&`, `||`) all land in this slice.

**The headline shape — early-return classifier:**

```clifford
#effect classify() -> u8 #mutates: [Telemetry] {
  if Telemetry.bytes_total == 0u32    { return 0u8; }
  if Telemetry.bytes_total < 100u32   { return 1u8; }
  if Telemetry.bytes_total < 1000u32  { return 2u8; }
  return 3u8;
}
```

→

```llvm
define i8 @classify() {
entry:
  %tmp.0 = getelementptr %struct.Telemetry, %struct.Telemetry* @Telemetry.state, i32 0, i32 1
  %tmp.1 = load i32, i32* %tmp.0
  %tmp.2 = icmp eq i32 %tmp.1, 0
  br i1 %tmp.2, label %if.then.0, label %if.exit.0
if.then.0:
  ret i8 0
if.exit.0:
  ; ... reload + icmp ult + br ...
  br i1 %tmp.5, label %if.then.1, label %if.exit.1
if.then.1:
  ret i8 1
if.exit.1:
  ; ... third comparison ...
if.then.2:
  ret i8 2
if.exit.2:
  ret i8 3
}
```

**Pipeline changes (5 crates):**

- **`clifford-ast`**: `StmtKind::If { cond, then_block, else_block }`.
  `else_block` is `Option<Block>`. For `else if` chains the parser
  builds a synthetic single-stmt `Block` containing the next `If`
  — keeps the AST shape uniform.
- **`clifford-parser`**: `parse_if_stmt` recognises `if`, `else`,
  and `else if`. Wired into `parse_stmt` dispatch. Four parser
  tests cover no-else / with-else / else-if-chain / complex-cond.
- **`clifford-resolve`**: `walk_stmt` arm walks the cond in the
  outer scope, then opens a new scope for each branch. Lets
  inside one branch are invisible outside.
- **`clifford-types`**: `walk_stmt` arm types the cond and walks
  each branch with a fresh typing scope. Bool-ness check on the
  condition is deferred to a later validation slice.
- **`clifford-codegen`**: `emit_if` emits the conditional-branch
  CFG with optional else and a merge label. Plus three concurrent
  improvements:

**Codegen detail (`emit_if`):**

Emits one of these shapes depending on `else_block`:

```text
; with else:
  br i1 %cond, label %if.then.<id>, label %if.else.<id>
if.then.<id>:
  <then body>
  br label %if.exit.<id>      ; suppressed if then returned
if.else.<id>:
  <else body>
  br label %if.exit.<id>      ; suppressed if else returned
if.exit.<id>:
  <subsequent stmts>

; without else:
  br i1 %cond, label %if.then.<id>, label %if.exit.<id>
if.then.<id>:
  <then body>
  br label %if.exit.<id>      ; suppressed if then returned
if.exit.<id>:
  <subsequent stmts>
```

Each branch tracks its own `current_block_terminated` flag. If a
branch's body emits a `return` mid-block (or has a fall-through
that terminates), the `br label %if.exit.<id>` is suppressed so
we don't try to add a second terminator to the same basic block.
If BOTH branches terminate, the merge block is unreachable; LLVM
tolerates blocks with no predecessors and DCE collapses them.

Each branch also opens a fresh local-scope marker so `let`s inside
one branch don't leak into the other or into the merge block.
Mirrors the slice-11 sigma-loop scope handling.

**Bonus: comparison + bitwise + shift + logical operators.** To
make boolean conditions work, `emit_binary` grew arms for every
remaining `BinaryOp` variant:

| Operator | LLVM op |
|---|---|
| `==` | `icmp eq` |
| `!=` | `icmp ne` |
| `<` | `icmp slt` (signed) / `icmp ult` (unsigned) |
| `<=` | `icmp sle` / `icmp ule` |
| `>` | `icmp sgt` / `icmp ugt` |
| `>=` | `icmp sge` / `icmp uge` |
| `&&` / `&` | `and` |
| `\|\|` / `\|` | `or` |
| `^` | `xor` |
| `<<` | `shl` |
| `>>` | `ashr` (signed) / `lshr` (unsigned) |

Comparison ops produce `i1` regardless of input type;
`expr_ir_type` for `Binary` was updated to return `i1` for the
six comparison ops and the two logical ops so `br i1 %cond` sees
the right type.

**Bonus: `emit_block` rewrite.** The slice-1 emit_block tracked
"did a top-level Return statement appear?" to decide whether to
emit a synthetic terminator. With if/else, the LAST top-level
statement can be an `if` whose branches all return — current
block IS terminated, but the old check fired the synthetic-ret
path anyway. Refactored to consult `current_block_terminated`
directly. The "non-unit @fn body without explicit return"
diagnostic became a check-pass concern (deferred); codegen now
emits `unreachable` to satisfy LLVM's terminator requirement and
trusts upstream validation.

**Deferred:**

- Expression-form `if` (yields a value via phi nodes). Needs
  `Block` to gain a tail-expression slot. For now, the early-
  return idiom covers most use cases.
- `match` expressions / statements. Bigger arc; needs pattern
  matching support all the way through.
- Short-circuit evaluation of `&&` / `||` when operands have side
  effects. v0.1's `and` / `or` lowering is full-eval (both sides
  always evaluated). Fine for pure conditions; the short-circuit
  CFG lands when needed.

**Tests added:** 4 parser + 10 codegen = **14 new tests**.

*Parser (4):*
- `if_stmt_no_else` — bare `if cond { … }`.
- `if_stmt_with_else` — `if … else …`.
- `if_else_if_chain_nests` — `else if` produces synthetic
  single-stmt else-block.
- `if_stmt_with_complex_condition` — Binary-comparison cond.

*Codegen (10):*
- `s13_if_no_else_emits_conditional_branch_to_exit`
- `s13_if_with_else_emits_three_blocks`
- `s13_if_condition_uses_dynamic_value` (icmp from runtime cmp)
- `s13_if_then_returns_no_back_edge_emitted`
- `s13_if_else_both_return_exit_block_unreachable`
- `s13_else_if_chain_emits_nested_blocks`
- `s13_if_let_inside_branch_invisible_outside` (resolver-side)
- `s13_if_with_local_assign_in_branch` (slice-12 + slice-13)
- `s13_nested_if_inside_if`
- `s13_if_inside_sigma_body_works` (slice-11 + slice-13)

Plus `examples/traffic_classifier.cl`: end-to-end smoke
demonstrating early-return classifier, `else if` chain, and a
slice-12+slice-13 `clamp_to_8` accumulator. Compiles to a 90-line
LLVM module via `cliffordc compile`.

Total codegen tests: **163** (153 pre-slice-13 + 10 new). Total
parser tests: **242** (238 pre-slice-13 + 4 new). All green;
clippy clean across the workspace.

**v0.1 firmware language status:** with this slice the v0.1
target is feature-complete for the Appendix A examples. The
remaining items (compound assign on locals, `break` / `continue`,
`match`, methods, strings) are nice-to-haves, not blockers. The
QEMU integration test is the next milestone.

### Added — Slice 12: local mutable re-assignment (2026-05-15)

Adds the missing piece between `let mut` (already parseable but
codegen-unused) and the firmware patterns that need a stack-allocated
mutable local: `name = expr;`. Closes the friction that blocked the
`sum_signed_range` example in slice 11 and is the prerequisite for
`if`/`match` (whose bodies need to mutate locals).

**The headline shape — accumulator pattern that matches the spec's
"bog-standard local" phrasing:**

```clifford
@fn sum_signed_range() -> i32 #mutates: [] {
  let mut total: i32 = 0i32;
  sigma i in -5i32..=5i32 {
    total = total + i;
  }
  return total;
}
```

→

```llvm
define i32 @sum_signed_range() {
entry:
  %tmp.0 = alloca i32                ; stack slot for `total`
  store i32 0, i32* %tmp.0           ; let mut total = 0
  %tmp.1 = sub i32 0, 5              ; lo = -5
  br label %sigma.header.0
sigma.header.0:
  %sigma.i.0 = phi i32 [ %tmp.1, %entry ], [ %sigma.i_next.0, %sigma.body.0 ]
  %sigma.cond.0 = icmp sle i32 %sigma.i.0, 5
  br i1 %sigma.cond.0, label %sigma.body.0, label %sigma.exit.0
sigma.body.0:
  %tmp.2 = load i32, i32* %tmp.0     ; load total
  %tmp.3 = add i32 %tmp.2, %sigma.i.0 ; total + i
  store i32 %tmp.3, i32* %tmp.0      ; total = …
  %sigma.i_next.0 = add nsw i32 %sigma.i.0, 1
  br label %sigma.header.0
sigma.exit.0:
  %tmp.4 = load i32, i32* %tmp.0     ; return total
  ret i32 %tmp.4
}
```

The `let mut` triggers an `alloca`; every read of the local emits a
`load` through the alloca pointer; every assignment emits a `store`.
Immutable `let` bindings keep their slice-1 SSA-direct lowering — no
behavioural change for code that doesn't use `mut`.

**Pipeline changes (5 crates):**

- **`clifford-ast`**: new `StmtKind::Assign { name, value }` variant.
  Distinct from `Mutate` / `MutateShort` (which target automaton
  fields). v0.1 scope: single-ident LHS only; tuple destructuring
  and `local.field = …` deferred.
- **`clifford-parser`**: new `parse_local_assign_stmt` recognises
  `Ident = expr;`. Wired into `parse_stmt` dispatch AFTER the
  mutate-short check so `Auto.field = expr;` continues to win
  (mutate-short has a `.` between two idents). Three new tests
  cover the basic form, no-collision-with-mutate-short, and
  complex-RHS cases.
- **`clifford-resolve`**: new `ResolveError::AssignToImmutable`
  (E0410) with a description of which flavour of binding was
  targeted (immutable `let`, `let :=`, parameter, `sigma`-loop
  var). `walk_stmt` arm for `Assign` walks the RHS in the outer
  scope (for free-ref resolution), then verifies the LHS resolves
  to a `let mut` binding via `lookup_local`. UndefinedName for
  unknown locals.
- **`clifford-types`**: new `walk_stmt` arm types the RHS so any
  references inside it are recorded; assignment-compatibility
  checks (assigning a u32 to an i64 local) are deferred to a later
  slice — codegen will surface a NotYetImplemented if the IR types
  mismatch.
- **`clifford-codegen`**: this slice's heavy lifting.

**Codegen detail:**

- New `LocalStorage { Ssa, Stack }` enum and a `storage` field on
  `LocalBinding`. `Ssa` (the slice-1 default) means the binding's
  `value` field IS the SSA name holding the value; `Stack` means
  it's an alloca-produced pointer that reads/writes go through.
- All five `LocalBinding` construction sites (params on `@fn` /
  `#effect` / `#interrupt`, the sigma loop var, and the immutable
  `let` / `let :=` paths) now record `storage: LocalStorage::Ssa`.
  Only `let mut` records `LocalStorage::Stack`.
- `StmtKind::Let` arm now branches on `*mutable`. For `let mut`:
  - Emit `<ptr> = alloca <ir_ty>`
  - Emit `store <ir_ty> <v>, <ir_ty>* <ptr>` for the initial value
  - Push a `LocalBinding` with `storage = Stack` and the alloca
    pointer as `value`.
  For immutable `let`: keep the slice-1 `bind_via_identity` path
  unchanged.
- `ExprKind::Path` lowering now dispatches on `LocalStorage`:
  - `Ssa`: return the SSA name directly (slice-1 path).
  - `Stack`: emit `<val> = load <ir_ty>, <ir_ty>* <ptr>` and
    return the loaded SSA name.
- `StmtKind::Assign` arm calls new `emit_local_assign` which
  looks up the binding's storage, emits the RHS, and emits
  `store <ir_ty> <v>, <ir_ty>* <ptr>`. Defensive `NotYetImplemented`
  if the binding is somehow SSA-direct (the resolver should have
  rejected the case upstream; the defensive arm only fires if
  upstream gates are bypassed).
- New `lookup_local_with_storage` helper returns
  `(value, ir_type, storage)` triple as owned strings so callers
  don't borrow `self` while emitting follow-up IR. Replaces the
  unused `lookup_local` (deleted).

**Why the resolver enforces mutability rather than codegen:** the
upstream gate is the right place — diagnostics there get the source
span, the binding's `def_span`, and the original declaration kind
in the AST. Codegen would have to plumb all that just to produce
the same error. Routing the check through the resolver also keeps
codegen free of policy decisions ("which binding kinds can be
assigned?") — that lives entirely in the resolver's E0410 arm.

**Deferred:**

- Compound-assignment statements on locals (`x += 1u32;`,
  `x &= 0xFFu32;`) — would need parser + AST + codegen work. The
  workaround `x = x + 1u32;` works today.
- Tuple destructuring (`(a, b) = …`).
- Field-of-local assignment (`local.field = …`) — requires path-
  expression LHS in the AST.
- Assignment-compatibility check in the type checker (today only
  surfaced indirectly via codegen IR-type mismatch).
- Mem2reg / SROA optimisation hints — LLVM's standard passes do
  the right thing, so this is purely cosmetic.

**Tests added:** 3 parser + 11 codegen = **14 new tests**.

*Parser (3):*
- `local_assign_basic_form` — `x = 5u32;` parses correctly.
- `local_assign_does_not_collide_with_mutate_short` —
  `Counter.value = 5u32;` still parses as MutateShort.
- `local_assign_with_complex_rhs` — `total = total + i;` parses
  with a Binary RHS.

*Codegen (11):*
- `s12_let_mut_emits_alloca_and_initial_store` — alloca + store +
  load on read.
- `s12_immutable_let_keeps_ssa_direct_lowering` — no alloca, no
  load; `bind_via_identity`'s `add 0, 5` survives.
- `s12_assign_emits_store_to_alloca` — exactly two stores
  (initial + assign).
- `s12_accumulator_pattern_in_sigma_body` — the spec's bog-
  standard accumulator pattern: `let mut total = 0; sigma i in 0..n
  { total = total + i; }` lowers to load-add-store inside the body.
- `s12_assign_to_immutable_let_rejected_by_resolver` — E0410.
- `s12_assign_to_let_short_rejected_by_resolver` — E0410.
- `s12_assign_to_param_rejected_by_resolver` — E0410.
- `s12_assign_to_sigma_var_rejected_by_resolver` — E0410.
- `s12_assign_to_undefined_local_rejected_by_resolver` — E0402.
- `s12_multiple_assigns_to_same_local` — three sequential
  assigns produce three stores plus a final load.
- `s12_let_mut_does_not_affect_sibling_immutable_let` — exactly
  one alloca, no loads on the immutable-`let` return path.

Plus the `examples/buffer_init_sigma.cl` example was extended
with a `sum_signed_range` effect demonstrating the canonical
local-mut accumulator pattern. End-to-end smoke verified.

Total codegen tests: **153** (142 pre-slice-12 + 11 new). Total
parser tests: **238** (235 pre-slice-12 + 3 new). All green;
clippy clean across the workspace.

### Added — Slice 11: sigma loops (Decision #14 / §5.8) (2026-05-14)

The first language-feature slice that crosses the entire pipeline.
Adds `sigma <var> in <range_expr> { body }` end-to-end: lexer
keyword, AST node, parser, resolver scoping, type checker, and
codegen (counted-loop CFG with header / body / exit basic blocks).
Closes the last v0.1 firmware language gap — bounded iteration
that lowers to a tight LLVM loop with proper SSA / phi shape.

**The headline shape:**

```clifford
#automaton RingBuffer {
  #states: [Uninitialized, Ready];
  storage: [u8; 64];
  zeroed:  u32;

  #transition boot -> Ready $ [Release] {
    sigma i in 0u32..64u32 {
      #mutate RingBuffer { storage[i] = 0u8 };
      RingBuffer.zeroed += 1u32;
    }
  }
}
```

→

```llvm
define void @RingBuffer_boot() {
entry:
  br label %sigma.header.0
sigma.header.0:
  %sigma.i.0 = phi i32 [ 0, %entry ], [ %sigma.i_next.0, %sigma.body.0 ]
  %sigma.cond.0 = icmp ult i32 %sigma.i.0, 64
  br i1 %sigma.cond.0, label %sigma.body.0, label %sigma.exit.0
sigma.body.0:
  ; storage[i] = 0u8 — two-level GEP using the loop var
  %tmp.0 = getelementptr %struct.RingBuffer, %struct.RingBuffer* @RingBuffer.state, i32 0, i32 1
  %tmp.1 = getelementptr [64 x i8], [64 x i8]* %tmp.0, i32 0, i32 %sigma.i.0
  store i8 0, i8* %tmp.1
  ; zeroed += 1u32
  ; ... three lines of load+add+store at idx 2 ...
  %sigma.i_next.0 = add nuw i32 %sigma.i.0, 1
  br label %sigma.header.0
sigma.exit.0:
  ; tag write (Ready=1) → release fence → ret  (Decision #22 + slice 9)
  store i32 1, i32* (... idx 0 ...)
  fence release
  ret void
}
```

**Implementation across six crates:**

- **`clifford-lexer`**: new `KwSigma` token and `"sigma"` keyword
  match. The `all_bare_keywords` test was extended to cover it.
- **`clifford-ast`**: new `StmtKind::Sigma { var, source, body }`
  variant. Source is stored as a generic `Expr` so future array-
  source forms (`sigma x in &arr`) drop in without a variant change.
- **`clifford-parser`**: new `parse_sigma_stmt` recognises `sigma
  <ident> in <expr> <block>`. Wired into `parse_stmt` dispatch.
  Five parser tests cover half-open / inclusive / body statements
  / missing-`in` / missing-var error paths.
- **`clifford-resolve`**: new `LocalKind::Sigma` variant and a
  matching arm in `walk_stmt`. The source expression is resolved
  in the OUTER scope (so range bounds reference outer bindings),
  then a new scope is pushed, the loop var is declared, the body
  is walked, and the scope is popped. Loop var is invisible after
  the loop. `LocalKind` was marked `#[non_exhaustive]` so adding
  variants stays non-breaking.
- **`clifford-types`**: new arm in `walk_stmt` infers the loop
  var's type from the range source's `Type::Range::element`.
  Pushes/pops a typing scope around the body so the var is typed
  inside but not outside.
- **`clifford-codegen`**: this is where the real work happens.

**Codegen detail (`Emitter::emit_sigma`):**

- New Emitter fields:
  - `current_block: String` — name of the basic block currently
    being emitted into. Used by phi nodes to label their
    predecessor edges. Reset to `"entry"` per function.
  - `next_label_id: u32` — per-function counter for unique label
    suffixes (`sigma.header.0`, `.1`, …).
  - `current_block_terminated: bool` — `true` after a `ret` is
    emitted. Sigma's back-edge consults this so a body that
    `return`s mid-loop doesn't try to add a second terminator
    to the same basic block.
- New `Emitter::reset_per_function_state()` helper consolidates
  the four (now five-field) per-function reset that was duplicated
  across `emit_fn` / `emit_effect` / `emit_interrupt` /
  `emit_transition`. Avoids drift as new fields are added.
- The three existing `ret` emission sites now set
  `current_block_terminated = true` so loop emitters can detect
  block termination uniformly.
- `emit_sigma`:
  1. Match the source — `ExprKind::Range` only for v0.1; array
     sources surface a structured `NotYetImplemented`.
  2. Get the iteration IR type + signedness from the lower bound
     (the existing `expr_ir_type` and `expr_is_signed_int`
     helpers handle all integer primitives).
  3. Emit `lo` and `hi` as SSA values in the predecessor block.
     Capture the predecessor's name BEFORE emitting the branch
     so phi labels are correct.
  4. Allocate fresh label IDs and SSA names for the loop's
     `header`, `body`, `exit`, `i`, `i_next`, `cond`.
  5. Branch into the header. Emit the header label, the phi node,
     the comparison (`ult` / `ule` / `slt` / `sle` based on
     signedness × inclusiveness), and the conditional branch.
  6. Emit the body label, push the loop var as a local binding,
     snapshot the locals length so any body-local lets are
     dropped at scope close, then walk the body statements.
  7. After the body, truncate locals back to the snapshot. If
     the body's current block isn't terminated, emit the
     increment + back-edge. (If it is terminated — e.g. a
     `return` mid-body — emit a synthetic `add … 0` to keep the
     phi well-formed; LLVM DCE will collapse the loop in that
     case.)
  8. Emit the exit label and update `current_block`.

**CFG correctness verified by tests:**

- Phi predecessor edge labels are derived from `current_block` at
  branch time, so nested sigmas (which reset `current_block` to
  the inner exit block) emit the OUTER loop's back-edge into
  whatever block is open at that point — naturally correct.
- Compare opcode picks correctly across the four (signed × incl)
  combinations.
- Bounds are evaluated once in the predecessor block (not on every
  iteration), per the spec's note that "the bound expression is
  evaluated once at loop entry."
- The body's loop-var binding is popped on scope close, so a `let`
  inside the body doesn't leak into the exit block.

**End-to-end smoke** (`examples/buffer_init_sigma.cl`): a 60-line
sample exercising sigma with literal bounds, dynamic bounds (effect
parameter), inclusive range (`..=`), indexed-field write inside the
body, and the multi-state `-> Ready $ [Release]` pairing. Compiles
to a 60-line LLVM module ready for `clang`.

**Deferred to later slices (per spec §5.8):**

- Array sources (`sigma x in &arr`) — needs slice/borrow
  infrastructure.
- The `(index, value)` tuple pattern for array iteration.
- `sigma <pat> in <range>.rev()` — descending iteration is v0.2.
- Bounds-tracking + bounds-check elision per §5.8 (the `bounded<lo,
  hi>` refinement type). Today every `arr[i]` inside a sigma still
  GEPs without a runtime check (because we don't insert checks
  yet either); when bounds checking lands, the elider needs the
  refinement to suppress them on provable accesses.
- `break` / `continue` inside sigma — needs Break / Continue
  statement variants in the AST and target labels in codegen.

**Tests added:**

- `clifford-lexer` (1 modified): `all_bare_keywords` updated.
- `clifford-parser` (5 new): half-open range, inclusive range,
  body statements, missing-`in` error, missing-var error.
- `clifford-codegen` (11 new):
  - `s11_sigma_basic_half_open_emits_loop_cfg`
  - `s11_sigma_inclusive_uses_ule_compare`
  - `s11_sigma_signed_range_uses_slt_and_nsw`
  - `s11_sigma_loop_var_bound_inside_body`
  - `s11_sigma_ranges_with_dynamic_bounds`
  - `s11_sigma_bounds_emitted_in_predecessor_block`
  - `s11_sigma_followed_by_statements_emit_in_exit_block`
  - `s11_sigma_nested_uses_distinct_label_ids`
  - `s11_sigma_loop_var_invisible_after_loop` (resolver-side)
  - `s11_sigma_non_range_source_returns_e0810`
  - `s11_sigma_with_mutate_short_inside_body`

Plus `examples/buffer_init_sigma.cl`: end-to-end sample compiled
via `cliffordc compile`.

Total codegen tests: **142** (131 pre-slice-11 + 11 new). Total
parser tests: **235** (230 pre-slice-11 + 5 new). All green;
clippy clean across the workspace.

### Fixed — Codegen: multi-state automaton field reads used unshifted index (2026-05-13)

Slice 9 prepended an `i32` state-tag at LLVM struct index 0 for
multi-state automatons and shifted every user-field GEP index up
by one — but only on the **write paths** (`emit_mutate`,
`emit_mutate_short`, `emit_index_expr`'s indexed write helper).
The read path `emit_field_access` continued to use the bare user
index, so any `Auto.field` read on a multi-state automaton with
multiple user fields produced a GEP at the wrong LLVM struct
index, returning the value of an adjacent field.

The bug was hidden by the existing
`s9_user_field_index_shifts_for_multi_state` test because it
only exercised the write path (`+= 1u32`) and only had a single
user field, where the slice-9 shift coincidentally lined up.

The bug was found while writing
`examples/dual_uart_telemetry.cl` for ergonomics review: the
generated IR for `drain_total() -> u32 { return Telemetry.bytes_total; }`
read LLVM idx 2 (`bytes_uart2`) instead of idx 3 (`bytes_total`).

**Fix:** `emit_field_access`'s non-register-block branch now
applies `info.llvm_field_index(idx)` exactly the way the write
paths do, so reads and writes share the same shift policy.

**Regression test:**
`s9_field_read_on_multi_state_uses_shifted_index` reads the second
user field on a 2-field multi-state automaton and asserts both
the correct GEP (idx 2) and the absence of the unshifted GEP
(idx 1) so any future regression of this exact shape is caught.

Total codegen tests: **131** (130 pre-fix + 1 regression). All
green; clippy clean across the workspace.

Plus `examples/dual_uart_telemetry.cl` is checked in — a 100-line
multi-producer telemetry sample that exercises every codegen
slice (register-block volatile MMIO + multi-state automaton +
state-tag dispatch + Acquire/Release fences + interrupt section
attributes + integer cast + cross-callable transition mangling)
and serves as the canonical "what does idiomatic Clifford
firmware look like" reference for the v0.1 surface.

### Added — CLI slice 10: `cliffordc compile` driver (2026-05-13)

The thin CLI bridge from a `.cl` source file on disk to a `.ll` LLVM
IR file on disk. Wires the `lex → parse → resolve → types → codegen`
pipeline behind one subcommand and ships the first invocable
`cliffordc` binary that does real work.

**Usage:**

```text
cliffordc compile <input.cl> [-o <output.ll>] [--module-name <name>]
cliffordc --version | -V
cliffordc --help    | -h
```

Defaults:
- `-o` defaults to the input path with the extension swapped to
  `.ll` (`uart_fsm.cl` → `uart_fsm.ll` next to the source).
- `--module-name` defaults to the input file's stem (basename
  without extension) so the IR's `ModuleID` and `source_filename`
  match the project's expectation.

**Exit codes:**
- `0` — success
- `1` — compilation error (any of lex / parse / resolve / type /
  codegen surfaces a structured error)
- `2` — usage error (bad arguments)
- `3` — I/O error (input unreadable, output unwritable)

**Implementation (`crates/cli/src/main.rs`):**

- Hand-rolled argv parser. No `clap` dependency: the surface is
  small and stable enough that a 50-line dispatch is cheaper than
  the new dep + macro hygiene burden. The parser returns a
  `Cli` enum (`Compile`, `Version`, `Help`, `Unknown`), and `main`
  routes it to the matching handler.
- `compile_source(source, module_name)` is the pure-function
  pipeline core: takes a source string + module name, returns the
  IR text or a structured `CompileError`. Reused by tests so the
  pipeline is exercisable without touching the filesystem.
- `run_compile(input, output, module_name)` is the I/O wrapper:
  reads the source file, calls `compile_source`, and writes the
  IR to disk. Handles default-output-path computation
  (`default_output_path`) and default-module-name computation
  (`default_module_name`).
- Errors are pre-formatted with a phase prefix (`error[parse]:
  ...`) inside the lib so `main` can `eprintln!` them verbatim.
  Multi-error phases (resolve, types, codegen) are joined with
  newlines via `format_phase_errors`.

**End-to-end smoke verified on a real firmware shape**
(`examples/uart_fsm.cl`):

```clifford
#automaton Uart {
  #address: 0x4000_4000;
  tx_data: u32 #offset: 0x00;
  status:  u32 #offset: 0x18;
  #transition send { Uart.tx_data = 65u32; }
}

#automaton TxFsm {
  #states: [Idle, Sending, Done];
  bytes_sent: u32;
  #transition start  -> Sending { TxFsm.bytes_sent = 0u32; }
  #transition tick               { TxFsm.bytes_sent += 1u32; }
  #transition finish -> Done    $ [Release] { return; }
}

#effect peek_state() -> u32 #mutates: [TxFsm] { return TxFsm@state; }
#interrupt USART1_IRQ() #mutates: [Uart] #priority: HIGH { #> send(); }
```

`cliffordc compile examples/uart_fsm.cl` produces a 1571-byte IR
module containing every slice's contribution: register-block
volatile MMIO writes (slice 4), the `.interrupts` section
attribute (slice 4), multi-state struct layout `{ i32, i32 }`
(slice 9), state-tag writes at transition exits (slice 9), the
exit-fence ordering (Decision #22 + slice 9: tag write < release
fence < ret), state-tag reads (slice 9), and cross-callable
mangled transition calls (slice 4).

**Why this is the right v0.1 milestone:** with this slice the
compiler is invocable as a standalone binary by anyone with a
Rust toolchain. The output is real LLVM IR that `clang` or `llc`
can pipe into a Cortex-M ELF — the end-to-end firmware path is
unblocked. What's left for the v0.1 release is the QEMU
integration test (slice 11) plus any control-flow surface the
Appendix A examples need.

**Deferred to later slices:**

- The `test`, `lint`, `audit`, `inspect` subcommands sketched in
  the rustdoc top-of-file are still future-only. They land when
  there's a concrete user need.
- `clifford-check` / `clifford-effect` / `clifford-ortho` aren't
  yet wired into the pipeline. They surface enforcement gates
  (mutation-set checks, reachability, GA orthogonality) that
  v0.1 codegen doesn't depend on; integrating them is a separate
  slice with its own test matrix.
- Span → `(line, column)` conversion for nicer error messages
  (today every error reports a byte offset). The
  `codespan-reporting` crate is wired as a dependency for this;
  the integration is a slice 11+ piece.
- `--target <triple>` / `--verbose-basis` / other flags listed
  in the rustdoc usage block.

**Tests added (`crates/cli/src/main.rs::tests`):** 17 new tests.

*Argv parsing:*
- `empty_argv_prints_help`, `dash_h_and_long_help_are_help`,
  `version_flags`, `unknown_top_level_arg_is_unknown`.
- `compile_minimum_args`, `compile_with_output_flag`,
  `compile_with_module_name`, `compile_with_output_before_input`.
- `compile_missing_input_is_unknown`,
  `compile_missing_output_value_is_unknown`,
  `compile_unrecognised_flag_is_unknown`.

*Default-path helpers:*
- `default_output_path_swaps_extension` — `.cl` → `.ll`,
  no-extension → append `.ll`, subdirs preserved.
- `default_module_name_uses_stem` — `path/to/uart.cl` → `"uart"`.

*Pipeline smoke:*
- `compile_source_lowers_minimal_program` — empty program → IR
  module header.
- `compile_source_lowers_real_firmware_shape` — multi-state
  automaton + transition + effect lowers cleanly via the public
  pipeline function.
- `compile_source_surfaces_parse_error_with_prefix` — garbled
  source produces an `error[parse]: ...`-prefixed message.
- `format_phase_errors_joins_multiple_with_newlines` — multi-
  error phase output formatting.

Plus the `examples/uart_fsm.cl` example file is checked in as a
canonical end-to-end smoke target. Generated `.ll` artifacts are
gitignored.

Total tests this session: **130** codegen + **17** CLI = **147**
new-or-extended tests across slices 5–10. All green; clippy
clean across the workspace.

### Added — Codegen slice 9: multi-state automatons (Decision #5 categorical) (2026-05-12)

The biggest single firmware-relevant piece left for v0.1: multi-
state automatons. Closes the codegen story for Decision #5
(categorical automatons), Refinement #5b (`-> Dest` transition
destinations), and Refinement #5d (`Auto@state` state-tag reads).
Unlocks the canonical firmware shape — UART `Idle` → `Sending` →
`Done`, lock state machines, polling FSMs, init sequencers — for
v0.1.

**The headline shape — multi-state automatons now lower:**

```clifford
#automaton Counter {
  #states: [Idle, Counting, Done];
  count: u32;
  #transition start  -> Counting { Counter.count = 0u32; }
  #transition tick                { Counter.count += 1u32; }
  #transition finish -> Done      { return; }
}

#effect peek() -> u32 #mutates: [Counter] {
  return Counter@state;
}
```

→

```llvm
%struct.Counter = type { i32, i32 }   ; field 0 = state tag, field 1 = count
@Counter.state = global %struct.Counter zeroinitializer   ; tag=0 (Idle), count=0

define void @Counter_start() {
entry:
  %0 = getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 1
  store i32 0, i32* %0
  ; pending tag write before ret:
  %1 = getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0
  store i32 1, i32* %1                ; Counting = tag 1
  ret void
}

define void @Counter_tick() { … no tag write …  ret void }

define void @Counter_finish() {
entry:
  %0 = getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0
  store i32 2, i32* %0                ; Done = tag 2
  ret void
}

define i32 @peek() {
entry:
  %0 = getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0
  %1 = load i32, i32* %0
  ret i32 %1
}
```

**Implementation (`crates/codegen/src/lib.rs`):**

- `AutomatonInfo` gained `state_tags: Vec<(String, u32)>` and three
  helpers:
  - `is_multi_state()` — `true` iff `state_tags` is non-empty.
  - `llvm_field_index(user_idx)` — shifts user field indices up by
    one for multi-state automatons (the i32 tag occupies LLVM
    struct index 0).
  - `state_tag(name)` — looks up a state's integer tag.
- `collect_automatons` populates `state_tags` from
  `AutomatonDecl.states`. The first listed state always gets tag 0
  so the global's `zeroinitializer` correctly represents the
  initial state — no special-case emission needed.
- `emit_automaton_state_structs` prepends an `i32` field to the
  struct layout for multi-state automatons. Monoid automatons
  keep the slice-3 layout exactly.
- All five `getelementptr` sites that compute a user-field index
  now route through `info.llvm_field_index(idx)` so the +1 shift
  is applied uniformly. Monoid automatons hit the `idx → idx`
  identity branch; existing tests are unchanged.
- New `Emitter` field `pending_transition_tag_write:
  Option<(String, u32)>` mirrors `pending_exit_fence`. Set in
  `emit_transition` when the transition has a `-> Dest` target on
  a multi-state automaton; consumed by `emit_exit_fence_if_pending`
  before every `ret` site.
- New `Emitter::emit_state_read(automaton)` lowers
  `ExprKind::StateRead`. Emits a GEP at LLVM struct index 0 plus
  a `load i32`. Surfaces structured `NotYetImplemented` for
  monoid automatons (no tag exists) and register-block automatons
  (no defined MMIO offset for the tag yet).
- `emit_expr` dispatch grew a `StateRead(name)` arm.

**Order of writes at exit:** when a transition has both `-> Dest`
and `$ [Release]` / `$ [SeqCst]`, the order at every `ret` is:
1. State-tag write (the new state lands in memory)
2. Release / SeqCst fence (makes the new state visible to other
   agents)
3. `ret void`

This is the contract Decision #22 implies — a release fence
publishes everything that came before it, and the state tag is
exactly the kind of write that needs publishing on a state
change.

**Initial state convention:** the first state in `#states: [...]`
is the initial state (per spec convention). It gets tag 0, which
matches LLVM's `zeroinitializer`. Users who want a different
initial state must reorder their `#states: [...]` list — there's
no separate `#initial_state:` knob.

**Transitions without destinations:** `#transition tick { … }` (no
`->`) emits no tag write — the state stays the same. The
`pending_transition_tag_write` is left `None` for the duration of
that transition's body. Verified by
`s9_transition_without_destination_emits_no_tag_write`.

**Deferred to later slices:**

- Register-block multi-state combos. The spec doesn't yet pin down
  which MMIO offset stores the tag for `#address: 0x… #states:
  [...]` automatons. Surfaces as `NotYetImplemented` until a
  spec slice resolves the question.
- State-tagged data (per-state field subsets). Per Decision #5,
  fields can be associated with specific states (`#in: [Counting]`
  on a field). The codegen for that is a future slice — today
  every user field lives at the same offset for every state.
- `match Auto@state { Idle => …, Counting => … }` style dispatch.
  The AST has no `Match` node yet; sigma loops + match arrive
  together with the §5.8 control-flow slice.
- Tag-width packing. We use `i32` for every tag today; once
  state counts cross 256, a future slice can switch to `i64` (or
  pick the smallest int that fits) and update the layout helper.

**Tests added (`crates/codegen/src/lib.rs::tests`):** 13 new
tests, organised in three groups.

*Layout / structural sanity:*
- `s9_monoid_struct_unchanged` — monoid struct keeps `{ <user
  fields> }` — no `i32` tag prepended.
- `s9_multi_state_struct_prepends_i32_tag` — multi-state struct is
  `{ i32, <user fields> }` and the global is still
  `zeroinitializer` (Idle = tag 0).
- `s9_user_field_index_shifts_for_multi_state` — `Counter.count
  += 1u32;` GEPs at LLVM idx 1 (user idx 0 + tag offset).
- `s9_helper_llvm_field_index_monoid` /
  `s9_helper_llvm_field_index_multi_state` — direct unit tests on
  the helpers.

*StateRead lowering:*
- `s9_state_read_emits_gep_load_at_index_0` — `Counter@state` →
  GEP idx 0 + `load i32`.
- `s9_state_read_on_monoid_returns_e0810` — monoid Auto@state
  rejected with structured `NotYetImplemented`.
- `s9_state_read_on_register_block_returns_e0810` — register-
  block Auto@state rejected.

*Transition destination handling:*
- `s9_transition_with_destination_writes_tag_before_ret` — `start
  -> Counting` writes tag 1 before `ret void`.
- `s9_transition_without_destination_emits_no_tag_write` —
  destination-less transition emits no tag GEP at all.
- `s9_transition_destination_uses_correct_tag_for_third_state` —
  `finish -> Done` writes tag 2.
- `s9_destination_tag_write_combines_with_release_fence` — order
  at exit: tag write < release fence < ret.
- `s9_full_three_state_program_lowers_cleanly` — end-to-end smoke
  on a 3-state, 2-transition, state-reading program.

Total codegen tests: **130** (117 pre-slice-9 + 13 new). All
green; clippy clean across the workspace.

### Added — Codegen slice 8: Decision #17 / #19 unsafe primitives (2026-05-11)

Closes the codegen story for the spec's six narrow-unsafe escape
hatches: `#unchecked_load` / `#volatile_load` (expressions),
`#unchecked_store` / `#volatile_store` (statements),
`#unchecked_cast` (expression with mandatory non-empty reason
string per Refinement #19a), and `#unchecked_offset` (Decision #19
pointer arithmetic).

These are the spec's blessed escape hatches for talking to memory
the type system can't see (raw MMIO outside register-block
automatons, ABI bridges to C, hand-rolled DMA descriptor builders,
…). Every use is recorded in the AST with its reason and surfaces
through `cliffordc audit --list-unsafe`.

**The headline shape:**

```clifford
@fn read_byte(p: &u8) -> u8 {
  return #unchecked_load<u8>(p);
}

@fn write_byte(p: &u8) {
  #unchecked_store<u8>(p, 65u8);
}

@fn write_mmio(p: &u32) {
  #volatile_store<u32>(p, 0xDEAD_BEEFu32);
}

@fn step(p: &u32) -> &u32 {
  return #unchecked_offset<u32>(p, 1i32);   // p + sizeof(u32)
}

@fn capture_address(p: &u32) -> u64 {
  return #unchecked_cast<&u32, u64>("addr capture for log", p);
}
```

→

```llvm
%v1 = load i8, i8* %p                              ; #unchecked_load
store i8 65, i8* %p                                ; #unchecked_store
store volatile i32 -559038737, i32* %p             ; #volatile_store (0xDEADBEEF)
%v2 = getelementptr i32, i32* %p, i32 1            ; #unchecked_offset
%v3 = ptrtoint i32* %p to i64                      ; #unchecked_cast (ptr→int)
```

**Implementation (`crates/codegen/src/lib.rs`):**

- `emit_expr` dispatch grew four arms — `UncheckedLoad`,
  `VolatileLoad`, `UncheckedCast`, `UncheckedOffset` — and
  `emit_stmt` grew two arms — `UncheckedStore`, `VolatileStore`.
- New `Emitter::emit_unchecked_load(ty, ptr, is_volatile)` —
  `load`/`load volatile` against a raw pointer; element type comes
  from the user-written `TypeExpr` so the storage width is
  explicit.
- New `Emitter::emit_unchecked_store(ty, ptr, value, is_volatile)`
  — symmetric write side; statement form (no result).
- New `Emitter::emit_unchecked_cast(from_ty, to_ty, value)` —
  picks the LLVM opcode by source / dest IR-type shapes:
  - same IR type → no-op (return value as-is)
  - both integers → `trunc` / `sext` / `zext` (signedness from
    the user-written source type, not the value's inferred type;
    `#unchecked_cast` is explicit at this level)
  - source pointer + dest int → `ptrtoint`
  - source int + dest pointer → `inttoptr`
  - any other shape → `bitcast` (LLVM accepts bitcast between
    same-bit-width values; for size mismatches LLVM will reject
    at IR-load time, surfacing the user's error)
- New `Emitter::emit_unchecked_offset(ty, ptr, n)` — single
  `getelementptr` against the raw pointer; the signed `n` is the
  element-count offset, typed by `expr_ir_type`.
- New `type_expr_is_signed_int(t)` free helper — same shape as the
  expr-side `expr_is_signed_int` but operates on a syntactic
  `TypeExpr` (used by `#unchecked_cast` since the source type is
  user-written, not inferred).

**Reason-string handling:** the mandatory non-empty reason on
`#unchecked_cast` (Refinement #19a) is preserved on the AST and
surfaced by the audit-log tool. We deliberately do NOT embed it in
the emitted IR — LLVM strips comments during parse, so a comment
would be lost. The reason lives where it's queryable by tooling
(the AST), not where it would silently disappear (the IR text).

**Volatile semantics:** `#volatile_load` / `#volatile_store`
produce `load volatile` / `store volatile` ops, matching the
register-block field-access path's volatile handling (slice 4).
The contract is the same: word-sized integer volatile loads/stores
are a single hardware instruction on every supported target. This
is the hatch firmware uses for MMIO that lives *outside* a
register-block automaton (e.g. dynamically-resolved peripheral
addresses, IPC mailboxes).

**Deferred to later slices:**

- The `#unchecked_cast` `bitcast` fallback is best-effort. If a
  user attempts a same-shape cast LLVM rejects (e.g. mismatched
  bit widths between aggregate types), the error surfaces from
  `llc` / `clang`, not from `cliffordc`. A future slice could add
  pre-validation by walking the type pair and producing a clean
  `E0810`-shaped error.
- Float ↔ int via `#unchecked_cast` (`fptoui`, `sitofp`, etc.) —
  not exercised by firmware yet; falls through to `bitcast` today
  which LLVM will reject.
- Audit-log emission is the AST's job (already wired); the IR
  pass deliberately does not touch it.

**Tests added (`crates/codegen/src/lib.rs::tests`):** 12 new tests.

- `s8_unchecked_load_emits_plain_load` — `#unchecked_load<u32>(p)`.
- `s8_volatile_load_emits_volatile_load` — `#volatile_load<u32>`.
- `s8_unchecked_store_emits_plain_store` — `#unchecked_store<u32>`.
- `s8_volatile_store_emits_volatile_store` — `#volatile_store<u32>`.
- `s8_unchecked_cast_int_widen_unsigned_emits_zext` — `<u8, u32>`.
- `s8_unchecked_cast_int_widen_signed_emits_sext` — `<i8, i32>`.
- `s8_unchecked_cast_int_narrowing_emits_trunc` — `<u32, u8>`.
- `s8_unchecked_cast_same_type_is_noop` — `<u32, u32>` no-op.
- `s8_unchecked_cast_pointer_to_int_emits_ptrtoint` — `<&u32, u64>`.
- `s8_unchecked_offset_emits_getelementptr` — `<u32>(p, 4i32)`.
- `s8_unchecked_load_inside_binary_op` — load result feeds an add.
- `s8_type_expr_is_signed_int_table` — direct unit test on the
  helper for every primitive type.

Total codegen tests: **117** (105 pre-slice-8 + 12 new). All green;
clippy clean across the workspace.

### Added — Codegen slice 7: integer cast expressions (2026-05-10)

Lowers `expr as Type` for the integer-to-integer cases that v0.1
firmware actually uses (widening / narrowing across `i1`, `i8`,
`i16`, `i32`, `i64`, `i128`). Float casts and pointer-int casts
remain `NotYetImplemented` and surface a structured error.

**The headline shape — integer casts now lower:**

```clifford
@fn widen_unsigned() -> u32 { return 5u8 as u32; }      // zext
@fn widen_signed()   -> i32 { let v: i8 = -3i8; return v as i32; }   // sext
@fn narrow()         -> u8  { return 5u32 as u8; }      // trunc
@fn bool_to_int()    -> u32 { return true as u32; }     // zext i1
@fn redundant()      -> u32 { return 5u32 as u32; }     // no-op
```

→

```llvm
%v1 = zext i8 5 to i32                  ; widen unsigned
%v2 = sext i8 %v_signed to i32          ; widen signed
%v3 = trunc i32 5 to i8                 ; narrow
%v4 = zext i1 1 to i32                  ; bool to int
ret i32 5                               ; redundant cast: no instruction
```

The opcode is selected from the source / dest IR types and the
source's signedness:

| dest vs source            | opcode  | notes                          |
|---------------------------|---------|--------------------------------|
| same IR type              | (none)  | thread the SSA value through   |
| dest narrower             | `trunc` | sign-agnostic at the IR level  |
| dest wider, source signed | `sext`  | preserve sign on widening      |
| dest wider, source unsigned | `zext` | zero-fill upper bits          |

**Implementation (`crates/codegen/src/lib.rs`):**

- New `Emitter::emit_cast_expr(value, ty)` is the entry point.
  Computes the source IR type via `expr_ir_type` and the dest IR
  type via `lower_type` of the user-written `TypeExpr`. If both are
  identical, returns the source SSA value as-is (no instruction).
  Otherwise, dispatches on the bit widths to pick `trunc` / `sext`
  / `zext`.
- New `int_bits(ir_ty)` free helper maps `i1` / `i8` / `i16` /
  `i32` / `i64` / `i128` → bit count. Returns `None` for non-integer
  IR types (`void`, `i32*`, `[N x T]`, `{T1, T2}`, `float`, …) so
  the caller can dispatch to a different code path.
- Source signedness is determined via the existing
  `expr_is_signed_int` helper, which consults the typing record
  first and falls back to literal-suffix inspection.
- `emit_expr` dispatch grew a `Cast { value, ty }` arm; the
  fall-through `NotYetImplemented` arm only catches the remaining
  unimplemented variants.

**No-op casts:** the `src_ir_ty == dst_ir_ty` short-circuit means
that redundant casts (`5u32 as u32`, `v as TypeOf(v)`) emit no
instruction. This keeps the IR clean and avoids confusing LLVM with
spurious `bitcast` ops.

**Bool casts:** `bool` lowers to `i1`. Casting `bool` to a wider
integer goes through `zext` (bool is treated as unsigned for
widening, matching Clifford's spec semantics — `true as u32` is
`1`, not `-1`).

**Deferred to later slices:**

- Float casts (`f32` ↔ `f64`, `f32` ↔ `i32`, etc.) — `fptrunc` /
  `fpext` / `fptoui` / `sitofp` / `fptosi` / `uitofp`. The
  firmware tier doesn't use floats yet; deferred until a host /
  scientific-computing slice needs them.
- Pointer ↔ integer casts (`ptrtoint` / `inttoptr` outside of the
  register-block address machinery). Decision #19 already covers
  the `#unchecked_cast` shape for this case.
- Reference type casts (e.g. `&T` to `&U` of compatible layout).
  Most of these should go through `#unchecked_cast` per Decision
  #17, not the regular `as` operator.

**Tests added (`crates/codegen/src/lib.rs::tests`):**

- `s7_int_bits_table` — direct coverage of every supported width.
- `s7_int_bits_none_for_non_integer` — non-integer IR types return
  `None` (`void`, `i32*`, `[N x T]`, `{T1, T2}`, `float`, empty).
- `s7_widening_unsigned_emits_zext` — `5u8 as u32` → `zext`.
- `s7_widening_signed_emits_sext` — `-3i8 as i32` → `sext`.
- `s7_narrowing_emits_trunc` — `5u32 as u8` → `trunc`.
- `s7_same_type_cast_is_noop` — `5u32 as u32` emits no
  `zext`/`sext`/`trunc`; the literal `5` is returned directly.
- `s7_bool_to_int_emits_zext` — `true as u32` → `zext i1 1 to i32`.
- `s7_chained_cast_widening_then_narrowing` — `(5u8 as u32) as u16`
  emits both `zext` and `trunc`.
- `s7_cast_used_inside_larger_expression` — cast result feeds an
  `add`; verifies SSA threading.
- `s7_signed_narrowing_uses_trunc_not_sext` — `-1i32 as i8` —
  signed narrowing is still `trunc` (sign doesn't matter for
  narrowing at the IR level).

Total codegen tests: **105** (95 pre-slice-7 + 10 new). All green;
clippy clean across the workspace.

### Added — Codegen slice 6: tuple / array / array-repeat literals as values (2026-05-09)

Lowers the three remaining aggregate-literal expression shapes that
slice 1 stubbed as `NotYetImplemented`. Tuples, array literals, and
array-repeat literals can now appear anywhere a value is expected
— `let` initialisers, function arguments, return values, automaton
field reads.

**The headline shape — aggregate literals lower as SSA values:**

```clifford
@fn build() {
  let triple: (u32, bool, u8) = (5u32, true, 7u8);
  let row:    [u32; 3]        = [10u32, 20u32, 30u32];
  let buf:    [u8; 64]        = [0u8; 64];
  return;
}
```

→

```llvm
; tuple
%1 = insertvalue {i32, i1, i8} undef, i32 5, 0
%2 = insertvalue {i32, i1, i8} %1, i1 1, 1
%3 = insertvalue {i32, i1, i8} %2, i8 7, 2

; array literal
%4 = insertvalue [3 x i32] undef, i32 10, 0
%5 = insertvalue [3 x i32] %4, i32 20, 1
%6 = insertvalue [3 x i32] %5, i32 30, 2

; array repeat
%7  = insertvalue [64 x i8] undef, i8 0, 0
%8  = insertvalue [64 x i8] %7, i8 0, 1
…
%70 = insertvalue [64 x i8] %69, i8 0, 63
```

LLVM's `insertvalue` instruction is uniform across struct and array
aggregates, so the chain shape is identical for tuples and arrays;
the only difference is the aggregate type-text on each line.

**Implementation (`crates/codegen/src/lib.rs`):**

- New `Emitter::emit_tuple_expr(expr, elems)` lowers
  `ExprKind::Tuple(elems)`. Pulls the aggregate IR type from
  `expr_ir_type` (typing-driven when typing has the record, syntactic
  fallback otherwise) and threads each element value into an
  `insertvalue` chain on `undef`.
- New `Emitter::emit_array_expr(expr, elems)` lowers
  `ExprKind::Array(elems)`. Same shape as tuples; rejects empty
  array literals (`[]`) with a structured `NotYetImplemented` since
  the type-checker rarely produces a usable element type for them.
- New `Emitter::emit_array_repeat_expr(expr, value, count)` lowers
  `ExprKind::ArrayRepeat`. Requires the count to be a const integer
  literal (decimal / hex / binary, possibly parenthesised). The
  value is emitted **once** and re-used in every `insertvalue` —
  preserves "the same value at every index" semantics without
  re-evaluating side-effecting expressions.
- New `Emitter::emit_aggregate_insertvalue_chain(agg_ty, elems)`
  shared core: walks `elems`, emits each element, captures
  `(ir_type, ssa_value)` pairs, then writes the chain. Used by the
  tuple and array entry points; the repeat path inlines its own
  loop because it doesn't need per-element typing.
- New `const_int_count(expr)` free helper extracts a `usize` count
  from `IntLit` / `HexLit` / `BinLit` (and their `Paren` wrappers).
  Returns `None` for any other shape; callers surface that as
  `NotYetImplemented`.
- `emit_expr` dispatch grew three arms: `Tuple`, `Array`,
  `ArrayRepeat`. The fall-through `NotYetImplemented` arm only
  catches the remaining unimplemented variants.

**Empty edge cases:**

- `[T; 0]` (zero-count repeat) emits a chain of zero `insertvalue`
  ops; the helper returns `"undef"` directly. The type-checker
  permits this even though it's rarely useful.
- `()` (unit) is `TypeKind::Unit` and is not produced by the parser
  as a tuple expression — it's a separate shape.
- Single-element tuple syntax `(x,)` isn't part of the v0.1 surface;
  the parser treats `(x)` as a `Paren` expression.

**Deferred to later slices:**

- Non-const array-repeat counts (`[v; n]` where `n` is a runtime
  value) — needs a runtime memset / loop. Surfaces as
  `NotYetImplemented` today.
- Constant-folding of pure-constant aggregates into LLVM's inline
  constant-aggregate form (`{i32, i1} {i32 5, i1 true}`) — pure
  optimization; LLVM's mem2reg + SROA already collapse the
  `insertvalue` chain into the same code, so deferring this costs
  nothing at `-O1`+.
- String literals (`"hello"`) — separate slice; lowers to a global
  byte array plus a fat pointer.
- Struct-literal expressions for nominal types — needs ADT
  lowering support upstream.

**Tests added (`crates/codegen/src/lib.rs::tests`):**

- `s6_const_int_count_parses_decimal_hex_binary` — direct unit
  test on the helper covering all three integer-literal forms.
- `s6_const_int_count_returns_none_for_non_literal` — path
  expressions and other shapes return `None`.
- `s6_tuple_literal_lowers_to_insertvalue_chain` — 3-tuple of
  `(u32, bool, u8)`; verifies all three indices.
- `s6_two_tuple_with_different_element_types` — `(u32, bool)`;
  smaller smoke.
- `s6_array_literal_lowers_to_insertvalue_chain` — 3-element u32
  array.
- `s6_array_repeat_literal_const_count` — `[0u8; 4]` produces
  exactly 4 `insertvalue` ops on `[4 x i8]`.
- `s6_array_repeat_with_non_constant_value_emits_value_once` —
  `[v; 3]` where `v` is a binary-op SSA name; verifies 3
  `insertvalue` ops appear (the value is re-used, not
  re-evaluated).
- `s6_array_repeat_zero_count_emits_nothing` — `[0u8; 0]` emits
  zero `insertvalue` ops.
- `s6_array_repeat_non_const_count_returns_e0810` — `[0u8; n]`
  where `n` is a runtime variable surfaces
  `NotYetImplemented`.
- `s6_nested_tuple_in_array_literal` —
  `[(1u32, 2u32), (3u32, 4u32)]`; verifies 2 outer + 4 inner
  `insertvalue` ops.

Plus the existing `unsupported_expression_emits_e0810` test is
renamed to `tuple_expression_now_lowered_per_slice_6` per the
project's behavioural-change convention; the assertion flips to
verify the slice-6 lowering instead of the slice-1 stub error.

Total codegen tests: **95** (85 pre-slice-6 + 10 new). All green;
clippy clean across the workspace.

### Added — Codegen slice 5: indexed field operations (2026-05-08)

Closes the slice-3 deferral on indexed-field assignment and adds
the symmetric read side. Unblocks array-typed automaton fields —
UART FIFOs, lookup tables, ring buffers — for both struct-backed
and register-block automatons.

**The headline shape — array-typed automaton fields now lower:**

```clifford
#automaton Counter { buf: [u8; 64]; }

#effect peek() #mutates: [Counter] {
  let _x: u8 = Counter.buf[3u32];
}

#effect poke() #mutates: [Counter] {
  #mutate Counter { buf[3u32] = 5u8 };
}
```

→

```llvm
; read
%v1 = getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0
%v2 = getelementptr [64 x i8], [64 x i8]* %v1, i32 0, i32 3
%v3 = load i8, i8* %v2

; write
%v4 = getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0
%v5 = getelementptr [64 x i8], [64 x i8]* %v4, i32 0, i32 3
store i8 5, i8* %v5
```

For register-block automatons (Decision #6) the struct-field GEP is
replaced by an `inttoptr` literal at the absolute MMIO address, and
the load / store becomes volatile:

```clifford
#automaton Uart {
  #address: 0x4000_4000;
  fifo: [u8; 16] #offset: 0x20;
}

#effect tx() #mutates: [Uart] {
  #mutate Uart { fifo[2u32] = 65u8 };  // 'A'
}
```

→

```llvm
%v1 = getelementptr [16 x i8], [16 x i8]* inttoptr (i64 1073758240 to [16 x i8]*), i32 0, i32 2
store volatile i8 65, i8* %v1
```

(`0x4000_4000 + 0x20 = 0x4000_4020 = 1073758240`.)

**Implementation (`crates/codegen/src/lib.rs`):**

- New `Emitter::emit_index_expr(obj, index)` lowers `Auto.field[i]`
  / `Self.field[i]` reads. Today it only accepts the canonical
  firmware shape — `Index { obj: FieldAccess(Path([Auto|Self]),
  field), index }`. Indexing on local arrays / slices / tuples
  surfaces a `NotYetImplemented` and waits for the alloca-based
  borrow machinery (separate slice).
- New `Emitter::emit_indexed_field_store(automaton, loc,
  field_ir_ty, index_expr, value, is_register_block)` is the write
  counterpart. Mirrors `emit_index_expr` but stores instead of
  loads, and walks the index expression up front to satisfy the
  borrow checker.
- `emit_mutate` now dispatches on `fa.index.is_some()`: indexed
  assigns route through `emit_indexed_field_store`, plain assigns
  keep the slice-3 / slice-4 `emit_field_store` path. The
  slice-3 deferred `NotYetImplemented` for indexed assigns is
  removed.
- New `array_element_ir_type(ir_ty)` free helper parses
  `[N x T]` → `T`. Splits on the first ` x ` only so nested arrays
  (`[4 x [4 x i8]]` → `[4 x i8]`) survive. Returns `None` for
  non-array IR types so the caller can surface a structured
  `NotYetImplemented`.

**Both stages use a 2-level GEP** (struct-field pointer → array-element
pointer) for struct-backed automatons. For register-block
automatons the first GEP is replaced by `inttoptr (i64 <abs> to
[N x T]*)`; the array GEP and the volatile load / store still
apply, so the dispatch shape stays uniform across the two backends.

**Index-expression typing:** the index expression goes through
`expr_ir_type` so the GEP's second index carries the LLVM type the
program supplied (e.g. `i32` for `3u32`). LLVM's GEP semantics
treat the array offset as signed extending to pointer width, so
both `i32` and `i64` indices work; we don't truncate or extend in
codegen.

**Self-resolution in transitions:** indexed field operations on
`Self` (e.g. `#mutate Self { buf[0u32] = 1u8 };` inside a
`#transition`) resolve the owner from `enclosing_owner`, the same
way slice-3 / slice-4 plain field accesses do. A test
(`s5_indexed_field_in_transition_uses_self_owner`) pins this.

**Deferred to later slices:**

- Indexing on local arrays / slices / tuples (requires the
  alloca-pre-pass for `&x` borrow expressions).
- Multi-dimensional indexing (`Auto.matrix[i][j]`) — today the
  outer index returns an element-type value; nested indexing on
  that value falls back to the `NotYetImplemented` for
  non-field-access receivers.
- Range / slice indexing (`buf[1..4]`) — not in v0.1 scope.
- Bounds-check insertion — Decision #18 says register-block
  fields are unchecked and the size is part of the spec; ordinary
  fields will gain checks in a later slice.

**Tests added (`crates/codegen/src/lib.rs::tests`):**

- `s5_array_element_ir_type_extracts_element` — `[64 x i8]` → `i8`,
  `[16 x i32]` → `i32`.
- `s5_array_element_ir_type_handles_nested_arrays` — `[4 x [4 x i8]]`
  → `[4 x i8]` (verifies the `split_once` boundary).
- `s5_array_element_ir_type_returns_none_for_non_array` — primitives,
  refs, structs, empty string all return `None`.
- `s5_indexed_read_on_struct_field_emits_two_level_gep_and_load` —
  full read pipeline on a non-register-block automaton; checks
  the absence of `load volatile`.
- `s5_indexed_read_on_register_block_emits_inttoptr_gep_and_volatile_load`
  — read pipeline on a register-block automaton with a non-zero
  field offset; checks `inttoptr (i64 1073758240 to [16 x i8]*)`,
  the array GEP, and `load volatile i8`.
- `s5_indexed_write_in_mutate_block_emits_two_level_gep_and_store`
  — write pipeline on a non-register-block automaton; checks the
  absence of `store volatile`.
- `s5_indexed_write_on_register_block_emits_volatile_store` — write
  pipeline on a register-block automaton; checks `store volatile
  i8 65`.
- `s5_indexed_field_in_transition_uses_self_owner` — verifies
  `Self.field[i]` inside a `#transition` body resolves the owner
  from `enclosing_owner` and emits `@Counter_init` with the right
  GEP / store sequence.
- `s5_indexed_write_alongside_plain_writes_in_same_mutate_block` —
  mixed indexed + plain assigns in one `#mutate` block; verifies
  the `fa.index.is_some()` dispatch routes each correctly.

Total codegen tests: **85** (76 pre-slice-5 + 9 new). All green;
clippy clean across the workspace.

### Added — Codegen slice 4: register-block volatile MMIO + interrupt section attribute (2026-05-07)

Closes the v0.1 firmware codegen story. Slice 3 lowered non-
register-block automatons via state struct + `getelementptr`; slice
4 adds the **Decision #6 register-block surface** (volatile loads /
stores at fixed MMIO addresses) and **Decision #10 interrupt
section attribute** (`section ".interrupts"` so the linker can
place all interrupt handlers in a contiguous block for the vector
table).

**The headline shape — a real MMIO driver now lowers:**

```clifford
#automaton Uart {
  #address: 0x4000_4000;
  tx_data: u32 #offset: 0x00;
  status:  u32 #offset: 0x18;
  #transition send { Uart.tx_data = 65u32; }   // 'A'
}

#interrupt USART1_IRQ() #mutates: [Uart] #priority: HIGH {
  #> send();
}
```

→

```llvm
define void @Uart_send() {
entry:
  store volatile i32 65, i32* inttoptr (i64 1073758208 to i32*)
  ret void
}

define void @USART1_IRQ() section ".interrupts" {
entry:
  call void @Uart_send()
  ret void
}
```

(`0x4000_4000` = 1073758208; the volatile store goes directly to
that MMIO address with no intermediate buffering.)

**Implementation (`crates/codegen/src/lib.rs`):**

- `AutomatonInfo.fields` evolved from `(name, ir_type)` tuples to
  `(name, ir_type, optional_offset)` triples. Non-register-block
  fields keep `optional_offset = None` (they index into the state
  struct); register-block fields carry `Some(offset_value)` (their
  MMIO offset relative to the automaton's `#address:` base).
- New `AutomatonInfo.base_address: u64` — parsed from the
  `#address: 0xHEX` clause via the new
  `parse_address_literal` helper. Sum of `base_address + offset` is
  the absolute MMIO address for each register-block field access.
- New `parse_address_literal(s)` free helper: recognises hex
  (`0x4000_0000`), binary (`0b1010`), and decimal (`42`) literals
  with `_` separators.
- New `FieldLocation` enum with two variants:
  - `Struct { idx: usize }` — non-register-block field; lowers via
    `getelementptr` against `@<Auto>.state` (slice-3 path).
  - `RegisterBlock { absolute_address: u64 }` — register-block
    field; lowers via `inttoptr (i64 <abs> to <T>*)` plus volatile
    load / store (slice-4 path).
- New `Emitter::emit_field_load(automaton, struct_name, loc,
  ir_ty)` and `Emitter::emit_field_store(... value ...)` helpers
  consolidate the dispatch on `FieldLocation`. Used by
  `emit_field_access` (read), `emit_mutate` (block form), and
  `emit_mutate_short` (sugar).
- `emit_field_access` rewritten to support both branches; emits
  `load volatile <T>, <T>* inttoptr (i64 <abs> to <T>*)` for the
  register-block case.
- `emit_mutate` and `emit_mutate_short` rewritten to dispatch
  through the new helpers; compound assigns (`Mmio.ctl |= 1u32;`)
  on register-block fields lower to `load volatile + or + store
  volatile` at the MMIO address.
- `emit_automaton_transitions` no longer skips register-block
  automatons; transitions inside register-block automatons (e.g.
  the `Uart_send` example) now lower with their bodies going
  through the volatile-load/store path.
- `emit_interrupt` adds `section ".interrupts"` to the `define`
  line per Decision #10. Linker symbol still equals the source
  name. Target-specific calling convention (`thumb_intrcc`, etc.)
  remains a future slice when the target-data-layout pass lands;
  LLVM's default cc handles the common Cortex-M / RISC-V cases for
  v0.1.

**New `Emitter` field:** `transition_owners: HashMap<String, String>`.
Built at pass 1 alongside the automaton registry. Maps every
transition's name → its owning automaton, used by the proc-call
lowering to mangle cross-callable transition references (e.g.
`#> send()` from inside an `#interrupt` whose `#mutates` lists the
transition's automaton). Slice 3's mangling logic only handled the
intra-transition case via `enclosing_owner`; slice 4 closes the
gap.

**`emit_proc_call` refactored** to do a clean two-step resolution:

1. Consult `Resolution::lookup` for the call's `BindingRef::Proc {
   ctx, … }`.
2. If `ctx == CallContext::Transition`, find the owner: prefer
   `enclosing_owner` (intra-transition case), fall back to
   `transition_owners` (cross-callable case). Mangle as
   `<Owner>_<name>`.
3. Otherwise emit the bare name (effect / interrupt linker symbol
   = source name).

**Volatile-load/store atomicity (Decision #6 contract):** the IR
`store volatile` / `load volatile` of word-sized integers
(`i8` / `i16` / `i32` / `i64` for `u8/i8` … `u64/i64`) is a
single hardware instruction on every supported target. Decision
#6's "register access goes through normal `#mutate` machinery on
register-block automata" claim translates directly to LLVM's
volatile semantics — no manual atomic-instruction selection
needed for v0.1's word-aligned register fields.

**Tests (15 new slice-4 tests, codegen crate now 76 total, was 61):**

- Register-block field read → volatile load at absolute address.
- Register-block field write → volatile store at absolute address.
- Field offset added to base correctly (`base + 0x04` vs
  `base + 0x00`).
- Register-block automaton no longer emits a state struct or
  global state (slice-3 invariant preserved).
- Compound assign (`|=`) on register-block field → volatile load
  + or + volatile store.
- `#mutate Mmio { ctl = …, status = … };` block form on register-
  block: each field gets its own absolute address.
- Register-block transition lowers (slice-3 punted on this; slice 4
  closes it).
- Interrupt `define` line carries `section ".interrupts"`.
- Effect (non-interrupt) does NOT carry section attribute.
- Interrupt with `Acquire` fence: section attr + fence coexist.
- End-to-end MMIO program (Uart with transition, IRQ dispatching
  it, full pipeline through codegen).
- `parse_address_literal`: hex / decimal / binary / malformed →
  None.

The slice-3 `s3_register_block_field_access_emits_e0810` test was
renamed to `s3_register_block_field_access_now_supported_per_slice_4`
and updated to assert the volatile-load form.

Workspace remains green; clippy clean.

**v0.1 firmware milestone reached.** The codegen surface now covers
the *complete* v0.1 firmware program shape:

- Pure `@fn`s (slice 1): primitives, arithmetic, calls.
- Composite types + typing (slice 2): refs, arrays, slices,
  tuples, deref, signed/unsigned ops.
- Automaton state + effects + transitions + `#mutate` (slice 3).
- Decision #22 fences (Acquire / Release / SeqCst).
- Register-block MMIO + interrupt sections (slice 4).

A real firmware program — UART driver, GPIO toggler, scheduler —
now goes from `.cl` source through lexer / parser / resolve /
types / check / effect / ortho all the way to runnable `.ll` IR.

What's still on the deck for v0.2+:

- Multi-state automatons with state-tag dispatch.
- Sigma loops.
- Indexed field assignment (`Counter.buf[3] = …`).
- Borrow expressions (`&x` → `alloca + store`).
- Tuple/array literals as values.
- Index expressions (`x[i]`).
- Generic effect monomorphisation (Decision #16).
- Bit-field RMW (Decision #20).
- Decision #21 / #26 lock machinery (v0.7+).
- CLI driver (`cliffordc build foo.cl`).
- End-to-end QEMU integration test (`.ll` → `llc` → linked binary).

### Added — Decision #22 codegen: LLVM memory-ordering fences for `Acquire` / `Release` / `SeqCst` (2026-05-07)

Closes the codegen gap from Decision #22 / ADR 0003. The earlier
trait-validation slice (E0541 / E0544) ensures predeclared trait
names are recognised on the right layer; this slice makes `Acquire`
/ `Release` / `SeqCst` actually *do something* — emit LLVM `fence`
instructions at the right points in the function body.

**The headline shape:**

```clifford
#effect strict_publish() #mutates: [Counter] $ [SeqCst] {
  Counter.value = 1u32;
}
```

→

```llvm
define void @strict_publish() {
entry:
  fence seq_cst
  %tmp.0 = getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0
  store i32 1, i32* %tmp.0
  fence seq_cst
  ret void
}
```

**Implementation (`crates/codegen/src/lib.rs`):**

- New `MemoryOrdering { entry, exit }` struct holding optional LLVM
  ordering keywords for the entry fence and the per-`ret` exit
  fence.
- New `memory_ordering_from_traits(trait_list)` free helper:
  - `SeqCst` present (alone or with others) → entry + exit both
    `seq_cst` (supersedes `Acquire` / `Release`).
  - `Acquire` only → entry `acquire`, no exit fence.
  - `Release` only → exit `release`, no entry fence.
  - `Acquire` + `Release` → both, with respective orderings.
  - No ordering trait → no fences.
- `Emitter` gains `pending_exit_fence: Option<&'static str>` field
  set per-callable so every `ret` site can emit the configured
  fence before the actual `ret`.
- New `Emitter::emit_exit_fence_if_pending` helper called at every
  `ret` emission point (explicit `Return(Some)` / `Return(None)`
  in `emit_stmt`, and the implicit-`ret void` path in `emit_block`).
- `emit_effect` / `emit_interrupt` / `emit_transition` each:
  1. Compute `MemoryOrdering` from the callable's `trait_list`.
  2. Set `self.pending_exit_fence = ordering.exit`.
  3. After emitting the `entry:` label, emit the entry fence if
     `ordering.entry.is_some()`.
  4. Reset `pending_exit_fence` to `None` at the end of the
     callable's emission (defensive cleanup).

**LLVM ordering selection:**

The `fence <ordering>` IR instruction is target-abstract; the
backend selects target-appropriate instructions:
- `dmb ish` on ARM for `seq_cst`
- `dmb ishld` on ARM for `acquire`
- `dmb ishst` on ARM for `release`
- `mfence` / `lfence` / `sfence` on x86 as appropriate
- `fence` (RISC-V) with appropriate `pred,succ` masks

This abstraction is exactly what Decision #22 wants: source-level
`$ [Acquire]` / `$ [Release]` / `$ [SeqCst]` is portable; the
target-specific instruction selection is LLVM's job.

**What this slice deliberately does NOT do:**

- **Per-operation ordering on individual loads/stores.** Today the
  fence is a *function-level* annotation (entry / exit). Future work
  could extend `Acquire` / `Release` / `SeqCst` to apply to specific
  `#shared` field accesses (Decision #21 territory) by attaching
  ordering to individual `load` / `store` instructions instead of
  separate `fence` instructions.
- **Atomic operations (`atomicrmw`, `cmpxchg`).** These are needed
  for the rotor-lock acquire / release primitives (ADR 0005 /
  Decision #26) and the `#shared` field with `LockingDiscipline`.
  Future slice when Decision #21 / #26 implementation lands.
- **Cross-function ordering inference.** A `@fn` calling a
  `$ [Acquire]` `#effect` doesn't currently inherit any ordering;
  ADR 0003 Q2's row-direction discussion is the right model when
  `@fn` lowering needs ordering.

**Tests (10 new fence tests, codegen crate now 61 total, was 51):**

- `Acquire` → entry fence (no exit).
- `Release` → exit fence before `ret` (no entry).
- `Acquire` + `Release` → both ends fenced.
- `SeqCst` → both ends `seq_cst`.
- `SeqCst` supersedes `Acquire` / `Release` (no double-fences).
- No ordering trait → no fences emitted.
- `Acquire` on `#interrupt` works the same as on `#effect`.
- `Release` on `#transition` works the same.
- Explicit `return expr;` (not just falling through) — exit fence
  still goes before the `ret <ty> <val>`.
- `Hardware` / `Realtime` / `LockingDiscipline` / `PureState` /
  `Encapsulated` traits emit no fences — declarative-only consumers.

Workspace remains green; clippy clean.

This closes the v0.1 codegen surface for the locked Decision #22
imperative-side traits. The remaining unimplemented codegen
consumer is `LockingDiscipline` (gated to v0.7+ alongside Decision
#21 / #26 implementation — needs the rotor-lock runtime
infrastructure).

### Added — Codegen slice 3: §8.4 automaton state + effects + transitions + `#mutate` (2026-05-07)

The substantive v0.1 firmware piece. Slice 1 lowered `@fn` bodies;
slice 2 added typing integration + composite types + deref. Slice 3
covers the §8.4 surface: per-automaton state structs, effect /
transition / interrupt LLVM functions, `#mutate` block + sugar
mutation, and automaton field reads through `getelementptr` + `load`.

**The headline shape — a full v0.1 firmware program now lowers:**

```clifford
#automaton Counter { value: u32; }
#effect bump() #mutates: [Counter] { Counter.value += 1u32; }
#effect reset() #mutates: [Counter] { Counter.value = 0u32; }
#effect main() #mutates: [Counter] {
  #> bump();
  #> bump();
  #> reset();
}
```

→

```llvm
%struct.Counter = type { i32 }
@Counter.state = global %struct.Counter zeroinitializer

define void @bump() {
entry:
  %tmp.0 = getelementptr %struct.Counter, %struct.Counter* @Counter.state, i32 0, i32 0
  %tmp.1 = load i32, i32* %tmp.0
  %tmp.2 = add i32 %tmp.1, 1
  store i32 %tmp.2, i32* %tmp.0
  ret void
}

define void @reset() { … }   ; getelementptr + store i32 0
define void @main() {
entry:
  call void @bump()
  call void @bump()
  call void @reset()
  ret void
}
```

**Implementation (`crates/codegen/src/lib.rs`):**

- New `AutomatonInfo { name, fields: Vec<(String, String)>,
  is_register_block: bool }`. The `fields` vec preserves declaration
  order — its index is the LLVM struct field index used by
  `getelementptr`.
- `Emitter` gains `automatons: HashMap<String, AutomatonInfo>` (built
  by pass 1) and `enclosing_owner: Option<String>` (set when emitting
  a `#transition` body so `Self.field` reads resolve correctly).
- New three-pass `lower()`:
  1. `collect_automatons` — walks every `Item::Automaton`, lowers
     each field's IR type, populates the registry.
  2. `emit_automaton_state_structs` — emits
     `%struct.<Name> = type { … }` and
     `@<Name>.state = global %struct.<Name> zeroinitializer` for
     every non-register-block automaton.
  3. Function emission — `@fn` (slice 1+), `#effect` (new),
     `#interrupt` (new), and per-automaton `#transition`s (new).
- New `Emitter::emit_effect` — lowers like `@fn` but in
  imperative-layer context (mutation statements / proc calls work).
- New `Emitter::emit_interrupt` — same shape as effect; the source
  name becomes the LLVM linker symbol per Decision #10. Section
  attribute (`.interrupts`) and target-specific calling convention
  defer to slice 4.
- New `Emitter::emit_automaton_transitions` — walks each automaton's
  inner `#transition`s. Each transition becomes
  `define void @<Owner>_<transition_name>()` (namespaced) so
  cross-automaton names don't clash.
- New `Emitter::emit_mutate` — `#mutate Auto { f1 = e1, f2 = e2 };`
  lowers to a sequence of `getelementptr` + `store` per field.
- New `Emitter::emit_mutate_short` — `Auto.field <op>= expr;` sugar.
  `=` is a plain `store`; `<op>=` is `load` + op + `store`. Compound
  ops (`+= -= *= /= %= &= |= ^= <<= >>=`) handled via
  `compound_assign_opcode` helper.
- New `Emitter::emit_proc_call` — `#> name(args);` lowers to an
  LLVM `call`. The callee symbol is mangled `<Owner>_<name>` if the
  resolver records it as a transition of the enclosing automaton;
  otherwise emit the bare name (effect / interrupt).
- New `Emitter::emit_field_access` — `Auto.field` /
  `Self.field` (in expression position) lowers to `getelementptr` +
  `load`. `Self` resolves to `enclosing_owner` (set by the transition
  emission); outside a transition, `Self.field` surfaces as
  `NotYetImplemented`.
- New `compound_assign_opcode(op, ir_ty)` free helper — maps each
  `AssignOp` variant to the LLVM opcode for its load+op+store
  expansion.

**What slice 3 deliberately defers (slice 4+):**

- **Register-block automatons** (`#address: 0x…`). Their fields lower
  to volatile loads/stores at fixed addresses (Decision #6), not
  through a global state struct. Slice 3 records the
  `is_register_block` flag and surfaces `NotYetImplemented` on any
  attempt to mutate or read a register-block field.
- **Multi-state automatons** with `#states: [Init, Running, Halted]`
  — need a state-tag field added to the struct and `#> proc()` calls
  to dispatch on current state. Slice 3 lowers monoid (single-state
  / no `#states` clause) automatons only.
- **Transition-atomicity wrapping** (Refinement #5e): `cli`/`sti` for
  `R(A)` overlap on Cortex-M, `LDREX`/`STREX` on Cortex-A, etc.
  Decision #21 / #26 territory; slice 4+.
- **Interrupt section attribute** (`.interrupts`) and target-specific
  calling convention. Slice 4.
- **Indexed field assignment** (`#mutate Counter { buf[3] = …}`).
  Needs 2-level GEP. Slice 4.
- **Bit-field RMW** with target-atomic when concurrent writer exists
  (Decision #20). Slice 5+.
- **Generic effect monomorphisation** (Decision #16's
  `(generic_effect, interface_arg)` specialisation). Needs the
  monomorphisation pass.
- **`#interface` / `#impl` method bodies** — parser-slice work first.
- **Effect return values** at proc-call sites — slice 3 emits all
  proc calls as `call void`; the typing-aware return-type plumbing
  for effects/transitions is slice 4.
- **Sigma loops** (§5.8 + §8.4 codegen).

**Tests (15 new slice-3 tests, codegen crate now 51 total, was 36):**

- Single-field automaton emits state struct + global.
- Multi-field struct preserves declaration-order layout
  (`%struct.Multi = type { i32, i1, i8 }`).
- Register-block automaton skipped (no struct emitted).
- Effect lowers to `define`.
- `Auto.field = expr;` → GEP + store.
- `Auto.field += expr;` → GEP + load + add + store.
- `#mutate Auto { f1 = …, f2 = … };` block form → multiple GEP +
  store pairs.
- Field read in effect body → GEP + load.
- Transition lowers to namespaced `@<Owner>_<name>` fn.
- `Self.field` (read position) inside transition resolves to owner.
- Proc call to effect uses bare name; proc call to transition uses
  namespaced `<Owner>_<name>`.
- Interrupt emits with source name as linker symbol.
- Register-block field read → E0810 (slice 4 work).
- Full canonical Counter program (automaton + effects with mutation
  sugar + compound assign + proc calls) lowers cleanly.

The pre-slice-3 test `non_fn_items_silently_skipped` was renamed to
`non_fn_items_now_lowered_per_slice_3` and updated to assert the
new state-struct emission.

Workspace remains green; clippy clean.

**v0.1 firmware milestone status:** the codegen surface now covers
the full *non-register-block, monoid-automaton* program shape. A
program with `@fn`s, `#effect`s, `#interrupt`s, monoid `#automaton`s
with `#mutate` / sugar / `#> proc()` / field reads goes from `.cl`
source all the way to runnable `.ll` IR through the full pipeline.
The remaining v0.1 firmware pieces — register-block lowering,
multi-state automatons, interrupt section attributes — are slice 4.

### Added — Codegen slice 2: `Typing` integration + sign-aware ops + composite types + deref/negation (2026-05-07)

Second codegen slice. Slice 1 lowered `@fn` bodies using syntactic
type guesses (`infer_expr_ir_type`); slice 2 wires in the
authoritative `Typing` from `clifford-types` and adds the unary,
deref, signed-int, and composite-type pieces.

**Public API change (breaking):**

```rust
// Before (slice 1):
pub fn lower(program: &Program, module_name: &str) -> Result<String, Vec<CodegenError>>;

// After (slice 2):
pub fn lower(
    program: &Program,
    resolution: &Resolution,
    typing: &Typing,
    module_name: &str,
) -> Result<String, Vec<CodegenError>>;
```

The CLI driver passes the upstream phase outputs through; tests in
this crate use the standard
`tokenize → parse → resolve → infer → lower` pipeline.

**Implementation (`crates/codegen/src/lib.rs`):**

- `Emitter` now holds `&Resolution` and `&Typing`; `LocalBinding`
  struct (replacing the previous tuple) tracks `name` + SSA
  `value` + recorded `ir_type` so path lookups know the right LLVM
  type without re-walking typing per use.
- New `expr_ir_type(&Expr) -> String` consults `Typing::lookup`
  first; falls back to syntactic clues only when typing is silent.
  Removes the syntactic-only `infer_expr_ir_type`.
- New `expr_is_signed_int` for sign-aware op selection.
- New `bind_via_identity` consolidates the `add 0,v` / `fadd 0.0,v`
  binding pattern; falls back to value-passthrough for non-scalar
  types.

**Sign-aware integer ops:**

- `i8` / `i16` / `i32` / `i64` / `isize` → `sdiv` / `srem`
- `u8` / `u16` / `u32` / `u64` / `usize` → `udiv` / `urem`
- Driven by the operand's recorded `Type` (signed vs unsigned).

**Unary expressions (new in slice 2):**

- `-x` (integer) → `sub T 0, %x`
- `-x` (float) → `fneg T %x`
- `!x` (bool) → `xor i1 %x, true`
- `~x` (integer) → `xor T %x, -1`
- `*p` (deref) → `load T, T* %p` — pointee type read from the
  operand's recorded `Type::Ref { inner, … }`

**Composite types (new in slice 2):**

- `&T` / `&mut T` → `T*` (mutability-as-attribute is a future slice
  — IR-type form is `T*` for both)
- `[T; N]` → `[N x T]`
- `[T]` (slice) → `{T*, i64}` standard fat-pointer layout per §8.3
- `(T1, T2, …)` (tuple) → LLVM struct `{T1, T2, …}`
- `Range<T>` → `{T, T}` (lo, hi pair) — sigma-loop slice will
  refine this
- `Type::StringSlice` → `{i8*, i64}`
- Nominals (aliases / ADTs) and `Unknown` lower as `i32`
  best-effort; ADT lowering with tagged-union representation lands
  in codegen slice 3.

**`type_to_ir(&Type) -> String`** is a free function mirroring
`Emitter::lower_type` but operating on the semantic `Type`. Used
by `expr_ir_type` and the call-return type path.

**Lookup helpers:**

- `lookup_local(name)` — returns the SSA value-ref string
- `lookup_local_ir_type(name)` — returns the recorded IR type

**Tests (16 new slice-2 tests, codegen crate now 36 total, was 20):**

- `i32` / `i64` div → `sdiv`; rem → `srem`
- `u32` / `usize` div still uses `udiv` (regression guard)
- `isize` → signed; `usize` → unsigned (the i64-as-pointer-sized
  ambiguity resolved correctly)
- Unary `-x` int / `!x` bool / `~x` int — each new IR shape
- `&T` and `&mut T` signatures lower to `T*`
- `[T; N]` signature → `[N x T]`
- `(T1, T2)` signature → struct
- `*p` deref → typed `load`
- `let x: u8 = …` recorded IR type honored at path-position read
  (slice 1 would have defaulted to i32; slice 2 picks i8)
- Bool-returning call `call i1 @returns_bool()` (slice 1 always
  picked i32 for call return types; slice 2 reads typing)

The pre-slice-2 `unsupported_type_emits_e0810` test was retargeted
from `&u32` (now supported) to `access<u32>` (still deferred to
codegen slice 3+ for nominal-pointer provenance).

Workspace remains green; clippy clean.

The codegen surface now covers the full v0.1 *pure-`@fn`* program
shape with primitives, refs, arrays, slices, tuples, all unary +
binary ops over integers and bool, deref, and direct calls. The
remaining v0.1 firmware piece is **§8.4 automaton/transition/effect
lowering** (codegen slice 3 — the substantive piece for the QEMU
integration milestone).

### Added — Codegen slice 1: text-form LLVM IR for @fn + primitives + arithmetic + calls (2026-05-07)

First real lowering in `clifford-codegen`. The crate was a stub with
a single `NotYetImplemented` error code (E0810) since Phase-4
scaffolding; this slice fills it in for the v0.1 minimum surface.

**The decision recorded in `crates/codegen/Cargo.toml`:**

> Text-form LLVM IR emission in v0.1 (no native LLVM linkage). The
> inkwell-vs-llvm-sys decision is deferred until a slice needs the
> native binding (target-machine introspection, JIT, or in-process
> verification). Until then the IR is emitted as a `.ll` text the
> user can pipe to `llc` / `clang` externally.

**What lowers in this slice:**

- **Module header.** `; ModuleID = '<name>'` + `source_filename =
  "<name>"` per LLVM convention.
- **Primitive types** (§4.1):
  - `bool` → `i1`
  - `u8`/`i8` → `i8`, `u16`/`i16` → `i16`, `u32`/`i32`/`char` →
    `i32`, `u64`/`i64` → `i64`
  - `usize`/`isize` → `i64` (v0.1 default 64-bit target;
    target-aware lowering is a future slice)
  - `f32` → `float`, `f64` → `double`
  - `()` (unit) → `void`
- **`@fn` declarations** with primitive params + return types +
  bodies. One `define <ret_ty> @<name>(...)` per `@fn`; per-fn SSA
  counter reset; `entry:` label per function.
- **Statements:** `Let`, `LetShort`, `Expr`, `Return(Some)` /
  `Return(None)`. `let` and `let :=` bindings emit an SSA-add-zero
  identity to give the binding a named SSA temp (LLVM optimises it
  away). Future slice with allocator/store machinery will replace
  this idiom for mutable bindings.
- **Expressions:** integer / hex / binary / boolean literals, path
  expressions (single-segment, resolves to local / param), parens,
  binary arithmetic (`+`, `-`, `*`, `/`, `%`), direct function
  calls (single-segment Path callee).
- **Hex / binary literals** lowered to their decimal IR forms (LLVM
  text accepts decimal integer constants).
- Per-`@fn` SSA reset (`%tmp.0` starts each function), so callers
  and callees don't share the temp namespace.

**Public API:**

- `pub fn lower(program: &Program, module_name: &str) -> Result<String,
  Vec<CodegenError>>` — entry point. Returns the `.ll` text on
  success; errors accumulate across the program in source order.
- `CodegenError` enum:
  - `E0810 NotYetImplemented { what: &'static str }` — AST shape
    not in this slice (e.g. `"reference type"`, `"tuple
    expression"`, `"sigma loop"`).
  - `E0811 UnresolvedName { name }` — internal safety net for
    upstream resolver bugs.
  - `E0812 BadLiteral { literal, reason }` — internal safety net
    for malformed literals after upstream typing.

**What slice 1 deliberately defers (subsequent codegen slices):**

- **§8.4 Automaton/transition/effect lowering** — state struct per
  non-register-block automaton; state-tag field for multi-state
  automata; one LLVM function per effect / transition / hardware
  mutator / interface-method specialisation; transition-atomicity
  wrapping per Refinement #5e (cli/sti or LDREX/STREX based on R(A)
  and target); register-block field reads/writes as volatile
  loads/stores at `address + offset` (Decision #6); bit-field RMW
  with target-atomic when concurrent writer exists (Decision #20).
- **§8.5 Interrupt handler emission** — `#interrupt NAME` produces
  an LLVM function with linker symbol `NAME`, target-specific
  calling convention, `.interrupts` section (Decision #10).
- **§8.3 Composite types** — references (`T*` with `noalias` for
  `&mut`), arrays (LLVM `[N x T]`), slices (`{T*, i64}`), tuples
  (LLVM struct), ADTs (tagged-union representation).
- **Sigma loops** — counted loop with bounds-check elision (§5.8).
- **Decision #22 codegen consumers** — `Acquire` / `Release` /
  `SeqCst` memory-ordering fences (consumed by the v0.4-α slice
  when imperative-callable lowering lands).
- **Native LLVM binding** (inkwell or llvm-sys) — deferred until a
  slice needs it for target-machine introspection, JIT, or
  in-process IR verification. v0.1 ships text-form `.ll` only.
- **`Typing` integration** — slice 1 uses a syntactic guess for IR
  types (literal suffixes + path-default-i32 + binary-operand-of-
  lhs); a typing-aware future slice will replace this with
  authoritative type info from `clifford-types`.
- **Optimisation passes** — none in v0.1; LLVM's own passes do the
  heavy lifting downstream.

**Tests (20 total, codegen crate previously had 1 smoke test):**

- Module header (ModuleID, source_filename).
- Non-`@fn` items silently skipped (partial program lowers cleanly).
- `@fn` no-args / void return; with-params + return; bool param →
  `i1`; integer literal return; arithmetic; multiple ops; call
  expression with typed args; `let` binding with `_x: u32`; `let
  :=` binding; multiple fns each emit independently.
- E0810 surfaces correctly for unsupported expressions (tuple) and
  unsupported types (reference).
- Primitive-type-mapping smoke test enumerating all 13 primitives
  → IR-type table.
- Hex literal `0xFFu32` lowers to `255`; binary `0b1010u32` →
  `10`.
- Determinism (same input → same output) and snapshot-style locks
  for the canonical add-fn shape and a call-chain shape.

**Cargo.toml changes:**

- `clifford-codegen` now depends on `clifford-ast` (was missing,
  needed to walk the AST).
- New dev-dependencies: `clifford-lexer`, `clifford-parser` (tests
  parse real source for end-to-end coverage).

Workspace remains green; clippy clean.

This slice unblocks the v0.1 GA milestone path: combined with the
existing lexer/parser/resolver/types/check/effect/ortho pipeline, a
program of pure `@fn`s with primitives + arithmetic + calls now
goes from `.cl` source to runnable `.ll` IR.

### Type Checker — Slice T4e: compound-position generic unification for variant calls (2026-05-07)

Fifth slice of type-checker path-resolution work. T4d added variant-
call typing with leaf-only generic-param pinning; T4e walks compound
positions (`(T, T)`, `&T`, `[T; N]`, `Pair<T>`) so generic params pin
through nested structure too.

**The headline gap T4d left:**

```clifford
@type Pair<T> = | Both((T, T));      // declared arg = (T, T)

@fn make() -> Pair<u32> {
  return Pair::Both((5u32, 6u32));   // T4d: T not pinned → result Pair<Unknown>
                                     // T4e: T = u32 pinned via tuple unification
}
```

**Implementation (`crates/types/src/lib.rs`):**

- New `unify_pin(declared, actual, params, bindings, registry)` free
  function. Recursively walks `declared` and `actual` in parallel:
  - Leaf generic-param reference (`Nominal{path:[name], args:[]}`
    where `name ∈ params`) — pins or checks the binding.
  - Matching compound shapes (`Tuple ↔ Tuple` of same arity, `Ref ↔
    Ref` of same mutability, `Array ↔ Array` of same size, `Slice ↔
    Slice`, `Range ↔ Range` of same inclusivity, `Nominal ↔ Nominal`
    of same path + arity) — recurses through corresponding positions.
  - Fallback: substitute current bindings into `declared` and run
    `types_compatible` for structural+alias-following equality.
  - Returns `Result<(), ()>` — caller diagnoses on `Err`.
- Permissive on `Type::Unknown` on either side (matches
  `types_compatible` behaviour; avoids cascading errors when one
  position is upstream-unresolved).
- `variant_call_type` rewritten to call `unify_pin` per arg. The
  diagnostic surface is unchanged (E0522 with `displayed_expected`
  showing the substituted form so users see `u32`, not `T`, after
  partial inference).

**Semantics enforced:**

- `Pair<T> = | Both((T, T));` + `Pair::Both((5u32, 6u32))` → pins
  `T = u32`; result is `Pair<u32>`.
- `Pair<T> = | Both((T, T));` + `Pair::Both((5u32, true))` → first
  position pins `T = u32`; second position conflicts → E0522.
- `Boxed<T> = | Wrap(&T);` + `Boxed::Wrap(&x)` (where `x: u32`) →
  pins `T = u32` through the `Ref`.
- `Buf<T> = | Of([T; 4]);` + `Buf::Of([1u32, 2u32, 3u32, 4u32])` →
  pins `T = u32` through the `Array`.
- `Both<A, B> = | Pair((A, B));` + `Both::Pair((5u32, true))` →
  pins `A = u32`, `B = bool` independently from tuple positions.
- `W<T> = | M(&(T, T));` (doubly nested) → walks Ref then Tuple.
- Shape mismatch (`(T, T)` declared, `u32` actual) → E0522.
- E0522 diagnostic uses `displayed_expected` with substitution
  applied: in `Both<A, B> = | Pair(A, B, A);` calling
  `Both::Pair(1u32, true, false)`, the third arg's expected type
  shows as `u32` (substituted from A's pin), not the raw `A`.
- `@type Count = u32;` + `W<T> = | Wrap(T);` + `W::Wrap(n)` (where
  `n: Count`) → unify_pin's structural fallback unaliases Count to
  u32 before pinning T.

**What T4e deliberately defers:**

- Trait-bound satisfaction on generic params (`@type Vec<T: Copy>`).
  The bound list is parsed and stored on `GenericParam.bounds`; a
  future slice will check that pinned type arguments satisfy those
  bounds. Today bounds are silent.
- Inference from let-annotations *back* to variant constructors
  (`let x: Result<u32, bool> = Ok(5u32);` would benefit from
  bidirectional flow that uses `bool` to pin `E` via the
  annotation). v0.4+ HM-extension work.
- Where-clause-style constraints, higher-kinded params, associated
  types — full HM territory; out of scope.

**Tests (11 new T4e tests, types crate now 176 total, was 165):**

- Tuple position pins T (multi-arg variant `Both(T, T)`).
- Tuple *inside* one arg pins T (single arg `(T, T)`) — the T4d gap.
- Tuple-inside-arg conflict → E0522.
- Ref position pins T (`&T`).
- Array position pins T (`[T; 4]`).
- Two-param tuple pins independently (A, B from same tuple).
- Nested compound (`&(T, T)`).
- Shape mismatch (tuple-vs-non-tuple) → E0522.
- Partial-pin substituted in diagnostic (E0522 expected shows
  `u32`, not `A`).
- Alias unfolds during unification (`Count = u32` then `W::Wrap(n)`
  with `n: Count` pins `T = u32` via structural fallback's
  `types_compatible`).
- Baseline two-param flat case (sanity).

Workspace remains green; clippy clean.

The T4 series — path resolution surface for the type checker — is
now substantively complete:

- T4a: `Type::Nominal` AST + simple `Path → Nominal` translation.
- T4b: `@type` alias following + ADT terminal markers.
- T4c: generic alias substitution + path validation (E0518/E0519).
- T4d: ADT variant resolution + variant-call typing (E0521/E0522).
- T4e: compound-position generic unification for variant calls.

What's left in the T4 line: trait-bound satisfaction (T4f? — folded
into a future HM slice), module-qualified paths (T4g+ — needs a
module system), inference flow from annotations to constructors
(v0.4+ HM extension).

### Type Checker — Slice T4d: ADT variant resolution + variant-call typing (2026-05-07)

Fourth slice of type-checker path-resolution work. T4a-T4c covered
type-position paths (alias following, generic substitution, path
validation). T4d covers **expression-position multi-segment paths
that resolve to ADT variants**: `Color::Red`, `Maybe::Some(5u32)`,
`Result::Ok(5u32)`, etc.

**Implementation (`crates/types/src/lib.rs`):**

- `NominalDecl::Adt` extended with `variants: Vec<VariantInfo>` —
  each variant carries its name and arg-type list (with generic-
  parameter references surviving as `Type::Nominal { path: [param],
  args: [] }` leaves, same shape as alias targets).
- New `VariantInfo { name, args }` struct.
- `build_type_registry` now populates the variants from the AST.
  `VariantData::Tuple(types)` and `VariantData::Struct(fields)`
  both flatten to positional args (T4d treats struct-style
  variants positionally; named-field semantics is post-T4d).
- New `TypeRegistry::lookup_variant(path) -> Option<(adt_name,
  params, &VariantInfo)>` for two-segment paths where segment[0]
  is a registered ADT and segment[1] matches one of its variants.
- `Inferer::infer_expr` for `ExprKind::Path(segments)` extended:
  multi-segment path that resolves to a unit-like variant of a
  non-generic ADT yields the ADT directly (`Color::Red` → `Color`).
  Data-carrying or generic-ADT variants referenced bare (without
  a Call) yield `Type::Unknown` — call-site typing fills them in.
- `Inferer::call_type` extended: when callee is a multi-segment
  path resolving to a variant, dispatches to the new
  `variant_call_type` helper.
- New `Inferer::variant_call_type` performs:
  - Arity check on variant args → `E0521 VariantArityMismatch` on
    fail; returns a best-effort `Nominal { adt_name, args:
    [Unknown; param_arity] }`.
  - Per-arg type check. When the declared arg is a leaf
    generic-param reference (`T` for `@type Result<T, E>` —
    declared as `Nominal{path:[T]}`), the actual arg's type pins
    `T`'s instantiation. First occurrence binds; subsequent
    occurrences must match (else `E0522 VariantArgMismatch`). Non-
    generic-leaf declared types use plain `types_compatible`.
  - Builds the result `Nominal { adt_name, args: [...] }` from
    the bindings; uninferred params become `Type::Unknown`.

**Two new error variants:**

- `E0521 VariantArityMismatch { adt_name, variant_name, expected,
  actual, at }` — diagnostic shows `Maybe::Some` (qualified form)
  so users see exactly which variant.
- `E0522 VariantArgMismatch { adt_name, variant_name, arg,
  expected, actual, at }` — same qualified-name format; `arg` is
  1-based.

**Semantics:**

- `@type Color = | Red | Green | Blue;` + `Color::Red` →
  `Type::Nominal { path: ["Color"], args: [] }`.
- `@type Maybe = | None | Some(u32);` + `Maybe::Some(5u32)` →
  `Type::Nominal { path: ["Maybe"], args: [] }`.
- `@type Result<T, E> = | Ok(T) | Err(E);` + `Result::Ok(5u32)`
  → `Type::Nominal { path: ["Result"], args: [Primitive(U32),
  Unknown] }`. The let-annotation `Result<u32, bool>` is
  structurally compatible because `Unknown` short-circuits in
  `types_compatible`.
- `Maybe::Some(true)` → E0522 (declared `u32`, got `bool`).
- `Maybe::Some(5u32, 6u32)` → E0521 (arity 1 vs 2).
- Struct-style `@type Shape = | Circle { r: f32 };` flattens to
  positional: `Shape::Circle(1.0f32)` works; named-field syntax
  (`Shape::Circle { r: 1.0f32 }`) is post-T4d work.

**What T4d deliberately defers:**

- Bidirectional inference for non-leaf generic positions (e.g. a
  variant with arg type `(T, T)` — T4d's pin-on-leaf doesn't reach
  through compounds).
- Named-field syntax for struct-style variant constructors —
  positional only in T4d.
- Single-segment unqualified variant references (`Red` without
  `Color::` prefix). Today these go through the local-lookup arm
  and return Unknown if not declared as locals; future slice could
  add scope-based variant lookup.
- Unknown-variant diagnostic in the type checker — falls through
  to the resolver's existing surface; type checker just doesn't
  crash.
- Multi-segment paths in *type position* (`let _: Result::Ok = …`)
  — semantically nonsensical without a module system; T4d does
  not enable them.

**Tests (11 new T4d tests, types crate now 165 total, was 154):**

- Unit variant bare path → ADT type.
- Unit variant in let annotation typechecks.
- Data-carrying variant constructor call typechecks.
- Variant arg type mismatch → E0522.
- Variant arity mismatch (too many args) → E0521.
- Variant arity mismatch (too few args) → E0521.
- Generic ADT: arg pins first param (`Result::Ok(5u32)`).
- Generic ADT: arg pins second param (`Result::Err(true)`).
- Unknown variant doesn't crash the type checker.
- Non-ADT first segment (`MyAlias::Foo`) doesn't crash.
- Struct-style variant flattens positionally.

Workspace remains green; clippy clean.

This closes the T4d sub-slice. T4e remains: bound-aware trait
satisfaction on generic params; full HM unification for non-leaf
generic-arg positions. T4f / module work would handle
multi-segment paths beyond ADT variants.

### Added — Decision #22: layer-aware trait validation (E0544 TraitLayerMismatch) (2026-05-05)

Closes the layer-direction gap from the earlier Decision #22
trait-validation slice. The previous slice validated trait names
universally — `Realtime` on a `@fn` was accepted, `Pure` on a
`#effect` was accepted. ADR 0003 Q2's pure-side / imperative-side
distinction now has compile-time enforcement.

**Implementation (`crates/types/src/lib.rs`):**

- New `TraitLayer` enum: `Pure`, `Imperative`, `Universal`. The first
  two correspond to the existing `PREDECLARED_PURE_TRAITS` /
  `PREDECLARED_IMPERATIVE_TRAITS` constants; `Universal` covers
  user-defined `@trait` declarations.
- `TraitLayer::is_usable_on(callable_layer)` answers "may a trait
  with this layer appear on a callable in *that* layer?":
  - `Universal → *` always allowed.
  - `Pure → Pure` only.
  - `Imperative → Imperative` only.
- `TraitRegistry` upgraded from `HashSet<String>` of known names to
  `HashMap<String, TraitLayer>` so each name carries its layer
  classification.
- New `TraitRegistry::layer_of(name) -> Option<TraitLayer>`.
- `check_traits` now takes the callable's layer, and emits the right
  diagnostic per case:
  - Unknown name → `E0541 UnknownTrait` (existing).
  - Known name, wrong layer → `E0544 TraitLayerMismatch` (new).
  - Known name, correct layer → silent.
- New `TypeError::TraitLayerMismatch { trait_name, expected_layer,
  callable, actual_kind, at }` variant. The diagnostic names the
  trait, its required layer (`"pure"` / `"imperative"`), the callable,
  the callable's syntactic kind (`"@fn"` / `"#effect"` / etc.), and
  the byte offset.

**Spec (`docs/CLIFFORD_SPEC.md` §2.5):**

- Added a normative paragraph after the imperative-traits table
  describing the layer-aware validation rule and its diagnostic
  shape.

**Semantics enforced:**

- `Pure` / `Readable` / `Observable` / `Opaque` on `#effect` /
  `#interrupt` / `#transition` → E0544.
- `Hardware` / `Realtime` / `Acquire` / `Release` / `SeqCst` /
  `LockingDiscipline` / `PureState` / `Encapsulated` on `@fn` →
  E0544.
- User-defined `@trait MyTrait { … }` valid on both layers (no
  E0544; the trait is `Universal`).
- Unknown name → E0541 only (no double-report; we don't know the
  layer).
- Mixed-layer list (`$ [Pure, Realtime]` on `@fn`): each entry
  checked independently — `Pure` validates, `Realtime` triggers
  E0544.

**Tests (11 new layer-mismatch tests, types crate now 154 total,
was 143):**

- Pure-side trait on `#effect` / `#interrupt` / `#transition` →
  E0544 (one test per kind).
- Imperative trait on `@fn` → E0544; per-name smoke test for
  `Acquire` / `Release` / `SeqCst`.
- Per-name iteration over both predeclared sets: every imperative
  trait on `@fn` rejected; every pure trait on `#effect` rejected
  — guards against accidental misclassification if someone adds
  a new predeclared trait.
- User-defined `@trait` validates on both layers (Universal).
- Unknown trait → E0541 only, NOT E0544.
- Mixed-layer list: correct-layer entries validate, wrong-layer
  entries get E0544 independently.
- Smoke test enumerating every predeclared name on its correct
  layer remains silent (regression guard for the per-set
  classification).

What's still deferred (future slices):
- Explicit layer tags on `@trait` declarations (currently
  Universal). Would need a new syntactic form like `@trait MyTrait
  $ [Pure] { … }` or similar.
- Trait-bound checking on generic parameters (`@fn f<T: Realtime>`
  — needs full HM unification anyway).
- Cross-layer call row inheritance (ADR 0003 Q2's "`@fn → @fn`
  one-directional row check") — this is the *call-site* check, a
  separate concern from the *declaration-site* layer check this
  slice adds.

Workspace remains green; clippy clean.

### Added — Decision #23: mutual-recursion detection via Tarjan SCC (E0543) (2026-05-05)

Closes the documented gap from the v0.2-β totality slice. Direct
self-recursion was already caught (E0540 TotalityViolation); the
canary test `mutual_recursion_not_yet_caught` flagged that mutual
recursion was deliberately deferred. This PR replaces the singleton
direct-recursion finder with a unified SCC-based pass that catches
both shapes.

**Implementation (`crates/check/src/lib.rs`):**

- New `CheckError::MutualRecursionViolation { fn_names, decl_at_first }`
  variant (E0543). One diagnostic per SCC; member names listed in
  lex-smallest-first canonical order so the same cycle isn't reported
  twice from different starting points.
- `check_totality` rewritten around Tarjan's strongly-connected-
  components algorithm:
  1. Build the `@fn` call graph (`HashMap<String, HashSet<String>>`)
     by walking each `@fn` body and recording direct calls to other
     `@fn`s. `#`-layer callees are deliberately excluded (the
     boundary checker rejects cross-layer calls separately;
     totality is a pure-side discipline).
  2. Run Tarjan SCC. The implementation is textbook Tarjan with
     deterministic neighbour ordering (sorted) and deterministic
     entry-point ordering (sorted) so SCC contents and order are
     stable across hash-iteration runs.
  3. For each SCC: skip non-cyclic singletons; skip cycles whose
     every member is `@partial`; emit E0540 for self-loop singletons
     (preserving the existing direct-recursion diagnostic shape) or
     E0543 for size-≥-2 cycles (new mutual-recursion diagnostic).
- New helpers: `build_fn_call_graph`, `collect_fn_calls_in_block` /
  `_in_stmt` / `_in_expr` (walk `@fn` bodies for direct calls),
  `tarjan_scc` (the SCC algorithm).
- Existing `SelfRecursionFinder` retained — it's now used to find
  the *first* self-call site within a singleton SCC so the E0540
  diagnostic still points at the actual call expression rather than
  just the declaration.

**Semantics (closes ADR 0003 v0.2-β + this slice):**

- `@fn even → odd → even` (size-2 cycle, no `@partial`) → E0543
  with `fn_names = ["even", "odd"]`.
- `@fn a → b → c → a` (size-3 cycle) → one E0543 with
  `["a", "b", "c"]`.
- `@partial @fn even → @partial @fn odd → even` (all-`@partial`
  cycle) → silent.
- `@partial @fn even → @fn odd → even` (subset `@partial`) →
  E0543 still fires; users must mark *every* member.
- `@fn loop_me(n) { return loop_me(n); }` (size-1 cycle / self-loop)
  → E0540 (existing direct-recursion shape, preserved).
- `@fn h → g → f` (linear chain, no cycle) → silent.
- Two disjoint cycles → two E0543s, one per SCC.

**What this slice deliberately defers:**

- Structural-recursion three-rule cut (ADR 0003 Q1: pattern-matched
  constructor args, sigma-bounded indexing, tail position) — common
  total recursions still need `@partial` today; v0.4+ slice will
  accept them automatically.
- `#`-layer callees in the graph — totality is pure-side only;
  cross-layer is the boundary checker's job.
- Bound-aware totality (e.g. recursing on a sigma-bounded index)
  — same v0.4+ slice as above.

**Tests (10 new, check crate now 70 total, was 61):**

- 2-member cycle → E0543 with both names lex-sorted.
- 3-member cycle reported as one E0543 with all members.
- All-`@partial` cycle → silent.
- Subset-`@partial` cycle → E0543 still fires.
- One E0543 per SCC, not per member (no triple-reporting on size-3
  cycles).
- Two disjoint cycles → two E0543s.
- Linear non-cyclic chain → silent.
- Self-loop (size-1 cycle) → E0540, NOT E0543 (distinct shapes
  preserved).
- Diagnostic carries `decl_at_first` pointing at the lex-smallest
  member's declaration byte.
- Isolated `@fn` (no calls) → silent.

The pre-slice canary test `mutual_recursion_not_yet_caught` was
flipped to `mutual_recursion_now_caught_as_e0543` and asserts the
new behaviour — the documented gap is closed.

Workspace remains green; clippy clean.

### Type Checker — Slice T4c: generic alias substitution + E0518/E0519 path validation (2026-05-05)

Third slice of type-checker path-resolution work. T4a translated
paths to `Type::Nominal` verbatim; T4b followed non-generic aliases.
T4c adds **generic alias substitution** (the headline feature) and
the **separate validation pass** that surfaces unknown-name and
arity-mismatch diagnostics at signature time.

**Implementation (`crates/types/src/lib.rs`):**

- `NominalDecl` upgraded from tuple variants to struct variants
  carrying generic-parameter names:
  - `Alias { params: Vec<String>, target: Type }`
  - `Adt { params: Vec<String> }`
- New `Type::substitute(&HashMap<&str, &Type>) -> Type` helper:
  walks the type tree replacing single-segment `Nominal` leaves
  whose name matches a key in the mapping. Recurses through `Ref`,
  `Array`, `Slice`, `Tuple`, `Range`, and the args of generic-arg
  nominals; preserves atoms (`Unit`, `Primitive`, `StringSlice`,
  `Unknown`).
- `TypeRegistry::unfold_one` extended: when a nominal has args
  matching the alias's params arity, builds a `param-name → arg`
  mapping and applies `Type::substitute` to the alias body. Arity
  mismatches return `None` (don't unfold) — the validation pass
  reports E0519 separately.
- New `validate_nominal_paths(program, registry, errors)` pass
  walks every type-bearing position in the AST (`@fn` /
  `#effect` / `#interrupt` params + return types + bodies,
  `#automaton` field types + transition bodies, `@type`
  alias/ADT body type expressions) and validates each path
  against the registry. Threads `params_in_scope: &[String]`
  through the recursion so a `@type Pair<T> = (T, T);` body
  treats `T` as known (a generic parameter, not a missing
  declaration).
- Two new `TypeError` variants:
  - `E0518 UnknownNominalType { name, at }` — path doesn't
    resolve to any registered `@type` decl and isn't a generic
    param in scope.
  - `E0519 GenericArityMismatch { name, expected, actual, at }`
    — known nominal whose arg count differs from declared arity.

**Semantics enforced:**

- `@type Pair<T> = (T, T);` + `Pair<u32>` ⇒ unfolds to `(u32, u32)`
  (positive headline case).
- `@type Both<A, B> = (A, B);` + `Both<u32, bool>` ⇒
  `(u32, bool)`.
- `Pair<u32, bool>` (too many args) ⇒ E0519 expected=1 actual=2.
- `Pair` (no args) ⇒ E0519 expected=1 actual=0.
- `NotARealType` in any type position ⇒ E0518 with the user's name.
- `Container<NotReal>` ⇒ both `Container` (E0518) AND `NotReal`
  (E0518) reported — generic args walked even when outer name is
  unknown.
- `Pair<Foo, NotReal>` ⇒ E0519 (Pair has 1 param, given 2) plus
  E0518 (NotReal unknown) — both diagnostics surface.
- `@type Result<T, E> = | Ok(T) | Err(E);` + `Result<u32, bool>`
  ⇒ silent (ADT arity-checked, body types validated under T/E
  in scope).

**What T4c deliberately defers:**

- Trait-bound satisfaction on generic params (`@type Wrapper<T:
  Copy>`) — full HM-unification work.
- Multi-segment paths (`clifford::core::Option`) — module work,
  T4d+. Today they always trigger E0518.
- Variant-position resolution (`Result::Ok` in expression
  position) — T4d.
- Generic params on `@fn` declarations — parser slice for `@fn<T>(…)`
  is post-T4c; the validation pass already threads
  `params_in_scope` through, so the wiring is ready when the AST
  catches up.

**Tests:**

- *Types*: 15 new T4c tests (143 total, was 128).
  - Generic alias substitutes: one-param, two-param, used in call
    arg.
  - Arity mismatch: too many args; too few args; on ADT.
  - Unknown nominal: in param / return / let-annotation / nested
    in tuple / nested in generic args.
  - Known alias / known ADT with correct arity → silent.
  - `Type::substitute` direct unit tests: leaf replacement;
    leaf-not-in-mapping unchanged; recursion through `Tuple`,
    `Ref`, generic-arg nominals.
- Three pre-T4c test scaffolds updated to the new
  `NominalDecl::Alias { params, target }` struct-variant form.
- One T4b test (`t4b_generic_args_block_alias_unfolding`)
  re-comment-ed to reflect T4c's reading: a non-generic alias
  given args is now an arity mismatch (handled by the validation
  pass), not "args block unfolding."

Workspace remains green; clippy clean.

This closes the third T4 sub-slice. T4d (module resolution +
ADT-variant resolution) and T4e (trait bounds, full HM unification)
are the remaining pieces; both deferred until use cases push for
them.

### Added — Decision #24: `@snapshot Self.field` inside `#transition` rejected as E0553 (2026-05-05)

Third slice of the v0.2-β batch. Closes the last open piece from
Decision #24 / ADR 0004's locked Q2: `@snapshot Self.field` (or
`@snapshot Owner.field`) inside a `#transition` body is now
diagnosed as `E0553 SnapshotInImperative`, with the diagnostic
suggesting the canonical bare `Self.field` form.

**Parser (`crates/parser`):**

- `parse_snapshot_expr` now accepts `Self` as the first segment
  alongside ordinary `Ident`s. The parser stores the literal
  string `"Self"` on the AST so downstream crates can pattern-match
  on it. (Previously `@snapshot Self.field` failed to parse with
  "expected automaton name" — meaning users hit a bare parse error
  rather than the intended E0553.)
- New parser test `snapshot_self_field_parses_with_self_recorded`
  asserting the `Self` form lands as `Snapshot { automaton: "Self",
  field: ... }`.

**Resolver (`crates/resolve`):**

- The `ExprKind::Snapshot` arm now special-cases `automaton ==
  "Self"`: inside a transition body, resolves it to the enclosing
  transition's owning automaton so the existence + hidden-field
  checks operate on the real automaton name. Outside a transition
  (`@fn`, `#effect`, `#interrupt`), `Self` is meaningless and
  produces `NotAnAutomaton`.

**Check (`crates/check`):**

- New `CheckError::SnapshotInImperative { automaton, field,
  transition_name, owner, at }` variant (`E0553`). Diagnostic
  identifies the offending automaton (`"Self"` or the owner name
  literally), the field, the enclosing transition, and the owner;
  message reminds the user that bare `Self.field` is the canonical
  form.
- New `SelfSnapshotScanner` walker invoked from
  `walk_transition_decl`. Scans the transition body for
  `@snapshot` expressions whose automaton is `"Self"` or matches
  the transition's owner; emits one E0553 per occurrence (unlike
  E0540 which is first-call-wins, E0553 reports every redundant
  snapshot since each is independently fixable).
- Snapshots of *sibling* automata from inside a transition (e.g.
  `@snapshot OtherAutomaton.field`) are *not* flagged — observing
  external state is legitimate.

**Semantics enforced:**

- `@snapshot Self.field` in `#transition Counter::tick` → E0553
  (canonical: bare `Self.value`).
- `@snapshot Counter.field` in `#transition Counter::tick` → also
  E0553 (same redundancy with explicit owner name).
- `@snapshot OtherAutomaton.field` in any transition → silent.
- `@snapshot Self.field` in `@fn` body → resolver emits
  `NotAnAutomaton` (`Self` is meaningless there).
- `@snapshot` inside `#effect` / `#interrupt` bodies → silent
  (E0553 is transition-specific per ADR 0004 Q2).

**Tests:**

- *Parser*: 1 new test (236 total, was 235).
- *Check*: 7 new tests (68 total, was 53 — earlier this batch we
  added 13 totality tests). Coverage: `Self` field inside transition
  → E0553; owner-name field inside transition → E0553; sibling
  automaton silent; multiple redundant snapshots all reported;
  snapshot in `#> proc()` arg-position caught; snapshot in
  mutate-short RHS caught; `#effect` body snapshots silent;
  diagnostic carries full context (`automaton`, `field`,
  `transition_name`, `owner`, `at`).

Workspace remains green; clippy clean.

This closes the third of three planned slices (totality →
snapshot typing/gating → E0553 inside transitions). Decision #24
parser + type-inference + check support is now complete for v0.2-α/β
scope.

### Added — Decision #24: `@snapshot` type inference + `Readable`-row gating (E0550) (2026-05-05)

Second slice of v0.2-α follow-up work. The parser slice (`feat/v0.2a-
partial-snapshot-readable`) landed `@snapshot` AST nodes; this PR
gives them types and enforces ADR 0004 Q1's "controlled effect"
discipline by gating `@snapshot` from `@fn` bodies behind the
`Readable` row.

**Type inference (`crates/types/src/lib.rs`):**

- New `infer_expr` arm for `ExprKind::Snapshot { automaton, field }`:
  returns the field's declared type via the existing
  `automaton_field_types` registry. If the lookup fails (unresolved
  automaton or field), returns `Type::Unknown` — the resolver
  already reported E0403 / E0405 for that case, so a parallel
  E0xxx from the type checker would be noise.

**Readable-row gate (`crates/types/src/lib.rs`):**

- New `TypeError::SnapshotInUnreadableFn { fn_name, at, decl_at }`
  variant (`E0550`). Diagnostic names the offending `@fn`, the
  byte offset of the first `@snapshot` in the body, and the
  declaration site so users see where to add `$ [Readable]`.
- New `validate_snapshot_row_gates(program, errors)` pass walks
  every `Item::Fn`, runs a `SnapshotFinder` over the body, and if
  any `@snapshot` is found, verifies the function's `trait_list`
  contains `Readable`. Otherwise emits E0550. Per ADR 0004 P3,
  `#`-layer callables (`#effect` / `#interrupt` / `#transition`)
  are *not* gated — they are imperative and may always observe
  state.
- New `SnapshotFinder` walker mirroring `clifford-check`'s
  `SelfRecursionFinder`: visits Call args, MethodCall, Binary,
  Unary, Ref, Paren, Tuple, Array, ArrayRepeat, FieldAccess,
  Index, Cast, Range — every place a snapshot could hide.
  First-snapshot-wins → one E0550 per `@fn`.

**Semantics enforced:**

- `Pure` and `Observable` rows do *not* subsume `Readable` —
  `@snapshot` requires the explicit `Readable` row label per
  ADR 0003 P2's row design.
- Empty `$ []` and absent trait list both default to `[Pure]`
  (Emergent Rule 2), which lacks `Readable` — so they correctly
  emit E0550 when the body uses `@snapshot`.
- `@fn`s without any `@snapshot` are silent regardless of trait
  list (no spurious diagnostics).
- The diagnostic invariant `decl_at < snapshot_at < src.len()`
  holds.

**Tests (12 new tests, types crate now 128 total, was 116):**

- Snapshot yields field type (positive control); type-mismatch
  via wrong annotation correctly fires E0512.
- `$ [Readable]` accepted; missing/`$ [Pure]`/`$ [Observable]` →
  E0550.
- Snapshot in arg position / binary expression / let RHS — all
  caught by the SnapshotFinder.
- `@fn` without snapshot silent regardless of row.
- One E0550 per offending fn (multiple snapshots → one error).
- `#`-layer (`#effect`) snapshot silent (ADR 0004 P3).
- Diagnostic offset invariant verified.

Workspace remains green; clippy clean.

This is the second of three planned slices in the v0.2-β batch
(totality → snapshot typing/gating → E0553 inside transitions).

### Added — Check Slice 3: Decision #23 totality check (E0540) (2026-05-05)

First implementation of Decision #23 / ADR 0003's totality
requirement. Non-`@partial` `@fn`s with direct self-recursion now
emit `E0540 TotalityViolation`.

**Implementation (`crates/check/src/lib.rs`):**

- New `CheckError::TotalityViolation { fn_name, call_at, decl_at }`
  variant. The diagnostic carries the offending `@fn`'s name,
  the byte offset of the recursive call site, and the byte offset
  of the `@fn` declaration so users can find the place to add
  `@partial`. The `Display` form names both spans and explicitly
  suggests `@partial @fn` as the opt-out.
- New `check_totality(program, errors)` pass: walks every
  `Item::Fn` whose `partial = false`, builds a `SelfRecursionFinder`
  for that function's body, and emits E0540 if it finds any direct
  self-call. Runs as a separate pass (not interleaved with the
  layer-boundary walk) so totality errors surface even on `@fn`s
  whose bodies the boundary walker has nothing to report on.
- New `SelfRecursionFinder` walker that recursively visits every
  expression form in `@fn` bodies: `Call`, `Binary`, `Unary`,
  `Ref`, `Paren`, `Tuple`, `Array`, `ArrayRepeat`, `FieldAccess`,
  `Index`, `MethodCall`, `Cast`, `Range`. First-recursive-call
  wins (one E0540 per function, matching rustc's "report each
  error once" convention).

**Slice scope (per ADR 0003 implementation milestones):**

- **This slice (v0.2-β):** direct self-recursion → E0540 unless
  `@partial`. Most conservative possible form of the check.
- **v0.4+:** layered three-rule cut so common total recursions
  (constructor-arg destructuring, sigma-bound indexing, tail
  position) are accepted without `@partial`.
- **Future slice (no version yet):** mutual recursion via Tarjan
  SCC analysis over the `@fn` call graph. Today mutual recursion
  passes silently — documented gap, not a soundness bug. The
  test `mutual_recursion_not_yet_caught` is the canary; flipping
  it to `expect_err` is the marker for that future slice.

**Tests (13 new totality tests, check crate now 53 total, was 40):**

- Non-recursive `@fn` passes (positive control).
- Direct recursive `@fn` without `@partial` → E0540.
- Direct recursive `@partial @fn` is silent (opt-out works).
- Recursion buried in arg-position / let-RHS / paren / field-
  access receiver — walker finds it through compound forms.
- Calls to *other* fns (not self) silent (negative control).
- First-recursive-call-wins (multiple call sites → one E0540).
- Diagnostic carries `decl_at < call_at < src.len()` byte offsets.
- `@partial` on non-recursive fn is silent.
- Mutual recursion explicitly NOT yet caught (slice-scope canary).
- Totality runs alongside S1 boundary check (both errors fire).

Workspace remains green; clippy clean.

This is the first of three planned slices in this batch (totality
check → snapshot type inference → E0553 inside transitions).

### Added — Decision #22: imperative trait validation in `clifford-types` (E0541) (2026-05-05)

Second slice of Decision #22 implementation (parser scaffolding
landed earlier; this PR adds the *semantic* validation that closes
the loop). Today the parser accepts any identifier list in `$
[...]`; this PR rejects unknown trait names with `E0541 UnknownTrait`
so typos surface as compile errors instead of silent acceptance.

**Implementation (`crates/types/src/lib.rs`):**

- Two predeclared trait lists (the names locked in Decision #22 +
  ADR 0003):
  - `PREDECLARED_PURE_TRAITS = ["Pure", "Readable", "Observable", "Opaque"]`
  - `PREDECLARED_IMPERATIVE_TRAITS = ["Hardware", "Realtime",
    "Acquire", "Release", "SeqCst", "LockingDiscipline",
    "PureState", "Encapsulated"]`
- New `TraitRegistry { known: HashSet<String> }` built from the
  two predeclared lists ∪ every `Item::Trait` (user-defined
  `@trait Name { … }`) in the program.
- New `validate_trait_lists()` pass walks every `@fn`, `#effect`,
  `#interrupt`, and `#transition` and emits `E0541 UnknownTrait`
  for any entry not in the registry. Runs after the body walk so
  errors are collected in one pass.
- New `TypeError::UnknownTrait { trait_name, callable, kind, at }`
  variant. The diagnostic names the offending trait, the
  containing callable's source identifier, and the source-form
  kind (`@fn` / `#effect` / `#interrupt` / `#transition`) so users
  see what *they* wrote, not internal type names. The error
  message also enumerates the predeclared sets and points at
  `@trait` declarations as the user-defined route.

**ADR-driven semantics enforced:**

- The dropped `Diverges` trait (per ADR 0003 Q4 — superseded by
  `@partial`) is *correctly* rejected as unknown. Source code
  still using `Diverges` will fail to compile, signalling the
  user to switch.
- Empty `$ []` and absent trait list both pass without diagnostic
  (per Emergent Rule 2 — empty ≡ `[Pure]`).
- Cross-layer trait usage (e.g. `Realtime` on a `@fn`, or `Pure`
  on a `#effect`) is *not* layer-checked in this slice — both
  predeclared sets validate together. Layer-aware checking (which
  is more nuanced per ADR 0003 Q2's row-direction discussion)
  lands in a follow-up slice.

**Tests (15 new D22 trait-validation tests, types crate now 116
total, was 101):**

- Each predeclared pure trait accepted on `@fn`.
- Predeclared imperative traits accepted on `#effect`,
  `#interrupt`, `#transition`.
- Unknown trait emits E0541 from each callable kind.
- Classic typo (`Realtim` for `Realtime`) caught.
- User-defined `@trait MyOwnTrait` validated.
- `Diverges` correctly rejected (ADR 0003 Q4 enforcement).
- Empty `$ []` and absent trait list both silent.
- Multiple unknown traits all reported in one pass.
- Smoke test enumerates every name in
  `PREDECLARED_{PURE,IMPERATIVE}_TRAITS` and verifies acceptance —
  guards against accidental omissions if someone edits the
  constants.

Workspace remains green; clippy clean.

This closes the second of the three planned slices for this batch
(T4b → v0.2-α scaffolding → trait validation). The next natural
follow-ups: layer-aware trait checking, `@snapshot` row-gating
(needs `Readable`-trait recognition this slice provides),
generic-arg validation for `LockingDiscipline<RwLock>`-style
references.

### Added — v0.2-α: `@partial` and `@snapshot` lexer + AST + parser scaffolding (2026-05-05)

First implementation slice for Decisions #23 (Haskell-clean `@fn`)
and #24 (`@snapshot` boundary operator). This PR lands the *parser
surface* — lexer tokens, AST nodes, parser rules, and minimal
resolver wiring — so subsequent v0.2 slices (totality check, row
gating, atomicity check) have AST nodes to consume.

**Lexer (`crates/lexer`):**
- New `KwAtPartial` token (Decision #23 / ADR 0003) for `@partial`
  totality opt-out modifier.
- New `KwAtSnapshot` token (Decision #24 / ADR 0004) for
  `@snapshot Auto.field` boundary-crossing read operator.
- Both added to `all_functional_sigil_forms` test.

**AST (`crates/ast`):**
- `FnDecl` gains `pub partial: bool` (default `false`). Stamped by
  the parser when the source carries a leading `@partial` modifier.
- New `ExprKind::Snapshot { automaton: String, field: String }`
  variant for the `@snapshot Auto.field` expression form.

**Parser (`crates/parser`):**
- `parse_item` recognises `@partial` as an item-level prefix on
  `@fn`. Splits `parse_fn_decl` into the legacy entry point and a
  new `parse_fn_decl_with_partial(start, partial)` that the
  partial-prefix path calls with `partial=true`.
- `parse_atom` recognises `@snapshot` as an expression form;
  `parse_snapshot_expr` consumes the `Auto.field` shape (single-
  segment automaton + dot + field). Composite reads
  (`@snapshot Auto.field[i]`) and `Self.field` snapshots are
  out of scope for v0.2-α (deferred per ADR 0004 Q3 and Q2).
- `@partial` followed by anything other than `@fn` → `ParseError::
  Expected("@fn after @partial modifier")`.
- Snapshot rejects missing dot / missing field with descriptive
  diagnostics.

**Resolver (`crates/resolve`):**
- Added `ExprKind::Snapshot` arm to the body walker. Calls
  `require_automaton(automaton)` and `require_field(automaton,
  field)` against the encountered snapshot site. The hidden-field
  visibility rule from Decision #25 / `require_field` applies *for
  free*: snapshotting `Uart.parity_errors` (a `#hidden` field) from
  `@fn` correctly produces `E0407 HiddenFieldNotAccessible`.

**What v0.2-α deliberately defers:**
- Totality check (`E0540` for non-structural recursion in non-
  `@partial` `@fn`s) — `clifford-check` work, lands in v0.2-β.
- `Readable`-row gating of `@snapshot` from `@fn` (`E0550
  SnapshotInUnreadableFn`) — depends on Decision #22 trait
  validation in `clifford-types`, which lands as the next slice.
- `@snapshot Self.field` rejection inside `#transition` (`E0553
  SnapshotInImperative`) — `clifford-check` work; today it falls
  through as a parse error since `Self` isn't an Ident.
- `#shared`-field snapshot lock-holding proof (`E0552`) — gated to
  v0.7+ alongside Decision #21 / #26 implementation.
- Atomicity check (`E0551 SnapshotNotAtomic` for non-`Copy`
  fields) — needs T4c trait-impl machinery.
- Type inference for the `Snapshot` expression — `clifford-types`
  lands the field-type lookup in the next slice.

**Tests:**

- *Lexer*: 1 test updated (`all_functional_sigil_forms` now exercises
  the two new tokens). 48 tests passing total.
- *Parser*: 9 new tests (235 total in parser, was 224 — we added 11
  for Decision #22 in an earlier slice, then 9 more here for
  v0.2-α; some skips intentional due to deferred features).
  Coverage: `@partial` stamps the flag, default is false,
  `@partial @type` rejected, `@partial @fn` with `$ [...]` works;
  `@snapshot` in `let` RHS / binary expr / call arg, missing dot
  rejected, missing field rejected.
- *Resolver*: 4 new tests covering snapshot of known automaton +
  field (passes), unknown automaton (E0403/E0402), unknown field
  (E0405), hidden field (E0407 — the Decision #25 interaction).

Workspace remains green; clippy clean.

The next two slices (Decision #22 trait validation, then totality
check + row gating) land on top of this scaffolding without
revisiting the parser.

### Type Checker — Slice T4b: `@type` alias following + ADT terminal markers (2026-05-05)

Second slice of type-checker path-resolution work. T4a (the previous
slice) translated `TypeKind::Path` to `Type::Nominal` verbatim;
T4b registers the program's top-level `@type` declarations and
follows non-generic aliases for compatibility checks.

**The headline behaviour change:**

```clifford
@type ByteCount = u32;
@fn f() {
  let _x: ByteCount = 5u32;   // T4a: E0512 mismatch (Nominal ≠ Primitive)
  return;                     // T4b: typechecks (alias unfolds to u32)
}
```

**Implementation (`crates/types/src/lib.rs`):**

- New `TypeRegistry { decls: HashMap<String, NominalDecl> }` built
  once per `infer()` call from every `Item::Type` in the program.
- New `NominalDecl` enum: `Alias(Type)` (unfolds via `unalias`) or
  `Adt` (terminal nominal — does not unfold).
- New `TypeRegistry::unalias()`: recursively unfolds nominal aliases
  with a depth-32 cycle safeguard. Idempotent on non-aliases.
- New `TypeRegistry::unfold_one()`: single-layer unfold returning
  `None` for non-aliases, generic-arg nominals, multi-segment paths,
  ADT nominals, and unknown nominals.
- `types_compatible(declared, actual, registry)` now takes the
  registry and unaliases both sides before structural comparison.
- `Inferer` gains a `type_registry: &'a TypeRegistry` field;
  threaded through both `types_compatible` call sites (let
  annotation E0512, call-arg E0513).

**What T4b deliberately defers (kept for T4c+):**

- Generic alias substitution (`@type Vec<T> = …` applied to
  `Vec<u32>`). Generic-arg nominals don't unfold today; the
  conservative choice is forward-compatible — the alias just stays
  Nominal until T4c lands the substitution machinery.
- Validation pass for unknown nominal paths (a separate `E0518
  UnknownNominalType` walk over all TypeExprs). Today an unknown
  nominal stays Nominal and trips structural mismatch with whatever
  it's compared against — the diagnostic still names the user's
  identifier correctly, just doesn't say "this name doesn't exist."
- Multi-segment paths (e.g. `clifford::core::Option`) — module
  resolution is T4d+ work.
- ADT-variant resolution for `Result::Ok` style paths in *value*
  position — variants live in expression position, not type
  position, so type-position resolution doesn't need them.

**Tests (13 new T4b unit tests, types crate now 101 total, was 88):**

- One-step alias typechecks; transitive alias (A→B→u32);
  three-deep chain.
- Alias mismatch after unfolding still errors with the alias name
  preserved in the diagnostic (user sees their identifier).
- Alias-to-tuple, alias-to-ref typecheck.
- Two distinct aliases to the same underlying compare equal
  (transparent-alias semantics; strong newtype semantics would need
  a separate `@newtype` declaration this PR doesn't introduce).
- ADT does not unfold (`@type Color = | Red | Green | Blue` stays
  terminal; `let _x: Color = 0` mismatches as expected).
- Unknown nominal path treated as Nominal for compat — diagnostic
  still names it correctly.
- `unalias` self-reference safeguard returns `Type::Unknown` (no
  stack overflow on `@type A = A;`).
- `unalias` two-step cycle (`A → B → A`) hits the safeguard.
- Generic args block alias unfolding (forward-compat with T4c).
- Call-arg mismatch through alias works (other `types_compatible`
  call site).

The pre-T4b test `nominal_let_annotation_emits_e0512_with_nominal_in_message`
was renamed to `nominal_let_annotation_alias_follows_to_underlying_type`
and updated to assert the new behaviour: the alias unfolds and the
typecheck succeeds.

Workspace remains green; clippy clean on the types crate.

### Locked — Decision #27: GA across scales + ADR 0006 Accepted (2026-05-05)

Architect signed off "lock it in" on ADR 0006 after reviewing the
cost-model + utility analysis. ADR 0006 flips from **Proposed** to
**Accepted**; new **Decision #27** added to DECISIONS.md elevating
the strategic commitment.

**The unifying claim Decision #27 makes explicit:**

> GA is the unifying *algebra*; standard primitives (CAS spinlocks,
> flags, RPCs, atomics) are the *implementation*.

The same `outer_product` operation now runs at three scales:

| Scale | When | Algebra carries | Runtime carries |
|---|---|---|---|
| Compile-time, single-process | `cliffordc` invocation | Static `actual_writes` per callable | (none — pure proof) |
| In-process runtime (#21/#26) | Lock acquire/release | `lock(L) = pri(L) + e_L` | Normal CAS spinlock + owner-ID + depth counter |
| Distributed runtime (#27) | Mutation phase publish/retract | `Behaviour { (resource, slice) bits }` | RPC publish + central coordinator + RPC retract; `&` op on coordinator |

The architect's framing during the lock-in: *"rotors that could be
designed via single locks and flags"* — the GA is the framework, the
runtime is whatever's already cheap. Same pattern Decisions #21 and
#26 already validated.

**ADR 0006 Accepted with five locked resolutions:**

- **Q1** Coordinator topology: central for v0.4-α; gossip pluggable
  for v0.5+.
- **Q2** Publication scope: per-transaction (`#effect` body or
  explicit `@dist_phase("name") { … }` block).
- **Q3** Race response: configurable per `#rotor_lock` via
  `#on_dist_race: Log | Abort | Quarantine`; default `Log`.
- **Q4** Resource basis assignment: pre-agreed schema at link time;
  `E0702 SchemaIncompatible` for mismatches.
- **Q5** Interaction with #21/#26: opt-in per resource via
  `#dist_shared` field qualifier; in-process resources unchanged.

**Cost model (verified during lock-in):**

| Setup | Compile time | Binary size | Runtime |
|---|---|---|---|
| No `#dist_shared`, no flag | 0 | 0 | 0 |
| Has `#dist_shared`, no flag | epsilon (parser sees keyword) | 0 | 0 |
| Has `#dist_shared`, flag on | small (codegen hook) | ~few KB | ~10–100× write cost on marked resources only |

Three layers of opt-in (per-resource via `#dist_shared`, per-build
via `--dist-check`, per-program via Cargo feature flag). Programs
that don't opt in pay nothing. Mirrors Rust's allocator hooks /
sanitizers / miri.

**DECISIONS.md updates:**
- New entry for Decision #27 (full text covering the unifying claim,
  the three-scale table, locked resolutions, and the rationale for
  why this is a Decision and not just an ADR).
- Decision Matrix extended with #27.
- Status header: "Decisions #1–#27 LOCKED."
- Open Questions text refreshed.

**ADR 0006 update:**
- Status flipped to Accepted (2026-05-05).
- `## Decision` section gains the locked-resolutions table and
  action items for v0.4+ implementation.

**decision-index.md:** #27 row added.

**Implementation status:** Phase 5+ work. v0.1, v0.2, v0.3
unaffected. Lexer reservations land alongside Decision #21/#26 or
in v0.4-α; plugin crate `crates/dist-check` and central-coordinator
backend in v0.4; gossip backend / dynamic schema in v0.5+/v0.6+.

The compile-time engine ships entirely unaware of Decision #27;
programs that don't use `#dist_shared` are unaffected.

### Locked — Decisions #23, #24, #26 + Proposed ADR 0006 runtime distributed engine (2026-05-05)

Architect signed off "yes to all" on the propositions in ADRs 0003,
0004, and 0005. Three ADRs flip from **Proposed** to **Accepted**;
two existing Decisions transition from DESIGN-IN-PROGRESS to
**LOCKED**; one new Decision (#26) is added.

**Decision #23 — Tighten `@fn` toward Haskell-clean.** ✓ LOCKED
2026-05-05 per ADR 0003.

- **Totality required by default** (`@partial @fn` opt-out;
  Idris-style structural-recursion check; non-structural recursion
  → `E0540`).
- **First-class effect rows** as `$ [TraitList]` extension:
  `Readable`, `Observable`, `Pure`, `Opaque` with row-composition
  checking (`E0541`). `@fn → @fn` row check is one-directional;
  `#`-layer freely calls any `@fn`.
- **Limited refinement types** via §5.8 sigma-bound (Decision #14)
  extension to function arguments. **No SMT in v0.2** (`E0542`);
  SMT-backed refinements deferred to v1.0+ separate ADR.
- `Result<_, E>` only in v0.2 (no `Throws<E>`); `Diverges` trait
  dropped (`@partial` covers it).
- Implementation v0.2-α: totality skeleton in `clifford-check`;
  rows in `clifford-types`; book Ch. 23 graduates from stub.

**Decision #24 — `@snapshot` boundary operator.** ✓ LOCKED
2026-05-05 per ADR 0004.

- **Expression form** (`let v := @snapshot Counter.value;`).
- **Copy-by-value** for `Copy` types in v0.2; `@snapshot_ref`
  borrow form deferred to v0.4+. Larger types → `E0551
  SnapshotNotAtomic`.
- **`#shared` snapshots require lock-holding proof** — from `@fn`
  in v0.2: `E0552 SnapshotNeedsLockProof` (only from `#`-layer).
- **Two-phase migration**: v0.2 deprecation warning `W0001
  ImplicitFieldRead`; v0.4+ hard `E0101`.
- **Not pure** — `Readable` row (from ADR 0003) is the marker.
  `@snapshot Self.field` inside transitions → `E0553
  SnapshotInImperative` (use bare `Self.field`).
- Implementation v0.2-α: `@snapshot` lexer + AST; `Readable`-row
  gating; E0550–E0553 + W0001 in §10; book Ch. 24 graduates from
  stub; Ch. 43 (formerly Ch. 39) SPSC example migrates.

**Decision #26 — Rotor-based plane-confined locks (refines #21).**
✓ LOCKED 2026-05-05 per ADR 0005. New entry in DECISIONS.md.

- Reframes rotors from same-priority *tiebreak* to *acquisition
  primitive itself*. A `#rotor_lock L` is conceptually a multivector
  cell; acquire is `M ← R_t · M` where `R_t = exp(-θ_t · B_t / 2)`.
- Mutual exclusion + wrong-thread-release detection + re-entrancy
  all fall out of the algebra. Static check is the existing wedge
  primitive (`caller.thread_plane ∧ lock.plane`).
- **Runtime cost is zero `exp`** — lowered code is a normal CAS
  spinlock with integer owner-ID + depth counter. GA is the proof
  system, not the runtime.
- Five locked resolutions: pool-based plane assignment for v0.7
  (default `p=16` → 8 planes); counted re-entry (POSIX-style);
  hard error `E0539 DuplicateThreadPlane`; lock owns its full
  state including `θ`; rotor-as-acquisition supersedes Decision
  #21's priority-ordering proof (priority becomes derived total
  order on planes).
- Diagnostic family: `E0535 PlaneeMismatch`, `E0536
  NoThreadPlane`, `E0537 SharedFieldOutsideLock`, `E0538
  ReEntryViolation`, `E0539 DuplicateThreadPlane`.
- Implementation gated to **v0.7+** alongside Decision #21's
  shared-state machinery; lexer reservations land alongside the
  existing #21 reservations.

**ADRs flipped to Accepted:**
- `docs/adr/0003-haskell-clean-fn-discipline.md` (Decision #23)
- `docs/adr/0004-snapshot-boundary-operator.md` (Decision #24)
- `docs/adr/0005-rotor-plane-confined-locks.md` (Decision #26)

Each gains a "Locked resolutions" table in its `## Decision`
section recording the architect's specific answers to each open
question, so future readers see exactly what was decided and why.

**DECISIONS.md updates:**
- Decision #23, #24 transitioned from 🔬 DESIGN-IN-PROGRESS → ✓
  LOCKED with locked-resolutions sections.
- Decision #26 added (full entry; refines #21).
- Decision Matrix table extended with #26.
- Status header updated: "Decisions #1–#26 LOCKED."
- Open Questions section text refreshed to remove obsolete
  references to #23/#24 being in-progress.

**Book updates:**
- Ch. 23 (Haskell-clean `@fn`) status → LOCKED stub awaiting v0.2-α.
- Ch. 24 (`@snapshot`) status → LOCKED stub awaiting v0.2-α.
- New Ch. 26 (rotor plane locks) stub awaiting v0.7-α.
- SUMMARY.md: Part II gains Ch. 26; Part III/IV/V renumbered by +1
  (now 27-34 / 35-42 / 43-47).
- decision-index.md: #23/#24 status flipped to LOCKED with ADR
  references; #26 row added.

### Proposed — ADR 0006: Runtime distributed race & deadlock detection via dynamic multivector check (2026-05-05)

New ADR formalising the user's distributed-engine intuition: extend
the GA wedge-product primitive from compile-time static check to
runtime distributed check, scoped to **plugin / debug mode only**.

The compile-time engine cannot reason about distributed peers,
dynamic resource sharding, or cross-process coordination. This ADR
proposes a runtime check using the *same wedge primitive*: each node
publishes its current `Behaviour` multivector; a coordinator
computes pairwise wedges on every join/mutation; any collapse is a
race detected at runtime with a source-level diagnostic.

Same algebra as §7. Same `&` instruction. Same diagnostic shape
("nodes N₁ and N₂ both wrote `Resource.slice_42`"). Only the
*lifecycle* changes — static → dynamic.

**Crucial constraint:** zero impact on release builds. The runtime
check is opt-in via `#[cliffordc::dist_check]` attribute or
`cliffordc test --dist-check` flag; release builds elide the
publish/check/retract instrumentation entirely.

**Status remains Proposed** until the five open questions in ADR
§6 close:
1. Coordinator topology (proposed: central for v0.4-α, gossip
   pluggable for v0.5+).
2. Behaviour publication scope (proposed: per-transaction).
3. Race response (proposed: configurable `Log | Abort | Quarantine`,
   default `Log`).
4. Resource basis assignment in distributed (proposed: pre-agreed
   schema at link time; `E0702 SchemaIncompatible`).
5. Interaction with Decision #21 / #26 (proposed: `#dist_shared`
   field opt-in; in-process `#shared` resources unchanged).

Implementation is **Phase 5+ work** (v0.4 / v0.5 alongside
`clifford::core::sync` and networking stdlib). New crate
`crates/dist-check`; new error-code range `E07xx` reserved in
spec §10. Compile-time engine unchanged.

This is a pure documentation ADR — no code, no spec edits yet.

### Added — Decision #25: `#hidden` field encapsulation (parser + resolver + book Ch. 25) (2026-05-04)

First implementation of Decision #25 (locked 2026-05-03): a per-field
`#hidden` modifier on automaton fields with algebraic-trivial-
orthogonality semantics. A hidden field's basis vector cannot enter
the `actual_writes` set of any callable outside the owning automaton
(because the resolver rejects the reference), so the §7.4 wedge
product never collapses against it from outside. **Encapsulation by
construction; no engine machinery.**

**Lexer (`crates/lexer`):**
- New `TokenKind::KwHashHidden` for the `#hidden` keyword.
- Added to the `all_imperative_sigil_forms` test alongside `#offset`,
  `#access`, etc.

**AST (`crates/ast`):**
- `AutomatonField` gains `pub hidden: bool` (default `false`),
  orthogonal to the existing `kind: FieldKind` axis (Decision #21).

**Parser (`crates/parser`):**
- `parse_automaton_field` accepts `#hidden` as a third field-meta
  clause, in any order with `#offset` and `#access`. Duplicate
  `#hidden` is `ParseError::DuplicateClause("#hidden")`.
- Five new tests: hidden alone, default false, hidden in any
  intermixed order with `#offset`/`#access`, duplicate rejection,
  multiple-fields-mixed.

**Resolver (`crates/resolve`):**
- New `ResolveError::HiddenFieldNotAccessible` (`E0407`); diagnostic
  names the owning automaton and field by source identifier.
- `AutomatonMeta` gains a `hidden_fields: HashMap<String,
  HashSet<String>>` side table.
- `require_field` extended: after confirming the field exists (E0405),
  if it's hidden, reject unless the enclosing context is a
  `#transition` of the *owning* automaton (i.e.,
  `enclosing.transition_of == Some(automaton_name)`). The check is
  six lines past the existence check.
- Eight new tests covering every cell of the visibility table:
  accessible from owning transition (positive control), inaccessible
  from `#effect #mutates: [A]`, from another automaton's transition,
  from `@fn`, from full-path cross-automaton reference; non-hidden
  remains accessible (negative control); E0407 distinct from E0405;
  hidden array indexed write inside `#mutate` block from owning
  transition.

**Spec (`docs/CLIFFORD_SPEC.md`):**
- §2.5 grammar: `field_attr` extended with the new `#hidden`
  alternative; added a normative bullet describing the semantic
  intent (algebraic-trivial orthogonality) and the E0407 visibility
  rule.
- §3 parser-behavior: new point 6a documenting parser handling
  alongside the existing register-block dispatch (point 6).
- Updated §2.5's old "Decision #9 removed `#hidden` and `#visible`"
  language to clarify the *visibility-clause system* was removed but
  the per-field encapsulation modifier is back under Decision #25.

**Book (`book/src/`):**
- New Chapter 25 (`part2/25-d25-hidden.md`): full chapter (~440 lines)
  covering surface syntax, the algebraic insight, the visibility
  table (every cell), why "owning-transition only" is the right
  scope, what `#hidden` enables, a worked UART driver example with
  the parity-error counter, the full `~50 LoC` implementation (lexer
  + AST + parser + resolver) so a reader could implement it
  themselves, what `#hidden` deliberately is *not*, and
  cross-references. Per the book's editorial bar: "someone reading
  this should be able to write their own compiler."
- New stub Chapters 22, 23, 24 (`part2/22-d22-imperative-kinds.md`,
  `23-d23-haskell-clean-fn.md`, `24-d24-snapshot-operator.md`) so
  the four locked / in-progress Decisions all have chapter slots
  reserved. Stubs cite `DECISIONS.md` and the targeted ADRs;
  full text lands with the respective implementation PRs.
- `SUMMARY.md`: inserted four new Part-II entries (Ch. 22-25) and
  renumbered Part III chapters 22-29 → 26-33, Part IV 30-37 →
  34-41, Part V 38-42 → 42-46. File names unchanged; only chapter
  titles in SUMMARY shift.
- `decision-index.md`: #22-#25 rows now point to their actual
  chapter numbers (was previously aspirational).

### Added — Decision #22: imperative trait list on `#effect` / `#interrupt` / `#transition` (2026-05-05)

First implementation of Decision #22 (locked 2026-05-03). Extends
the `$ [TraitList]` mechanism from `@fn` (Decision #2) to imperative-
layer callables, with semantic interpretation switching from *purity*
to **kind classification** — what kind of imperative work the
callable does.

**AST (`crates/ast`):**

- `EffectDecl`, `InterruptDecl`, and `TransitionDecl` each gain a
  `pub trait_list: Vec<TraitRef>` field. Empty if no `$ [...]` clause
  appears. Same `TraitRef` shape as `FnDecl::trait_list`.

**Parser (`crates/parser`):**

- `parse_effect_decl`, `parse_interrupt_decl`, `parse_transition_decl`
  each accept an optional `$ [TraitList]` clause between their
  metadata clauses (or destination, for transitions) and the body
  block. Implementation is one three-line `if matches!(...)
  Dollar` block per decl, reusing `parse_trait_list` (already
  factored from Decision #2).
- Trait names are stored verbatim — the parser performs no
  predeclared-trait validation. Downstream tools (codegen,
  `cliffordc audit`, `clifford-types` once it grows imperative-side
  trait checking) interpret the list.
- 11 new parser tests (215 total, was 204). Coverage: single trait,
  multiple traits, empty/missing list, `#cannot_mutate` then `$
  [...]` ordering, transition with destination then `$ [...]`,
  generic trait names (`$ [LockingDiscipline<RwLock>]`), non-
  predeclared user-defined names pass through.

**Spec (`docs/CLIFFORD_SPEC.md` §2.5):**

- Grammar `effect_decl` and `transition_decl` extended with optional
  `trait_list?` between metadata and body. New `trait_list` non-
  terminal added with cross-reference to `@fn`'s usage.
- New normative bullet describing the eight predeclared imperative
  traits — `Hardware`, `Realtime`, `Acquire`, `Release`, `SeqCst`,
  `LockingDiscipline`, `PureState`, `Encapsulated` — along with
  their consumers (codegen for memory-ordering markers; `cliffordc
  audit --traits` and certification artefacts for the rest). Spec
  is explicit that the orthogonality engine ignores `trait_list`
  entirely, and explains why (§7's race-detection question is
  decided by `actual_writes`, which no trait can change).

**Book (`book/src/part2/22-d22-imperative-kinds.md`):**

- Replaces the stub created on the unmerged Decision #25 PR with
  ~270-line full chapter. Covers: the one-line summary, the
  predeclared traits with their consumers, why the engine
  deliberately ignores trait lists (separation of concerns —
  race-detection vs memory-ordering), surface syntax + grammar,
  a worked example (UART RX driver with full classification across
  `#interrupt`, `#transition`, `@fn`), the full ~30-LoC
  implementation guide (AST + parser + tests), what Decision #22
  enables (memory-ordering codegen, real-time audit, certification,
  encapsulation reporting), and what it explicitly doesn't (race
  detection, `#mutates` replacement, type-system effects).

What Decision #22 deliberately defers:

- Codegen for `Acquire`/`Release`/`SeqCst` memory-ordering fences
  (v0.2 codegen work).
- `clifford-types` validation of predeclared trait names (v0.2
  type-check work).
- `cliffordc audit --traits` flag and certification report formats
  (v0.2 tooling).
- `LockingDiscipline` interaction with `#shared` fields (v0.7+
  alongside Decision #21).

Workspace remains green; clippy clean. The Decision is **locked**;
parser/AST scaffolding ships now (this PR), downstream consumers in
v0.2.

### Added — Check Slice S2: §5.4 mutation-authorisation checking (2026-05-04)

Second slice of `clifford-check`. Slice 1 implemented §5.5 sigil-layer
boundary checking (rejecting `#`-constructs in `@fn` bodies); S2 walks
the *imperative-layer* bodies that S1 deliberately skipped (`#effect`,
`#interrupt`, `#transition`) and verifies every mutation against its
enclosing context's authorisation set.

Two new diagnostics:

- **E0302 WriteToUndeclaredAutomaton.** A `#mutate A { ... }` (canonical
  form) or `Auto.field <op>= …` (sugar) statement targets automaton `A`
  that is **not** in the enclosing context's permitted-mutation set.
  The set is:
  - For an `#effect` body: the names in its `#mutates: [...]` clause.
  - For an `#interrupt` body: the names in its `#mutates: [...]` clause.
  - For a `#transition` body of automaton `Owner`: the singleton
    `[Owner]` (transitions implicitly mutate only their owning
    automaton, per Decision #5).
  - For an `#impl` method body: the implementing automaton (Decision
    #16's implicit `#mutates: [self]` — deferred until parser slice 7+
    materialises method bodies).

- **E0306 WriteToCannotMutate.** A `#mutate A { ... }` or sugar
  statement targets automaton `A` that explicitly appears in the
  enclosing `#effect`'s `#cannot_mutate: [...]` exclusion list.
  Prohibition wins over `#mutates`-membership: if `A` is in both lists,
  E0306 fires (the more specific user error) and E0302 is suppressed.

The diagnostic display name names the enclosing callable verbatim
(e.g. `"#effect bump"`, `"#transition tick in #automaton Counter"`)
so users see *their* identifier, never an internal handle.

What S2 deliberately defers:

- **E0301 cross-boundary mutation through references.** §5.4's first
  rule — "every mutation through a reference rooted in shared state
  occurs inside a mutation context" — needs the type checker to
  classify references by their root's mutability. That's post-T4b
  territory; lands in Slice S3.
- **E0303 unknown automaton field.** Already covered by the resolver's
  E0405 UnknownField. Spec §5.4 was clarified to note this overlap
  rather than duplicating the check.
- **`#impl` method body authorisation.** Method bodies don't exist on
  the AST yet (parser slice 7+).

Tests: 15 new unit tests (40 total in `clifford-check`, was 25):

- `#effect` permits declared automatons (single + multi-target +
  canonical `#mutate` form);
- `#effect` rejects undeclared targets (sugar + canonical form);
- empty `#mutates: []` rejects every write;
- `#cannot_mutate` rejects explicit-target writes (E0306);
- `#cannot_mutate` is silent for unrelated targets;
- E0306 wins over E0302 when both rules would fire;
- `#interrupt` accept/reject parallel to `#effect`;
- `#transition` implicitly permits owning automaton;
- `#transition` rejects writes to other automata;
- multiple errors collected in one pass;
- Slice 1 boundary check still runs alongside S2 (no regression).

Spec edit (`docs/CLIFFORD_SPEC.md` §5.4): clarified E0303 overlap
with the resolver's E0405; fixed earlier draft text that wrongly said
`#cannot_mutate` lists "fields" — the grammar at §2.5 has always
taken automaton names per Decision #3, and S2 now enforces this
correctly with E0306.

Workspace remains green; clippy clean on the check crate.

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

### Proposed — ADR 0003 + ADR 0004: Haskell-clean `@fn` discipline + `@snapshot` boundary operator (2026-05-04)

The two design-in-progress ADRs that close out Decisions #23 and
#24's open questions. Status: **Proposed** for both — locks pending
architect sign-off on the proposed resolutions.

**ADR 0003 — Haskell-clean `@fn` discipline.** Surveys Haskell,
Idris, Liquid Haskell, and Koka on three axes (totality, effect
rows, refinement types) and proposes a concrete design for what
"Haskell-clean `@fn`" means in Clifford:

- **Total by default** with `@partial @fn` opt-out (Idris-style
  structural-recursion check; non-structural recursion → `E0540`).
- **First-class effect rows** as an extension of `$ [TraitList]`
  (Decision #2 + #22): `Readable`, `Observable`, `Pure`, `Opaque`
  with row-composition checking (`E0541`).
- **Limited refinement types** via the §5.8 sigma-bound machinery
  (Decision #14) — extended from loop variables to function
  arguments. Catches "index in bounds" without an SMT solver
  (`E0542 RefinementNotDischarged`). Full SMT-backed refinements
  deferred to v1.0+ ADR.
- **Local mutation** (Refinement #1a) already locked, no change.

The headline trade: totality + effect rows are real wins; full
refinement types via SMT are not (yet) — the firmware target makes
a 50 MB solver dependency a deal-breaker. The sigma-bound carve-out
gives 80% of the value at 5% of the cost.

Five open questions answered with proposed resolutions (structural-
recursion rule, `#`-layer effect-row interaction, Throws<E> vs
Result<_, E>, Diverges trait drop, SMT timeline). Implementation
gated to v0.2 (totality + effect rows) and v0.4+ (refinements
beyond sigma-bound).

**ADR 0004 — `@snapshot` boundary operator.** Resolves Decision
#24's four explicit open questions:

1. **Expression vs statement?** → Expression. `let v := @snapshot
   Counter.value;` composes in any expression position.
2. **Copy-by-value vs ref-to-snapshot?** → Copy-by-value for `Copy`
   types in v0.2; `@snapshot_ref` borrow form deferred to v0.4+.
3. **Interaction with `#shared` (Decision #21)?** → `@snapshot` of
   a `#shared` field requires the lock to be held by the caller's
   thread-plane (statically demonstrable per ADR 0005). From `@fn`
   in v0.2: `E0552 SnapshotNeedsLockProof` (snapshot of `#shared`
   only from `#`-layer).
4. **Backward compat with the implicit-read pattern in book Ch. 39?**
   → Two-phase migration: v0.2 deprecation warning (`W0001
   ImplicitFieldRead`); v0.4+ hard `E0101`.

Atomicity: only word-size `Copy` fields snapshot atomically;
larger types → `E0551 SnapshotNotAtomic` (use `#shared` + lock).
The `Readable` trait from ADR 0003 is the gate for `@snapshot`
from `@fn` (`E0550 SnapshotInUnreadableFn`).

Five additional open questions resolved (purity status of
`@snapshot`, `Self.field` snapshot inside transitions, complex
composite reads, migration timing, explicit ordering annotation).

The two ADRs are **complementary** and should land together —
ADR 0003's `Readable` trait is the gate that ADR 0004 uses; ADR
0004's `@snapshot` operator is the only `@fn`-side mechanism for
discharging `Readable`. Locking one without the other leaves an
unfilled hole.

If accepted, both Decisions #23 and #24 transition from
DESIGN-IN-PROGRESS to ✓ LOCKED with one-paragraph entries in
DECISIONS.md citing the respective ADRs. Implementation milestones
laid out in each ADR's §"Implementation milestones" section: bulk
of work in v0.2; tail (refinements, `@snapshot_ref`, ordering
control) in v0.4+ / v0.7+.

Pure documentation — no code changes, no spec amendments yet.
Spec edits (§2, §4, §5, §10) and `clifford-check` work land per
ADR acceptance.

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
