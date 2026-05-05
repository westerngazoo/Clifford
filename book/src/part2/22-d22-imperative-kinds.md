# Chapter 22: Decision #22 — Kinds of imperative

> **Status:** Locked 2026-05-03; parser/AST landed in this chapter's
> implementation PR (`feat/decision-22-effect-trait-list`, 2026-05-05).
> Downstream consumers (codegen memory ordering, `cliffordc audit
> --traits`, certification) come online in v0.2.

## 22.1 What you'll learn

By the end of this chapter you'll be able to:

1. Add an imperative trait list to an `#effect`, `#interrupt`, or
   `#transition` and explain what each predeclared trait means.
2. Predict which traits affect generated code (memory-ordering
   markers) versus which are purely declarative (audit, certification).
3. Explain why the orthogonality engine *deliberately* ignores trait
   lists, and why that design is right.
4. Implement the parser and AST extension yourself from this chapter
   (~30 LoC across two files), and write your own consumer tool that
   reads trait lists from the AST.

## 22.2 The one-line summary

Decision #2 (hybrid trait system) gave `@fn` a `$ [TraitList]` clause
that classifies the function's *purity* — `[Pure]`, `[Readable]`,
`[Observable]`, `[Opaque]`. Decision #22 extends the same syntactic
mechanism to `#effect`, `#interrupt`, and `#transition` declarations,
but switches the semantic interpretation from *purity* to **kind** —
what kind of imperative work the callable does.

```clifford
#interrupt USART1_IRQHandler() #mutates: [Uart] #priority: HIGH
                                $ [Hardware, Realtime, Acquire] {
  // ... handler body ...
}
```

Same `$ [...]` token sequence; same `TraitRef` AST node; same parse
rule. The change is *which traits are predeclared* and *which tools
consume the list*.

## 22.3 The predeclared imperative traits

Eight traits ship as predeclared in v0.2:

| Trait               | Meaning                                                                                                       | Consumed by                                          |
|---------------------|---------------------------------------------------------------------------------------------------------------|------------------------------------------------------|
| `Hardware`          | Mutates memory-mapped registers (typically `#mutates` a register-block automaton per Decision #6)             | `cliffordc audit`; certification (DO-178C, IEC 61508)|
| `Realtime`          | Has a stated worst-case execution-time bound; permitted in real-time scheduling decisions                     | `cliffordc audit --realtime`; WCET tooling           |
| `Acquire`           | Carries acquire memory ordering (`Ordering::Acquire` semantics)                                               | **Codegen** — emits an acquire fence                 |
| `Release`           | Carries release memory ordering                                                                               | **Codegen** — emits a release fence                  |
| `SeqCst`            | Carries sequential consistency (the strongest ordering)                                                       | **Codegen** — emits a SeqCst fence                   |
| `LockingDiscipline` | Manipulates a `#shared` field's lock per Decision #21 (v0.7+)                                                 | `cliffordc audit`; v0.7+ deadlock analysis           |
| `PureState`         | Mutates only its own automaton's private state (no externally-visible side effects on other automata)         | `cliffordc audit`; certification                     |
| `Encapsulated`      | Mutates only `#hidden`-marked fields per Decision #25 (effectively no externally-visible state mutation)      | `cliffordc audit`; certification                     |

Three of those (`Acquire`, `Release`, `SeqCst`) **affect generated
code**. The others are *declarative* — they don't change codegen, but
they show up in audit reports, in `cliffordc audit --traits` output,
and in certification artefacts that auditors review.

User-defined traits are syntactically permitted (the parser accepts
any identifier list — same as on `@fn`). Whether a downstream tool
recognises a non-predeclared trait is up to the tool.

## 22.4 The orthogonality engine deliberately ignores `trait_list`

This is the design decision that keeps the GA core minimal.

The orthogonality engine (§7) is concerned with *write-write race
detection*: which `(automaton, field)` pairs do which callables
write, and do any concurrent pair's behaviour multivectors collapse
under the wedge product? That question is decided entirely by the
`actual_writes` set — extracted in §6.2, fed into the bitmask check
in §7.4. **No trait can change which fields a callable writes**, so
no trait can change the engine's verdict.

If we wired `Acquire` / `Release` into the engine, we'd be encoding
*memory-ordering policy* into the *race-detection* algebra. Those are
two separate concerns:
- **Race detection** (§7) is about *whether* two writes can conflict.
- **Memory ordering** (codegen) is about *how those writes become
  visible to other cores* once they happen.

A Clifford program that races (engine rejects) cannot be saved by
adding `Acquire`. A Clifford program that doesn't race (engine
accepts) doesn't need `Acquire` for correctness *of the race
discipline*; ordering matters for inter-core visibility, which is a
downstream codegen concern. Mixing the two would conflate them; the
spec keeps them separate.

