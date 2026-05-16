# Clifford Language: Critical Design Decisions

**Status:** Decisions #1–#27 locked, **except #21, #26, #27 — deferred to research by the 2026-05 audit** (`docs/decision-audit-2026-05.md`; full text relocated to `docs/research/`). Implementation gating: **#12 implemented in v0.2** (slices 18–19, 24–25 — `#staged`, `#flush`, `@shadow`, register-block-staged rejection); **#18 implemented in v0.2** (slices 20–23, 26 — `#audit` surface + markers; slices 37–44 — runtime `PointerAuditor` pass, counting `ShadowSanitizer`, trap-on-false); #22, #23, #24, #25 design locked, implementation slated for v0.2 (subject to the audit's NARROW recommendations on #22/#23). Refinement #1a LOCKED. Phase 1 implementation underway. The project is in a post-GA-narrative pivot — see `docs/foundations.md` and `docs/decision-audit-2026-05.md`.
**Dates:** see footer below for the full chronological log.
**Owner:** Goose (Gustavo Delgadillo)
**Positioning:** General-purpose systems language; embedded firmware is the canonical first target but not the only target. Decisions are language-level and apply across domains.

---

## Decision #1: Syntactic Layering via Sigils ✓ LOCKED

**Question:** How do we distinguish imperative from functional code syntactically and semantically?

**Options Considered:**
- **Option A (Hard Keywords):** Reserve `automaton`, `effect`, `fn`, etc. as language keywords
- **Option B (Soft Keywords):** Context-dependent — keywords only in specific positions
- **Option C (Sigils):** Prefix with visible symbols: `#` for imperative, `@` for functional

**Chosen:** Option C (Sigils with Strict Layering)

**Rationale:**
- Sigils are visually distinctive — developer intent is immediately obvious
- Avoids ballooning reserved keyword list
- Extensible: future sigils (`$`, `!`, etc.) can be added without syntax conflicts
- Clear boundary enforcement at parse time

**Rules:**

1. **Imperative constructs** use `#` prefix:
   - `#automaton Name { ... }` — state machine declaration
   - `#effect name() { ... }` — effect procedure declaration
   - `#> name()` — effect procedure call
   - `#mutate AutomatonName { field = value }` — state mutation
   - `#transition State1 -> State2 { ... }` — state transition
   - `#hardware name() { ... }` — low-level hardware mutator

2. **Functional constructs** use `@` prefix:
   - `@fn name(args) -> ReturnType $ [Traits] { ... }` — pure function
   - `@type Name = Variant | Variant` — algebraic data type
   - `@module name { ... }` — pure library/module

3. **Boundary Enforcement:**
   - `@fn` scope cannot reference or call `#` constructs
   - `#effect` scope can freely call `@fn` and execute `#mutate`
   - No cross-boundary inlining (enforced by compiler)

**Example:**
```
@fn compute_direction(left: f32, right: f32) -> Direction $ [Pure] {
  match (left, right) {
    (l, r) if l < threshold -> Left
    _ -> Right
  }
}

#automaton RobotState {
  #states: [Idle, Moving]
  direction: Direction
}

#effect main_loop() #mutates: [RobotState] {
  dir := compute_direction(left_dist, right_dist)
  #> update_motors(dir)
}

#effect update_motors(dir: Direction) #mutates: [RobotState] {
  #mutate RobotState { direction = dir }
}
```

**Implications:**
- Parser sees sigil immediately, knows syntactic rules for that construct
- Type system can enforce purity via sigil context
- Error messages are unambiguous: "mutation (#) not allowed in functional (@) scope"

### Refinement #1a: Local-Stack Mutation Permitted in Any Layer ✓ LOCKED (2026-05-02)

**Question:** Does the §5.5 / Decision #1 layer-boundary rule forbid local mutable bindings in `@fn` bodies?

**Answer:** No. **Local-stack mutation is permitted in any function body, including `@fn`.** The boundary rule keeps `#`-effects (mutation of shared automaton/register state, side effects on hardware, calls into the imperative layer) out of `@fn` — it does *not* forbid stack-local mutation that's invisible to the caller.

**Rule (refined):** A `let mut x: T = e;` binding plus subsequent `x = e';` assignments is permitted regardless of layer, *provided* `T` does not contain any reference into mutable shared state (no `&mut Auto.field`, no `&Auto.field`, no `access<T>` rooted in shared state).

The leakage case (a `mut` binding holding a reference *to* shared state) is closed by Decision #13 Rule 0 at the type level: `&mut Auto.field` is `E0700` and cannot be constructed; therefore no `mut` binding can smuggle out write authority to shared state.

**Why this matters:** without this refinement, the bog-standard local-accumulator pattern (`let mut total = 0u32; sigma i in 0..n { total = total + arr[i]; }`) would be illegal in `@fn` bodies. That would force every reduce-shaped algorithm into recursion (cumbersome) or out of `@fn` (which means it can't be marked `$ [Pure]`, which means it can't be called from other `@fn`s, which is a usability cliff).

The refinement preserves the *semantic* notion of purity (referential transparency, no caller-visible side effects) by recognizing that local mutation inside a function call's stack frame doesn't violate it.

**Implications:**
- `CLIFFORD_SPEC.md` §5.4 updated with the refined wording.
- `clifford-check` slice S2 (when it ships) implements the refined rule, not the original.
- Decision #13's reference-provenance machinery does the heavy lifting for the leakage case; §5.4 just defers to it.
- `$ [Pure]` annotation remains semantically correct on functions whose bodies use only local-stack mutation.

---

## Decision #2: Hybrid Trait System with Signature Markers ✓ LOCKED

**Question:** How do functions declare and prove they conform to traits without forcing explicit `impl` blocks?

**Options Considered:**
- **Option A (Nominal):** Explicit `impl Trait for Type { ... }` blocks (Rust-style)
- **Option B (Structural):** Compiler infers from function shape (duck typing)
- **Option C (Hybrid):** Auto-inference with optional explicit annotation

**Chosen:** Option C (Hybrid with `$ [TraitList]` markers)

**Rationale:**
- C FFI code doesn't have `impl` blocks; we need to wrap it safely
- Developers shouldn't need to understand type theory to use the language
- Tooling (IDE hover, error messages) can make traits discoverable
- Power users (GA enthusiasts) can annotate explicitly when needed

**Rules:**

1. **For functions touching state:**
   - Must declare `$ [TraitList]` in signature
   - Compiler verifies function behavior matches declared traits
   - Syntax: `@fn name(args) -> ReturnType $ [Readable, Observable, Pure]`

2. **For functions with only local computation:**
   - `$ [TraitList]` is optional
   - Defaults to `$ [Pure]` implicitly

3. **For C FFI functions:**
   - Default to `$ [Opaque]` — compiler doesn't prove conformance
   - Developer can override: `extern "C" @fn c_read_register(addr: u32) -> u32 $ [Readable]`

