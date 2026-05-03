# Clifford Language Technical Specification

**Version:** 0.6.0-draft
**Status:** Pre-implementation; reconciled with `DECISIONS.md` (Decisions #1–#21 locked; #12 and #18 designed but deferred to v0.2; Decision #21 implementation gated on v0.7+)
**Target:** LLVM IR via custom frontend
**Audience:** Compiler implementers (human and AI agents)
**Positioning:** General-purpose systems language. Embedded firmware is the canonical first target because the safety properties matter most there; the language is not embedded-only and the same constructs work for servers, robotics, scientific computing, game engines, and other systems-software domains. Domain-specific support (heap allocators, IO primitives, etc.) lives in Phase 5 stdlib, not in the core language.

---

## 0. How to Use This Document

This is an implementation-oriented spec. It defines *what* Clifford is precisely enough that an implementer can build a compiler from it. It does not motivate design decisions — see `clifford_spec_draft0.docx` for rationale and `DECISIONS.md` for the four locked design decisions.

**Conventions:**
- Grammar uses EBNF. `?` means optional, `*` means zero-or-more, `+` means one-or-more, `|` means alternation.
- Code in `monospace` is concrete syntax. Code in *italics* is metasyntactic.
- Sections marked **[NORMATIVE]** define required behavior. Sections marked **[INFORMATIVE]** explain or illustrate.
- Sections marked **[OPEN]** are unresolved design questions.

**Companion documents:**
- `DECISIONS.md` — locks four design decisions (sigil layering, hybrid `$ [TraitList]` traits, named effect procedures with `#>`, auto-assigned GA basis vectors) and five emergent rules. Where this spec and `DECISIONS.md` disagree, `DECISIONS.md` wins until reconciled here.
- `clifford_spec_draft0.docx` — rationale and worked examples (informative).

**Implementation order recommendation:**
1. Lexer + Parser (§2, §3) → emits AST
2. Type checker (§4, §5) → emits typed AST
3. Effect & FSM extraction (§6) → emits state graphs
4. GA orthogonality engine (§7) → verification pass
5. Codegen to LLVM IR (§8)
6. Standard library bootstrap (§9)

Each phase has a self-contained test suite spec in §10.

---

## 1. Lexical Structure **[NORMATIVE]**

### 1.1 Source Encoding

Source files are UTF-8. File extension is `.cl`. Line endings are `\n`; `\r\n` is normalized to `\n` on read.

### 1.2 Tokens

```
identifier  := [a-zA-Z_][a-zA-Z0-9_]*
integer     := [0-9]+ ('_' [0-9]+)* type_suffix?
hex         := '0x' [0-9a-fA-F]+ ('_' [0-9a-fA-F]+)* type_suffix?
binary      := '0b' [01]+ ('_' [01]+)* type_suffix?
float       := [0-9]+ '.' [0-9]+ ('e' [+-]? [0-9]+)? float_suffix?
string      := '"' (escape | [^"\\])* '"'
char        := "'" (escape | [^'\\]) "'"
byte        := 'b' "'" (escape | [^'\\]) "'"

type_suffix  := 'u8'|'u16'|'u32'|'u64'|'usize'|'i8'|'i16'|'i32'|'i64'|'isize'
float_suffix := 'f32'|'f64'
escape       := '\\' ('n'|'r'|'t'|'\\'|'\''|'"'|'0'|'x' hex_digit hex_digit)
```

Byte literals (`b'X'`) are typed `u8` and accept the same escape sequences as `char` literals. They differ from `'X' as u8` only in being a single token (no cast required) and in rejecting non-ASCII characters at lex time. Per Decision #15 ergonomics, byte literals are the canonical form for embedded code that works in bytes (UART, registers, byte buffers).

### 1.3 Keywords and Sigil-Prefixed Forms

Per `DECISIONS.md` Decision #1, Clifford uses two sigils to layer the language:

- **`#`** prefixes imperative constructs (state machines, effects, hardware mutators)
- **`@`** prefixes functional constructs (pure functions, types, modules)

The lexer treats sigils as primary tokens. Sigil-prefixed identifier forms are recognized atomically (e.g., `#automaton` is one token kind, not two).

**Bare keywords** (no sigil; valid in either layer per their syntactic position):

```
let mut const static
match if else while loop for in break continue return
extern unsafe as access null
self Self true false
```

The `access` keyword introduces nominal access types `access<T>` and `access const<T>` (Decision #19). The `null` keyword is the context-typed null access literal.

**Imperative sigil-prefixed forms** (`#`):

```
#automaton  #effect  #interrupt
#interface  #impl    #test
#mutate     #transition
#states     #mutates  #cannot_mutate
#invariant  #priority  #atomic        #basis
#address    #offset    #access
#> name(args)                        // effect-procedure call (Decision #3)
```

**Narrow unsafe primitives** (Decision #17 + Decision #19 — replace Rust-style `unsafe` block):

```
#unchecked_load    #unchecked_store
#volatile_load     #volatile_store
#unchecked_cast    #unchecked_offset
#asm
```

Each primitive is its own ordinary statement or expression inside a `#`-context body. There is no surrounding "unsafe block." Safety reviews grep for `#unchecked_*`, `#volatile_*`, and `#asm` to enumerate every unsafe occurrence in a project. Per Decision #19, the load/store/offset primitives operate on nominal access types (`access<T>` / `access const<T>`), not raw pointers — cross-type pointer use requires an explicit `#unchecked_cast<S, T>`, which is itself a grep target.

**Functional sigil-prefixed forms** (`@`):

```
@fn  @type  @trait  @module
@sequential       (top-level attribute)
@initial @terminal (state markers, optional)
```

**Composite sigils:**

- `#>` — effect-procedure call operator (Decision #3)
- `$` — trait-list marker, followed by `[Trait, Trait, ...]` (Decision #2)

**Notes:**
- `@trait` is the trait-declaration form. Traits describe purity-, reading-, and observation-categories assigned to functions; trait declarations live in the functional layer.
- `#interrupt` is a sigil-prefixed effect kind: an interrupt-context effect whose handler symbol is emitted per Decision #10 (§8.5).
- `#interface` declares an effect-signature interface; `#impl Interface for Automaton { … }` provides its implementation (Decision #16).
- `#test` declares a top-level test block with mixed-layer access (Decision #7).
- `#address`, `#offset`, `#access` are register-block annotations on `#automaton` declarations and their fields (Decision #6); `#hardware` from earlier drafts is retired.
- The Rust-style `#unsafe { ... }` block is removed in this revision (Decision #17). Unsafe operations are expressed through the narrow primitives listed below — `#unchecked_load`, `#unchecked_store`, `#volatile_load`, `#volatile_store`, `#unchecked_cast`, `#asm` — each as its own ordinary statement or expression inside a `#`-context body.
- `@sequential(A, B);` is a top-level attribute (Decision #11). `@initial` and `@terminal` are optional state markers per §6.1.
- Bare-keyword `extern` may modify a `@fn` declaration: `extern "C" @fn name(...) -> T $ [Opaque]`.
- `#visible` and `#hidden` from earlier drafts are removed (Decision #9); use `#mutates` and `#cannot_mutate` instead.

### 1.4 Operators and Punctuation

```
+  -  *  /  %  &  |  ^  <<  >>  !  ~
== != <  >  <= >=  &&  ||
=  :=  +=  -=  *=  /=  %=  &=  |=  ^=  <<=  >>=
->  =>  ::  :  ;  ,  .  ..  ..=  ?
#  @  $  #>
(  )  [  ]  {  }
```

The `---` token from earlier drafts (effect metadata/body separator) is removed: under the reconciled grammar, `#effect` metadata clauses appear inline between the parameter list and the body block (§2.5).

**Operator notes:**
- `:=` is the short-binding form (Decision #8). `let x := expr;` is sugar for `let x = expr;` with required type inference. `let mut x := expr;` is rejected (`E0210`); mutable bindings require the explicit `let mut x: T = expr` form.
- `<op>=` (compound assignment) is also valid in the mutation-sugar form `Auto.field <op>= expr;` (Decision #15), which desugars to `#mutate Auto { field <op>= expr };`.
- `@` appears in three contexts: as a sigil prefix (`@fn`), as a state-read operator (`<Auto>@state`, see §2.6), and as an attribute prefix (`@sequential(...)`, see §2.1).
- `::` appears in two contexts: as a path separator (`module::name`) and as a state-constructor reference (`<Auto>::<StateName>`, see §2.6).

### 1.5 Comments

```
line_comment  := '//' [^\n]* '\n'
block_comment := '/*' (block_comment | .)*? '*/'
doc_comment   := '///' [^\n]* '\n'
```

Block comments nest. Doc comments are attached to the next declaration and preserved in the AST.

---

## 2. Grammar **[NORMATIVE]**

### 2.1 Top Level

```
program     := item*
item        := fn_decl              // @fn
             | type_decl            // @type
             | trait_decl           // @trait
             | module_decl          // @module
             | automaton_decl       // #automaton (incl. register blocks per Decision #6)
             | effect_decl          // #effect / #interrupt (top-level per Refinement #5a)
             | interface_decl       // #interface (Decision #16)
             | impl_decl            // #impl Interface for Automaton (Decision #16)
             | test_decl            // #test (Decision #7)
             | static_decl
             | const_decl
             | extern_block
             | use_decl
             | sequential_attr      // @sequential(A, B); (Decision #11)
```

A program is a sequence of items. The leading sigil of each item identifies its layer: `@`-prefixed items are functional, `#`-prefixed items are imperative. `static`, `const`, `extern`, and `use` are unsigiled and can appear in either layer per their semantics. `@sequential(A, B);` is a top-level attribute that asserts non-concurrency between two automata; it is consumed by the GA engine (§7.3) and produces no runtime artifact.

### 2.2 Functional Declarations

```
fn_decl       := extern_modifier? '@fn' ident generic_params?
                 '(' params? ')' return_type? trait_list? where_clause? block
extern_modifier := 'extern' string_literal

trait_list    := '$' '[' trait_ref (',' trait_ref)* ']'
trait_ref     := ident generic_args?

type_decl     := '@type' ident generic_params? '=' type_body ';'
type_body     := type_expr | adt_body

trait_decl    := '@trait' ident generic_params? '{' trait_method* '}'
trait_method  := '@fn' ident generic_params? '(' params? ')' return_type? trait_list? ';'

module_decl   := '@module' ident '{' item* '}'

generic_params := '<' generic_param (',' generic_param)* '>'
generic_param  := ident (':' trait_bound)?
trait_bound    := type_expr ('+' type_expr)*
where_clause   := 'where' (ident ':' trait_bound (',' ident ':' trait_bound)*)?

params         := param (',' param)*
param          := 'mut'? ident ':' type_expr
return_type    := '->' type_expr
```

The `trait_list` (`$ [Trait, ...]`) is the trait-marker form from `DECISIONS.md` Decision #2; semantics are in §4.5. An `@fn` without a `trait_list` defaults to `$ [Pure]` (Emergent Rule 2; verified in §5.6).

`@trait` declarations contain only method signatures — no default bodies in v0.1. Trait satisfaction is structural at the call site (§5.3) and may optionally be documented with explicit conformance (§4.5).

`extern "C" @fn` declares an externally-linked function; its trait list defaults to `$ [Opaque]` if absent (compiler proves nothing about its behavior).

### 2.3 Storage Declarations

```
static_decl := 'static' ident ':' type_expr '=' const_expr ';'
const_decl  := 'const' ident ':' type_expr '=' const_expr ';'
```

Both forms declare immutable values. **There is no `static mut`** in this revision: mutable state is owned by automaton fields (§2.5). Memory-mapped hardware registers are modeled as register-block automata (Decision #6) with `#address`/`#offset`/`#access` annotations; access goes through normal `#mutate` and field-read machinery and is lowered to volatile loads/stores in §8.4.

### 2.4 ADTs

```
adt_body  := '|'? variant ('|' variant)*
variant   := ident variant_data?
variant_data := '(' type_expr (',' type_expr)* ')'
              | '{' field (',' field)* '}'
field     := ident ':' type_expr
```

### 2.5 Imperative Declarations

```
automaton_decl   := '#automaton' ident generic_params? address_clause? basis_clause?
                    '{' automaton_body '}'
automaton_body   := state_decl? automaton_member*
state_decl       := '#states' ':' '[' state_ident (',' state_ident)* ']'
state_ident      := ident state_marker?
state_marker     := '@initial' | '@terminal'
automaton_member := automaton_field
                  | transition_decl

automaton_field  := ident ':' type_expr field_attr*
                    // owned mutable state; each gets a GA basis vector (§7.1)
field_attr       := '#offset' ':' integer_literal      // for register-block fields
                  | '#access' ':' access_kind          // for register-block fields
access_kind      := 'r' | 'w' | 'rw'

address_clause   := '#address' ':' integer_literal
                    // marks this automaton as a register block (Decision #6);
                    // every field must declare #offset; #access defaults to rw if omitted

basis_clause     := '#basis' ':' '{' field_basis (',' field_basis)* '}'
field_basis      := ident ':' basis_vector
basis_vector     := 'e' integer

effect_decl      := effect_kind ident generic_params?
                    '(' params? ')' return_type?
                    effect_meta+
                    block
effect_kind      := '#effect' | '#interrupt'

effect_meta      := mutates_clause
                  | cannot_mutate_clause
                  | invariant_clause
                  | priority_clause
                  | atomic_clause

mutates_clause       := '#mutates'       ':' '[' ident (',' ident)* ']'
cannot_mutate_clause := '#cannot_mutate' ':' '[' ident (',' ident)* ']'
invariant_clause     := '#invariant'     ':' expr
priority_clause      := '#priority'      ':' priority_level
atomic_clause        := '#atomic'        ':' atomic_kind
priority_level       := 'LOW' | 'MEDIUM' | 'HIGH' | integer
atomic_kind          := 'interrupt_critical' | 'multicore_critical' | ident

transition_decl  := '#transition' ident ':' ident '->' ident block
                    // named transition: #transition <name>: Source -> Target { body }

interface_decl   := '#interface' ident generic_params? '{' interface_method* '}'
interface_method := 'effect' ident '(' params? ')' return_type? ';'
                    // signatures only; #mutates: [self] is implicit per Decision #16

impl_decl        := '#impl' interface_ref 'for' ident '{' impl_method* '}'
interface_ref    := ident generic_args?
impl_method      := 'effect' ident '(' params? ')' return_type? effect_meta* block
                    // body provided; #mutates defaults to [Automaton] for the
                    // implementing automaton, may be widened to include others

test_decl        := '#test' string_literal block
                    // top-level test with mixed-layer access (Decision #7);
                    // each test runs in isolation with automata reset to initial state

sequential_attr  := '@sequential' '(' ident ',' ident ')' ';'
                    // top-level non-concurrency assertion (Decision #11)
```

**Notes on imperative declarations:**

- Per `DECISIONS.md` Decision #5 and Refinements #5a–d, every `#automaton` is a small category `C_A` whose objects are the identifiers in `#states` and whose morphisms are the `#transition` declarations (plus implicit identity morphisms at every state). The categorical semantics are stated formally in `Appendix B`; user-facing prose in this section uses FSM language (states, transitions, reachability).
- The `#states` clause is **optional**. If omitted, the automaton is treated as `#states: [Ready]` with no declared transitions — a *one-object monoid* category whose only morphism is identity. This is the canonical form for allocators, loggers, register blocks, and any other "bag of related mutable state."
- **Per Refinement #5a, `effect_decl` is a top-level item** (§2.1), not an `automaton_member`. Effects are associated with automata via their `#mutates` clauses; one effect may touch multiple automata.
- **Per Refinement #5b, transitions are named:** `#transition <name>: Source -> Target { body }`. The name disambiguates multiple transitions between the same state pair (e.g., a `start` and a `timeout` transition both `Idle -> Reading`). A transition is fired by writing `#> name(args)` from a `#`-context.
- The `#mutates` and `#cannot_mutate` clauses list **automaton names** (per Decision #3), not paths. Field-level granularity is recovered statically by the compiler from the bodies of `#mutate` statements (§2.6) and used for GA orthogonality (§7). Per Decision #9, `#visible` and `#hidden` from earlier drafts are removed; use `#mutates`/`#cannot_mutate` instead.
- `#effect` declarations do **not** carry a `:: SourceState -> TargetState` coupling. State changes happen exclusively inside `#transition` blocks (Decision #5 Rule 2). An `#effect` is reusable across multiple transitions and may also be invoked outside any transition; in the latter case it fires during the implicit identity morphism on the current state (Decision #5 Rule 3).
- An effect with `effect_kind = #effect` requires `#mutates` (possibly empty list). An effect with `effect_kind = #interrupt` requires `#mutates`, `#priority`, and a name that is the linker-symbol-correct interrupt vector name for the target (Decision #10; see §8.5).
- A `#transition <name>: Source -> Target { … }` body is a sequence of `#> name(args)` calls, `#mutate` statements, and `@fn` calls. On successful body completion the automaton's state-tag is set to `Target`. Self-loops `#transition <name>: State -> State { … }` are *permitted* but never required — same-state work is naturally expressed by calling effects in identity-context (Decision #5 Rule 5).
- The `#basis` clause overrides auto-assignment per Decision #4. Each named field must exist in the automaton; basis vectors must not collide with other `#basis`-annotated automata in the same compilation unit.
- **Register blocks (Decision #6):** an `#automaton` with an `#address: <integer>` annotation is a register block. Every field must declare a `#offset: <integer>`; `#access` defaults to `rw` if omitted. Reads from write-only fields and writes to read-only fields are compile errors. The lowering in §8.4 emits volatile loads/stores at `address + offset`. `#hardware` from earlier drafts is retired; register access goes through normal `#mutate` machinery on register-block automata.
- **Interface declarations (Decision #16):** an `#interface` lists effect signatures (`effect name(params) -> ret;`); `#mutates: [self]` is implicit and refers to the implementing automaton at monomorphization. Multiple `#impl Interface for Automaton { … }` blocks provide bodies; coherence (one impl per `(Interface, Automaton)` pair) is checked in §5.
- **Test declarations (Decision #7):** `#test "name" { body }` is a top-level item with mixed-layer access. Each test runs in isolation; automata are reset to their declared initial state before each test invocation. Tests are discovered by `cliffordc test` and elided from production binaries.
- **Sequential attribute (Decision #11):** `@sequential(A, B);` at top level is a *trusted assertion* that the named automata never run concurrently. Consumed by the GA engine (§7.3) to suppress orthogonality checks between the named pair. The attribute is symmetric (`@sequential(A, B)` = `@sequential(B, A)`).

### 2.6 Statements and Expressions

```
stmt              := let_stmt
                  | let_short_stmt
                  | expr_stmt
                  | mutate_stmt
                  | mutate_short_stmt
                  | proc_call_stmt
                  | unsafe_primitive_stmt   // Decision #17 narrow unsafe ops
                  | return_stmt

let_stmt          := 'let' 'mut'? ident (':' type_expr)? '=' expr ';'
let_short_stmt    := 'let' ident ':=' expr ';'         // type-inferred immutable (Decision #8)
expr_stmt         := expr ';'

mutate_stmt       := '#mutate' ident '{' field_assign (',' field_assign)* ','? '}' ';'?
field_assign      := ident '=' expr | ident '[' expr ']' '=' expr
mutate_short_stmt := ident '.' ident assign_op expr ';'
                    // sugar (Decision #15): Auto.field <op>= expr
                    // desugars to #mutate Auto { field <op>= expr };
assign_op         := '=' | '+=' | '-=' | '*=' | '/=' | '%='
                  | '&=' | '|=' | '^=' | '<<=' | '>>='

proc_call_stmt    := '#>' qualified_name generic_args? '(' arg_list? ')' ';'?
qualified_name    := ident                         // local effect or transition
                  | ident '::' ident                // <Interface or Type>::<method>
arg_list          := expr (',' expr)*
return_stmt       := 'return' expr? ';'

expr    := literal
         | ident
         | path
         | block
         | if_expr
         | match_expr
         | loop_expr
         | while_expr
         | sigma_expr                  // sigma loop (Decision #14)
         | call_expr
         | method_expr
         | binary_expr
         | unary_expr
         | index_expr
         | field_expr
         | cast_expr
         | range_expr
         | struct_init
         | array_init
         | tuple_expr
         | state_read_expr             // <Auto>@state (Refinement #5d)
         | state_ref_expr              // <Auto>::<StateName> (Refinement #5d)
         | unsafe_primitive_expr       // narrow unsafe primitive (Decision #17)

unsafe_primitive_expr :=
            '#unchecked_load'   '<' type_expr '>' '(' expr ')'
         |  '#volatile_load'    '<' type_expr '>' '(' expr ')'
         |  '#unchecked_cast'   '<' type_expr ',' type_expr '>' '(' expr ')'
         |  '#unchecked_offset' '<' type_expr '>' '(' expr ',' expr ')'

unsafe_primitive_stmt :=
            '#unchecked_store'  '<' type_expr '>' '(' expr ',' expr ')' ';'
         |  '#volatile_store'   '<' type_expr '>' '(' expr ',' expr ')' ';'
         |  '#asm' '(' string_literal (',' asm_arg)* ')' ';'

// Per Decision #19, the load/store/offset primitives expect access types:
//   #unchecked_load<T>   takes  access const<T>      returns T
//   #unchecked_store<T>  takes (access<T>, T)        returns ()
//   #volatile_load<T>    takes  access const<T>      returns T
//   #volatile_store<T>   takes (access<T>, T)        returns ()
//   #unchecked_offset<T> takes (access<T>, isize)    returns access<T>
//   #unchecked_cast<S, T> takes S returns T          (works on access types and others)

block         := '{' stmt* expr? '}'
match_expr    := 'match' expr '{' match_arm (',' match_arm)* ','? '}'
match_arm     := pattern ('if' expr)? '=>' expr
pattern       := literal | ident | wildcard | constructor_pat | tuple_pat | struct_pat
wildcard      := '_'
constructor_pat := path ('(' pattern (',' pattern)* ')')?

sigma_expr    := 'sigma' sigma_pattern 'in' sigma_source block
sigma_pattern := ident                              // value-only iteration
              |  '(' ident ',' ident ')'             // (index, value) iteration
sigma_source  := range_expr                          // 0..len
              |  '&'? expr                           // &arr or arr (array source)

state_read_expr := ident '@state'                    // returns the state-tag
state_ref_expr  := ident '::' ident                  // Auto::StateName

range_expr      := expr '..' expr                    // half-open range [low, high)
                |  expr '..=' expr                   // inclusive range [low, high]
```

**Notes on statements:**

- `#mutate AutomatonName { field = expr, ... };` is the canonical bulk-write form for automaton fields. The sugar form `Auto.field <op>= expr;` (Decision #15) handles the common single-field case and desugars identically. Both are legal only inside an `#effect`, `#interrupt`, `#transition`, or `#impl` method body (§4.6). Decision #17 removed `#unsafe` blocks from this list; unsafe operations use the narrow primitives instead.
- `#> name(args)` calls a named effect procedure. The name must resolve statically to an `#effect` or `#transition` declaration in scope (Emergent Rule 5). It is illegal inside `@fn` (Emergent Rule 4). Calling another procedure unions its declared `#mutates` set into the caller's effective set for orthogonality checking (§6).
- **Per Refinement #5b's generalization, every `#> name(args)` call site is classified by the resolver/type-checker by callee kind:**
  - **transition-context** — the resolved callee is a `#transition`. After the called transition's body completes, the automaton's state-tag is written to the transition's `Target`.
  - **identity-context** — the resolved callee is an `#effect` (or `#interrupt` body invoking another `#effect`). The call mutates fields per its `#mutates` clause but does not change the state-tag.
  The classification is invisible to the user (no syntax change) and is recorded in the typed AST for §6 (FSM analysis) and §8.4 (codegen).
- State-changing transitions are expressed through `#transition <name>: Source -> Target { … }` declarations and invoked via `#> name(args)`. The legacy `transition ident;` statement is removed.
- **`:=` short binding (Decision #8)** is type-inferred immutable only. `let x := expr;` is sugar for `let x = expr;`. `let mut x := expr;` is rejected (`E0210`); use the explicit `let mut x: T = expr` form.
- **State inspection (Refinement #5d):** `Auto@state` reads the current state-tag of a multi-state automaton (an enum-like value); `Auto::StateName` is the constructor for a specific state value, used in pattern matching and equality comparisons. For monoid automata (no `#states` declared), `@state` and `::<name>` are static errors (`E0420`) since only one state exists.
- **Sigma loop (Decision #14):** `sigma <pat> in <source> { body }` is a bounded iteration construct. The loop variable carries an implicit bound annotation; direct array accesses with `arr[i]` are statically bounds-checked when provable. See §5.8 for the bounds-tracking algorithm.

### 2.7 Types

```
type_expr := primitive_type
           | path generic_args?
           | ref_type
           | access_type
           | array_type
           | slice_type
           | tuple_type
           | fn_type

primitive_type := 'u8'|'u16'|'u32'|'u64'|'usize'
                | 'i8'|'i16'|'i32'|'i64'|'isize'
                | 'f32'|'f64'|'bool'|'char'|'()'
ref_type     := '&' 'mut'? type_expr
access_type  := 'access' 'const'? '<' type_expr '>'
                // nominal: distinct identity per @type declaration (Decision #19)
array_type   := '[' type_expr ';' const_expr ']'
slice_type   := '[' type_expr ']'
tuple_type   := '(' type_expr (',' type_expr)+ ')'
fn_type      := '@fn' '(' (type_expr (',' type_expr)*)? ')' return_type? trait_list?
generic_args := '<' type_expr (',' type_expr)* '>'
```

A function-pointer type `@fn(...) -> T $ [Trait, ...]` carries its trait list as part of the type. Two function pointers with different trait lists are distinct types.

**Nominal access types (Decision #19).** The `access<T>` type constructor produces a *nominal* type — distinct identity per `@type` declaration. `@type UartPtr = access<Usart1>` and `@type SpiPtr = access<Spi1>` are different types even when their underlying representation (`Usart1*` and `Spi1*` at LLVM level) might be congruent. Mixing them in any operation is `E0710: nominal access types differ; use #unchecked_cast<S, T> if intentional`. The `access const<T>` form is the read-only variant; passing an `access<T>` where `access const<T>` is expected is automatic (covariant), but the reverse is `E0712: cannot convert read-only access to mutable access`.

The legacy raw pointer forms `*const T` and `*mut T` from earlier drafts are removed; all pointer-typed values use `access const<T>` and `access<T>` respectively. Pointer arithmetic is performed via the narrow primitive `#unchecked_offset<T>(p, n)` (§2.6).

**Reference-type restrictions (Decision #13 Rules 1 and 2):**

- A `ref_type` (`&T` or `&mut T`) is **forbidden in function return positions**: any `return_type` clause containing a reference is `E0702: reference returns are not permitted in v0.1; return owned values or indexes`. This applies to `@fn`, `#effect`, `#interrupt`, `#transition`, and `impl_method` declarations.
- A `ref_type` is **forbidden in field positions**: an `automaton_field`, struct field, ADT variant field, or tuple component cannot have reference type (`E0703`).
- References live exclusively on the call stack. They are created by borrow expressions (e.g., `&x`, `&mut x`), used during a single function body's execution, and never escape it.
- An `access_type` has no such return/field restrictions — access types are first-class values intended to be passed and stored. Their safety story is Decision #19's nominal type identity plus Decision #17's narrow-operation discipline.

These restrictions are what make Clifford's borrow check (§5.7) annotation-free: with no reference escapes, lifetimes are always "until the end of the containing body," and the analysis collapses to a single-pass intra-body provenance walk.

**`&mut` restriction (Decision #13 Rule 0):** an `&mut T` reference may only target a stack-local value. Taking `&mut Auto.field` is `E0700: cannot take mutable reference to automaton field; use #mutate instead`. Automaton-field writes go through `#mutate` exclusively. Immutable references `&Auto.field` are permitted but invalidated by subsequent `#mutate Auto { field = … }` (Rule 4 in §5.7).

---

## 3. Parser Behavior **[NORMATIVE]**

The parser is a recursive descent parser with one-token lookahead, augmented by sigil-driven dispatch:

1. **Sigil dispatch at item position:** The first token of an item determines the parsing mode. `@fn`, `@type`, `@trait`, `@module` enter the functional grammar; `@sequential` enters the attribute grammar; `#automaton`, `#effect`, `#interrupt`, `#interface`, `#impl`, `#test` enter the imperative grammar; `static`, `const`, `extern`, `use` are unsigiled and dispatched on the keyword. A sigil at item position followed by an unrecognized identifier is a parse error.
2. **Sigil dispatch at statement position:** Inside an `#effect`, `#interrupt`, `#transition`, or `#impl` method body, a leading `#mutate` or `#>` selects the corresponding statement form. The narrow unsafe primitives (`#unchecked_load`, `#unchecked_store`, `#volatile_load`, `#volatile_store`, `#unchecked_cast`, `#asm`) parse as ordinary statements/expressions per §2.6. The sugar form `Auto.field <op>= expr;` (Decision #15) is parsed as a `mutate_short_stmt` when `Auto` resolves to an automaton in scope. Other sigil-prefixed tokens at statement position are an error unless explicitly listed in §2.6.
3. **Generic vs. less-than disambiguation:** When parsing an expression containing `<`, peek ahead to determine whether it begins a generic argument list or is a comparison. This requires bounded backtracking.
4. **Inline effect metadata:** `#effect` and `#interrupt` declarations consume zero or more `effect_meta` clauses between the parameter list and the body block. The body block is recognized by an opening `{` that does not begin a `#mutates` / `#cannot_mutate` / `#basis` clause.
5. **`#states` omission default (Decision #5):** if an `#automaton` declaration has no `#states` clause, the parser inserts a synthetic `#states: [Ready]` into the AST and marks the declaration as a *monoid automaton* for downstream phases. No transitions are inserted; the category is one-object with the implicit identity morphism only.
6. **Register-block automaton dispatch (Decision #6):** an `#automaton` with a `#address` clause is marked as a register block in the AST. Every `automaton_field` in such an automaton must declare `#offset`; missing offsets are `E0610: register-block field requires #offset`. `#access` defaults to `rw` when omitted.
7. **Call-site context classification (Refinement #5b generalization):** during name resolution (after parsing), every `#> name(args)` call site is tagged with `CallContext::Transition { source, target }` if the resolved callee is a `#transition`, and `CallContext::Identity` if the resolved callee is an `#effect` (including `#interrupt`-resolved interface methods). The tag is recorded on the AST node and consumed by §6 (FSM extraction) and §8.4 (codegen).
8. **Interface-method dispatch (Decision #16):** a `#> Name::method(args)` call where `Name` is a generic type parameter bound by an interface is recorded as a `Generic` call site; the resolution to a concrete `#impl` happens at monomorphization (§5).
9. **Sigma-loop parsing (Decision #14):** the `sigma` keyword opens a `sigma_expr`; the parser captures the iteration pattern, the source expression, and the body block. Bound-tracking annotations are attached to the iteration variable in the typed AST for §5.8 to consume.

The parser preserves the originating sigil on every AST item and statement node so later phases can enforce the layer boundary (Decision #1, Emergent Rule 4) without re-scanning source.

The parser produces an AST with the following node kinds (informative):

```
Program
├── Item[]
│   ├── FnDecl       { sigil=@, name, generics, params, return_ty,
│   │                  trait_list, where_clause, body, extern_abi? }
│   ├── TypeDecl     { sigil=@, name, generics, body }
│   ├── TraitDecl    { sigil=@, name, generics, methods }
│   ├── ModuleDecl   { sigil=@, name, items }
│   ├── AutomatonDecl{ sigil=#, name, generics, basis_clause?,
│   │                  states, fields, effects, transitions,
│   │                  monoid: bool   /* true if #states omitted (Decision #5) */ }
│   ├── HardwareDecl { sigil=#, name, params, return_ty, body }
│   ├── StaticDecl   { name, ty, value }
│   ├── ConstDecl    { name, ty, value }
│   ├── ExternBlock  { abi, items }
│   └── UseDecl      { path }
```

Every `ProcCallStmt` (a `#> name(args)` invocation) carries a `context: CallContext` slot, populated during name resolution to one of:

```
CallContext::Transition { source: StateId, target: StateId }
CallContext::Identity   { state: StateId }   // current state at call site, when statically inferable
CallContext::Unresolved                       // fallback when the enclosing state cannot be statically pinned
```

The `Unresolved` variant covers cases where an effect is reusable across multiple call sites with different contexts; in those cases the FSM extractor (§6) discharges the obligation per-call-site rather than per-effect.

Each `#effect` or `#interrupt` inside an automaton is a node with explicit `mutates`, `cannot_mutate`, `visible`, `hidden`, `invariant`, `priority`, `atomic` slots — all optional except where required by effect_kind:

- `#effect`: requires `mutates` (may be empty list for pure effects)
- `#interrupt`: requires `mutates` and `priority`
- An extern-visibility effect form (carrying `visible` / `hidden`) is **[OPEN]**

The trait list on an `@fn` is recorded as a possibly-empty list of trait references. An absent trait list is recorded as `None` (distinct from an empty list `Some([])`); §5.6 maps `None` to the implicit `[Pure]` default while preserving the source distinction for diagnostics.

---

## 4. Type System **[NORMATIVE]**

### 4.1 Primitive Types

| Type | Size (bits) | Range / Notes |
|------|-------------|---------------|
| `u8`, `u16`, `u32`, `u64` | 8/16/32/64 | unsigned integer |
| `i8`, `i16`, `i32`, `i64` | 8/16/32/64 | two's complement signed |
| `usize`, `isize` | target pointer width | |
| `f32`, `f64` | 32/64 | IEEE 754 |
| `bool` | 1 (stored as 8) | `true` / `false` |
| `char` | 32 | Unicode scalar value |
| `()` | 0 | unit |

### 4.2 Composite Types

```
struct      — named record, layout = C ABI by default, repr(packed) available
enum        — tagged union, layout = (tag : smallest_int_fitting_variants, payload : largest_variant)
array       — [T; N], stack-allocated
slice       — [T], fat pointer (ptr + len)
tuple       — (T1, T2, ...), C-ABI struct
ref         — &T or &mut T, single-word pointer with body-scoped aliasing rules (Decision #13)
access      — access<T> or access const<T>, single-word nominal pointer (Decision #19);
              distinct identity per @type declaration; operated on via narrow
              unsafe primitives (#unchecked_load, #unchecked_store, etc.)
fn pointer  — @fn(...) -> T $ [TraitList], single word; trait list is part of the type
```

### 4.3 Algebraic Data Types

ADTs are sum types with named variants. The discriminant is the smallest unsigned integer that fits all variant indices. Layout:

```
type Result<T, E> =
  | Ok(T)
  | Err(E);

// Layout:
//   tag:     u8 (or smaller if fewer than 256 variants)
//   payload: union { ok: T, err: E }
//   total:   max(sizeof(T), sizeof(E)) + sizeof(tag), padded to alignment
```

Pattern matching on an ADT must be exhaustive. Non-exhaustive matches are a compile error unless a `_` arm is present.

### 4.4 Generics

Generics are monomorphized at compile time. Each instantiation produces a distinct LLVM function. Generic bounds are checked at the use site, not at the definition site (structural).

### 4.5 Hybrid Traits with Signature Markers

Per `DECISIONS.md` Decision #2, Clifford uses a hybrid trait scheme: traits are *declared* with `@trait` and *attached* to functions via the `$ [TraitList]` marker on the function's signature. Conformance is verified structurally at the call site (§5.3) without requiring an `impl` block.

A trait declaration enumerates method signatures:

```
@trait Initializable {
  @fn init(self) -> Self;
}
```

A type satisfies the trait if and only if it has methods with matching signatures (name, parameter types, return type, trait list). `Self` is substituted by the candidate type during checking.

**Built-in behavior traits.** The compiler ships with a set of predeclared traits used by the GA orthogonality engine (§7). Each predeclared trait has a globally-fixed basis vector (Emergent Rule 1).

| Trait | Meaning |
|---|---|
| `Pure` | No state reads, no state writes, no I/O; deterministic for fixed inputs |
| `Readable` | May read from declared automaton fields and `static` storage; no writes |
| `Observable` | May invoke `@fn`s with `$ [Readable]`; no writes |
| `Opaque` | Compiler proves nothing; reserved for unverified `extern "C" @fn` |

**Trait-list semantics on `@fn`:**

- An `@fn` declared with `$ [T1, T2, ...]` asserts conformance to every trait in the list. The body is checked against the conjunction of trait obligations (§5.6).
- An `@fn` with no `$ [...]` defaults to `$ [Pure]` (Emergent Rule 2). If the body violates `Pure`, compilation fails.
- An `extern "C" @fn` with no `$ [...]` defaults to `$ [Opaque]`.
- The trait list is part of the function's *type*. Two `@fn` types differing only in trait lists are distinct types; assigning a `$ [Readable]` `@fn` to a slot expecting `$ [Pure]` is a type error.

**Optional explicit conformance** (informative): a `@trait Foo for Bar { ... }` form for documenting that a concrete type satisfies a trait is reserved for v0.2.

### 4.6 Mutability and Mutation Contexts

Bindings are immutable by default. The `mut` keyword on a binding makes it mutable. Per Refinement #5c (Decision #5) and Decision #13 Rule 0, the rules differ for *automaton-field* mutation versus *local-stack* mutation:

**Automaton-field mutation** is only legal inside a designated **mutation context**:

1. Inside an `#effect`, `#interrupt`, or `#transition` body
2. Inside an `#impl` method body (which is itself an effect by Decision #16)
3. During initialization of a `static` declaration (compile-time only; the resulting binding is immutable)

Note: per Decision #17 the previous `#unsafe { ... }` block syntax has been removed. Raw memory access happens through narrow unsafe primitives (`#unchecked_load`, `#unchecked_store`, `#volatile_load`, `#volatile_store`, `#unchecked_cast`, `#asm`), each of which is an ordinary expression or statement inside one of the mutation contexts above. The unsafe primitives do not introduce a new mutation context — they live inside the existing ones.

Field writes are spelled `#mutate AutomatonName { field = expr };` (or its sugar `Auto.field <op>= expr;` per Decision #15) and are only legal inside a mutation context whose `#mutates` clause names the target automaton (§6.2). Writes outside a mutation context, or to an automaton not in the surrounding `#mutates` set, are a compile error (`E0301`, `E0302`).

**Local stack-allocated mutation** (`let mut x = …; x = …;`) is **legal in any function body, including `@fn`** (Refinement #5c). Local mutation does not affect a function's purity classification (`Pure`/`Readable`/etc.) because it is invisible to the caller — the function's input/output behavior is the same as a recursive or fold-based equivalent. Without this, no iterative algorithm would be writable in `@fn` (forced into recursion), which is a non-starter for embedded.

**`&mut` references are restricted to stack-local values** (Decision #13 Rule 0). Taking `&mut Auto.field` is `E0700: cannot take mutable reference to automaton field; use #mutate instead`. Immutable references `&Auto.field` are permitted but invalidated by subsequent `#mutate Auto { field = … }` (§5.7 Rule 4).

Clifford has no borrow checker in the Rust sense; intra-body memory safety is provided by the body-scoped reference discipline of §5.7 (Decision #13). The cross-effect mutation discipline is gated by sigil layering and by `#mutates` declarations consumed by the GA engine (§7).

### 4.7 Sigil-Layer Boundary

Per Decision #1, the parser stamps every item and statement with its layer (`@` functional, `#` imperative). The type checker enforces:

- A `@fn` body may not contain any `#`-sigil construct (no `#mutate`, no `#> proc()`, no `#unsafe`, no reference to an `#automaton` field by name).
- An `#effect`, `#interrupt`, or `#transition` body may freely call `@fn`s and execute `#mutate` / `#> proc()` statements per its declared `#mutates` set.
- Cross-boundary *upward* inlining is forbidden at all optimization levels (Emergent Rule 4): the compiler may not inline a `#`-construct into an `@fn` body, even after monomorphization or dead-code elimination. This is a constraint on §8 lowering, not just §5 checking.
- Cross-boundary *downward* inlining (`@fn` body inlined into an `#effect` body) is **explicitly permitted and forms the standard optimization path** for hot loops. Pure functions are inlined exactly like Rust `#[inline]` or C++ `inline`; nothing about the layer boundary precludes this. Only the safety-violating direction is forbidden.

Violations are reported with `E0101: imperative construct in functional scope` or `E0102: cross-boundary call`.

### 4.8 Type Inference

Local variable types are inferred via Hindley–Milner with extensions for:
- Integer literal default to `i32` if unconstrained
- Float literal default to `f64` if unconstrained
- Generic parameters inferred from call site arguments

Function signatures must be fully annotated (no inference at item boundaries). Trait lists on `@fn` signatures are not inferred — they are declared and verified.

---

## 5. Type Checking Algorithm **[NORMATIVE]**

The type checker operates on the AST and produces a typed AST with explicit type annotations on every expression node.

### 5.1 Phases

```
1. Resolve names → bind every identifier to a definition,
                   bind every #> call to its #effect target
2. Build type environment Γ; assign default trait list ([Pure] for @fn,
                   [Opaque] for extern "C" @fn) where absent
3. Walk AST, infer/check types bottom-up
4. Verify sigil-layer boundary (§5.5)
5. Verify trait satisfaction at every method call site (§5.3)
6. Verify trait-list obligations on each @fn body (§5.6)
7. Verify pattern exhaustiveness in match expressions
8. Verify mutability and mutation-context rules (§5.4)
9. Emit typed AST (carries trait list, sigil-layer, effect-procedure target on every node)
```

### 5.2 Inference Rules

Standard Hindley–Milner. Key rules:

```
Γ ⊢ x : Γ(x)                              [Var]

Γ ⊢ e1 : T1 -> T2    Γ ⊢ e2 : T1
─────────────────────────────────         [App]
       Γ ⊢ e1(e2) : T2

Γ, x:T1 ⊢ e : T2
─────────────────────────                 [Lam]
Γ ⊢ |x:T1| e : T1 -> T2

Γ ⊢ e : T    T satisfies trait Tr
─────────────────────────────────         [TraitSat]
       Γ ⊢ e : Tr
```

### 5.3 Trait Satisfaction Algorithm

```
fn satisfies(ty: Type, tr: Trait) -> bool {
  for method in tr.methods {
    let candidate = ty.find_method(method.name)?;
    if !signature_compatible(candidate.sig, method.sig) {
      return false;
    }
  }
  true
}
```

`signature_compatible` performs structural equivalence on parameter and return types, with `Self` substituted by `ty`.

### 5.4 Mutability Checking

After type checking, walk the AST and verify:

- Every assignment `x = e`, `x.field = e`, `x[i] = e` occurs inside a mutation context (§4.6); otherwise emit `E0301`.
- Every `mut` binding is reachable only inside a mutation context.
- Every `#mutate A { ... }` statement (canonical form) and `Auto.field <op>= expr;` sugar (Decision #15) appears inside an `#effect`, `#interrupt`, `#transition`, or `#impl` method body whose `#mutates` clause names automaton `A`; otherwise emit `E0302: write to undeclared automaton`.
- Every assignment inside `#mutate A { f = expr }` targets a real field `f` of automaton `A`; otherwise emit `E0303: unknown automaton field`.
- Every field listed in a `#cannot_mutate` clause is *not* written by any `#mutate` in the body.

### 5.5 Sigil-Layer Boundary Checking

Walk the AST. For each node, the parser-stamped layer must obey Decision #1 and Emergent Rule 4:

- A `@fn` body must contain no `#`-sigil construct: no `#mutate` (canonical or sugar form), no `#> name()`, no narrow unsafe primitive (`#unchecked_*`/`#volatile_*`/`#unchecked_cast`/`#asm`), no reference to an `#automaton` field by name. Violations emit `E0101`.
- A `#`-context body may call `@fn`s freely (downward call), but never the reverse — calling a `#effect` from `@fn` emits `E0102`.
- The boundary is preserved at every later phase (§6 effect graph, §8 codegen). No optimization may inline a `#`-construct into a `@fn` (Emergent Rule 4).

### 5.6 Trait-List Verification

For each `@fn f` with declared trait list `T = [t_1, ..., t_k]` (defaulted to `[Pure]` if absent):

```
1. For each trait t_i in T, look up the trait's body-obligation predicate P_i
2. Walk the body of f. For each statement / expression, verify all P_i hold:
     - Pure       : no field reads, no field writes, no extern calls
     - Readable   : no field writes; reads only from automaton fields and `static`
     - Observable : may call @fns whose trait list ⊆ {Readable, Observable, Pure}; no writes
     - Opaque     : no obligations (used only on extern declarations)
3. If any obligation fails, emit E0201 with the offending source location:
     "function f declared $ [T] but performs <action> (forbidden by t_i)"
4. For user-declared traits in T, the obligation is the conjunction of:
     - structural conformance to the trait's method signatures (§5.3)
     - any compiler-recognized predicates declared in §[future: effect-trait extension]
```

The verifier records the *minimal* trait list a function actually requires, for diagnostic purposes (so an over-declared `$ [Readable, Observable]` on a function that is actually `[Pure]` produces a hint, not an error).

### 5.7 Reference Provenance and Body-Scoped Borrowing

This section formalizes Decision #13's six-rule reference discipline. The check runs after type checking and before §5.8. Together with §2.7's structural restrictions on reference types, it provides Clifford's intra-body memory safety guarantee — equivalent to Rust's borrow-check coverage for the cases that matter (UAF, double-free, dangling-ref, iterator invalidation) without lifetime annotations.

**Rule 0 (no `&mut` to automaton fields).** `&mut T` references are restricted to stack-local values. Taking `&mut Auto.field` is `E0700: cannot take mutable reference to automaton field; use #mutate instead`. This is enforced at parse/resolution time when the borrow expression is inspected; no provenance graph is needed for it.

**Rule 1 (no reference returns).** Function signatures (`@fn`, `#effect`, `#interrupt`, `#transition`, `impl_method`) cannot have a return type containing `&T` or `&mut T`. Enforced at item parsing; `E0702`. Already stated in §2.7.

**Rule 2 (no reference fields).** Struct fields, ADT variants, automaton fields, and tuple components cannot have reference type. Enforced at item parsing; `E0703`. Already stated in §2.7.

**Rule 3 (single-flow `&mut` uniqueness).** Within a single body's straight-line control flow, at most one mutable reference to any given memory location may be live at any program point.

**Rule 4 (field-provenance invalidation).** A reference `r` derived from automaton field `Auto.f` (via `&Auto.f`, `&mut Auto.f` — though Rule 0 forbids the latter — or any chain of borrows ultimately rooted in `Auto.f`) is invalidated by the next `#mutate Auto { f = … }` (or its sugar `Auto.f = …`) in execution order. Using an invalidated reference is `E0701: reference invalidated by mutation`.

**Rule 5 (linear allocator products).** A type marked `#[linear]` (e.g., the `Box<T>` produced by an allocator) is consumed by a free operation (`#free` or analogous). After consumption, the original binding is moved-from and inaccessible; references derived from it are invalidated by Rule 4 logic. `E0704: use of moved value`.

**The provenance algorithm (informative):**

```
For each #-context body (effect / interrupt / transition / impl method body):

  1. Initialize an empty provenance map P : Reference -> Source
     where Source = AutomatonField(name) | StackLocal(binding) | Linear(binding)

  2. Walk the body in execution order. For each statement / expression:

     a. Borrow expression (`&x`, `&mut x`, `&Auto.f`):
          create a new reference `r`; record P[r] = source(x).
          If &mut and source ∈ AutomatonField, error E0700.
          If &mut and another `&mut` to the same source is currently live,
          error E0705 (Rule 3 violation).

     b. Mutation statement (#mutate Auto { f = … } or sugar Auto.f = …):
          for every reference `r` with P[r] = AutomatonField(Auto.f),
          mark `r` as invalidated. Future use is E0701.

     c. Free statement (#free p, where p has type Box<T>):
          for every reference `r` with P[r] derived from `p`,
          mark `r` as invalidated. Mark `p` itself as moved.
          Future use of `p` is E0704.

     d. Use of a reference (e.g. *r, r.field, passing as arg):
          if `r` is invalidated, emit the appropriate error
          with a note pointing at the invalidating statement.

  3. At body exit, drop all references in P. References cannot escape
     by Rules 1, 2.
```

The algorithm is linear in body size. No fixpoint iteration. No SMT solver. No lifetime annotation system.

**What this catches (vs. Rust's UAF taxonomy):**

- UAF-1 (free-then-deref): caught by Rule 4 + Rule 5.
- UAF-2 (return ref to local): caught by Rule 1 (rejected at signature parsing).
- UAF-3 (iterator invalidation): caught by Rule 3 + Rule 4 (no `&v[0]` and `&mut v` simultaneously; mutation invalidates references derived from automaton fields).
- UAF-4 (cross-thread race): caught by the GA orthogonality engine (§7), not §5.7.
- UAF-5 (hold ref across in-place mutation): directly caught by Rule 4.

**What this does not catch (the v0.1 honest scope):**

- Bugs through narrow unsafe primitives (`#unchecked_*`/`#volatile_*`/`#asm`): raw pointer reads/writes are not provenance-tracked. The audit story is per-occurrence visibility (Decision #17) plus optional runtime auditing via `#audit` + `PointerAuditor` (Decision #18, v0.2).
- Cross-effect aliasing: handled by the GA engine, not §5.7.
- Non-aliasing logic bugs (off-by-one, etc.): out of scope; that's what `#test` (§7) and runtime assertions are for.

### 5.8 Sigma Bounds Tracking

Per Decision #14, a `sigma` loop binds an iteration variable with an implicit upper bound. The compiler tracks the bound through the loop body and uses it to elide runtime bounds checks on direct array accesses.

**Bound assignment.** For a `sigma_expr` with source `0..n` (where `n` is an integer expression evaluated once at loop entry), the iteration variable `i` is assigned the refinement type `bounded<0, n>` — informally, `i: usize` with the static fact `0 ≤ i < n`. For an array source `&arr` (where `arr: [T; N]`), the iteration variable is the array's element type `T` and an implicit `i: bounded<0, N>` is also bound (visible via the `(i, x)` pattern form).

**Bound usage at array access.** When the compiler sees an expression `arr[expr]` inside the sigma body, it tries to prove `expr < arr.len()`:

```
1. If `expr` is the iteration variable `i` directly, and `arr.len() >= n`
   (where n is the loop's upper bound), the access is safe; emit GEP+load
   with no runtime check.
2. If `expr` is the iteration variable in a constant-shifted form
   (e.g., `i + 1` inside `sigma i in 0..(n-1)` where arr.len() >= n), the
   compiler tries algebraic simplification; if it succeeds, no check.
3. Otherwise, fall back to a runtime bounds check (panic on failure) and
   emit a hint diagnostic suggesting refactoring.
```

**Loss-of-bound on arithmetic.** Any non-trivial arithmetic on the iteration variable widens its type back to plain `usize`:

```
sigma i in 0..n {
  arr[i]            // i : bounded<0, n> ; bounds-check elided if arr.len() >= n
  let j := i + 5;   // j : usize  (bound lost; future arr[j] runtime-checked)
  arr[j]            // runtime bounds-check
  arr[i + 5]        // simple shift; tracked if arr.len() >= n + 5
}
```

The compiler is intentionally conservative: it tracks trivial cases (the variable itself, simple constant shifts), not arbitrary expressions. Refinement-type sophistication via SMT is v0.2 territory.

**Bound capture timing.** The upper bound of a `0..n` source is captured once at loop entry. Mutating `n` inside the loop body has no effect on the iteration count and does not invalidate the bound:

```
let mut n: usize = 10;
sigma i in 0..n {
  n = 20;    // E0802: bound expression is captured at loop entry; this assignment has no effect on the loop bound.
}
```

This is a hard error rather than a silent quirk to prevent confusion.

**Edge cases:**

- Empty range `sigma i in 0..0 { … }`: body never executes; bound is `bounded<0, 0>` which is uninhabited.
- Reverse range `sigma i in 5..3 { … }`: empty iteration, no error. (Optional `.rev()` form for descending iteration is v0.2.)
- Nested sigma: each level is tracked independently.
- Sigma over an array element pattern `sigma x in &arr`: `x` is an immutable copy of each element; `&arr` is borrowed for the duration of the loop body and Rule 4 invalidation applies.

---

## 6. Effect & FSM Extraction **[NORMATIVE]**

This is the phase that distinguishes Clifford from C. After type checking, the compiler walks every `#automaton` declaration and constructs:

1. **Category `C_A`** — objects are the states declared in `#states` (or the synthetic `[Ready]` for monoid automata, per Decision #5 Rule 4); morphisms are `#transition` declarations plus the implicit identity morphism at every state.
2. **Mutation profile (per-effect, per-field)** — for each `#effect` / `#interrupt`, the set of automaton fields it actually writes via `#mutate` statements
3. **Read profile** — for each effect, the set of automaton fields and `static` paths it reads (inferred from body)
4. **Effect-procedure call graph** — directed graph whose edges are `#> name(...)` calls, statically resolvable per Emergent Rule 5; each edge is labeled with its `CallContext` (transition or identity, per §3 step 6).

The categorical structure underlying `C_A` is formalized in `Appendix B`. Prose in this section uses FSM language; the analyses described here operate uniformly across multi-state automata (full FSMs) and one-object monoid automata (no `#states` clause).

### 6.1 Category Construction and Validation

For each automaton A:

```
1. Build category C_A:
   - Objects: identifiers in `#states: [...]`, or {Ready} if #states is omitted
   - Morphisms: one per `#transition Source -> Target` declaration in A,
                plus an implicit identity morphism at every object
   - Initial object: first state in #states declaration order, or @initial-marked
   - Terminal objects (optional): states marked @terminal
2. If A is a monoid automaton (one object, identity morphism only),
   skip steps 3–5; the validation succeeds trivially.
3. Verify every object is reachable from the initial object via composed morphisms
   (warn on unreachable: W6101).
4. Verify no non-terminal object has only the identity morphism on it
   (warn on stuck-state: W6102).
5. Detect strongly-connected components with no exit (potential deadlock: W6103).
```

State-graph analysis is the same algorithm regardless of whether A has many states or one — the monoid case just trivially satisfies every check.

### 6.2 Mutation Profile Extraction

For each effect E in automaton A:

```
declared_mutates  = E.#mutates           // automaton names listed
declared_excluded = E.#cannot_mutate     // automata that must NOT be touched

actual_writes = walk_body(E.body, collect_field_writes)
  // a field write is a `field = expr` slot inside `#mutate A { ... }`
  // (canonical or sugar form `A.field <op>= expr`),
  // recorded as the pair (A, field).
  // `#> name(args)` calls union the callee's transitive declared field-writes
  // into the caller's actual_writes (Decision #3).

actual_automata = { A | (A, _) ∈ actual_writes }

assert: actual_automata ⊆ declared_mutates
assert: actual_automata ∩ declared_excluded = ∅
assert: every name in declared_mutates resolves to an in-scope `#automaton`
```

If `actual_automata ⊄ declared_mutates`, emit `E0410: effect mutates undeclared automaton`. If an automaton in `declared_excluded` is touched, emit `E0411: effect mutates excluded automaton`. (Per Decision #9, the earlier `#visible`/`#hidden` clauses have been retired; `#cannot_mutate` is the sole exclusion mechanism.)

The per-field set `actual_writes` is what the GA engine consumes (§7.2) — automaton-level granularity is too coarse to detect disjoint-field non-interference.

The same analysis runs uniformly for `#transition` bodies: the body of a transition is a sequence of statements whose `#mutate` writes and `#>`-call mutation profiles are gathered exactly as for effect bodies. A transition's effective mutation profile is the union of its body's writes; the GA engine treats a transition as a morphism whose blade is the wedge of its mutated fields.

### 6.3 Effect-Procedure Call Resolution and Context Classification

For each `#> name(args)` call inside an effect or transition body:

```
1. Resolve `name` to an `#effect`, `#transition`, or interface-method
   declaration in scope (a top-level item, or imported via `use`).
   Failure → E0420.
2. Verify the call site is inside an `#effect` / `#interrupt` /
   `#transition` / `#impl` method / `#test` body (never inside `@fn`).
   Failure → E0421.
3. Verify argument arity and types match the callee signature.
4. Classify the call site's CallContext per Refinement #5b's
   generalization (callee-kind, not lexical scope):
     - Transition { source, target } if the resolved callee is a
       `#transition <name>: source -> target { … }` declaration.
       After the callee body completes, the automaton's state-tag is
       written to `target`.
     - Identity { state } if the resolved callee is a `#effect` or
       `#interrupt`. The call mutates fields per its `#mutates` clause
       but does not change the state-tag of any automaton.
     - Generic if the callee is an interface method on a type parameter
       (`#> S::method(args)` where `S: Interface`); the actual context
       is determined per monomorphized specialization at §5.
5. Add a directed edge caller → callee in the procedure call graph,
   labeled with the CallContext.
6. Reject any cycle in the call graph that includes the current
   effect/transition, unless explicitly marked recursive: E0422.
```

The compiler propagates `#mutates` transitively along call edges so that §6.2's `actual_automata` includes all automata reachable through `#>` calls. CallContext labels are passed to §8.4 to drive state-tag prologue/epilogue emission: only Transition-context calls produce a state-tag write; Identity-context calls do not. Generic-context calls are resolved per monomorphization and reduced to either Transition or Identity in the typed AST.

### 6.4 State-Tag Update Semantics (Transitions)

For each `#transition Source -> Target { body }` in automaton A:

```
1. Verify Source and Target are both members of A.#states
   (E0430: unknown state in transition).
2. The morphism Source → Target is added to C_A's morphisms.
3. The body's mutation profile is computed per §6.2.
4. On execution, after the body runs to completion, the state-tag of A
   is set to Target. This is realized by codegen (§8.4 step 5) as a
   single store to the state-tag field generated for A.
5. If Source == Target, the transition is a self-loop. Self-loops are
   permitted but rarely needed (Decision #5 Rule 5): same-state work is
   normally expressed via identity-context #> calls without ceremony.
```

A transition with an empty body (`#transition S -> T { }`) is legal and acts as an unconditional state change — sometimes useful for explicit one-shot lifecycles (e.g., `Init -> Done`).

### 6.5 Invariant Verification

Each automaton may declare zero or more `#invariant: expr` clauses (one per `#effect` / `#interrupt` / `#transition` declaration that has an invariant clause, or as a top-level annotation on the automaton itself for global invariants — global form is **[OPEN]** for v0.2). Each `expr` must:
- Have type `bool`.
- Reference only state visible to the surrounding context (the automaton's fields, parameters of the surrounding effect/transition, locals in scope).
- Be free of side effects: the invariant expression is itself `$ [Pure]` regardless of its enclosing context.

**When invariants run.** An invariant attached to automaton `A` is checked **after every mutation-context exit that mutated any field of A** — i.e., after each `#effect`, `#interrupt`, `#transition`, or `#impl` method body completes if its `actual_writes` (§6.2) touched a field of `A`. The check fires once at the *outermost* mutation-context exit, not after each individual `#mutate` statement, so multi-step state changes within a single body don't trigger spurious mid-mutation failures. Identity-context `#>` calls within a larger body don't trigger separate checks; the enclosing top-level effect/interrupt/transition's exit is the single check point.

This is more conservative than the previous draft, which only checked after `#transition` body completion. **Identity-context effects can also break invariants**, so the spec now requires invariant checking at every mutation-context exit. Closes a real gap surfaced by external review.

**Static vs. runtime checking** is **[OPEN]** for v0.1: static discharge via SMT (Z3) requires significant tooling and is unlikely to land in the v0.1 timeframe. Runtime checking in debug builds is the reference v0.1 implementation, with a `--verify-invariants=static` flag to opt into SMT discharge as an experimental v0.2 feature. Production builds elide invariants entirely unless the user passes `--verify-invariants=release`.

**Composition with the GA engine.** Invariant checks are inserted by §8.4 codegen at mutation-context exit points; they do not participate in the §7 orthogonality analysis (they are read-only with respect to the same fields they're verifying). However, an invariant evaluation that itself mutates state is a hard error (`E0621: invariant must be pure`) — invariants describe state, they don't change it.

### 6.6 Atomic Annotation Lowering

```
#atomic: interrupt_critical    → wrap body in cli/sti pair (or target equivalent)
#atomic: multicore_critical    → wrap body in compare-and-swap loop or hardware lock
#atomic: <ident>               → user-defined atomic primitive, looked up in scope
```

The compiler emits the wrapping in the LLVM IR; the user sees a normal effect body.

---

## 7. GA Orthogonality Engine **[NORMATIVE]**

The orthogonality engine runs after FSM extraction and before codegen. It assigns basis vectors to **automaton fields** and **traits**, computes behavior multivectors for each effect and automaton, and verifies pairwise non-interference for all automata that may execute concurrently.

The semantics in this section are the normative form of `DECISIONS.md` Decision #4 and Emergent Rules 1 and 6.

**What the wedge-product check proves (Emergent Rule 6).** For any two concurrent automata `A` and `B`, the parallel composition is modeled as the product category `C_A × C_B` (built from the per-automaton categories of §6.1). The orthogonality check `behavior(A) ∧ behavior(B) ≠ 0 of grade |A| + |B|` is the constructive existence proof for that product: it succeeds exactly when no two parallel morphisms (one in `C_A`, one in `C_B`) touch overlapping mutable state, which is the well-formedness condition for the product category. The bitmask implementation in §7.4 is the algorithm; `Appendix B` states the theorem.

### 7.0 Algebra: restricted form (v0.1–v0.6) and full form (v0.7+) **[NORMATIVE]**

This spec version (v0.5.0-draft, targeting language v0.1–v0.6) defines the orthogonality engine over the **restricted Clifford algebra Cl(0,0,n)**: every basis vector squares to zero. The bitmask check in §7.4 (`a & b != 0 ⇒ wedge == 0`) is the operational form of this restriction. Under the restricted form, two automata are concurrent-safe iff they touch *literally disjoint* state — the strongest possible isolation property and the right one for embedded firmware that does not deliberately share mutable state.

**Reservation for v0.7+: mixed-metric extension.** Per `DECISIONS.md` Decision #21 (and ADR 0002), v0.7.0-draft will extend the engine to the **mixed-metric Clifford algebra Cl(p,0,n)** in which:

- Private fields (the v0.1 default; `FieldKind::Private`) contribute null basis vectors, with the current §7.4 collapse-on-overlap behavior.
- Shared fields (the v0.7+ `#shared` field qualifier, AST `FieldKind::Shared { lock }`) contribute non-null basis vectors that do *not* collapse the wedge product. Overlap on a shared basis vector is permitted; it generates a separate proof obligation that the lock guarding the shared resource is held by both concurrent contexts.

The mixed-metric algebra is sketched in §7.9 below. v0.1–v0.6 implementations MUST treat every automaton field as `FieldKind::Private` and MUST reject `#shared` / `#lock` / `#with_lock` / `#reads` / `#rotor` token forms with a "reserved for v0.7" diagnostic. The lexer reserves these tokens from v0.6+ so that v0.7 enabling is a non-breaking change.

**Implications for v0.1–v0.6 implementations.** No semantic change. `crates/ortho` operates on the restricted algebra. The `crates/ast` `AutomatonField::kind` field is always `FieldKind::Private`. Diagnostics, conformance tests, and downstream phases all behave as if Decision #21 did not exist — except that the reserved tokens above produce the early-rejection diagnostic.

### 7.0.1 Safety Pillars **[NORMATIVE]**

Two precise statements about what the v0.1 GA orthogonality engine guarantees, and an explicit list of what it does *not*. Users designing systems in Clifford should keep both lists in mind: the *guarantees* are what they can rely on; the *limits* are what they remain responsible for.

**The engine guarantees:**

1. **Procedural mutation safety.** Two callables that the engine considers concurrent (per §7.3) cannot both perform `#mutate` / `Auto.field <op>= …` / `#> proc()`-routed writes to the same automaton field. The Cl(0,0,n) wedge-product check is the constructive proof per §7.4. This holds *transitively*: writes propagated through proc-call chains (per §6.2's `actual_writes` closure) participate in the check on equal footing with direct writes.

2. **Parallel verification by exhaustive pairwise check.** The §7.4 check is performed for every pair in the §7.3 concurrency matrix. A program that compiles cannot exhibit a write-write race through the structured mutation surface. There is no opt-out; there is no "safety-mode" toggle that disables the check.

**The engine deliberately does not guarantee:**

1. **Safety of mutations through narrow unsafe primitives** (`#unchecked_store`, `#volatile_store`, `#asm`). These are audit-loggable per Decision #17 and intentionally outside the proof boundary. `cliffordc audit --list-unsafe` enumerates every such call site for review; their correctness is the user's responsibility, not the engine's.

2. **Read-write race-freedom at field granularity.** v0.1 catches *write-write* races; one callable writing while another reads is permitted (and frequently expected, per the SPSC ring-buffer pattern documented in book Ch. 39). The graded read/write algebra extension that catches read-write races is reserved for v0.2 (see the §7.2 closing note for the cost / benefit analysis).

3. **Concurrency between automata excluded by `@sequential(A, B)`** (Decision #11). The user's assertion that A and B never run concurrently is *trusted*; the compiler does not verify it. Misuse of `@sequential` (declaring two automata sequential when in practice they may concur) is a user-introduced soundness bug — outside the engine's proof boundary by design, with the trade-off that legitimate sequential-execution programs become checkable.

These pillars carve the precise boundary of v0.1 safety. Subsequent versions tighten the boundary in the directions noted; the pillars themselves do not change.

### 7.1 Basis Vector Assignment

The compiler builds two disjoint basis spaces and concatenates them into a single Geometric Algebra `G(n, 0, 0)` over Euclidean signature.

```
1. Field basis (per compilation unit):
   - Collect all automaton fields across all `#automaton` declarations:
       F = { (A_i, f_j) | A_i is an automaton, f_j is its declared field }
   - Assign basis vectors e_1, e_2, ..., e_|F| in declaration order
     (automaton order × field order within each automaton)
   - Honor `#basis: { f: e_k }` overrides if present; verify they don't
     collide with any other automaton's `#basis` clause.

2. Trait basis (global, Emergent Rule 1):
   - Collect every trait T appearing in any `$ [TraitList]` in the program,
     predeclared (Pure, Readable, Observable, Opaque) or user-defined.
   - Assign each trait a globally-fixed basis vector e_{|F|+1}, e_{|F|+2}, ...
     in canonical order (predeclared traits first by §4.5 table order,
     then user-defined traits in `@trait` declaration order).

3. Total dimension n = |F| + |Traits|.

4. Each blade is represented as a u64 bitmask (1 bit per basis vector).
   — supports up to 64 combined dimensions per compilation unit
   — for n > 64, use a fixed-size bit array (implementation detail).
```

The trait basis is what makes `@fn` purity contracts and `#effect` mutation profiles live in the same algebra: a `$ [Readable]` function carries the `Readable` basis bit; concurrently calling a `$ [Readable]`-only function with an effect that mutates fields is automatically orthogonal because their bitmasks are disjoint.

### 7.2 Behavior Multivector Construction

For each `#effect` (or `#interrupt`) E in automaton A:

```
behavior(E) = ∧ { basis(A, f) | (A, f) ∈ E.actual_writes }
            ∧ { basis(t)      | t ∈ E.trait_list (if any) }
```

That is: the outer product of basis vectors corresponding to fields the effect writes (transitively, through `#>` calls — see §6.2/§6.3) and traits it carries. An effect that mutates `k` fields and carries `m` traits yields a blade of grade `k + m`.

For each `@fn` g (used in concurrency analysis when threading or interrupts call into pure code):

```
behavior(g) = ∧ { basis(t) | t ∈ g.trait_list }
```

For each automaton A:

```
behavior(A) = Σ behavior(E) for E in A.effects
```

This is a multivector — a sum of blades of potentially different grades.

**v0.1 scope: write-write races only.** The behavior multivectors above track *writes* (the field-write component) and *trait categories* (the trait-basis component). They do **not** track *reads* of automaton fields. Consequently, the §7.4 orthogonality check catches **write-write races at field granularity** (two concurrent computations writing the same field — a hard error) but **does not catch read-write races at field granularity** (one computation writing a field while another reads it — silent in v0.1).

For most embedded code this is acceptable because:

1. **Single-field writes are atomic on aligned 32-bit targets.** A read-from-interrupt of a field that main is writing will see either the old or new value, never torn — the value crosses cleanly because the store is one CPU cycle.
2. **Multi-field consistency must use `#atomic: interrupt_critical`.** When a write touches multiple fields and an interrupt could observe the partial state, the writer must declare `#atomic: interrupt_critical` (§6.6), which wraps the body in CLI/STI on Cortex-M (or equivalent on other targets). The resulting mutation is atomic from the interrupt's perspective.
3. **The snapshot-and-decide pattern eliminates the issue inside `@fn`.** When pure code reads from automaton state, it does so by constructing a snapshot value at a single point in the calling effect, then operating on the snapshot. The snapshot is owned, immutable, and not subject to in-place mutation — no read-write race possible.

**v0.2 work:** extend the engine to a *graded read-write algebra* where reads contribute to a separate read-blade and the orthogonality check becomes `(write_A ∧ write_B) ⊕ (read_A ∧ write_B) ⊕ (write_A ∧ read_B)`. Catches read-write races at field granularity at the cost of roughly 2× engine work per pair. Tracked in §12.

### 7.3 Concurrency Inference

The compiler determines which pairs of automata (and which automaton/free-function pairs) can execute concurrently:

```
can_concur(X, Y) =
    X and Y have at least one pair of effects/calls that are reachable
    in overlapping time windows.

Heuristic (sound, conservative):
- Any `#interrupt` automaton can_concur with the automaton running in main
- Any two automata invoked from different threads can_concur
- Any two `#interrupt` automata at different `#priority` levels can_concur
  (higher priority preempts lower)
- Two effects within the same automaton never can_concur
  (automata are inherently sequential within themselves)
- A `@fn` invoked from a #-context can_concur with anything its caller can,
  inheriting the caller's concurrency class.
```

### 7.4 Orthogonality Check

For each pair (X, Y) where can_concur(X, Y):

```
M = behavior(X) ∧ behavior(Y)

case grade(M) of:
  | non-zero, grade(M) = grade(X) + grade(Y) :
      → orthogonal, no synchronization required, OK

  | zero (some basis vector squared) :
      → conflict; identify shared basis vectors and emit E0520

  | grade(M) < grade(X) + grade(Y) but non-zero :
      → partial overlap (some shared, some disjoint)
        emit E0520 listing shared dimensions
```

Bitmask implementation:

```rust
fn outer_product(a: u64, b: u64) -> Option<u64> {
    if a & b != 0 { None }              // shared basis → wedge is zero
    else          { Some(a | b) }       // disjoint → bitwise union
}

fn check_pair(beh_a: &[u64], beh_b: &[u64]) -> Result<(), Conflict> {
    for &blade_a in beh_a {
        for &blade_b in beh_b {
            if outer_product(blade_a, blade_b).is_none() {
                let shared = blade_a & blade_b;
                return Err(Conflict { shared_vectors: bits_to_indices(shared) });
            }
        }
    }
    Ok(())
}
```

This is O(|X.effects| × |Y.effects|) per pair, and O(|automata|²) pairs — feasible for any realistic embedded system.

### 7.5 Error Reporting

When a conflict is detected, the compiler emits error code **`E0520: Orthogonality violation`**. The message must:

1. Name both automata (or automaton + free function)
2. Name the specific effects whose behavior blades conflict
3. Name the specific automaton fields and/or traits shared between them, decoded from the bitmask back to their original source identifiers (Emergent Rule 1; never expose raw `e_5` indices unless `--verbose-basis` is on)
4. Suggest one of: (a) restructure state ownership, (b) add a `#atomic` annotation, (c) declare the automata as non-concurrent via a `@sequential(A, B)` attribute (**[OPEN]** — pending design)

Example:

```
error[E0520]: orthogonality violation between automata `UartRx` and `UartTx`
  --> uart.fe:42
   |
42 | #automaton UartTx {
   | ^^^^^^^^^^^^^^^^^
   |
   = note: shared automaton field `UartRx.rx_head`
   = note: `UartRx::receive` writes {rx_buf, rx_head}  (basis: e1 ∧ e2)
   = note: `UartTx::send`    writes {tx_buf, rx_head}  (basis: e2 ∧ e3)
   = help: either remove `rx_head` from `UartTx::send` (#mutate UartRx { rx_head = ... }),
           or add `#atomic: interrupt_critical` to one of the effects.
```

### 7.6 Optional Explicit Basis Annotation

Per Decision #4, a user may override automatic basis assignment with a `#basis` clause inside `#automaton`:

```
#automaton Motor #basis: { speed: e1, direction: e2 } {
  #states: [Idle, Running]
  speed: f32
  direction: Direction
}

#automaton Sensor #basis: { temp: e3, humidity: e4 } {
  temp: f32
  humidity: f32
}
```

When `#basis` is present, the compiler verifies:

- Every named field exists in the automaton.
- Every field of the automaton is assigned a basis vector (no implicit mixing with auto-assignment within a single `#basis`-annotated automaton).
- Basis indices do not collide with any other `#basis`-annotated automaton in the same compilation unit.
- Trait basis vectors (assigned globally, §7.1 step 2) do not collide with any explicit field basis.

Mismatches emit `E0521: invalid #basis annotation`.

### 7.7 IDE Integration & --verbose-basis

Per Decision #4 rule 4, the compiler exposes its basis-vector decisions to tooling:

- `cliffordc compile --verbose-basis` dumps every field→basis and trait→basis assignment, plus the computed behavior multivector for each effect and automaton, to stderr (or a `.basis.json` sidecar).
- The driver writes a structured assignment table to a build artifact (path TBD) that an IDE/LSP server can read to render basis information on hover (e.g., "field `rx_head` basis: e2", "automaton `UartRx` behavior: e1 ∧ e2 (grade 2, bivector)").
- `E0520` errors include the basis indices alongside the source identifiers in `--verbose-basis` mode and only the source identifiers otherwise.

### 7.8 garust Integration **[OPEN]**

The orthogonality engine's blade representation (bitmask + XOR for products) is structurally identical to garust's representation. **[OPEN]**: whether to vendor garust into the compiler as a Rust dependency for the GA computations, or to implement a minimal in-tree blade arithmetic. Recommended: vendor garust once it stabilizes its API; in the meantime, the in-tree implementation is ~50 lines.

### 7.9 Mixed-metric extension (v0.7+) **[INFORMATIVE — locked, not yet implemented]**

This section sketches the v0.7 extension of the orthogonality engine per `DECISIONS.md` Decision #21 and ADR 0002. It is informative for v0.1–v0.6 conformance — the lexer reserves the relevant tokens (per §7.0), but no implementation work happens until v0.7.0-draft. ADR 0002 §5.5 carries the full algebraic exposition; this section is the spec's normative reservation.

**Algebra.** v0.7 replaces Cl(0,0,n) with Cl(p,0,n). Each basis vector carries a *metric tag*:

- `e_q` for q ∈ {1, …, n} are *null* — `e_q² = 0`. Used for private fields (the v0.1 default).
- `e_~q` for q ∈ {1, …, p} are *non-null* — `e_~q² = +1`. Used for shared fields declared `#shared`.

**Extended orthogonality theorem.** Two automata `A` and `B` are concurrent-safe iff:

1. The null-subspace projection of `behavior(A) ∧ behavior(B)` is non-zero (the existing §7.4 check, applied to the null basis vectors only).
2. AND for every shared basis vector `e_~q` appearing in both behaviors, the lock-coverage proof obligation is discharged (per §5.5 of ADR 0002): the lock `L` such that `field(e_~q) = guarded by L` is held by both A and B at the time they touch `e_~q`.

**Lock as multivector.** Per ADR 0002 §5.5, each lock `L` is a mixed-grade multivector `lock(L) = pri(L) + e_L`, where `pri(L)` is the lock's priority (an integer in the same priority space as `#interrupt #priority:` declarations per §2.5) and `e_L` is the lock's identity basis vector. The lock-context multivector held by an executing automaton is the wedge of every held lock; acquisition is right-wedge; release is the formal inverse.

**Acquisition validity.** Acquiring a new lock at priority `p_new` while holding a context whose maximum priority is `p_max` is valid iff `p_new > p_max`. Algebraically:

- `e_L ∧ e_M = + e_L ∧ e_M`  if `pri(L) < pri(M)` (canonical: ascending)
- `e_L ∧ e_M = − e_M ∧ e_L`  if `pri(L) > pri(M)` (anti-canonical: Koszul-flip)
- `e_L ∧ e_M = ROTOR(L, M)`  if `pri(L) = pri(M)` (rotor tiebreak — see ADR 0002 §5.5.5)

**Rotor tiebreak.** Same-priority locks resolve via a deterministic GA *rotor* parameterised by a canonical structural attribute of each lock — MMIO `#address` for register-block locks; `#rotor: SECTION_OFFSET` clause / link-section position / source-location hash for software locks. The rotor is fixed at compile time; "ring like a roulette without randomness."

**Theorem (priority-monotone deadlock-freedom).** Let `ctx(t)` be the lock-context multivector at time `t` for some hart. Execution is deadlock-free iff `ctx(t) ≠ 0` for all `t`. Wedge-collapse signals a priority inversion or unordered acquisition; the static walk of effect/transition bodies emits `E0521` (or successor code) for any program point where collapse occurs.

**Interrupts and locks unify.** A `#interrupt H #priority: N { … }` is a priority-ordered acquisition under §5.5: when `H` fires, the hart's effective priority rises to `N`; lower-priority interrupts get masked. Same semantics as `#with_lock(L) { … }` where `pri(L) = N`. Under v0.7's algebra, the orthogonality engine handles both with a single mixed-metric pass; §7.3's special-case interrupt-concurrency rules collapse into the one algebra.

**Reentrancy.** Deferred to a future minor decision (likely v0.8). Default non-reentrant: `e_L ∧ e_L = 0` (taking the same lock twice collapses the context). Opt-in reentrant via `#lock #reentrant L` would set `e_L² = pri(L)` (non-null self-square allows the wedge to survive recursive acquisition).

**Implementation reservation.** v0.1–v0.6 implementations:

- MUST treat `AutomatonField::kind` as always `FieldKind::Private`.
- MUST reject `#shared`, `#lock`, `#with_lock`, `#reads`, `#rotor` token forms with a "reserved for v0.7" diagnostic at parse time.
- SHOULD design `crates/ortho` data structures to carry a per-basis-vector metric tag from day one, even if v0.1–v0.6 only uses null tags. This avoids a refactor when v0.7 enables the mixed-metric algebra.

---

## 8. Code Generation **[NORMATIVE]**

### 8.1 Target

LLVM IR. Initial supported triples:
- `thumbv6m-none-eabi` (Cortex-M0/M0+)
- `thumbv7em-none-eabihf` (Cortex-M4F/M7F)
- `riscv32imac-unknown-none-elf`
- `riscv64gc-unknown-none-elf`
- `x86_64-unknown-linux-gnu` (for testing)

### 8.2 ABI

C ABI by default for all `extern` functions and for any function marked `#[no_mangle]`. Internal Clifford functions use a stable Clifford ABI (defined as: same as C ABI for primitives, ADTs lowered to tagged structs).

### 8.3 Lowering Rules

```
Clifford                                       LLVM
──────────────────────────────────────────────────────────────────────────
@fn f(x: T) -> U $ [...]                     define U @clifford_f(T %x)
let x: T = e                                 alloca + store
let x := e                                   alloca + store (type inferred)
@type S = { a: A, b: B }                     %S = type { A, B }
@type E = A | B(T)                           %E = type { i8, [N x i8] }  ; N = sizeof largest variant
&T                                           T*
&mut T (stack-local only, Decision #13 R0)   T*  (with noalias attr)
access<T> / access const<T>                  T*  (Decision #19; nominal identity is Clifford-level only;
                                                  every access type lowers to T* at LLVM IR)
[T; N]                                       [N x T]
[T]                                          { T*, i64 }                 ; fat pointer
trait/interface method dispatch              static call after monomorphization
#automaton field f: T                        element of the automaton's state struct (§8.4)
#automaton field with #offset+#address       fixed-address volatile slot (Decision #6, §8.4)
#mutate A { f = e }                          store e into the f-th element of A's state struct
                                             (volatile if A is a register block)
A.f <op>= e                                  desugars to #mutate A { f <op>= e }
#> name(args)                                static call to lowered effect/transition (§8.4)
sigma i in 0..n { … }                        counted loop with bounds-check elision (§5.8, §8.4)
<Auto>@state                                 load of the state-tag field
<Auto>::Name                                 integer constant (state-tag value)
#unchecked_load<T>(ptr)                      load T, ptr  (no volatile)
#unchecked_store<T>(ptr, v)                  store T v, ptr
#volatile_load<T>(ptr)                       load T, ptr volatile
#volatile_store<T>(ptr, v)                   store T v, ptr volatile
#unchecked_cast<S, T>(v)                     bitcast v to T
#asm("...", inputs, outputs)                 inline asm with target-specific constraints
```

The trait list `$ [...]` is consumed entirely by the type checker and GA engine; it produces no runtime artifact (zero-cost). Likewise `@sequential(A, B)` attributes (Decision #11) influence the GA engine but produce no runtime artifact.

### 8.4 Automaton, Transition, & Effect-Procedure Lowering

Each `#automaton A { ... }` is lowered to:

1. **State struct.** A module-level LLVM struct `%clifford_A_state` whose elements are A's automaton fields in declaration order. For *non-register-block* automata, a single mutable global of this struct type (`@clifford_A` with `internal` linkage) holds the live state. Boot-style automata where all writes are compile-time known may be elided in favor of LLVM constants. **For register-block automata (Decision #6, those declaring `#address: <addr>`), no global is allocated**; the struct is purely a layout description, and field accesses are lowered as volatile loads/stores at `(addr + offset)` (see step 9 below).
2. **State-tag field** (multi-state automata only): if A has more than one element in `#states`, a discriminator field is appended to the state struct, of the smallest unsigned integer that fits all states. **Monoid automata (single-state, no transitions, per Decision #5 Rule 4) emit no state-tag field** — there is nothing to discriminate. Boot-time-only automata where the state is statically pinned may also elide the tag.
3. **One LLVM function per effect**, mangled as `clifford_<automaton>__<effect>` for non-generic effects. `#interrupt` effects use the target's interrupt calling convention and the user-declared linker symbol per Decision #10 (§8.5).
4. **One LLVM function per `(generic_effect, interface_arg)` specialization** (Decision #16): a generic `#effect log_message<S: Serial>(...)` instantiated against `Usart1` is mangled as `clifford_log_message__S_Usart1`. Each specialization is independently optimizable and analyzable by the GA engine.
5. **One LLVM function per `#transition`**, mangled as `clifford_<automaton>__tr_<name>` (Refinement #5b: transitions are named). The function body is the lowered transition body followed by a single store to the state-tag field setting it to the transition's `Target` (Decision #5 Rule 2; §6.4 step 4). For monoid automata with no `#transition` declarations, no transition functions are emitted.
6. **CallContext-driven dispatch (Refinement #5b generalization):**
   - Calls labeled `CallContext::Transition { source, target }` in the typed AST are resolved to the corresponding `clifford_<automaton>__tr_<name>` transition function and lowered as a direct call. The transition function itself emits the state-tag write at its end.
   - Calls labeled `CallContext::Identity` are lowered as direct calls to the effect function with no state-tag manipulation. The state-tag is preserved across identity-context calls.
   - In debug builds, the compiler may emit an optional state-tag precondition assertion at transition function entry (`assert current_tag == Source`); production builds elide it.
7. **Invariant epilogue at every mutation-context exit (§6.5 refinement):** in debug builds, after each `#effect` / `#interrupt` / `#transition` / `#impl` method that wrote any field of an invariant-declaring automaton, assert the `#invariant` expression. Production builds elide invariant checks unless `--verify-invariants=release` is passed.
8. **Atomic wrapper** per §6.6: `interrupt_critical` wraps the body in target cli/sti, `multicore_critical` wraps in CAS or LL/SC.
9. **Register-block field lowering (Decision #6).** For an automaton declaring `#address: 0xADDR`, each field with `#offset: K` and `#access: <r|w|rw>` is lowered as follows:
   - A read of `Auto.field` emits `load <T>, ptr inttoptr (i64 (ADDR + K) to <T>*) !volatile`. Reading a `#access: w` field is `E0612: read of write-only register`.
   - A write `#mutate Auto { field = expr };` emits `store <T> expr, ptr inttoptr (i64 (ADDR + K) to <T>*) !volatile`. Writing a `#access: r` field is `E0613: write to read-only register`.
   - The compiler does not allocate any RAM-resident state for register-block automata.

**Sigma loop lowering (Decision #14).** A `sigma i in 0..n { body }` lowers to a counted loop:

```
  ; entry
  %n = ...                              ; bound expression, captured once
  br label %sigma_header
sigma_header:
  %i = phi i64 [ 0, %entry ], [ %i_next, %sigma_body ]
  %cond = icmp ult i64 %i, %n
  br i1 %cond, label %sigma_body, label %sigma_exit
sigma_body:
  ...                                    ; body, with arr[%i] accesses elided of bounds checks
  %i_next = add nuw i64 %i, 1
  br label %sigma_header
sigma_exit:
```

When the §5.8 bounds tracker proves `i < arr.len()`, the corresponding `getelementptr inbounds` is emitted without a bounds-check branch. Otherwise a runtime bounds check is inserted before the GEP.

**Narrow unsafe primitive lowering (Decision #17).** Each primitive is a one-to-one LLVM operation:

```
#unchecked_load<T>(p)          →  %v = load <T>, ptr %p
#unchecked_store<T>(p, v)      →  store <T> %v, ptr %p
#volatile_load<T>(p)           →  %v = load <T>, ptr %p, !volatile
#volatile_store<T>(p, v)       →  store <T> %v, ptr %p, !volatile
#unchecked_offset<T>(p, n)     →  %q = getelementptr inbounds <T>, ptr %p, i64 %n
#unchecked_cast<S, T>(v)       →  %v2 = bitcast <S> %v to <T>
                                  (or trunc/zext/sext/inttoptr/ptrtoint as needed)
#asm("instr", in, out)         →  call asm "instr", "constraints" (in)
```

In v0.1, the primitives compile directly with no runtime check. In v0.2, an `#audit`-annotated automaton (Decision #18) wraps each primitive in a `PointerAuditor` dispatch — the bare primitive becomes a call through the registered `Sanitizer` automaton, which validates against its shadow-allocation table before performing the operation.

A `#> name(args)` call lowers to a direct LLVM call to either the transition function or the effect function depending on its CallContext; per Emergent Rule 5, all such calls are statically resolvable, so no indirect calls are emitted for `#>`.

### 8.5 Interrupt Handler Emission

```
#interrupt RX_IRQ() #mutates: [UartRx] #priority: HIGH { ... }
```

lowers to a function with:

- Function name: target-specific interrupt vector name (e.g., `UART0_IRQHandler` on STM32). The mapping from `RX_IRQ` to a vector slot is target-config-driven and **[OPEN]**.
- Calling convention: target's interrupt CC (LLVM `arm_aapcs_vfpcc` or similar).
- Section: `.interrupts` or target-specific.
- The vector table is generated from the union of all `#interrupt` effects in the program.

---

## 9. Standard Library **[INFORMATIVE]**

Minimum viable stdlib for v0.1:

```
clifford::core
  ├── option        — Option<T>
  ├── result        — Result<T, E>
  ├── slice         — slice operations
  ├── mem           — size_of, align_of, transmute (unsafe)
  └── ptr           — null, null_mut, read, write (unsafe)

clifford::alloc
  ├── bump          — BumpAlloc (see Appendix A.2)
  └── pool          — PoolAlloc<BLOCK_SIZE, NUM_BLOCKS> (see Appendix A.3)

clifford::sync
  ├── atomic        — atomic_critical effect primitives
  └── mutex         — single-core mutex via interrupt_critical

clifford::hal
  └── (target-specific, vendored per-MCU)
```

---

## 10. Conformance Test Suite Outline **[INFORMATIVE]**

Tests are organized by phase. Each test is a `.cl` source file plus an expected outcome file.

```
tests/
├── lex/             — token stream tests, one file per token category
├── parse/           — AST shape tests, JSON-encoded expected ASTs
├── typecheck/
│   ├── pass/        — files that should type-check
│   └── fail/        — files that should fail with specific error codes
├── effect/          — FSM extraction tests, expected state graphs as DOT
├── ortho/
│   ├── pass/        — orthogonal automata, should compile
│   └── fail/        — non-orthogonal, expected error messages
├── borrow/          — body-scoped reference / provenance tests (Decision #13)
├── sigma/           — sigma-loop bounds-tracking tests (Decision #14)
├── interface/       — #interface + #impl + monomorphization tests (Decision #16)
├── unsafe/          — narrow unsafe primitive tests (Decision #17)
├── codegen/         — IR snapshot tests (LLVM IR golden files)
├── #test/           — programs exercising the #test sigil (Decision #7)
└── runtime/         — actual execution on QEMU for embedded targets
```

Critical test cases (must exist before declaring a phase complete):

- **Lex:** every bare keyword, every sigil-prefixed form (`#automaton`, `#effect`, `@fn`, `#interface`, `#impl`, `#test`, `#unchecked_load`, `#volatile_store`, …), every operator (`:=`, `<op>=`, `@`, `::`, `#>`), every literal form including `b'X'` byte literals, edge cases (unicode in strings, deeply nested block comments). One pass test per token kind in §1.3 / §1.4.
- **Parse:** every grammar production, ambiguity resolution (`<` as generic vs. comparison), sigil-dispatch correctness (an `@fn` containing `#mutate` must produce a parse-time AST that records the layer mismatch for §5.5 to flag); named transitions (`#transition name: A -> B`); register-block automata with `#address`/`#offset`/`#access`; sigma-loop pattern variants; mutation sugar (`Auto.field <op>= expr`); state-read/state-ref expressions (`Auto@state`, `Auto::Name`). Round-trip property: `source → AST → pretty-print → AST` is identity modulo whitespace.
- **Typecheck:** ADT exhaustiveness; generic monomorphization; structural trait satisfaction; mutability outside a mutation context (`E0301`, must fail); `#mutate` on an undeclared automaton (`E0302`, must fail); imperative construct in `@fn` body (`E0101`, must fail); `@fn` with no trait list defaults to `[Pure]` and rejects state writes; explicit `$ [Readable]` rejects writes; `extern "C" @fn` defaults to `[Opaque]`; `let mut x := …` rejected (`E0210`); register-block field with bad `#access` (`E0612`/`E0613`); interface implementation completeness (`E0900`).
- **Borrow / Provenance (Decision #13):** the five UAF cases caught — return reference (`E0702`); reference field (`E0703`); `&mut` to automaton field (`E0700`); reference held across `#mutate` (`E0701`); use after `#free` (`E0704`). Also: single-flow uniqueness violation (`E0705`); local mutation in `@fn` is permitted (Refinement #5c).
- **Effect & FSM:** unreachable-state warning (W6101); mutation outside `#mutates` list (`E0410`, must fail); transitive `#>` propagation of `#mutates`; cycle in effect-procedure call graph (`E0422`, must fail); identity-context vs transition-context classification correctness; **invariant runs after every mutation-context exit, not just after transitions** (§6.5 refinement).
- **Sigma (Decision #14):** bounds-check elimination on direct accesses; runtime check fallback on widened indices; bound-mutation inside loop body rejected (`E0802`); empty range; reverse range produces empty iteration; nested sigma loops.
- **Ortho:** the four examples from Appendix A (UART, Bump, Pool, composition); auto-assigned basis matches manually-computed basis for at least one canonical example; `#basis` override is honored and conflicts are caught (`E0521`); `E0520` message names original field/trait identifiers, not raw `e_n` indices, when `--verbose-basis` is off; trait-disjoint concurrent computations (one `$ [Readable]`, one `#mutate`-only) are accepted; `@sequential(A, B)` suppresses pairwise concurrency check (Decision #11); register blocks participate in GA orthogonality with field-level basis vectors (Decision #6); read-write race honesty: write-write races caught, read-write races not caught in v0.1 (§7.2).
- **Interface (Decision #16):** `#interface` + `#impl` declaration; coherence check rejects duplicate impls (`E0901`); generic effect with interface bound monomorphizes correctly; specialization-per-implementor produces distinct mangled symbols; orphan rule violations rejected (`E0902`).
- **Unsafe (Decision #17):** each narrow primitive lexes/parses individually; `#unsafe { … }` block syntax is rejected (no production for it); `#unchecked_*` and `#volatile_*` lower to the correct LLVM operations; `#access`-violating access through register fields is caught at compile time, not at the unsafe-primitive layer; tooling test that `cliffordc audit --list-unsafe` finds every primitive occurrence.
- **Access types (Decision #19):** two `@type` declarations producing distinct `access<T>` nominal types must not interchange without `#unchecked_cast` (`E0710`); `access const<T>` cannot be used where `access<T>` is expected (`E0712`); `access<T>` IS accepted where `access const<T>` is expected (covariance); `null` literal context-resolves to the appropriate access type; pointer arithmetic via `#unchecked_offset` produces the right LLVM `getelementptr inbounds` IR; `*const T`/`*mut T` syntax is rejected (no production for it).
- **#test (Decision #7):** test isolation (automaton state resets between tests); mixed-layer access (test body calls both `@fn` and `#effect`); `assert(expr)` and `panic(msg)` available; tests elided from non-test compilation modes.
- **Codegen:** ABI compatibility with hand-written C; interrupt vector linker symbols (Decision #10); `#> name()` lowers to a direct call (no indirect dispatch); trait list `$ [...]` and `@sequential` produce no runtime artifact (zero-cost); register-block field reads/writes lower to volatile loads/stores at `address + offset` (Decision #6); sigma loops compile to counted loops with bounds-check elimination where provable.
- **Runtime:** blink LED on QEMU Cortex-M3 (firmware proving ground); UART echo with FSM and ISR on QEMU stm32-virt; **a non-firmware example** (small CLI tool or numerical kernel) demonstrating the language is not embedded-only.

---

## 11. Reference Implementation Roadmap **[INFORMATIVE]**

### Phase 0 — Bootstrap (target: 4 weeks)
- Lexer, parser → AST
- Pretty-printer (AST → source) for debugging
- Implementation language: Rust
- Build: cargo workspace `cliffordc/` with crates `lexer`, `parser`, `ast`

### Phase 1 — Type System (target: 6 weeks)
- HM inference engine
- Structural trait resolution
- Mutability check
- Crates: `types`, `resolve`, `check`

### Phase 2 — Effect & FSM (target: 4 weeks)
- Mutation profile extraction
- State graph builder
- Reachability + deadlock heuristic
- Crate: `effect`

### Phase 3 — GA Engine (target: 2 weeks)
- Blade arithmetic (bitmask + XOR), or vendor garust
- Behavior multivector construction
- Concurrency inference
- Pairwise orthogonality check
- Crate: `ortho`

### Phase 4 — Codegen (target: 6 weeks)
- LLVM IR emission via `inkwell` or `llvm-sys`
- ABI lowering
- Interrupt vector table generation
- Crate: `codegen`

### Phase 5 — Stdlib + Tooling (target: 4 weeks)
- `clifford::core`, `clifford::alloc`, `clifford::sync`
- `cliffordc` CLI driver
- Cargo-style project layout
- Crates: `stdlib/`, `cli`

**Total estimate: ~26 weeks for a working v0.1 capable of compiling the Appendix A examples to a Cortex-M target.**

---

## 12. Open Questions

**Resolved by `DECISIONS.md` v0.4 (closed in this revision):**

- ~~§4.5: explicit `impl Trait for Type` blocks?~~ — Resolved by Decision #2 (hybrid trait scheme via `$ [TraitList]` markers).
- ~~§2.5 / §6.1 / §8.4: effect/state/transition coupling.~~ — Resolved by Decision #5 (automaton-as-category + named transitions).
- ~~`#hardware` capabilities; `static mut` survivability~~ — Resolved by Decision #6 (register blocks are automata with `#address`/`#offset`/`#access`; no separate `#hardware` construct).
- ~~Testing across the `@`/`#` boundary~~ — Resolved by Decision #7 (`#test "name" { … }`).
- ~~`:=` short-binding~~ — Resolved by Decision #8 (accepted, immutable only).
- ~~`#visible`/`#hidden`~~ — Resolved by Decision #9 (dropped; use `#mutates`/`#cannot_mutate`).
- ~~Interrupt vector mapping~~ — Resolved by Decision #10 (linker-symbol naming; user-supplied target-standard names).
- ~~`@sequential(A, B)` attribute~~ — Resolved by Decision #11.
- ~~Memory-safety story (intra-body UAF)~~ — Resolved by Decision #13 (body-scoped references with provenance + Rule 0).
- ~~Iteration construct~~ — Resolved by Decision #14 (sigma loop with bounds tracking).
- ~~Mutation-surface verbosity~~ — Resolved by Decision #15 (single-field sugar `Auto.field <op>= expr`).
- ~~Polymorphism over effects / HAL story~~ — Resolved by Decision #16 (`#interface` + `#impl` + monomorphization).
- ~~Unsafe-code surface area~~ — Resolved by Decision #17 (Ada-style narrow primitives; no aggregating `#unsafe { … }` block).
- ~~Pointer type identity~~ — Resolved by Decision #19 (nominal `access<T>` / `access const<T>` types replace raw `*const T`/`*mut T`; cross-type use requires explicit `#unchecked_cast`; pointer arithmetic via `#unchecked_offset`).
- ~~File extension~~ — Resolved: `.cl`.
- ~~Identity-context invariant gap~~ — Resolved by §6.5 refinement (invariants run after every mutation-context exit, not just transitions).
- ~~Downward inlining permission~~ — Clarified in §4.7: `@fn` → `#effect` inlining is permitted and is the standard optimization path.

**Still open in v0.1 (clarified design questions, not blockers):**

- **§6.5 invariant-checking strategy:** runtime checks in debug builds is the v0.1 default; static SMT discharge via Z3 (`--verify-invariants=static`) is opt-in experimental.
- **§7.8 garust integration:** vendor garust or keep in-tree implementation. v0.1 ships in-tree (~50 lines); revisit if garust API stabilizes.
- **§7 read-write race detection:** v0.1 catches write-write races at field granularity. Read-write races are addressable via `#atomic: interrupt_critical` or the snapshot-and-decide pattern. v0.2 may extend the engine to a graded read/write algebra.
- **Module system:** `use` declaration syntax exists in §2.1 but module resolution semantics are deferred. Interaction with `@module` declarations to be specified during Phase 5 stdlib work.

**Deferred to v0.2:**

- **Decision #12 — `#staged` automata for deferred mutation.** Designed in `DECISIONS.md`; adds `#staged` modifier and `#flush` statement; provides a first-class ISR-to-main-handoff primitive. Will be re-opened informed by v0.1 reference firmware.
- **Decision #18 — `#audit` runtime auditing of unsafe primitives.** Designed in `DECISIONS.md`; `PointerAuditor` interface (Decision #16-shaped) with default `ShadowSanitizer` automaton; debug-build pointer tracking layered on Decision #17's static visibility.
- **Hierarchical states (functorial sub-automata).** Foundation laid by Decision #5 / Appendix B (a hierarchical state is a functor `F_S : C_{A_S} → C_A`); full design deferred. Necessary for protocol stacks (USB, BLE) with combinatorially many states.
- **Linear types (`linear` keyword).** Beyond Decision #13 Rule 5's allocator-product linearity; e.g., file-handle-must-be-closed, capability-must-be-yielded patterns.
- **Refinement types beyond sigma-loop bounds tracking.** Predicate-bearing types like `{x: i32 | x > 0}` for richer static checking. Would require SMT discharge; aligns with the `--verify-invariants=static` work.
- **Dependent types (buffer-size-in-type).** Express `[T; N]` length as a value-level expression; v0.2 if there's user demand.
- **Macros / proc-macros.** No macros in v0.1; reconsider for v0.2 based on stdlib pain points.
- **`@phase(name)` sugar over `@sequential`.** Tag automata with phase names; the compiler infers `@sequential` between any two phases never simultaneously active. Cleaner than pairwise `@sequential`.
- **Lifetime annotations as opt-in.** If non-firmware ambitions reveal that Decision #13's "no `&T` returns" rule is too restrictive for some library patterns, v0.2 may add an opt-in lifetime-annotation form. Additive, does not break Decision #13 defaults.
- **`#valid_in: [State, ...]` clause on effects.** Statically reject identity-context calls of effects that are only meaningful in certain states. Deferred in favor of v0.1 ergonomics; users do explicit runtime checks today.
- **`dyn Interface` runtime dispatch (Decision #16 extension).** Currently v0.1 is monomorphization-only. v0.2 may add an opt-in trait-object form for cases that need runtime dispatch.
- **Tightened `#unsafe` operation set (analyst-flagged).** Decision #17's narrow primitives are the catalog; v0.2 may further restrict by introducing typed-pointer wrappers that constrain pointer arithmetic.
- **Closures and capture rules.** No closures in v0.1. v0.2 must specify that closures inherit their containing function's sigil layer (a closure inside `@fn` can only capture `@`-layer things).

**Out of scope:**

- **Async/await.** Use automaton-based event loops instead. The `#interrupt` mechanism plus `@sequential` covers the realistic embedded async patterns; Phase 5 stdlib will provide a `#staged`-based futures-equivalent in v0.2 if needed.
- **Garbage collection.** Not now, not ever. The whole point.

---

## 13. Glossary

| Term | Definition |
|------|------------|
| **Sigil** | A leading symbol (`#`, `@`, `$`) that classifies an item, statement, or marker into a syntactic layer (Decision #1). |
| **Functional layer** | The set of `@`-prefixed constructs (`@fn`, `@type`, `@trait`, `@module`, `@sequential`); pure by default, may not contain imperative constructs. |
| **Imperative layer** | The set of `#`-prefixed constructs (`#automaton`, `#effect`, `#interrupt`, `#interface`, `#impl`, `#test`, `#mutate`, `#transition`, `#> proc()`, narrow unsafe primitives); the only place automaton-field writes are permitted. |
| **Automaton** | A declared state machine that owns a set of mutable fields; the unit of state ownership in the GA engine. |
| **Automaton field** | A typed, named slot inside an `#automaton`; each gets a basis vector in the GA orthogonality engine (Decision #4). |
| **Register-block automaton** | An `#automaton` declaring `#address: <addr>`; its fields are memory-mapped peripheral registers with `#offset` and `#access` annotations. Reads/writes lower to volatile loads/stores at `addr + offset` (Decision #6). No RAM-resident state struct is allocated. |
| **Effect procedure** | A named `#effect` declaration callable via `#> name(args)` from another effect-context (Decision #3). |
| **Named transition** | A `#transition <name>: Source -> Target { body }` declaration. The name disambiguates multiple transitions between the same state pair and is the call target for `#> name(args)` (Refinement #5b). |
| **Trait list** | The `$ [Trait, Trait, ...]` marker on an `@fn` declaring its purity / read / observation obligations (Decision #2). |
| **Trait basis vector** | A globally-fixed basis vector assigned by the compiler to each trait, ensuring orthogonality checks across the program are consistent (Emergent Rule 1). |
| **Behavior multivector** | The sum, over an effect's (or automaton's) component blades, of the wedges of basis vectors corresponding to fields it writes and traits it carries. |
| **Blade** | A k-vector formed by the outer product of k linearly independent 1-vectors. |
| **Effect** | A `#effect` or `#interrupt` declaration with a declared `#mutates` set, optional invariant, and a body. |
| **Grade** | The number of basis vectors in a blade; for a multivector, the highest grade of its constituent blades. |
| **Orthogonality** | Two computations are orthogonal when the wedge of their behavior multivectors is non-zero with grade equal to the sum of their individual grades — i.e., they share no field or trait basis vectors. |
| **Mutation context** | A code region where mutation of automaton fields is permitted: `#effect` / `#interrupt` / `#transition` / `#impl` method bodies, and `static` initializers (Decision #17 removed `#unsafe` blocks from this list). Local stack mutation is permitted in any function body, including `@fn`. |
| **Category `C_A`** | The small category associated with automaton `A` per Decision #5: objects are `A`'s states, morphisms are its `#transition` declarations plus implicit identities. The categorical structure is internal — users see FSM language. |
| **Monoid automaton** | An automaton with `#states` omitted (or `#states: [Ready]` with no transitions); its category `C_A` has one object and only the identity morphism. The canonical form for allocators, loggers, register blocks, and other "stateless mutation bags." |
| **Identity-context call** | A `#> name(args)` invocation whose resolved callee is an `#effect` (not a `#transition`); fires during the implicit identity morphism on the current state and does not change the state-tag (Refinement #5b). |
| **Transition-context call** | A `#> name(args)` invocation whose resolved callee is a `#transition`; on body completion the automaton's state-tag is written to the transition's `Target` (Refinement #5b). |
| **Body-scoped reference** | A `&T` or `&mut T` reference whose lifetime is bounded by the enclosing function body (Decision #13). References cannot appear in return types or struct/automaton fields; intra-body uniqueness for `&mut T`; `&Auto.field` references invalidated by subsequent `#mutate Auto { field = … }`. |
| **Provenance** | The chain of derivation from a memory source (automaton field, stack local, allocator product) to a reference. The compiler tracks provenance per-body to invalidate references when their source mutates (§5.7). |
| **Sigma loop** | A bounded iteration construct (Decision #14): `sigma <pat> in <source> { body }`. The iteration variable carries an implicit refinement bound; direct array accesses are statically bounds-checked when provable (§5.8). |
| **Bounded integer** | An integer type with a static upper-bound refinement, e.g. `bounded<0, n>`. Sigma loops bind their iteration variable as a bounded integer; arithmetic widens it back to plain `usize` unless trivially provable. |
| **Interface** | A `#interface Name { effect sig; … }` declaration (Decision #16) listing effect signatures; one or more automata implement it via `#impl Interface for Automaton { … }` blocks. |
| **Implementation block** | A `#impl Interface for Automaton { … }` declaration providing bodies for an interface's effects on a specific automaton (Decision #16). |
| **Generic effect** | An effect declared with type parameters bound by interfaces, e.g. `#effect log<S: Serial>(...)`. Each call site monomorphizes against a concrete implementation. |
| **Narrow unsafe primitive** | An ordinary expression or statement that performs an unsafe operation (raw load/store, volatile load/store, bit-cast, pointer offset, inline asm), each its own sigil-prefixed form (Decision #17 + Decision #19). Replaces the Rust-style aggregating `unsafe { … }` block with per-occurrence visibility. |
| **Access type** | A nominal pointer type produced by the `access<T>` (or `access const<T>`) constructor (Decision #19). Each `@type` declaration of an access type produces a distinct nominal identity even when underlying representations are congruent. The narrow primitives operate on access types; cross-access-type use requires `#unchecked_cast`. |
| **Nominal type identity** | A type-system property where two types with identical structure but distinct declarations are considered different types. Used by access types (Decision #19) to catch peripheral confusion bugs (`UartPtr` ≠ `SpiPtr` even though both lower to `T*`). |
| **`@sequential` attribute** | A top-level attribute `@sequential(A, B);` asserting that automata `A` and `B` never run concurrently; consumed by the GA engine (§7.3) to suppress orthogonality checks between the named pair (Decision #11). |
| **`#test` block** | A top-level item declaring a unit test with mixed-layer access (Decision #7). Each test runs in isolation; automata reset to initial state before each invocation. |

---

## Appendix A: Reserved for Worked Examples **[INFORMATIVE]**

(UART, BumpAlloc, PoolAlloc, composition examples — to be migrated from `clifford_spec_draft0.docx` once the v0.1 grammar is settled. Conformance tests in §10 reference these by name.)

---

## Appendix B: Categorical Semantics **[INFORMATIVE, NORMATIVE FOUNDATION]**

This appendix states the formal semantics of automata, transitions, and the GA orthogonality engine in categorical terms. It is informative for users, but it is the *normative foundation* on which §6 and §7 stand: when the user-facing prose is ambiguous, this appendix decides the question.

### B.1 The Category of an Automaton

For each `#automaton A` declared in a Clifford program, the compiler associates a small category `C_A`:

- **Objects** `Ob(C_A)` are the identifiers in `A.#states`. If `#states` is omitted, `Ob(C_A) = { Ready }` (the singleton).
- **Morphisms** `Hom(C_A)` consist of:
  - One non-identity morphism `f_T : Source → Target` for each `#transition Source -> Target { … }` declaration in `A`.
  - The identity morphism `id_S : S → S` for every object `S ∈ Ob(C_A)` (always present by definition of category).
- **Composition** `g ∘ f` of compatible morphisms is sequential application: if `f : X → Y` and `g : Y → Z`, then `g ∘ f : X → Z` is the morphism realized by executing `f`'s body, then `g`'s body, with the state-tag passing through `Y` between them.
- **Initial object** `i_A` is the first state in declaration order, or one explicitly marked `@initial`.
- **Terminal objects** are those marked `@terminal`; they are categorical terminal objects only locally (they admit only the identity morphism after reaching them).

A *monoid automaton* is one where `Ob(C_A)` is a singleton; `C_A` is then a one-object category — equivalently, a monoid acting on the field state.

### B.2 Behavior Multivectors and the Mutation Functor

The compiler's GA basis (§7.1) defines a graded algebra `G(n, 0, 0)`. Each morphism `f` in `C_A` carries a *blade* `β(f) ∈ G` — the wedge of basis vectors corresponding to fields `f` writes and traits `f` carries (§7.2). This assignment extends to a functor:

```
β : C_A → (G, ∧)   sending objects to the scalar 1, identities to 1, and
                   transition morphisms to their behavior blades.
```

(Strictly, the codomain is the graded monoid of `G` under wedge — composition of morphisms maps to wedge of blades, which is associative and partial.)

### B.3 The Product Category and Orthogonality (Emergent Rule 6)

For any two automata `A, B` with `can_concur(A, B)` (§7.3), the parallel composition is modeled as the product category:

```
C_{A‖B} := C_A × C_B
   Ob(C_{A‖B}) = Ob(C_A) × Ob(C_B)
   Hom_{C_{A‖B}}((s_A, s_B), (t_A, t_B)) consists of pairs (f, g) where
     f : s_A → t_A in C_A and g : s_B → t_B in C_B, *provided*
     β(f) ∧ β(g) ≠ 0 of grade |β(f)| + |β(g)|.
```

The proviso is the well-formedness condition: pairs `(f, g)` whose blades share a basis vector cannot be parallel-composed because the underlying mutations would race.

**Theorem (GA Orthogonality = Product-Category Existence).**
*For automata `A` and `B` with `can_concur(A, B)`, the product category `C_A × C_B` is well-defined as a small category if and only if for every pair of effects `e_A ∈ A.effects` and `e_B ∈ B.effects`, `behavior(e_A) ∧ behavior(e_B) ≠ 0` is of grade `|behavior(e_A)| + |behavior(e_B)|`.*

**Proof (sketch).**
*(⇒)* If the product is well-defined, every pair `(f_A, f_B)` of parallel morphisms satisfies the proviso, so their blades share no basis vector. In the GA wedge product this is precisely `β(f_A) ∧ β(f_B) ≠ 0` of full grade. Generalizing over effects (as sums of transition blades plus identity), the result holds.
*(⇐)* If every pair of effects wedges to a full-grade blade, then for any `(f_A, f_B)` morphism pair their blades are disjoint, so the pair `(f_A, f_B)` is admissible. The set of admissible pairs is closed under composition (wedge of disjoint blades remains disjoint with any new disjoint blade in the third factor) and contains all identities (whose blade is the scalar 1). Hence the product category is well-defined. ∎

This theorem is what the bitmask check in §7.4 implements. The implementation is a constructive existence proof: each `outer_product(a, b)` call either certifies a single morphism-pair as admissible (returning `Some(a | b)`) or produces a counter-example (returning `None`, with the shared basis vectors recoverable from `a & b`).

### B.4 Identity-Context vs. Transition-Context, Categorically

Decision #5 Rule 3 distinguishes call contexts:

- A **transition-context** call corresponds to a non-identity morphism in `C_A`. Its execution moves the state-tag along `Source → Target`.
- An **identity-context** call corresponds to a *factorization* of the identity morphism `id_S` through a mutation that does not change the object: the morphism is `id_S` (the state-tag does not change), but the underlying field-state is updated. Categorically: `id_S` admits "decoration" by a non-trivial blade `β` (the mutation profile of the called effect) without changing the source/target object.

Self-loops `#transition S -> S { … }` are explicit non-identity endomorphisms on `S` — formally distinct from `id_S`-decorated identity-context calls, even when their net effect on the field state is the same. The compiler treats them as distinct morphisms in `C_A` for FSM-graph purposes (they appear as edges in the rendered state graph).

### B.5 Hierarchical States as Functors (v0.2 Foundation)

A hierarchical state `S` whose internal behavior is a sub-automaton `A_S` is modeled as a functor:

```
F_S : C_{A_S} → C_A
```

mapping every object of `C_{A_S}` to `S` (the parent state) and every morphism of `C_{A_S}` to `id_S` decorated with the appropriate sub-blade. Entry into `S` from outside `A` invokes `F_S(i_{A_S})` (the initial object of the substate); exit from `S` (a non-identity morphism in `C_A`) is invoked from any sub-automaton state per the parent's transition rules.

Full design of hierarchical states is **[OPEN]** for v0.2; this appendix exists in v0.1 to ensure the foundation is sufficient.

### B.6 What This Appendix Is Not

This appendix is *not* a full denotational semantics for Clifford. It does not:

- Model the full `@fn` layer (functions are just morphisms in a different ambient category and are not analyzed here).
- Model invariants or pre/post-conditions (those are §6.5 territory and **[OPEN]**).
- Describe error semantics, panics, or undefined behavior arising from narrow unsafe primitives (`#unchecked_*`/`#volatile_*`/`#asm`).

Its scope is exactly: the categorical foundation of automata, transitions, and the GA orthogonality engine, sufficient to prove the engine's claim and to extend cleanly to v0.2 hierarchical states.

---

*End of specification v0.5.0-draft.*