So: **the engine never reads `trait_list`.** Codegen reads
`Acquire` / `Release` / `SeqCst` to emit appropriate fences. Audit
tools read everything else. The split is clean.

## 22.5 Surface syntax

The trait list goes between the metadata clauses and the body block —
the same structural slot as `$ [TraitList]` on `@fn` (after the
return type, before the body):

```clifford
// #effect: trait list AFTER #mutates / #cannot_mutate, BEFORE body.
#effect tx_byte() #mutates: [Uart] $ [Hardware, Release] {
  Uart.tx_buffer[Uart.tx_head] = byte;
  Uart.tx_head = Uart.tx_head + 1usize;
}

// #interrupt: trait list AFTER #mutates / #priority, BEFORE body.
#interrupt SysTick() #mutates: [Sched] #priority: HIGH
                     $ [Realtime, Hardware] {
  // ...
}

// #transition: trait list AFTER (-> Dest)?, BEFORE body.
#automaton Counter {
  value: u32;
  #transition tick $ [PureState] {
    Counter.value = Counter.value + 1u32;
  }
}
```

Grammar (extension to §2.5):

```
effect_decl     := effect_kind ident generic_params?
                   '(' params? ')' return_type?
                   effect_meta+
                   trait_list?         // ← Decision #22
                   block

transition_decl := '#transition' ident ('->' ident)?
                   trait_list?         // ← Decision #22
                   block

trait_list      := '$' '[' trait_ref (',' trait_ref)* ']'
```

The clause is **optional everywhere** (empty trait list ≡ omitted
clause). No diagnostic fires for missing trait lists — they're
informative, not required. Users start with no lists, add them when
audit reports demand them or when they want explicit memory-ordering
control.

## 22.6 Worked example: a UART RX driver with full classification

```clifford
#automaton Uart {
  rx_buffer: [u8; 64];
  rx_head:   usize;
  rx_tail:   usize;

  // Hidden internal counter — Decision #25.
  parity_errors: u32 #hidden;

  // Owning transition: mutates only Self → PureState.
  #transition rx_byte $ [PureState] {
    let next_head: usize = (Uart.rx_head + 1usize) % 64usize;
    if next_head != Uart.rx_tail {
      Uart.rx_buffer[Uart.rx_head] = #volatile_load<u8>(uart_rbr_register);
      Uart.rx_head = next_head;
    }
  }

  // Hidden-field-only transition → both PureState and Encapsulated.
  #transition record_parity_error $ [PureState, Encapsulated] {
    Uart.parity_errors = Uart.parity_errors + 1u32;
  }
}

// Hardware-touching, real-time, acquire-ordered.
#interrupt USART1_IRQHandler() #mutates: [Uart] #priority: HIGH
                                $ [Hardware, Realtime, Acquire] {
  let status: u32 = #volatile_load<u32>(uart_iir_register);
  if (status & 0x10u32) == 0x10u32 {
    #> rx_byte();
  }
  if (status & 0x80u32) == 0x80u32 {
    #> record_parity_error();
  }
}

// Pure software effect with no externally-visible side effects.
@fn buffer_used() -> u32 $ [Readable] {
  return @snapshot Uart.rx_head - @snapshot Uart.rx_tail;
}
```

A skim through `cliffordc audit --traits` on this program tells the
auditor:

- **`USART1_IRQHandler`** — `Hardware` (touches MMIO via volatile),
  `Realtime` (caller has a deadline budget), `Acquire` (codegen
  emits an acquire fence so reads after the handler see hardware
  state in a well-defined order).
- **`rx_byte`** — `PureState` (only mutates `Uart`, no cross-automaton
  effects).
- **`record_parity_error`** — `PureState` + `Encapsulated` (only
  mutates `Uart`'s `#hidden parity_errors` field; never observable
  outside the automaton).

The certification artefact lists each callable with its trait set;
the auditor signs off on the design without reading every line of
implementation code. **That's the value.** Trait lists make the
*contract* of each imperative callable visible at a glance.

## 22.7 Implementing Decision #22 from this chapter

The implementation is small — about 30 LoC of parser changes plus
three AST field additions. Most of the work was already done by
Decision #2 (the `parse_trait_list` helper).

### 22.7.1 AST

Add a `trait_list: Vec<TraitRef>` field to three structs:

```rust
// crates/ast/src/lib.rs

pub struct EffectDecl {
    // ... existing ...
    pub mutates: Vec<String>,
    pub cannot_mutate: Vec<String>,
    pub trait_list: Vec<TraitRef>,   // ← Decision #22
    pub body: Block,
    pub span: Span,
}

pub struct InterruptDecl {
    // ... existing ...
    pub mutates: Vec<String>,
    pub priority: PriorityLevel,
    pub trait_list: Vec<TraitRef>,   // ← Decision #22
    pub body: Block,
    pub span: Span,
}

pub struct TransitionDecl {
    pub name: String,
    pub destination: Option<String>,
    pub trait_list: Vec<TraitRef>,   // ← Decision #22
    pub body: Block,
    pub span: Span,
}
```