4. **Trait Basis Vectors:**
   - Compiler auto-assigns basis vectors to each trait globally
   - `Readable` always maps to same basis vector `e_read` across program
   - Developer can optionally override with explicit annotation (see Decision #4)

5. **Verification:**
   - Compiler type-checks function body against declared traits
   - If function declares `$ [Readable]` but writes to state, compiler error: `E0201: mutation not allowed in Readable trait`

**Example:**
```
@fn read_sensor(addr: u32) -> f32 $ [Readable] {
  // Compiler verifies this only reads, doesn't write
  let value = read_register(addr)
  value * 1.5
}

@fn process_data(x: f32, y: f32) $ [Pure] {
  // Pure computation, no trait marker needed
  x + y
}

extern "C" @fn vendor_init() $ [Opaque] {
  // C function, compiler assumes nothing
}
```

**Implications:**
- Trait resolution is static, not runtime
- Trait basis vectors are globally consistent (required for GA orthogonality)
- IDE can show "this function is Readable + Observable" on hover
- Error messages teach GA through concrete examples

---

## Decision #3: Named Effect Procedures with `#>` Call Syntax ✓ LOCKED

**Question:** How do effect blocks compose without creating nesting complexity or losing orthogonality?

**Options Considered:**
- **Option A (No Nesting):** Top-level only, no composition beyond function calls
- **Option B (Nested Effects):** Allow `#effect` inside `#effect`, inherit context
- **Option C (Named Procedures):** Effect blocks are named, callable via `#>` syntax

**Chosen:** Option C (Named Effect Procedures)

**Rationale:**
- Composability without scope ambiguity
- GA orthogonality check remains static and decidable
- Clearer semantics: procedures are orchestration primitives, not nested contexts
- Naming convention ("effect procedure" not "effect function") signals intent to developers

**Rules:**

1. **Declaration:**
   - `#effect name() #mutates: [AutomatonList] { ... }`
   - Explicit automaton scope is mandatory
   - Cannot be nested syntactically

2. **Calls:**
   - `#> procedure_name(args)` from within another effect procedure
   - Visually distinct from `@fn` calls (no `@`, no `()` alone)
   - Must resolve statically — `#> name()` is bound at parse time

3. **Restrictions:**
   - Cannot be called from `@fn` (functional layer isolation)
   - Cannot be called outside effect context (e.g., from main directly)
   - Can call `@fn` freely
   - Can execute `#mutate` on declared automata

4. **Automaton Scope Declaration:**
   - Every effect procedure must declare `#mutates: [List]`
   - Compiler uses this for orthogonality checking
   - If procedure calls another procedure, their automaton scopes are unioned for conflict detection

**Example:**
```
#effect read_sensor() #mutates: [SensorState] {
  data := read_uart()
  #mutate SensorState { latest_reading = data }
}

#effect update_actuators() #mutates: [MotorState] {
  speed := compute_speed(sensor_state.latest_reading)
  #mutate MotorState { pwm = speed }
}

#effect main_loop() #mutates: [SensorState, MotorState] {
  loop {
    #> read_sensor()      // effect procedure call
    #> update_actuators() // effect procedure call
  }
}
```

**Implications:**
- Effect procedures are first-class orchestration constructs
- Compiler can statically verify automaton scope and detect conflicts
- No hidden side effects: declared scope is the actual scope
- Error messages: "procedure X (#mutates: [A, B]) and Y (#mutates: [B, C]) share automaton B; add synchronization"

---

## Decision #4: Auto-Assign GA Basis Vectors with Optional Explicit Annotation ✓ LOCKED

**Question:** Should developers annotate basis vectors for orthogonality checking, or should the compiler infer them?

**Options Considered:**
- **Option A (Auto-Assign):** Compiler assigns `e1, e2, e3` automatically, check is invisible
- **Option B (Explicit Annotation):** Developer declares `#basis: {field: e1, ...}` in automaton
- **Option C (Hybrid):** Auto-assign by default, optional explicit override

**Chosen:** Option C (Hybrid with transparent IDE integration)

**Rationale:**
- Programmers are not mathematicians; GA literacy cannot be assumed
- Orthogonality *must* be checked (safety requirement), but transparency is optional
- Tooling makes GA discoverable without forcing it into the syntax

**Rules:**

1. **Default Behavior (Auto-Assign):**
   - Compiler automatically assigns basis vectors to automaton fields
   - Assignment is deterministic: same automaton always gets same vectors (within a compilation unit)
   - Orthogonality check is invisible to developer
   - No syntax overhead in typical code

2. **Optional Explicit Annotation:**
   ```
   #automaton RobotState #basis: {
     left_speed: e1
     right_speed: e2
     battery: e3
   } {
     #states: [Idle, Moving]
     left_speed: f32
     right_speed: f32
     battery: f32
   }
   ```
   - Developer can annotate for clarity or debugging
   - No semantic difference; compiler uses annotation if provided, auto-assigns if not

3. **Multivector Representation:**
   - Each automaton's behavior is represented as a multivector (computed from field basis vectors and transitions)
   - Grade encodes state complexity: e1 (scalar), e_i ∧ e_j (bivector), etc.
   - Orthogonality is checked via outer product: `A ∧ B == 0` means no interference

4. **IDE Integration:**
   - Hover over automaton: IDE shows "behavior multivector: e1 ∧ e2 (grade 2, bivector)"
   - Hover over field: IDE shows "basis: e1"
   - Compilation error on conflict: compiler annotates conflicting automata with their basis assignments
   - `--verbose-basis` flag dumps all basis assignments and multivector products for debugging

5. **Error Messages:**
   ```
   error E0520: Orthogonality violation
     automaton RobotState (e1 ∧ e2) and UartState (e1 ∧ e3) share dimension e1
     → add #atomic annotation or refactor automata to separate concerns
   ```

**Example:**
```
#automaton RobotState {
  direction: Direction
  speed: f32
}
// Compiler auto-assigns: direction ← e1, speed ← e2
// Behavior multivector: e1 ∧ e2 (bivector, grade 2)

#automaton UartState {
  rx_buffer: [u8; 256]
  rx_head: u32
}
// Compiler auto-assigns: rx_buffer ← e3, rx_head ← e4
// Behavior multivector: e3 ∧ e4 (bivector, grade 2)

// Outer product: (e1 ∧ e2) ∧ (e3 ∧ e4) = e1234 (grade 4, non-zero)
// → No interference, orthogonal ✓
```

**Implications:**
- GA machinery is hidden by default, visible when needed
- Compiler remains auditable: `--verbose-basis` gives complete proof
- IDE becomes the primary interface for GA understanding
- Future extensions (grade constraints, custom basis assignments) are straightforward

---

## Decision #5: Automaton-as-Category, with State Changes via `#transition` Only ✓ LOCKED

**Question:** What is the precise relationship between `#effect` declarations, `#transition` declarations, and the `#states` list? When does an effect change the state of an automaton, and what happens when an automaton has no obvious state machine (an allocator, a logger, a register block)?

**Options Considered:**
- **Option A (Per-effect state coupling):** Effects declare their source/target state with `#from: S1 #to: S2` metadata; `#transition` blocks do not exist as separate constructs. Transitions are *implicit* in effect annotations.
- **Option B (FSM is optional):** State changes happen in `#transition` blocks; effects are state-agnostic. An `#automaton` may omit `#states` entirely, in which case it's a "stateless mutation bag" with no FSM machinery — a separate, simpler construct sharing the basis-vector mechanism.
- **Option C (Automaton-as-category):** Every `#automaton` is formally a small category whose objects are its states and whose morphisms are its transitions. The "stateless mutation bag" case is the *one-object monoid* of this construction — the same machinery, applied to a degenerate category. State changes happen in `#transition` blocks; effects called from anywhere else fire during the implicit identity morphism on the current state.

**Chosen:** Option C (Automaton-as-Category, with categorical foundation kept internal).

**Rationale:**
- Eliminates spec branching: there is one model with degenerate cases, not two parallel mechanisms.
- The GA orthogonality engine acquires a clean specification — the wedge-product check is exactly the existence proof for the product category `C_A × C_B` (Emergent Rule 6, below).
- Common patterns (allocator, logger, register block) become *one-object monoids* with zero ergonomic overhead — a user writes the `#automaton` declaration, omits `#states`, and the compiler treats it as a single-state-with-identity-only category.
- Hierarchical states (Statecharts-style) can be added in v0.2 as functors `C_substate → C_parent` without retrofitting the foundation.
- Aerospace and medical-device industries have a 30-year track record with formal-FSM tooling (Stateflow, Esterel, SCADE). Categorical foundations make Clifford continuous with that lineage rather than parallel to it.

**Rules:**

1. **Every `#automaton` is a small category `C_A`.**
   - Objects: identifiers in the `#states: [...]` clause.
   - Morphisms: `#transition Source -> Target { ... }` declarations, plus the implicit identity morphism at every state.
   - Initial object: the first state in the `#states` declaration order, or one marked `@initial`.
   - Terminal objects (optional): states marked `@terminal` from which no further transitions are declared.

2. **State changes happen exclusively inside `#transition Source -> Target { ... }` blocks.**
   - The body of a `#transition` block is a sequence of effect-procedure calls (`#> name(args)`), `#mutate` statements, and `@fn` calls. Upon successful completion of the body, the automaton's state-tag is updated to `Target`.
   - An effect cannot declare or perform a state change outside a transition block.

3. **Effects called outside `#transition` blocks fire during the identity morphism at the current state.**
   - These effects mutate fields per their `#mutates` clause but do not change the state-tag.
   - The compiler labels every `#> name(args)` call site at parse/resolve time as either *transition-context* (inside a `#transition` body) or *identity-context* (everywhere else). The labeling is invisible to the user and emitted only into the typed AST.

4. **The `#states` list is optional.**
   - If `#states: [...]` is absent, the compiler treats the automaton as `#states: [Ready]` with no declared transitions. The category `C_A` has one object and only the identity morphism — a *one-object monoid*. All effects on this automaton fire in identity-context.
   - This is the canonical form for allocators, loggers, register blocks, counters, and any other "bag of related mutable state" — no ceremony.

5. **Self-loops (`#transition State -> State { ... }`) are not required.**
   - Same-state work is expressed by calling effects in identity-context; the implicit identity morphism is always present in `C_A` by definition of category.
   - Explicit self-loops are *permitted* (and may be required by `--lint=require-explicit-self-loops` in safety-critical projects), but never mandatory.

6. **Category-theoretic terminology is internal.**
   - Spec error messages, IDE displays, and tooling output use FSM language: "states," "transitions," "initial state," "unreachable state."
   - The categorical foundation appears in `Appendix B: Categorical Semantics` of `CLIFFORD_SPEC.md` and in the engine's internal data model. Users do not have to learn category theory to use Clifford.
   - A `cliffordc inspect --as-category <Automaton>` mode exists for power users; otherwise the foundation is unobservable on the surface.

**Example (one-object monoid — no ceremony):**

```
#automaton Logger {
  buffer: [u8; 1024]
  head: usize
}

#effect log(msg: &[u8]) #mutates: [Logger] {
  // ...
}

// Internally: C_Logger = one object {Ready}, only identity morphism.
// Every #> log(...) call fires in identity-context.
```

**Example (multi-state FSM — only non-identity transitions written):**

```
#automaton UartRx {
  #states: [Idle, Receiving, Overflow]
  buf: [u8; 256]
  head: u32
}

#effect start_recv()       #mutates: [UartRx] { /* ... */ }
#effect ingest(b: u8)      #mutates: [UartRx] { /* ... */ }
#effect overflow_handler() #mutates: [UartRx] { /* ... */ }

#transition Idle -> Receiving      { #> start_recv() }
#transition Receiving -> Overflow  { #> overflow_handler() }

// Per-byte ingestion fires in identity-context on Receiving:
#interrupt UART_RX() #mutates: [UartRx] #priority: HIGH {
  let b = #> uart_read_data();
  #> ingest(b);   // identity-context, no transition declared or needed
}
```

**Implications:**

- §6.1 reachability and deadlock analysis runs uniformly: for the one-object monoid case, the graph is trivially well-formed (initial = terminal = the sole object, all paths trivial). For multi-state automata, standard graph analysis applies.
- §7 GA orthogonality: behavior multivectors are unchanged. The wedge-product check is reformulated as the existence proof for the product category (Emergent Rule 6).
- §8.4 codegen: the state-tag prologue/epilogue is generated for `#transition` block entry/exit, not per-effect. Identity-context effects emit no state-tag manipulation.
- The previous spec ambiguity around "does `#transition` exist as a separate construct?" is closed: yes, `#transition` is the only place state changes happen, and effects are reusable across multiple transitions.
- Future hierarchical states (v0.2) drop in as functors without retrofitting v0.1.

### Refinements to Decision #5 (locked April 30, 2026)

These four refinements emerged from writing concrete programs in the v0.3 syntax. They sharpen Decision #5 without changing its substance.

**Refinement #5a: Effects are top-level items, not automaton members.** `DECISIONS.md` examples and the canonical embedded patterns both write `#effect main_loop() #mutates: [...]` at module level rather than inside an `#automaton` block. The `#mutates` clause associates an effect with the automata it touches; effects do not need to live inside any one of them. Only `automaton_field` declarations and `#transition` declarations belong inside `#automaton` blocks.

**Refinement #5b: Transitions are named.** The grammar form is `#transition <name>: Source -> Target { body }`. Without names, you cannot disambiguate two transitions between the same state pair (e.g., `Idle -> Receiving` triggered by a start signal vs. by a timeout). Transitions are fired via `#> name(args)`; the compiler classifies the call as transition-context by virtue of the callee resolving to a `#transition` (not by lexical position inside another transition body — see the call-context generalization below).

**Refinement #5c: Local stack mutation is allowed in any function body, including `@fn`.** §4.6's "mutation context" rule governs *automaton-field* writes only. A `let mut i = 0; while ... { i = i + 1 }` inside an `@fn` is fine; it's invisible to the caller and doesn't break referential transparency. Without this, no iterative algorithm is writable in `@fn` (forced to recursion), which is a non-starter for embedded.

**Refinement #5d: State inspection via `<Auto>@state` and `<Auto>::<StateName>`.** The current state of a multi-state automaton is read with `RxState@state`; its named states are addressed as `RxState::Idle`. Required for guard conditions in ISRs and for tests. For monoid automata (one-object categories), `@state` is a static error — only one state exists, and the compiler tells you to remove the redundant comparison.

**Generalization of the CallContext classification (consequence of #5b):** the call-site classification rule (Decision #5 Rule 3) generalizes to: *transition-context ⟺ callee resolves to a `#transition`; identity-context ⟺ callee resolves to an `#effect` and the call is not transitively reached from inside a `#transition` body.* This is cleaner than the original "lexical scope of transition body" rule and handles named transitions without special cases.

**Refinement #5e (Transition Atomicity).** Per Decision #5's categorical model, every `#transition` is a single morphism in C_A. To make that abstraction hold against concurrent observation, the implementation enforces atomicity by the following rule.

For each `#transition T` in automaton A:

1. Compute the **interrupt-overlap set** `R(A) = { I | I is a #interrupt and I.#mutates names A, transitively through #> calls }`. The set computation is per-automaton mutates-intersection — same machinery §6.2 / §7 already maintains. No GA-engine extension needed.
2. **If R(A) is empty** (no interrupt can observe A's intermediate state), emit T's body with no atomic wrapping. The transition is naturally atomic from any concurrent perspective. Common case: one-shot Boot transitions that run before interrupts are enabled.
3. **If R(A) is non-empty:**
   - If T has an explicit `#atomic` clause, use the user-declared mechanism.
   - Otherwise, **default-wrap T's body in `#atomic: interrupt_critical`** (single-core) or `#atomic: multicore_critical` (multi-core targets where the concurrent peer is on another core). The state-tag write at body completion is *inside* the wrapper, so the entire morphism — field writes plus state-tag commit — is atomic to any concurrent observer.
4. **If T has an explicit `@non_atomic` top-level attribute,** emit no wrapping but issue `W5201: transition T can be preempted by interrupt I; explicit @non_atomic acknowledged`. This is the user's escape hatch for performance-critical cases (e.g., a transition body that does its own lock-free synchronization).

**Composition with §6.5 invariants:** invariants are checked after the atomic wrapper completes. They see the post-commit state, never the intermediate. Compose cleanly with §7 GA orthogonality: GA catches concurrent write-write races at field granularity; transition atomicity ensures multi-field writes within a transition appear as one event.

**Implications:** §6 grows the interrupt-overlap-set algorithm; §8.4 codegen wraps in target-atomic when R(A) non-empty. New warning code `W5201`. Spec impact: ~30 lines added.

---

## Decision #6: Memory-Mapped Registers as Automata ✓ LOCKED

**Question:** How does Clifford model memory-mapped peripheral registers? Should `#hardware` be a separate construct?

**Chosen:** Drop `#hardware`. Register blocks are normal `#automaton` declarations with a `#address: <integer>` annotation on the automaton and `#offset: <integer>` / `#access: r|w|rw` annotations on each field. Reads and writes go through the same `#mutate` and field-access machinery as application-state automata; the compiler emits volatile loads/stores at `address + offset`.

**Rationale:** Unifies the model — registers are the same kind of thing as application state from the GA engine's perspective (each register field gets its own basis vector). Register-race bugs become normal orthogonality violations. HAL crates become normal Clifford modules. No separate type system or syntax to learn.

**Example:**
```
#automaton Usart1 #address: 0x4001_3800 {
  CR1: u32 #offset: 0x00 #access: rw
  BRR: u32 #offset: 0x0C #access: rw
  ISR: u32 #offset: 0x1C #access: r
  TDR: u32 #offset: 0x28 #access: w
}
```

**Implications:** §2.5 grammar drops `hardware_decl`; adds `address_clause` on `automaton_decl` and `field_attr*` on `automaton_field`. §8.4 lowering: register-field reads emit `volatile load i32, ptr inttoptr(address + offset)`, writes emit the corresponding volatile store. `#access` annotations cause the compiler to reject reads from write-only fields (and vice versa) at compile time.

---

## Decision #7: Test Primitive `#test "name" { … }` ✓ LOCKED

**Question:** How do users write tests, given that `@fn` cannot call `#effect` (Emergent Rule 4)?

**Chosen:** A new top-level item form: `#test "name" { body }`. The body is in `#`-context and may freely call both `@fn` and `#effect`/`#transition`. Each test runs in isolation (automata reset to initial state before invocation). Tests are discovered by `cliffordc test`, do not appear in production binaries, and have access to a built-in `assert(expr)` and `panic(msg)`.

**Rationale:** The `@`/`#` boundary is what gives Clifford its safety guarantees, but it also blocks the most natural test pattern (pure setup → effect invocation → assertion). `#test` is a deliberately mixed-layer escape hatch confined to the test runner. It's the smallest viable testing primitive.

**Example:**
```
#test "scheduler picks lowest-vruntime ready task" {
  let snap := SchedulerSnapshot { /* ... */ };
  match pick_next(&snap) {
    Switch(3) => {},
    _         => panic("wrong choice"),
  }
}
```

**Implications:** §2.1 adds `test_decl` to top-level items. §10 conformance tests use `#test` as the primary harness. §8 codegen elides `#test` items entirely in non-test compilation modes.

---

## Decision #8: `:=` Short Binding ✓ LOCKED

**Question:** Should `let x = expr` have a short form?

**Chosen:** Yes. `let x := expr;` is sugar for `let x = expr;` with type inference required (no explicit type annotation). `let mut x := expr` is **not** allowed — mutable bindings require the explicit `let mut x: T = expr` form for visibility. The `:=` form is type-inferred immutable bindings only.

**Rationale:** Matches DECISIONS examples; reduces visual ceremony in the common case (immutable, type-inferred local bindings); matches Verse, Pony, and various pseudocodes.

**Example:**
```
let snap := build_snapshot();        // type inferred
let mut count: u32 = 0;              // mut requires explicit form
```

**Implications:** §2.6 adds `:=` to `let_stmt`. §1.4 adds `:=` as an operator. Compiler error if `mut` appears with `:=`: `E0210: := short form does not support mut`.

---

## Decision #9: Drop `#visible` / `#hidden` ✓ LOCKED

**Question:** What do the `#visible` / `#hidden` clauses on extern effects do?

**Chosen:** Drop them. Extern effects use `#mutates` and `#cannot_mutate` directly; the visibility clauses were redundant restatements with different names.

**Rationale:** One concept beats two. The naming difference (`visible/hidden` vs `mutates/cannot_mutate`) carried no semantic difference, only confusion.

**Implications:** §2.5 grammar drops `visible_clause` and `hidden_clause`. Extern effects (and any future cross-compilation-unit constructs) declare their mutation profile via the same `#mutates` / `#cannot_mutate` clauses as in-module effects. The §3 AST shrinks accordingly.

---

## Decision #10: Interrupt Vector Naming via Linker Symbol ✓ LOCKED

**Question:** How does `#interrupt NAME` resolve to a target-specific interrupt vector slot?

**Chosen:** By linker symbol. `#interrupt USART1_IRQHandler() { … }` emits a function with the linker symbol `USART1_IRQHandler`. The user uses target-standard symbol names; HAL crates document them. No config file is required for v0.1.

**Rationale:** Matches how Cortex-M / RISC-V startup files and vendor-supplied vector tables already work. No new abstraction layer; users already know these names from datasheets. Per-target config files are a v0.2 convenience (`clifford-target.toml` mapping logical names to physical vectors), not a v0.1 requirement.

**Implications:** §8.5 lowering: `#interrupt NAME` produces an LLVM function with `@NAME` symbol, target-specific calling convention (`arm_aapcs_vfpcc` for Cortex-M, etc.), and section attribute `.interrupts` (or target equivalent). The vector table is the user's startup-code responsibility for v0.1.

---

## Decision #11: `@sequential(A, B)` Attribute ✓ LOCKED

**Question:** How do users tell the compiler "these two automata never run concurrently" when the GA engine cannot infer it?

**Chosen:** A top-level attribute `@sequential(AutomatonA, AutomatonB);` that sets `can_concur(A, B) = false` regardless of the §7.3 inference. Multiple sequential pairs may be declared. Attribute is bidirectional (sequencing is symmetric).

**Rationale:** The field-level granularity of the GA engine is sometimes too coarse — e.g., a boot-phase automaton that only runs during `Init` shares fields with a runtime-phase automaton that only runs during `Running`. The fields are temporally disjoint but the GA engine cannot prove this. `@sequential` is the user-supplied escape hatch.

**Example:**
```
@sequential(Boot, RuntimeWorker);   // boot completes before runtime starts
```

**Implications:** §2.1 adds `sequential_attr` to top-level items. §7.3 `can_concur` consults the user-declared sequential pairs before applying the heuristic. The attribute is a *trusted assertion* — if the user declares `@sequential(A, B)` but they actually do run concurrently, behavior is undefined; the compiler does not check this. (A v0.2 `@phase(boot|runtime)` sugar will encode the common case more safely.)

---

## Decision #12: `#staged` Automata for Deferred Mutation ✓ IMPLEMENTED IN V0.2 (slices 18–19, 24–25)

**Question:** Should Clifford have a first-class deferred-mutation mechanism for ISR-to-main-handoff and multi-field consistent updates?

**Chosen design (implemented in v0.2):** A `#staged #automaton Foo { … }` modifier that buffers writes in a shadow state struct; an explicit `#flush Foo;` statement commits the shadow to live atomically. Inside `#staged` automata, `#mutate Foo { … }` writes to the shadow, not the live state. The GA engine treats `#staged` automata identically to non-staged ones for orthogonality (timing doesn't change field overlap).

**Implementation status (2026-05-09):**

| Slice | Surface / mechanism                              | Errors           |
|-------|--------------------------------------------------|------------------|
| 18    | `#staged` modifier + `#flush Name;` statement; shadow global per staged automaton; codegen redirects writes to `@Name.shadow`; `#flush` lowers to `llvm.memcpy` shadow → state | E0412 `FlushOnNonStaged`, E0413 `FlushOfUnknownAutomaton` |
| 19    | `#flush A;` flows into `clifford-effect`'s mutation profile as a write to **every declared field of A**, so the existing `#mutates: [...]` validator fires uniformly (E0410) and the §7 ortho engine sees flush-vs-field-write races | (uses existing E0410) |
| 24    | `@shadow Auto.field` operator — companion to `@snapshot` that reads from the pending shadow global instead of live state | E0414 `ShadowOnNonStaged` |
| 25    | Reject `#staged` on register-block automata at parse time (memory-mapped I/O has no meaningful shadow) | E0212 `StagedRegisterBlock` |

**Rationale for original deferral (preserved for context):** v0.1's job was to land the GA proof on real hardware. The pattern was implementable in v0.1 using `#atomic: interrupt_critical` plus a hand-written queue. The v0.2 reference-firmware work (audit-marker chain + sigma-loop ergonomics) confirmed `#staged` as a clean fit; implementation followed.

**Implications:** §6 of `CLIFFORD_SPEC.md` will be updated to formalise the staged write semantics and the `#flush` profile expansion. The reserved-keyword note in v0.1 is now consumed: `#staged`, `#flush`, and `@shadow` are real language keywords as of v0.2.

**What's deliberately NOT in v0.2 (deferred to later milestones):**

- **Reads-from-shadow inside the staged automaton's own transitions.** Currently writes target shadow but reads always come from live state. A future operator (or implicit redirection inside transitions) could let an ISR read its own pending writes. v0.2's `@shadow Auto.field` partly covers this — it reads pending shadow from any callable.
- **Partial flushes (`#flush Foo { x, y };`).** A field-subset commit. v0.2 always commits the whole struct.
- **Non-memcpy commit strategies.** v0.2 uses a single `llvm.memcpy.p0.p0.i64`. Larger structs may benefit from per-field volatile stores or other strategies on specific targets; v0.2's choice keeps the codegen target-pointer-width-agnostic.

---

## Decision #13: Body-Scoped References with Provenance Tracking ✓ LOCKED

**Question:** How does Clifford prevent use-after-free, double-free, dangling references, and iterator invalidation without adopting Rust's lifetime annotations?

**Chosen:** A six-rule discipline that catches the common UAF cases by structurally removing the language features that force lifetime annotations to exist.

**Rules:**

0. **`&mut T` references are restricted to stack-local values.** Automaton fields are mutated exclusively via `#mutate`. There is no `&mut Scheduler.field` form. `&Scheduler.field` (immutable) is permitted but governed by Rules 3 and 4.
1. **No reference returns.** A function (`@fn`, `#effect`, `#interrupt`, `#hardware`, `#transition`) cannot have `&T` or `&mut T` in its return type.
2. **No reference fields.** Struct fields, ADT variants, automaton fields, and tuple components cannot have reference type. References live only on the call stack.
3. **Single-flow `&mut` uniqueness within a body.** Within one body's straight-line control flow, at most one mutable reference to any given memory location is live at a time.
4. **Field-provenance invalidation.** A reference `r` derived from automaton field `A.f` is invalidated by any `#mutate A { f = … }` between `r`'s creation and its use. Using `r` after that point is `E0701: reference invalidated by mutation`.
5. **Owned values are linear with respect to deallocation.** A `Box<T>` (or any allocator-produced owned value) consumed by a free operation cannot be used afterward; the type is moved into `#free` and the original binding becomes inaccessible.

**Rationale:** Rust's lifetime annotations exist because references can outlive function calls (returned, stored in fields, captured in closures, suspended across async). Clifford has no closures, no async, no iterators-returning-refs, and Rules 1+2 forbid the remaining escape paths. With those features absent, lifetimes collapse to "until the end of the containing body," and the borrow checker collapses to a single-pass intra-body analysis with no annotations. Catches UAF cases 1, 2, 3, 5 from Rust's UAF taxonomy (case 4, cross-thread races, is already caught by the GA engine).

**Cost:** loses zero-copy view patterns (`Iterator::next() -> Option<&T>`, `String::as_str() -> &str`, `slice::split_at_mut`). For embedded firmware these are rarely needed; for general systems work some library patterns require rewriting to copy values or take callbacks.

**Implications:** New §5.7 "Reference Provenance and Body-Scoped Borrowing" in spec. Error code class `E0700–E0709`. Implementation in `check` crate: per-body provenance graph, ~2 weeks of work. Adding lifetime annotations later (v0.2 for non-firmware ambitions) is additive — does not break Decision #13's defaults.

---

## Decision #14: Sigma Loop as Primary Iteration Construct ✓ LOCKED

**Question:** Given Decision #13 forbids reference returns (which complicates traditional iterator traits), what is Clifford's primary iteration construct?

**Chosen:** A `sigma` loop with bounded iteration variable and compile-time bounds tracking.

**Syntax:**
```
sigma_loop    := 'sigma' sigma_pattern 'in' sigma_source block
sigma_pattern := ident                              // value-only
              |  '(' ident ',' ident ')'             // (index, value)
sigma_source  := range_expr                          // 0..len
              |  array_expr                          // arr or &arr
```

**Examples:**
```
sigma x in &arr { use(x); }
sigma i in 0..n { arr[i] = process(arr[i]); }
sigma (i, x) in &readings { if predicate(x) { flags[i] = true; } }
```

**Semantics:**
- The iteration variable carries an implicit bound annotation.
- Direct array access with `arr[i]` is statically bounds-checked: if the compiler proves `i < arr.len()` (because `arr` is a fixed-size array `[T; N]` matching the loop bound), no runtime check is emitted.
- Arithmetic on the iteration variable widens its type; subsequent accesses fall back to runtime checks unless trivially provable.
- The bound expression is evaluated once at loop entry; mutation of the bound during iteration has no effect (`E0802`).

**Rationale:** Decision #13 makes the `Iterator::next() -> Option<&T>` trait machinery structurally absent; we need a primary iteration construct anyway. Sigma loops compile to counted loops with bounds-check elimination at zero runtime cost — guarantee, not hope. Refinement-types-lite without an SMT solver. Allocation-free by construction (critical for Cortex-M0+ code-size).

**Implications:** §2.6 adds `sigma_loop` to expression grammar. §5.8 "Sigma Bounds Tracking" is a new spec section describing the per-loop bounds inference. §8.4 lowering: counted-loop emission with no bounds checks when statically proved. Error code class `E0800–E0809`.

**Refinement #14a (Sigma over runtime-sized slices).** Sigma loops support `&[T]` and `&mut [T]` as iteration sources. The iteration variable's bound is `slice.len()`, captured once at loop entry per the §5.8 "bound captured at loop entry" rule. Direct accesses `slice[i]` inside the body are statically bounds-elided because `i: bounded<0, slice.len()>` is a refinement type and `slice[i]` is the access at index `i` of length `slice.len()` — by construction safe.

```
#effect process_dma(buf: &[u8]) #mutates: [...] {
  sigma x in buf { process_byte(x); }                // element-only
  sigma (i, x) in buf { if x > t { flag(i); } }      // index + element
  sigma i in 0..buf.len() { let x := buf[i]; … }     // range over runtime length
}
```

For raw access pointers (DMA descriptors), the user passes `len: usize` separately and uses `#unchecked_offset` inside the body; the soundness of `i < len` is the user's assertion (the narrow unsafe primitive layer; not compiler-proven). This split is honest: typed slices = compiler-proven; raw access pointers = user-asserted.

**LLVM lowering for the slice case:** `slice.len()` is loaded once via `extractvalue` from the fat pointer at loop entry; the body uses `getelementptr inbounds` with the iteration variable; no per-iteration bounds check is emitted. Same code-size as a hand-written C loop.

---

## Decision #15: Single-Field Mutation Sugar ✓ LOCKED

**Question:** The `#mutate Auto { field = expr };` form is verbose for the common case of writing one field. Is there a sugar form that preserves the semantics?

**Chosen:** Yes. `Auto.field = expr;` and `Auto.field <op>= expr;` (for `op ∈ {+, -, *, /, %, &, |, ^, <<, >>}`) at statement position desugar to the canonical `#mutate Auto { field <op>= expr };`. The compiler tracks them identically — same orthogonality check, same field-provenance tracking. The sugar is statement-position only (never expression-position) and only inside a `#`-context body.

**Rationale:** Single-field writes are the most common imperative operation; the `#mutate Auto { ... };` wrapping is pure ceremony for that case. Sugar takes Clifford's imperative texture from "2× C verbosity" to "1.3× C verbosity" — the remaining overhead is the `#mutates` clause on the function header, which is load-bearing safety information. The bulk-write form `#mutate Auto { f1 = ..., f2 = ... };` stays for multi-field updates.

**Example (compare):**
```
// Canonical form (still legal):
#mutate Counter { blinks = self.blinks + 1 };

// Sugared form:
Counter.blinks += 1;
```

**Trade-off:** loses a small amount of "every line shows its sigil"; mitigated by the rule that the sugar is only valid inside `#`-context (so the layer is unambiguous from the enclosing function).

**Implications:** §2.6 grammar adds `mutate_short_stmt`. §5 typed AST treats short and canonical forms as the same node kind. No codegen changes. No GA changes.

---

## Decision #16: Plugin Mutators via Effect Interfaces ✓ LOCKED

**Question:** How does Clifford express "code that operates on any UART" / "code that operates on any allocator" without coupling to a specific implementation? How are HALs (Hardware Abstraction Layers) and pluggable drivers written?

**Chosen:** Effect interfaces (Interpretation A from the design discussion). An `#interface` declares a set of effect signatures; an `#impl Interface for Automaton { … }` block provides the bodies; effects parameterised over an interface accept any implementor and are monomorphized at compile time. Static dispatch only — no vtables, no runtime dispatch (consistent with Emergent Rule 5).

**Rationale:** Interfaces are to effects what Clifford's existing `@trait`s (Decision #2) are to pure functions. The two sit cleanly orthogonal: `@trait` gives polymorphism over `@fn`s by structural method matching; `#interface` gives polymorphism over `#effect`s by explicit implementation. Together they cover the polymorphism needs of a HAL ecosystem without inviting dynamic dispatch into the language.

The closest analog in production languages is Pony's interfaces, but Pony interfaces describe object methods. Clifford's `#interface` describes effect signatures — which include `#mutates` clauses — and so the GA orthogonality engine sees through the dispatch (each monomorphized variant has its own behavior multivector).

**Rules:**

1. **Interface declaration.** `#interface Name { effect sig; effect sig; … }` lists effect signatures. The `#mutates: [self]` clause is implicit on each signature; `self` refers to the implementing automaton.

   ```
   #interface Serial {
     effect send_byte(b: u8);
     effect recv_byte() -> u8;
   }
   ```

2. **Implementation block.** `#impl Interface for Automaton { … }` provides effect bodies. Each effect's `#mutates` set defaults to `[Automaton]` plus any other automata the body actually touches; the user may declare more. Implementation effects share namespace with their interface.

   ```
   #impl Serial for Usart1 {
     effect send_byte(b: u8) {
       while (Usart1.ISR & 0x80) == 0 {}
       Usart1.TDR = b as u32;
     }
     effect recv_byte() -> u8 {
       while (Usart1.ISR & 0x20) == 0 {}
       Usart1.RDR as u8
     }
   }
   ```

3. **Generic effects parameterised over an interface.** Effects may declare type parameters constrained by interface bounds. Calls to interface effects from the body resolve at monomorphization to the concrete implementation.

   ```
   #effect log_message<S: Serial>(msg: &[u8]) #mutates: [S] {
     sigma b in msg {
       #> S::send_byte(b);
     }
   }
   ```

4. **Call-site monomorphization.** Each instantiation of a generic effect produces a distinct LLVM function. `#> log_message<Usart1>("hi")` and `#> log_message<SoftwareUart>("hi")` are independent specializations. The GA engine analyzes each specialization separately — no interface-level basis vector is needed; each specialization carries the implementing automaton's field basis.

5. **No runtime dispatch.** Per Emergent Rule 5, all `#>` calls are statically resolvable. An interface is *not* a trait object; you cannot have a value of type `dyn Serial`. If a use case needs runtime dispatch (rare in firmware; sometimes useful in larger systems), users build it explicitly with function-pointer fields and `#unsafe` discipline. v0.2 may consider a `dyn Interface` opt-in form.

6. **Coherence.** A given interface may have at most one `#impl` per implementing automaton in the same compilation unit. Cross-module orphan rules: an `#impl Interface for Automaton` is permitted only in the module that declares either the `Interface` or the `Automaton`. Standard coherence; matches Rust's orphan rule.

**Example: a generic ring buffer over any Serial:**

```
#automaton TxQueue {
  buf: [u8; 64]
  head: usize
  tail: usize
}

#effect drain_tx<S: Serial>() #mutates: [TxQueue, S] {
  while TxQueue.tail != TxQueue.head {
    let b := TxQueue.buf[TxQueue.tail];
    #> S::send_byte(b);
    TxQueue.tail = (TxQueue.tail + 1) % 64;
  }
}

#effect main() #mutates: [Boot, Usart1, TxQueue] {
  // ...
  #> drain_tx<Usart1>();
}
```

The `drain_tx<Usart1>` specialization has behavior `{TxQueue.tail, Usart1.TDR, ...}`. Swap to `drain_tx<SoftwareUart>` and the behavior changes to `{TxQueue.tail, SoftwareUart.bit_state, ...}`. The GA engine sees the right thing in each case.

**Implications:**

- §2.1 grammar adds `interface_decl` and `impl_decl` to top-level items. Generics on `#effect` declarations gain interface-bound syntax: `<S: Serial>`.
- §5 type system: interface bound checking; coherence verification; monomorphization at call sites.
- §6 effect extraction: monomorphized specializations are analyzed individually; interface methods carry through correctly.
- §7 GA: no new basis assignments — interfaces are static-dispatch contracts only, not algebraic constructs.
- §8 codegen: each `(generic_effect, interface_arg)` pair produces a distinct mangled symbol.
- New error code class `E0900–E0909` for interface/implementation issues.

This decision opens the path to a real HAL ecosystem: `clifford::hal::serial`, `clifford::hal::spi`, `clifford::hal::adc` define interfaces; vendor crates implement them per-MCU; application code is portable across implementations. A v0.2 deliverable.

---

## Decision #17: Ada-Style Narrow Unsafe Primitives ✓ LOCKED

**Question:** How does Clifford handle operations that cannot be statically verified — raw pointer access, bit-casts, inline assembly, volatile MMIO that doesn't fit the register-block automaton model? How is the boundary between safe and unsafe code expressed?

**Chosen:** Ada-style narrow primitives. Replace the Rust-style aggregating `#unsafe { … }` block with a catalog of *specific unsafe operations*, each its own sigil-prefixed form. Each unsafe operation is individually visible, individually grep-able, individually auditable. There is no surrounding "unsafe block."

**v0.1 catalog of unsafe primitives:**

```
#unchecked_load<T>(ptr: *const T) -> T              // raw read
#unchecked_store<T>(ptr: *mut T, val: T)            // raw write
#volatile_load<T>(ptr: *const T) -> T               // volatile read (MMIO)
#volatile_store<T>(ptr: *mut T, val: T)             // volatile write (MMIO)
#unchecked_cast<S, T>(val: S) -> T                  // bit-cast / transmute
#asm("...", inputs, outputs)                         // inline assembly
```

These are ordinary expressions/statements inside `#`-context bodies. No block-level "unsafe" decoration. Each occurrence carries its own sigil, its own grep target, its own audit cost.

**Rationale:** The Rust-style `unsafe { … }` block is a known source of audit failure in safety-critical code — a 30-line block can be doing many things, and each line's audit cost is hidden behind block-level decoration. Ada and SPARK have used per-operation pragmas (`Unchecked_Conversion`, `Unchecked_Deallocation`, `Unchecked_Access`) for decades in DO-178C avionics and IEC 62304 medical software because they make every unsafe occurrence individually visible. Clifford's positioning as a safety-critical-first language makes the Ada approach the right fit.

The narrow-primitive approach also composes cleanly with everything else in Clifford:
- Each primitive is a normal statement/expression in a `#`-context — no mutation-context bookkeeping change needed (§4.6).
- Reference provenance tracking (§5.7) sees each primitive individually; no need to special-case "everything inside an unsafe block."
- Codegen (§8.4) emits a one-to-one LLVM operation per primitive — no block desugaring.
- Tooling (`cliffordc lint --max-unsafe-ops=N`) can fail builds with too much unsafety, since each occurrence is countable.

**Removed from v0.1:** the `#unsafe { … }` block syntax. Code that was inside such a block must be rewritten using the narrow primitives. If a use case is not covered by the catalog, file a v0.2 request to add a primitive — never reintroduce the aggregating block.

**Coverage check.** The catalog covers:
- Raw pointer reads/writes (`#unchecked_load`/`store`)
- Memory-mapped I/O outside the register-block model (`#volatile_load`/`store`) — note that register-block automata (Decision #6) use the same `#volatile_*` primitives under the hood, but users go through `#mutate Auto { field = … }` and the compiler emits the volatile op for them.
- Bit-casts and transmutes (`#unchecked_cast`)
- Inline assembly (`#asm`)

Anything else (e.g., reading from undefined memory, dereferencing null) is undefined behavior on the user, not a separate primitive.

**Tooling:**

```
cliffordc lint --max-unsafe-ops=N    // fail build if more than N occurrences of unsafe primitives
cliffordc audit --list-unsafe        // print every unsafe-op location with file:line
```

**Implications:**

- §1.3 sigil-prefixed forms: add `#unchecked_load`, `#unchecked_store`, `#volatile_load`, `#volatile_store`, `#unchecked_cast`, `#asm`. Remove `#unsafe`.
- §2.6 statements: each primitive is an `expr` or `stmt` form; the `unsafe_block` production is removed from the grammar.
- §4.6 mutation contexts: remove "Inside a `#unsafe { … }` block (raw pointer reads/writes only)" from the list. Mutation contexts are now just `#effect`, `#interrupt`, `#transition`, `#impl` method bodies, and `static` initializers. The narrow unsafe primitives can appear in any of these without adding a new context.
- §5.7 reference provenance: clarify that raw pointer ops (via `#unchecked_*` and `#volatile_*`) are not provenance-tracked; their safety is the user's responsibility per occurrence.
- §8.4 codegen: each primitive lowers to a single LLVM operation (`load`, `store`, `load volatile`, `store volatile`, `bitcast`, inline asm).
- §10 conformance tests: tests for each primitive's compile-time and runtime behavior; tests that the absence of `#unsafe` block syntax is enforced.

This decision sharpens Clifford's safety-critical positioning without changing the language's overall feel — the narrow primitives are *less* visually intrusive than a Rust-style `unsafe { … }` block once you stop trying to cluster unsafety.

---

## Decision #18: Runtime Auditing of Unsafe Primitives — V0.2 SURFACE + MARKERS LANDED (slices 20–23, 26); RUNTIME PASS DEFERRED

**Question:** Decision #17 makes unsafe operations visible per-occurrence at *compile time*. But static visibility doesn't catch *runtime* bugs — a `#unchecked_load` may compile fine and still dereference a freed pointer at execution. How does Clifford add runtime validation of unsafe primitive calls (KASAN/ASan-style) in a way that fits the language's automaton-and-interface model?

**Chosen design (surface + codegen markers in v0.2; runtime pass + stdlib in a later milestone):** A `#audit` annotation on automata or modules opts into runtime tracking of narrow unsafe primitives in *debug builds only*. The tracking is performed by a `Sanitizer` automaton implementing a compiler-supplied `PointerAuditor` interface (Decision #16); the default implementation maintains a shadow allocation table, validates pointer arithmetic, and reports violations with source locations. Users can swap in custom Sanitizer impls. Release builds elide all runtime checks; the `#audit` annotation produces zero overhead.

**Implementation status (2026-05-09):**

| Slice | Coverage                                                                                              |
|-------|-------------------------------------------------------------------------------------------------------|
| 20    | `#audit` modifier surface (AST `audited: bool` on `AutomatonDecl`; parser composes with `#staged` in any order) |
| 21    | Codegen markers at `#unchecked_*` / `#volatile_*` / `#unchecked_cast` emission sites inside an `#audit` automaton's transition bodies |
| 22    | Marker propagation to `#effect` / `#interrupt` whose `#mutates: [...]` lists an audited automaton    |
| 23    | Marker propagation to register-block volatile loads/stores (`Auto.field = …`, `@snapshot Auto.field`); marker names the touched peripheral specifically |
| 26    | Multi-owner marker text — `[A, B, C]` bracketed list when multiple audited automatons in one `#mutates` clause |

The marker shape is `; audit-wrap site for <Owner> (<primitive>) ; Decision #18` (single owner) or `; audit-wrap site for [A, B] (<primitive>) ; Decision #18` (multi). The future runtime pass turns each marker into a `PointerAuditor::validate_load/store` call dispatched against every named Sanitizer instance.

**What's still deferred (the v0.4+ runway):**

- **`PointerAuditor` interface** — needs `clifford-stdlib` scaffolding (no `.cl`-source stdlib infrastructure yet).
- **Default `ShadowSanitizer` impl** — depends on stdlib + allocator infrastructure (Decision #21+).
- **Marker → call rewrite pass** — straightforward IR rewrite once the stdlib interface exists.
- **Build-mode toggle** — release-build elision is trivial (markers are IR comments LLVM strips), but a release-mode `--strip-audit` flag would also drop the markers from debug-build IR-text dumps.

**Rationale for the partial deferral:** v0.2's job was to land the marker placement so the codegen surface is stable. The runtime pass requires stdlib work that's independent of the marker design — once the stdlib lands, the wrap-emitting pass is a self-contained slice. Splitting the work this way let v0.2 ship the `#audit` surface without blocking on stdlib design.

**Together with Decision #17:** the two form Clifford's complete unsafe-code story — Decision #17 (static, locked v0.1) is per-occurrence audit at compile time; Decision #18 (runtime, v0.2 surface + future runtime pass) is per-call validation at execution time.

**Sketch:**

```
#interface PointerAuditor {
  effect record_alloc(ptr: *mut u8, size: usize);
  effect record_free(ptr: *mut u8);
  effect validate_load(ptr: *const u8, size: usize) -> bool;
  effect validate_store(ptr: *mut u8, size: usize) -> bool;
}

// Compiler-supplied default
#impl PointerAuditor for ShadowSanitizer { … }

// User opts in:
#audit
#automaton MyBumpAllocator {
  // every #unchecked_*/#volatile_*/#unchecked_cast inside this
  // automaton's effects is wrapped (in debug builds) with
  // PointerAuditor calls dispatched through a (default or
  // user-overridden) Sanitizer instance.
  …
}
```

**Rationale for deferral:**

- v0.1's mandate is the GA proof on real hardware. Runtime tracking adds significant codegen complexity (per-primitive shadow state, allocation hash tables, dispatch-through-Sanitizer wrappers).
- We don't yet know which runtime bug patterns Decision #17's *static* audit lets through. Designing the Sanitizer interface speculatively is over-engineering; designing it informed by real v0.1 bug reports is engineering.
- Some embedded targets cannot afford even debug-build pointer tracking (RAM tight on Cortex-M0+). The opt-in nature is essential.
- Decision #16 (`#interface` + `#impl`) is the natural substrate; we have it. v0.2 just adds the `#audit` annotation, the `PointerAuditor` interface, and the default `ShadowSanitizer`.

**Together with Decision #17:** the two form Clifford's complete unsafe-code story:
- Decision #17 (static, locked v0.1) = per-occurrence audit at compile time.
- Decision #18 (runtime, v0.2) = per-call validation at execution time.

Static catches "too much unsafe in this codebase"; runtime catches "this particular call is wrong." Different bug classes; both worth tooling for.

**Implications:** No v0.1 spec changes. `#audit` and the `PointerAuditor` interface name are reserved keywords for v0.2. v0.2 will land `clifford::audit::ShadowSanitizer` in the standard library and add §X "Runtime Unsafe Auditing" to the spec.

---

## Decision #19: Nominal Access Types ✓ LOCKED

**Question:** After Decision #17 narrowed the unsafe *operations* and Decision #13 scoped the *references*, the remaining gap is the *types*. Raw pointer types (`*const T` / `*mut T`) are structurally identical between unrelated peripherals, so a `*mut Usart1Registers` and a `*mut Spi1Registers` are the same type and silently interchangeable through `#unchecked_cast`. How does Clifford close this gap to fully match Ada/SPARK's discipline?

**Chosen:** Replace raw `*const T` / `*mut T` types with **nominal access types**. The type constructor `access<T>` (and `access const<T>` for read-only) produces a *distinct* type per declaration — `@type UartPtr = access<Usart1>` and `@type SpiPtr = access<Spi1>` are different types even though they share the same underlying representation. Mixing them in any operation requires an explicit `#unchecked_cast<S, T>` and is therefore an individually grep-able audit point.

**Rationale:** This completes the Ada/SPARK story. We already had the operations narrow (Decision #17) and the references scoped (Decision #13). With nominal access types, the *type identity* is also Ada-style: each access type carries a distinct nominal identity, and mixing two pointer types is a compile error unless explicitly casted.

The cost is small. The compiler already does monomorphization and type-distinct generic instantiation. Nominal access types are a thin newtype layer with a distinct identity tag in the typed AST. LLVM lowering is unchanged: every access type lowers to `T*` regardless of nominal identity.

The bug class this catches: **peripheral confusion**. In real firmware, passing a `Uart` register pointer to an `Spi` driver is a not-uncommon configuration bug. Today (v0.4) `#unchecked_cast<*mut Usart1, *mut Spi1>` would silently accept it; with nominal access types, that cast is its own grep target and code review will see it.

**Rules:**

1. **The type constructor.** `access<T>` is the read-write access form; `access const<T>` is read-only. Both are *type-level* operators that take a type `T` and produce a new type. Each `@type Foo = access<Bar>` declaration produces a *nominal* type — distinct from any other `@type Baz = access<Bar>`, even though the underlying representation is identical.

2. **Replacement of raw pointers.** The legacy `*const T` and `*mut T` syntactic forms are removed from v0.1. All raw-pointer-typed values use `access const<T>` and `access<T>` respectively. The narrow primitives (Decision #17) operate on access types, not raw pointers.

3. **Type-distinct mixing requires explicit cast.** Passing a value of type `UartPtr` (where `@type UartPtr = access<Usart1>`) to a parameter expecting `RawAccess<u8>` (a different `@type` declaration) is `E0710: nominal access types differ; use #unchecked_cast<S, T> if intentional`. Decision #17's narrow `#unchecked_cast` is the explicit escape hatch.

4. **Pointer arithmetic via `#unchecked_offset`.** A new narrow unsafe primitive joins the Decision #17 catalog:
   ```
   #unchecked_offset<T>(p: access<T>, n: isize) -> access<T>
   ```
   Returns an access pointer offset by `n` *elements* (T-sized), not bytes. The result has the same nominal access type as the input. Pointer arithmetic is therefore individually visible just like load/store.

5. **Pointer equality.** Standard `==` and `!=` operate on access types of the same nominal identity. Comparing two access values of different nominal types is `E0711` (use `#unchecked_cast` to align them first).

6. **Null.** `null` is a context-typed literal that resolves to the null value of whichever access type the context expects. `null == p` where `p: UartPtr` is fine; `null == 0` is not (null is not an integer).

7. **No implicit decay.** `access<T>` does not implicitly decay to `access<u8>` or any other type. Anything that needs a type-erased pointer (e.g., an interface to memcpy-style routines) must use `#unchecked_cast` explicitly.

**Updated catalog of narrow unsafe primitives (Decision #17 + Decision #19):**

```
#unchecked_load<T>(p: access const<T>) -> T
#unchecked_store<T>(p: access<T>, v: T)
#volatile_load<T>(p: access const<T>) -> T
#volatile_store<T>(p: access<T>, v: T)
#unchecked_cast<S, T>(v: S) -> T              // applies to access types and others
#unchecked_offset<T>(p: access<T>, n: isize) -> access<T>
#asm("…", inputs, outputs)
```

**Example:**

```
@type UartPtr = access<Usart1>;
@type SpiPtr  = access<Spi1>;

#effect dma_send_uart(buf: access const<u8>, len: usize) #mutates: [...] {
  // … DMA peripheral programmed with `buf` …
}

#effect main() #mutates: [...] {
  let uart_ptr: UartPtr = … ;
  let spi_ptr:  SpiPtr  = … ;

  // Imagine the user writes this by mistake:
  // #> dma_send_uart(spi_ptr, 64);   //  ← E0710: SpiPtr not access const<u8>

  // The fix is to cast deliberately and visibly:
  let raw: access const<u8> = #unchecked_cast<SpiPtr, access const<u8>>(spi_ptr);
  #> dma_send_uart(raw, 64);
}
```

The explicit `#unchecked_cast` is now the audit point. Without nominal access types, the bug above would have compiled silently.

**Implications:**

- §1.3 sigil/keyword forms: `access` is a new bare keyword (used as `access<T>` and `access const<T>`); `#unchecked_offset` joins the narrow-primitive list.
- §2.7 type grammar: `ptr_type` production replaced by `access_type := 'access' 'const'? '<' type_expr '>'`.
- §4.2 composite types table: pointer entry rewritten.
- §4.6 mutability: nothing structural changes; access types are just typed pointers.
- §5 type checker: nominal type identity for `access` types; coherence of `#unchecked_cast` arguments; null literal context resolution.
- §8.3 lowering table: every access type lowers to `T*` at LLVM IR; nominal identity is a Clifford-level concept only.
- §8.4 codegen: `#unchecked_offset` lowers to `getelementptr inbounds T, T* %p, isize %n`.
- §10 conformance tests: type-distinct mixing rejected (`E0710`); cross-type cast accepted via `#unchecked_cast`; `#unchecked_offset` produces the right LLVM IR.
- §12 open questions: closes the implicit "what about pointer types" question that earlier drafts left dangling.
- §13 glossary: add `access type`, `nominal type identity`.

**Compatibility:** Decision #19 is a breaking change relative to v0.4's `*const T` / `*mut T` syntax. Since v0.1 has not shipped, this is the right time. After v0.5-draft, `*const T` and `*mut T` no longer parse; the migration is mechanical (`*const T` → `access const<T>`, `*mut T` → `access<T>`).

**Refinement #19a (Mandatory reason on `#unchecked_cast`).** Every `#unchecked_cast` invocation must carry a non-empty string-literal reason as a positional argument:

```
#unchecked_cast<UartPtr, SpiPtr>(
  "DMA descriptor reuses the peripheral base address — see datasheet §12.3",
  uart_ptr
)
```

Empty or whitespace-only reasons are rejected at parse: `E0713: #unchecked_cast requires a non-empty reason string`. The reason string is preserved in the typed AST and emitted to the audit log by `cliffordc audit --list-unsafe`. Code review can scan reasons collectively; PR review can challenge ones that look copy-pasted. This raises the audit cost of cast laundering ("strip nominal identity, then cast back to a different nominal type") without forbidding legitimate cross-type uses.

**Refinement #19b (`--max-cast-chain=N` lint).** The compiler driver accepts `cliffordc lint --max-cast-chain=N`, which fails the build if any function body contains more than N `#unchecked_cast` operations. Default is *unset* (no limit); safety-critical projects opt in to `--max-cast-chain=1` to catch laundering directly. This is tooling, not a language change — the lint reads the typed AST that already records cast operations per Refinement #19a's preservation rule.

---

## Decision #20: First-Class Bitfield Access on Register Block Fields ✓ LOCKED

**Question:** Register manipulation is the dominant pattern in firmware. Writing `#mutate Reg { control = (self.control & ~MASK) | (val << SHIFT) };` for every bit twiddle is exactly the C boilerplate Clifford should eliminate. How do users address individual bits or bit ranges within a register field cleanly?

**Chosen:** Add a `#bits { … }` annotation on register-block fields. Fields with `#bits` have named sub-components accessible via dot-syntax: `Reg.field.subfield`. Reads emit a volatile load + bit extract; writes emit a volatile load + bit insert + volatile store, wrapped in target-atomic when the GA engine determines a concurrent writer exists for the same register (mirroring Refinement #5e's policy).

**Grammar additions (folded into §2.5 `field_attr`):**

```
field_attr := '#offset' ':' integer_literal
            | '#access' ':' access_kind
            | '#bits'   '{' bit_field (',' bit_field)* '}'

bit_field   := ident ':' integer_literal '#at' ':' integer_literal
              // <name>: <width> #at: <lsb_offset>
```

**Example:**

```
#automaton Usart1 #address: 0x4001_3800 {
  CR1: u32 #offset: 0x00 #access: rw #bits {
    UE: 1 #at: 0,        // word length bit
    RE: 1 #at: 2,
    TE: 1 #at: 3,
    PCE: 1 #at: 10,
    M:   2 #at: 12,       // 2-bit subfield: word length [01]=8b, [10]=9b
  }
  // …
}

// Access:
Usart1.CR1.UE = true;       // read CR1, set bit 0, store CR1
Usart1.CR1.M  = 0b10;       // read CR1, clear bits 12-13, set 0b10 << 12, store CR1
let on := Usart1.CR1.UE;    // read CR1, extract bit 0, return bool
```

**Atomicity (the load-bearing part):**

Bit-field writes are read-modify-write, which is *not* atomic without help. The default policy mirrors Refinement #5e:

1. Compute the **register-overlap set** `RW(R) = { I | I is a #interrupt or other concurrent context that writes register R }`.
2. **If RW(R) is empty,** emit the bit-field write as a plain volatile load + insert + volatile store sequence with no atomic wrapping.
3. **If RW(R) is non-empty,** emit the bit-field write as a target-atomic read-modify-write:
   - Cortex-M3 / M4 / M7 / M33: LDREX/STREX retry loop.
   - RISC-V with A-extension: LR.W / SC.W retry loop.
   - x86: `lock cmpxchg`.
   - Cortex-M0 / M0+ (no exclusives): wrapped in `interrupt_critical` (cli/sti) — the only safe option on those targets.
4. **If a `@non_atomic` per-write attribute is present** (`Usart1.CR1.UE @non_atomic = true;`), emit the plain RMW with no wrapping and warning `W2001: bit-field write to register with concurrent writer; @non_atomic acknowledged`.

**Composition with the GA engine:**

The GA engine's basis-vector assignment treats each *bit-field subfield* as its own basis vector when bits are non-overlapping. Two effects writing different subfields of the same register `R` (e.g., one writes `R.UE`, another writes `R.M`) are orthogonal at the *bit-field* level even though they target the same `u32`. The atomic RMW ensures the two writes don't lose each other's contribution at runtime.

**Read-only bit-field access** (`#access: r` register or `#access: r` parent register) is unconditionally non-atomic — no RMW needed. Reads emit a plain volatile load + extract.

**Width / offset constraints:**

- A bit-field's `(width, offset)` must fit within its parent field's bit width: `offset + width <= bits_in(parent_type)`. Violation: `E0614: bit-field exceeds parent register width`.
- Bit-fields within the same parent must not overlap. Violation: `E0615: overlapping bit-fields in register`.
- A 1-bit field has type `bool`; multi-bit fields have type `u<N>` for `N <= 64`, or the smallest standard unsigned (u8/u16/u32/u64) ≥ N if a fitted type is preferred.

**Lowering example (Cortex-M4):**

```
; Usart1.CR1.UE = true  (bit 0 of u32 at 0x4001_3800)
loop:
  ldrex   r0, [r1]              ; r1 = 0x4001_3800
  orr     r0, r0, #1
  strex   r2, r0, [r1]
  cmp     r2, #0
  bne     loop
```

When no concurrent writer:

```
; same operation, no atomic
  ldr     r0, [r1]
  orr     r0, r0, #1
  str     r0, [r1]
```

The compiler picks the right form based on the register-overlap-set analysis.

**Implications:**

- §1.3 sigil/keyword forms: `#bits` and `#at` join the register-block annotation list.
- §2.5 grammar: `field_attr` extended with the `#bits` clause; new `bit_field` production.
- §5 type checker: width/offset/overlap validation; type assignment per bit-field width; access-kind propagation from parent.
- §6 mutation profile: bit-field writes contribute the relevant subfield basis vectors to `actual_writes`. Per-register-overlap-set computation drives §8.4 atomic-or-not decision.
- §8.4 codegen: target-atomic RMW emission per the policy above; plain RMW when no concurrent writer.
- New error codes `E0614`, `E0615`; new warning `W2001`.

**Trade-off honesty:** bit-fields add real spec surface (~80 lines across §2.5 / §5 / §6 / §8.4) and meaningful compiler complexity (per-target atomic RMW lowering). The win is firmware ergonomics — writing real STM32 / ESP32 / RP2040 driver code feels like Rust's `pac` crates without the macro-generated boilerplate. For a language whose canonical first target is firmware, this is not optional.

---

## Emergent Rules from Decision Interactions

These six rules emerge from the interaction of the fifteen decisions:

### Rule 1: Trait Basis Vectors Are Global
**From:** Decision #2 × Decision #4

When a trait (e.g., `Readable`) is used in multiple functions or automata, the compiler assigns it a single, consistent basis vector globally. This ensures orthogonality checks across the entire program remain sound.

**Implementation:** During compilation, build a global trait→basis map before any orthogonality checking.

---

### Rule 2: Unmarked `@fn` Defaults to `$ [Pure]`
**From:** Decision #2 × Decision #1

Any `@fn` without an explicit `$ [TraitList]` annotation is treated as `$ [Pure]` — no side effects, no state access. If the function actually reads or writes state, compilation fails.

**Rationale:** Safety by default. Developers must explicitly declare state access via trait markers.

---

### Rule 3: Effect Procedures Require Explicit Automaton Scope
**From:** Decision #3 × Decision #4

Every `#effect` procedure must declare `#mutates: [AutomatonList]`. The compiler uses this to compute the procedure's behavior multivector and check orthogonality.

**Rationale:** Static verification requires complete information upfront. Dynamic discovery is incompatible with GA orthogonality proofs.

---

### Rule 4: No Cross-Boundary Inlining
**From:** Decision #1 × Decision #3

Functional code (`@fn`) can never contain imperative code (`#effect`, `#mutate`) even after inlining or macro expansion. The compiler must verify this at every optimization pass.

**Rationale:** Maintaining the `@` / `#` boundary is essential for both semantics and GA orthogonality.

---

### Rule 5: Effect Procedure Calls Must Be Statically Resolvable
**From:** Decision #3 × Decision #1

Every `#> name()` call must be statically bound — the called procedure's name must exist and be visible in the current scope. No dynamic dispatch, no higher-order effect procedures.

**Rationale:** Static resolution enables compiler to build the full effect-procedure call graph and compute orthogonality across the entire program before linking.

---

### Rule 6: GA Orthogonality = Product-Category Existence
**From:** Decision #4 × Decision #5

Two automata `A` and `B` that may execute concurrently form a parallel composition modeled as the product category `C_A × C_B`, whose objects are state pairs `(s_A, s_B)` and whose morphisms interleave transitions of `A` and `B`. The product is well-defined as a category if and only if no two parallel morphisms touch overlapping mutable state — i.e., their behavior multivectors wedge to a non-zero blade of full grade.

**Equivalence (informal):**

```
behavior(A) ∧ behavior(B) ≠ 0 of grade |A| + |B|
   ⇔
C_A × C_B is well-defined as a small category in which interleaved
morphisms commute.
```

**Rationale:** This grounds the GA orthogonality engine (§7) in a categorical theorem rather than a clever bitmask trick. The bitmask implementation remains; what changes is the spec's claim about *what* it proves. Concretely:

- Concurrent automata that share basis vectors do not have a well-defined product category — there exist morphisms `(f, g)` that cannot commute because they touch the same field.
- The GA wedge-product check is the constructive existence proof for the product.
- Hierarchical and parallel composition extensions in v0.2 (functors, monoidal structure) build on this same machinery.

**Implementation:** No code changes from the existing §7 algorithm. This rule is a specification-level statement: the bitmask check is *precisely* the well-formedness proof for the product category, and `Appendix B` of `CLIFFORD_SPEC.md` states the formal theorem.

---

## Decision #21: Shared Automata via Mutator Multivectors — DEFERRED TO RESEARCH

**Status:** Deferred to research by the 2026-05 audit (`docs/decision-audit-2026-05.md` §5). Originally locked 2026-05-01.

The original mixed-metric Cl(p,0,n) design — shared fields contributing non-null basis vectors, locks as multivectors `lock(L) = pri(L) + e_L`, deadlock-freedom from a sketched algebraic theorem — was unimplemented, gated to v0.7, and rested on a categorical correctness proof that did not exist in writing. The real problem it named (shared mutable resources: run-queues, allocators, capability tables) is better served by Stack Resource Policy (Baker 1991, mechanized by RTIC) plus a Pony-`iso`-style `#owned`/`#sendable` field qualifier — see `docs/foundations.md` §3–§4.

Full original text: [`docs/research/ga-shared-automata.md`](research/ga-shared-automata.md). Tracking ADR (immutable): `docs/adr/0002-shared-automata-mutator-multivectors.md`. The Phase-1 scaffolding this decision landed (the `crates/ast` `FieldKind` enum, the `crates/lexer` `#shared`/`#lock`/`#with_lock`/`#reads`/`#rotor` reservations) was removed when the decision was deferred. A fresh decision will cover SRP-based shared resources once a comparison artifact validates the need.

---

## Decision #22: Kinds of Imperative — `$ [TraitList]` on Effects ✓ LOCKED

**Date locked:** 2026-05-03
**Spec impact:** §2.5 (effect grammar — extend with optional trait-list), §4.5 (predeclared effect-kind traits), §7 (informational — engine ignores these traits but downstream phases consume them).

### Summary

Extend `$ [TraitList]` markers from `@fn` (Decision #2) to `#effect`, `#interrupt`, and `#transition` declarations. The traits describe *what kind of imperative the body is*, not *whether it's pure* — a flat-hierarchy classification of mutation kinds:

```clifford
#effect read_uart()      #mutates: [Uart]    $ [Hardware, Acquire] { … }
#effect tick_counter()   #mutates: [Counter] $ [PureState]         { … }
#effect schedule_next()  #mutates: [RunQ]    $ [LockingDiscipline, Realtime(100us)] { … }
```

The orthogonality engine *does not* read these traits — they don't affect the wedge-product check. Downstream phases (codegen ordering choices, `cliffordc audit`, certification reports, optimization passes) read them. They make the imperative side legible: a reviewer can tell from a signature whether `read_uart()` requires acquire-fence semantics, whether it touches MMIO, whether it's bounded by a real-time deadline.

### Predeclared effect-kind traits (initial set)

In `clifford::core` (or wherever the stdlib settles):

| Trait | Meaning |
|---|---|
| `PureState` | Mutates only ordinary automaton fields; no MMIO, no fences, no real-time concerns |
| `Hardware` | Touches at least one register-block automaton (`#address`-tagged) |
| `Realtime(deadline)` | Hard real-time deadline on completion (deadline = duration literal); refinement-typed |
| `LockingDiscipline` | Acquires/releases locks per Decision #21 §5.5 |
| `Acquire` | Implies acquire-fence semantics on entry (memory ordering) |
| `Release` | Implies release-fence semantics on exit |
| `SeqCst` | Implies sequentially-consistent fencing on every memory op |
| `Relaxed` | No fences (default; explicit only when documenting intent) |
| `Encapsulated` | Mutates only `#hidden`-marked fields per Decision #25 (effectively no externally-visible side effect on shared state) |

User-declared traits via `@trait Name { … }` (Decision #2 hybrid scheme) work for `#effect` the same way they work for `@fn` — structural satisfaction, no explicit `impl` required.

### Why locked, not ADR-required

The design is mechanically straightforward: extend the parser's `#effect` grammar to accept the same `$ [...]` clause it already parses for `@fn`; extend the AST `EffectDecl` with a `trait_list: Vec<TraitRef>` field; downstream consumers add reads as needed. Implementation cost: ~50 LoC in the parser/AST. No engine impact — the orthogonality check ignores effect traits. No backward incompatibility.

### What this enables

- Codegen can choose memory-ordering instruction selection per effect (`Acquire` → `lda` on ARM, etc.) without users handwriting `#asm`.
- `cliffordc audit` can list every `Hardware`-tagged effect for hardware-driver review.
- Certification (DO-178C, IEC 61508) gets a structured per-effect kind classification at the source level.
- Optimization passes can move/reorder effects with confidence (a `PureState` effect can be re-ordered with respect to a `Hardware` effect; two `Hardware` effects cannot).

### Implementation status

Spec edit + parser + AST extension lands in a follow-up Phase-1 work item; no rush. The traits themselves can be predeclared in `clifford::core` once stdlib bootstrap begins.

---

## Decision #23: Tighten `@fn` Toward Haskell-Clean Discipline ✓ LOCKED

**Date proposed:** 2026-05-03
**Date locked:** 2026-05-05 (architect sign-off "yes to all" on ADR 0003 propositions)
**Status:** Locked.
**Tracking ADR:** `docs/adr/0003-haskell-clean-fn-discipline.md` (Accepted 2026-05-05).

### Summary

The pure side of Clifford should commit fully to its math. The current `@fn` excludes `#`-constructs (good) but doesn't enforce totality, refinement-typed effect rows, or termination — properties that distinguish "syntactically pure" from "semantically pure" in the Haskell / Idris / Koka tradition.

Direction agreed:

1. **Total by default.** Every `@fn` must terminate. Proven via structural recursion + sigma-loop bounds. Opt-out via `$ [Partial]` for the rare case where termination cannot be proven.
2. **Effect rows in signatures.** `$ [Pure]` is the strict default; observation of any external state requires explicit row markers (`$ [Reads<Auto>]`, `$ [Observable]`, etc.). Subsumes Decision #2's TraitList for the strict-effect-tracking case.
3. **Refinement types in argument positions.** Beyond Decision #14's sigma bounds: arbitrary refinement predicates on parameter types, checked at call sites. Liquid-Haskell-style.
4. **Local mutation per Refinement #1a remains permitted.** ST-monad-equivalent — invisible to callers, doesn't break purity.

### Why ADR-required

This is not mechanically simple. Totality checking is a real type-system feature (cf. Idris's totality checker, Coq's guard predicate). Effect rows are a research area. Refinement types beyond sigma bounds need an SMT solver or a constructive substitute. Each sub-component is a research-grade engineering effort.

The ADR will:

- Survey the literature (Idris totality; Liquid Haskell refinements; Koka effect rows; Eff handlers).
- Choose between full HM-with-effect-rows vs structural row polymorphism.
- Decide whether refinement types ride on top of an SMT call (F* style) or stay decidable-by-structural-induction.
- Identify which subset is in scope for v0.2 vs deferred to v0.3+.

### Locked resolutions (per ADR 0003, accepted 2026-05-05)

The four core trade-offs and five sub-questions all carry their proposed resolutions verbatim from ADR 0003 §"Decision":

- **P1 Totality:** required by default; `@partial @fn` opt-out via Idris-style structural recursion (three-rule cut: pattern-matched constructor args, sigma-bounded indexing, tail recursion). Non-structural recursion → `E0540`.
- **P2 Effect rows:** first-class via `$ [TraitList]` extension — `Readable`, `Observable`, `Pure`, `Opaque` with row-composition checking (`E0541`). `@fn → @fn` row check is one-directional; `#`-layer callers freely call any `@fn`.
- **P3 Refinement types:** limited via §5.8 sigma-bound (Decision #14) extension to function arguments. **No SMT in v0.2** (`E0542 RefinementNotDischarged`); SMT-backed refinements deferred to v1.0+ separate ADR.
- **P4 Local mutation:** Refinement #1a unchanged.
- **Q3:** `Result<_, E>` only in v0.2; Koka-style `@throw`/`try` deferred to v0.4+.
- **Q4:** `Diverges` trait dropped; `@partial` covers non-termination.

### Implementation status

v0.2: totality skeleton in `clifford-check`; `Readable`/`Observable` rows in `clifford-types`; `@partial` parser support; `Diverges` removed. Subsequent slices: refinements on function arguments. The book Ch. 23 chapter graduates from stub to full text alongside the implementation PR (target: v0.2-α).

---

## Decision #24: Explicit Boundary-Crossing via `@snapshot` ✓ LOCKED

**Date proposed:** 2026-05-03
**Date locked:** 2026-05-05 (architect sign-off "yes to all" on ADR 0004 propositions)
**Status:** Locked.
**Tracking ADR:** `docs/adr/0004-snapshot-boundary-operator.md` (Accepted 2026-05-05).

### Summary

Crossing the `#`/`@` boundary inward — bringing a value from the imperative side into pure analysis — currently has no syntactic marker. Users learn the snapshot pattern by convention (book Ch. 39): an `#effect` reads automaton fields into stack-local owned values, then passes those owned values to `@fn` for analysis.

Direction agreed: introduce `@snapshot Auto.field` (or equivalent surface) as the *only* way to read mutable automaton state into pure-side analysis. After the snapshot, the read value is an owned, immutable copy that lives entirely on the pure side.

```clifford
#effect bumper() #mutates: [Counter] {
  let snap: u32 = @snapshot Counter.value;     // explicit boundary crossing
  let next: u32 = double(snap);                // pure analysis — no automaton reach
  Counter.value = next;
}
```

The `@snapshot` operator:

- Is an expression yielding an owned value (no `&` to automaton state).
- Can only appear inside `#`-layer bodies (you can't snapshot from `@fn` — that would be reading automaton state, which §5.5 forbids).
- Renders the boundary crossing visible in source — reviewer/IDE/`cliffordc audit` can highlight every snapshot point.
- Provides a hook for §7 read-tracking when v0.2's read-write race detection lands (the snapshot is the read; the engine sees it).

### Why ADR-required

Subtleties to nail down:

- Is `@snapshot` an expression, a statement, or a type-level annotation?
- Does it copy by value (`u32` cleanly; `[u8; 64]` more expensive)?
- Does it work for refs to slices?
- What's the interaction with `#shared` fields (Decision #21) — does snapshotting a shared field require holding the lock at snapshot time?
- Does it desugar to a function call (so `cliffordc audit` can grep for it) or does it need first-class AST representation?
- Backward-compatibility with the existing snapshot-by-convention pattern in book Ch. 39: do existing programs need to be rewritten, or does the convention become syntactic sugar for `@snapshot`?

### Locked resolutions (per ADR 0004, accepted 2026-05-05)

- **P1 Form:** Expression. `let v := @snapshot Counter.value;` composes anywhere.
- **P2 Copy semantics:** Copy-by-value for `Copy` types in v0.2; `@snapshot_ref` borrow form deferred to v0.4+. Larger-than-word types → `E0551 SnapshotNotAtomic` (use `#shared` + lock).
- **P3 Interaction with `#shared` (Decision #21):** lock-holding proof required. From `@fn` in v0.2: `E0552 SnapshotNeedsLockProof` — only from `#`-layer.
- **P4 Migration:** Two-phase. v0.2: deprecation warning `W0001 ImplicitFieldRead`. v0.4+: hard `E0101`.
- **Q1:** `@snapshot` is **not pure** — controlled effect. `Readable` trait is the marker. Two snapshots of the same field MAY observe different values.
- **Q2:** `@snapshot Self.field` inside `#transition` → `E0553 SnapshotInImperative` (use bare `Self.field`).
- **Q3:** Single field path only in v0.2; composite reads (`@snapshot Auto.field[expr]`) deferred to v0.4+.
- **Q5 Memory ordering:** v0.2 implies `Acquire` for `#shared` snapshots; explicit ordering deferred to v0.7+.

### Implementation status

v0.2: `@snapshot` lexer token; `SnapshotExpr` AST; the `Readable` row from Decision #23 gates `@snapshot` usage from `@fn` (`E0550`); E0550–E0553 + W0001 enter the §10 error-code table. Book Ch. 24 graduates from stub to full text alongside the implementation PR. Book Ch. 43 (formerly Ch. 39) SPSC example migrates to `@snapshot` + `Readable` form.

---

## Decision #25: `#hidden` Encapsulation — Algebraic Trivial Orthogonality ✓ LOCKED

**Date locked:** 2026-05-03
**Spec impact:** §2.5 / §3.7 (automaton-field grammar — extend with optional `#hidden` modifier), §7 (informational — `#hidden` fields trivially orthogonal to anything outside their owning automaton).

### Summary

Re-introduce `#hidden` as a per-field modifier on automaton fields, with a precise algebraic interpretation. Decision #9 dropped the original `#hidden` / `#visible` system because it was subsumed by `#mutates` / `#cannot_mutate`. The reintroduced `#hidden` is *narrower* and *algebraically motivated*:

```clifford
#automaton Counter {
  value: u32;                  // ordinary field; visible to anything mutating Counter
  scratch: u32 #hidden;        // private to Counter's transitions/effects; invisible elsewhere
  cache:   [u8; 32] #hidden;   // same
}
```

A `#hidden` field has the property that its basis vector **cannot appear in any callable's `actual_writes` set unless the callable belongs to the owning automaton's transitions or to an effect declared `#mutates: [Counter]` AND specifically marked as having access to hidden state.**

### The algebraic insight (per the user's framing)

A `#hidden` field's basis vector is *automatically orthogonal to everything outside the owning automaton's surface.* The wedge product never collapses against it from outside, because the bit never appears outside. This is the trivial-orthogonality case of the §7.4 check — no special machinery needed in the engine; the field simply doesn't enter the basis assignment for callables that don't have access.

Practically:

- For callables in the owning automaton (its `#transition`s) — `#hidden` fields appear in their `actual_writes` and are checked normally.
- For callables outside the owning automaton (other automatons' transitions, effects in `#mutates: [OtherAuto]`) — `#hidden` fields *cannot* be referenced, so they cannot appear in `actual_writes`, so they cannot conflict.

This is encapsulation by construction. No visibility check pass; no special algebra; just "the bit isn't there for outsiders to even refer to."

### Surface syntax

`#hidden` is a per-field modifier alongside `#offset` / `#access`. Order-independent, optional:

```clifford
#automaton Counter {
  value:   u32;                          // ordinary
  scratch: u32 #hidden;                   // hidden
  status:  u32 #hidden #offset: 0x04 #access: read;  // hidden register-block field
}
```

The parser admits the modifier in any field-meta position. `clifford-resolve` (slice R3 `require_field` check) is extended: when checking `Auto.field` references from outside the owning automaton, hidden fields produce `E0407 HiddenFieldNotAccessible` instead of resolving.

### Why this is locked, not ADR-required

The algebraic justification is the entire design. Encapsulation is "the bit doesn't appear" — there's no engine machinery to design, no spec-extension theorem to write. Implementation cost: ~30 LoC in the parser (one new field-meta token) + ~20 LoC in `clifford-resolve` (visibility check in `require_field`). Spec amendment: one paragraph in §3.7.

### What this enables

- Implementation hiding for register-block automata (e.g. UART driver's internal `#hidden` parity-error counter that no other code should see).
- Caches and scratch buffers per automaton without polluting the global mutation analysis.
- Cleaner `cliffordc audit` reports — hidden fields don't appear in cross-automaton dependency graphs.

### Implementation status

Spec amendment + parser/resolve extension lands in a follow-up Phase-1 work item; mechanically simple.

---

## Decision #26: Rotor-Based Plane-Confined Locks — DEFERRED TO RESEARCH

**Status:** Deferred to research by the 2026-05 audit (`docs/decision-audit-2026-05.md` §5). Originally locked 2026-05-05. Refined the also-deferred Decision #21.

Rotor-as-acquisition-primitive (`M ← R_t · M`, `R_t = exp(-θ_t · B_t / 2)`) reframed lock acquisition as a Clifford-algebra rotation. The decision's own text conceded that "`exp` does not appear in generated code; runtime cost is a normal CAS-based spinlock with an integer owner-ID" — i.e. the rotor algebra was decoration over an ordinary owner-ID spinlock. Mutual exclusion, wrong-thread-release detection, and re-entrancy are standard properties of ordinary lock implementations.

Full original text: [`docs/research/ga-rotor-locks.md`](research/ga-rotor-locks.md). Tracking ADR (immutable): `docs/adr/0005-rotor-plane-confined-locks.md`.

---

## Decision #27: GA Across Scales — Distributed Runtime Race Detection — DEFERRED TO RESEARCH

**Status:** Deferred to research by the 2026-05 audit (`docs/decision-audit-2026-05.md` §5). Originally locked 2026-05-05.

The decision committed to lifting the wedge primitive to distributed runtime race detection, described in its own text as "RPC publish + central coordinator + RPC retract; `&` op on coordinator" — i.e. optimistic concurrency control (publish a read/write set, intersect at a coordinator, retract). Sinfonia (2007), Calvin (2012), and every modern OCC system already do this; the "GA is the unifying algebra" framing added nothing operational and was the marketing thesis the post-pivot project retired.

Full original text: [`docs/research/ga-across-scales.md`](research/ga-across-scales.md). Tracking ADR (immutable): `docs/adr/0006-runtime-distributed-multivector-engine.md`. If distributed race checking is ever built, it should be grounded in the OCC literature under its own decision.

---

## Decision Matrix

| # | Aspect | Question | Chosen | Impact |
|---|--------|----------|--------|--------|
| #1 | Syntax | Sigils vs Keywords vs Soft | **Sigils (`#`, `@`)** | Clear intent, extensible, no keyword bloat |
| #2 | Traits (pure) | Nominal vs Structural vs Hybrid | **Hybrid `$ [List]`** | C interop without boilerplate, GA tracking |
| #3 | Composition | Nesting vs Procedures vs Monolithic | **Named Procedures `#>`** | Orthogonal, static, composable |
| #4 | Basis Vectors | Auto vs Explicit vs Hybrid | **Hybrid + IDE** | Hidden by default, auditable on demand |
| #5 | State/Effect/Transition | Per-effect coupling vs Optional FSM vs Categorical | **Automaton-as-Category** | One model with degenerate cases; FSM mandatory but monoid case ergonomic |
| #6 | Hardware Registers | Separate `#hardware` vs Automaton-as-register-block | **Automaton-as-register-block with `#address`/`#offset`/`#access`** | Register races become normal GA orthogonality violations |
| #7 | Testing | Pure-side mocks vs Effect-runner vs Mixed-context block | **`#test "name" { … }`** | Smallest viable testing primitive; isolated state per test |
| #8 | Local binding | `let` only vs Add `:=` shorthand | **`:=` for type-inferred immutable** | Reduces ceremony; `mut` requires explicit form |
| #9 | Visibility clauses | Keep `#visible`/`#hidden` vs Drop | **Drop** | One concept beats two; collapses to `#mutates`/`#cannot_mutate` |
| #10 | Interrupt vectors | Config file vs Linker symbol vs Macro | **Linker symbol** | Matches Cortex-M / RISC-V startup conventions |
| #11 | Granularity escape | None vs `@sequential(A,B)` vs `@phase(name)` | **`@sequential(A,B)`** | Trusted assertion of non-concurrency |
| #12 | Deferred mutation | Now vs `#staged` vs Defer | **Designed; deferred to v0.2** | Validate against real firmware first |
| #13 | Memory safety | Rust BC + lifetimes vs Body-scoped + provenance vs Linear vs Pony caps | **Body-scoped + provenance (Rules 0–5)** | Catches UAF cases 1–5 with no lifetime annotations |
| #14 | Iteration | While+index vs Iterator-trait vs Sigma loop | **Sigma loop with bounds tracking** | Bounds-check elimination by construction; allocation-free |
| #15 | Mutation surface | `#mutate Auto { f = … };` only vs Add sugar | **Add `Auto.field <op>= expr;` sugar** | C-feel verbosity for single-field writes |
| #16 | Effect polymorphism | Trait objects vs `#interface` + `#impl` + monomorphization | **`#interface` + `#impl` + monomorphization** | HAL ecosystem foundation; static dispatch only |
| #17 | Unsafe operations | Rust-style `unsafe { … }` block vs Ada-style narrow primitives | **Ada-style narrow primitives** | Per-operation auditability; no aggregating block; safety-critical-friendly |
| #18 | Runtime auditing | None vs Built-in vs `#audit` + `PointerAuditor` interface | **`#audit` + interface (designed; deferred to v0.2)** | KASAN-style runtime checking layered on Decision #17's static visibility |
| #19 | Pointer types | Raw `*const T`/`*mut T` vs Nominal `access<T>` | **Nominal `access<T>` / `access const<T>`** | Type-distinct pointers; peripheral confusion caught at compile time; cross-type casts grep-able via `#unchecked_cast` |
| #20 | Bitfield access | `#mutate Reg { f = (self.f & ~M) \| (v << S) }` vs First-class `Reg.f.bit = v` | **First-class `#bits` annotation with target-atomic RMW** | Eliminates bit-twiddling boilerplate; atomic RMW where a concurrent writer exists, plain RMW otherwise; subfield-level GA basis vectors |
| #21 | Shared state | Mixed-metric Cl(p,0,n) algebra vs established concurrency literature | **DEFERRED TO RESEARCH** (2026-05 audit) | Mixed-metric-GA design unimplemented and proof-incomplete; shared mutable resources to be re-addressed via Stack Resource Policy + Pony-`iso`-style `#owned`/`#sendable`. See `docs/research/ga-shared-automata.md` |
| #22 | Imperative kinds | Flat `#effect` everywhere vs effect-traits classifying mutation kind | **`$ [TraitList]` on effects** (Hardware, Realtime, Acquire, Release, SeqCst, LockingDiscipline, PureState, Encapsulated) | Imperative side becomes legible without engine impact; codegen / audit / certification consume the traits |
| #23 | Functional discipline | Status quo `@fn` (excludes `#`-constructs) vs Haskell-clean (total + effect rows + refinement types) | **Haskell-clean (totality + Readable/Observable rows + sigma-bound refinements)** | Pure side commits fully to its math; brings Idris-style totality + Koka-style rows to the systems-language tier; SMT-backed refinements deferred to v1.0+ |
| #24 | Boundary crossing | Convention-based snapshot pattern vs `@snapshot Auto.field` operator | **Explicit `@snapshot` expression**; copy-by-value for Copy types in v0.2; lock-holding proof for `#shared` snapshots | Reading mutable state into pure analysis becomes a visible, named act gated by the `Readable` row; supports future read-tracking |
| #25 | Encapsulation | Re-add `#hidden` per-field modifier with algebraic-trivial-orthogonality interpretation | **`#hidden` on automaton fields** | Implementation hiding by construction; field never appears in outside callables' basis sets, so wedge never collapses against it from outside |
| #26 | Lock acquisition (refines #21) | Rotor-as-acquisition primitive vs ordinary owner-ID locks | **DEFERRED TO RESEARCH** (2026-05 audit) | Rotor algebra was decoration over a CAS spinlock with an owner-ID (the decision's own text conceded `exp` never reaches generated code). See `docs/research/ga-rotor-locks.md` |
| #27 | GA across scales (distributed runtime) | "GA across scales" vs established OCC literature | **DEFERRED TO RESEARCH** (2026-05 audit) | Mechanism is optimistic concurrency control (publish/intersect/retract read-write sets); GA framing added nothing operational. See `docs/research/ga-across-scales.md` |

---

## Implications for Phase 1 Implementation

### Lexer (§1)
- Recognize sigils `#`, `@`, `$` as primary tokens; `#>` as composite
- Recognize `:=` and `<op>=` (compound assignment) operators
- Byte-literal form `b'X'` (Decision #15 ergonomics)
- Everything else is standard identifier/keyword tokenization

### Parser (§2)
- Top-level items: `@fn`, `@type`, `@trait`, `@module`, `#automaton`, `#interface`, `#impl`, `#effect`, `#interrupt`, `#hardware`, `#test`, `static`, `const`, `extern_block`, `use_decl`, `@sequential` attribute (Refinement #5a + Decisions #7, #11, #16)
- Parse `@fn` with optional `$ [TraitList]`
- Parse `#automaton` with optional `#states` and `#address` annotations (Decisions #5, #6); zero or more `#transition` declarations inside the body
- Parse `#interface` and `#impl Interface for Automaton` blocks (Decision #16)
- Parse `#effect` with `#mutates: [...]` scope and optional generic parameters with interface bounds: `#effect name<S: Serial>(...)` (Decision #16)
- Parse `#> name(args)` as procedure call; classify call-site context based on whether callee resolves to `#effect` or `#transition` (Refinement #5b's generalization)
- Parse `sigma_loop` form (Decision #14): `sigma <pat> in <source> { body }`
- Parse `Auto.field <op>= expr;` mutation sugar (Decision #15)
- Build AST with sigil context preserved on every item and statement node

### Type System (§5)
- Verify `@fn` body against declared traits; default to `$ [Pure]`
- Enforce trait basis vector consistency globally (Emergent Rule 1)
- Reject `#` inside `@` scope; reject `@fn → #effect` calls (Emergent Rule 4)
- Verify reference rules (Decision #13): no reference returns (Rule 1), no reference fields (Rule 2), single-flow `&mut` uniqueness (Rule 3), field-provenance invalidation (Rule 4), linear allocator products (Rule 5), no `&mut` to automaton fields (Rule 0)
- Verify sigma-loop bound tracking; emit static bounds-check elimination where provable (Decision #14)
- Verify interface implementation completeness and coherence (Decision #16)
- Monomorphize generic effects at call sites (Decisions #2, #16)

### Effect & FSM Extraction (§6)
- Build the category `C_A` for every automaton: states as objects, `#transition` declarations as morphisms, identity morphisms implicit (Decision #5 Rule 1)
- An automaton with no `#states` clause is treated as `#states: [Ready]` with no transitions — a one-object monoid (Decision #5 Rule 4)
- Identity-context `#> name()` calls union their callee's `#mutates` into the caller's effective set without producing a state-tag update
- Transition-context `#> name()` calls additionally trigger a state-tag write to the transition's target on body completion
- Interface-method calls (`#> S::method`) resolve at monomorphization to the concrete implementation's effect; the call-site CallContext is determined by the resolved callee's kind (Decision #16)

### GA Orthogonality Engine (§7)
- Auto-assign basis vectors to automaton fields (Decision #4); register fields are normal automaton fields (Decision #6)
- Auto-assign basis vectors to traits (Emergent Rule 1)
- Compute behavior multivectors from field + trait vectors
- Check outer products for effect-pair orthogonality across concurrent automata (the wedge-product check is the existence proof for the product category `C_A × C_B`, per Emergent Rule 6)
- Honor `@sequential(A, B)` as user-supplied non-concurrency assertion (Decision #11)
- For monomorphized interface effects, analyze each specialization as if it were a distinct effect with the implementing automaton's field basis (Decision #16)
- Generate error messages with original source identifiers (E0520)

### Codegen (§8)
- Emit register-field reads/writes as volatile loads/stores at `address + offset` (Decision #6)
- Emit `#interrupt` handlers with target-specific calling convention and the user-declared linker symbol (Decision #10)
- Emit transition functions `clifford_<auto>__tr_<source>_to_<target>` with state-tag write at end (Decision #5)
- Emit one specialization per `(generic_effect, interface_arg)` pair (Decision #16)
- Sigma loops compile to counted loops; bounds checks elided when provable (Decision #14)
- Mutation sugar (`Auto.field <op>= expr;`) is desugared during AST construction; no codegen-level change (Decision #15)

### Conformance Testing (§10)
- Unit tests for each decision (sigil parsing, trait verification, procedure calls, basis assignment, monoid-automaton parsing, register blocks, named transitions, `:=` short binding, sigma loops, mutation sugar, interface impl coherence)
- Integration test: kernel scheduler example from §B compiles, orthogonality holds, tests pass
- Integration test: blinky firmware compiles to Cortex-M0+ in QEMU and runs
- Categorical edge cases: monoid (Logger) with no `#states`; one-shot transit-to-terminal (Boot); multi-state with implicit self-loops; ISR + main field-disjoint orthogonality
- Reference-safety tests: every UAF case from Rust's taxonomy (1, 2, 3, 5) is caught by Decision #13 rules; case 4 by GA engine
- Interface tests: `Serial` interface with three implementations (`Usart1`, `SoftwareUart`, `MockSerial` for tests) all share generic effects (Decision #16)
- Cross-decision tests: verify Rules 1–6 hold in real programs

---

## Open Questions

Decisions #1–#27 are all locked alongside six emergent rules and Refinement #1a. Implementation gating per the status header at the top of this file: #12 and #18 deferred to v0.2; #21 and #26 implementation gated to v0.7; #22, #23, #24, #25 design locked with implementation slated for v0.2; #27 (distributed runtime check) gated to v0.4+ Phase 5 plugin work. Items previously listed as open in `CLIFFORD_SPEC.md §12` and resolved here:

- ~~Effect/state/transition coupling~~ — Resolved by Decision #5.
- ~~`#hardware` capabilities~~ — Resolved by Decision #6 (subsumed into `#automaton` with hardware annotations).
- ~~Testing across the `@`/`#` boundary~~ — Resolved by Decision #7.
- ~~`:=` short-binding~~ — Resolved by Decision #8 (accepted, immutable only).
- ~~`#visible`/`#hidden`~~ — Resolved by Decision #9 (dropped).
- ~~Interrupt vector mapping~~ — Resolved by Decision #10 (linker symbol).
- ~~`@sequential(A, B)` attribute~~ — Resolved by Decision #11.
- ~~`static mut` survivability~~ — Resolved by Decisions #5/#6 (state ownership via automaton fields; registers via register-block automata).
- ~~Memory-safety story (intra-body UAF)~~ — Resolved by Decision #13.
- ~~Iteration construct~~ — Resolved by Decision #14.
- ~~Polymorphism over effects / HAL story~~ — Resolved by Decision #16.

**Items still open after this round (tracked in `CLIFFORD_SPEC.md §12`):**

- File extension (currently `.fe`; `.cl` recommended; awaiting final ratification)
- Invariant verification: static (SMT) vs runtime (Decision deferred to v0.2)
- garust integration: vendor vs in-tree (deferred until garust API stabilizes)
- Linear types beyond Decision #13 Rule 5 (deferred to v0.2)
- Dependent types / refinement types beyond sigma-loop bounds tracking (deferred to v0.2)
- Module system semantics: full design of `use` and `@module` resolution (Phase 5 stdlib will exercise this)
- Macros / proc-macros (deferred to v0.2)
- Async/await: out of scope (use automaton-based event loops)
- `#staged` deferred mutation (Decision #12; v0.2)
- Hierarchical states (Decision #5/Appendix B foundation; v0.2 design)
- `@phase(name)` sugar over `@sequential` (v0.2)
- Lifetime annotations as opt-in (v0.2 if non-firmware ambitions reveal need)
- `#valid_in: [State, ...]` clause on effects (v0.2)
- `dyn Interface` runtime dispatch (v0.2)
- Read-write race detection at field granularity (v0.2 if v0.1 dogfooding shows real bugs)
- **Sigma parallel decomposition** (`sigma … parallel { … }`): SIMD or task-parallel iteration. Recorded 2026-05-02. Requires the engine to verify per-iteration body independence, emit appropriate codegen (SIMD lanes / task spawn), and integrate with §7 orthogonality (do parallel iterations of one loop count as concurrent automata for race-detection?). Candidate for a future minor decision *if and when* a real use case surfaces; the door is reserved by keeping `parallel` out of the user identifier namespace. Library combinators built on plain `sigma` (sum, fold, count, find, etc.) are *not* future-decision material — those are stdlib code, not language extensions.

---

**Approved by:** Goose
**Dates:**
- Decisions #1–#4 approved April 29, 2026.
- Decisions #5–#16 (incl. Refinements #5a–d) approved April 30, 2026.
- Decisions #17–#20 approved April 30, 2026.
- Decision #21 approved May 1, 2026 (locked design direction; v0.7 implementation).
- Refinement #1a approved May 2, 2026.
- Decisions #22 and #25 approved May 3, 2026 (locked design; v0.2 implementation).
- Decisions #23 and #24 approved May 3, 2026 (DESIGN-IN-PROGRESS — ADRs `docs/adr/0003-haskell-clean-fn-discipline.md` and `docs/adr/0004-snapshot-boundary-operator.md` pending).
- Decisions #26 and #27 approved May 5, 2026.
- **Decisions #21, #26, #27 deferred to research May 16, 2026** by the post-GA-narrative pivot (`docs/decision-audit-2026-05.md`). Full text relocated to `docs/research/`; their compiler scaffolding removed from the live tree. The original ADRs (0002, 0005, 0006) remain immutable per CLAUDE.md §5.4.
**Next Step:** Propagate Decisions #6–#16 through `CLIFFORD_SPEC.md` (§1, §2, §4, §5, §6, §7, §8, §10, §12, §13, new §5.7/§5.8 sub-sections, new error code blocks). After that, Cargo workspace and Phase 1 implementation.