The struct is `#[derive(Debug, Clone, PartialEq, Eq)]`, so adding
the field is a non-breaking change for the AST consumers that use
`{ ..decl }` patterns. Direct constructors and field-by-field
destructures need updates — but those mostly live in tests, where
adding `trait_list: Vec::new()` is mechanical.

### 22.7.2 Parser

The trait-list parse helper (`parse_trait_list`) was already factored
out by Decision #2. Three identical insertions — one per declaration
parser — each reads "if `$` peek, parse the list; otherwise empty":

```rust
// crates/parser/src/lib.rs

let trait_list = if matches!(self.peek().kind, TokenKind::Dollar) {
    let (list, _) = self.parse_trait_list()?;
    list
} else {
    Vec::new()
};
```

Insert this immediately after the metadata clauses (or destination,
for `#transition`) and immediately before `parse_block()`. That's
the entire parser change.

### 22.7.3 Validation lives downstream

The parser does **no** validation of trait names. `$ [Hardware]`,
`$ [Realtime]`, `$ [SomeUserTrait]`, `$ [TypoTriat]` all parse
identically — each becomes one `TraitRef` in the list.

Validation of "is this a recognised trait?" lives in `clifford-types`
(once it exists for imperative-side traits — currently slice T4+
work). Codegen consumers (`Acquire`/`Release`/`SeqCst` fences) check
their *own* trait names; they ignore unrecognised ones with at most
a warning. Audit tools may report unrecognised traits or pass them
through. The contract is: **the parser stores; downstream tools
interpret.**

This matches the `@fn` trait-list policy and keeps the parser
forward-compatible. Adding new predeclared traits in v0.4+ doesn't
require a parser change.

### 22.7.4 Tests

The parser test surface is small but should cover every shape:

- `#effect` with one trait, multiple traits, no clause (empty list);
- `#effect` with `#cannot_mutate` then `$ [...]` (clause ordering);
- `#interrupt` with and without trait list;
- `#transition` with and without destination, with and without trait
  list;
- generic trait names parse (`$ [LockingDiscipline<RwLock>]`);
- non-predeclared identifiers pass through verbatim.

The Decision #22 implementation PR adds 11 such tests (parser test
count: 215, was 204). Each is ~10 lines of Rust.

## 22.8 What this enables — and what it doesn't

**Enables:**

- **Memory-ordering control without inline assembly.** A Cortex-M
  developer marks an interrupt handler `$ [Acquire]` and the
  generated code emits the appropriate `dmb ish`-style fence
  automatically. No need for `#asm("dmb ish")` in user code.
- **Real-time auditing.** `cliffordc audit --realtime` lists every
  callable claimed `Realtime` and verifies each has either a
  worst-case-execution-time annotation (v0.4+) or a documented
  manual analysis. Auditors get a checklist instead of a code review.
- **Certification artefacts.** DO-178C / IEC 61508 / ISO 26262
  reviews want a per-function classification ("which functions touch
  hardware? which are real-time? which are pure?"). The trait list
  *is* that classification, derivable mechanically from source.
- **Encapsulation reporting.** A callable `$ [Encapsulated]` has a
  guarantee no other automaton observes its state changes — a
  property the `#hidden`/`#mutates` machinery already enforces, now
  visibly stated in the signature.

**Does NOT:**

- Affect race detection. `$ [SeqCst]` does not save a racy program;
  the orthogonality engine still rejects it.
- Replace `#mutates` / `#cannot_mutate`. Those are *capability*
  declarations (which automata can be touched); trait list is
  *kind* classification (what the touching looks like). They coexist.
- Affect type checking of return values, parameters, or expression
  forms. Trait list is metadata, not part of the type.

## 22.9 Cross-references

- **Decision #2 (hybrid traits)** — the `$ [TraitList]` mechanism
  this Decision extends from `@fn` to `#`-layer callables. Same parse
  rule, same AST node, different semantics.
- **Decision #6 (register blocks)** — the `Hardware` trait's
  canonical use case.
- **Decision #11 (`@sequential`)** — the *other* declarative-only
  attribute on `#`-layer callables; both are read by audit tools but
  ignored by the orthogonality engine, for the same reason
  (separation of concerns).
- **Decision #21 (shared automata)** — the `LockingDiscipline` trait
  ties into the v0.7+ shared-state machinery.
- **Decision #25 (`#hidden`)** — the `Encapsulated` trait is
  satisfied iff every field the callable writes is `#hidden`.
- **Spec §2.5** — the grammar amendment.
- **Book Ch. 5 (Decision #2)** — the original `@fn` trait-list
  chapter; pair it with this one to see the symmetry.
