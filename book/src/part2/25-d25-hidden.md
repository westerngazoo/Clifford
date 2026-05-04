# Chapter 25: Decision #25 — `#hidden` field encapsulation

> **Status:** Design locked 2026-05-03. Implementation slated for v0.2;
> the parser, AST, and resolver pieces ship in the v0.1 GA work
> (lexer reservation already landed alongside Decision #21's
> reservations; full visibility check lands as part of this chapter's
> implementation PR). Spec amendment: §2.5 (grammar) and §3 point 6a
> (parser-level note).

## 25.1 What you'll learn

By the end of this chapter you'll be able to:

1. Mark a field as `#hidden` and explain *which* callables can still
   access it and *why*.
2. State the Decision #25 rule precisely enough to predict whether any
   given source position will compile (the visibility table in §25.4).
3. Explain why `#hidden` requires *no special algebra* — the
   orthogonality engine doesn't even know hidden fields exist; the
   property falls out of the §7.4 wedge product because the bit is
   never present in outside callables' behaviour multivectors.
4. Implement the parser + resolver pieces yourself from this chapter,
   including the `E0407 HiddenFieldNotAccessible` diagnostic shape.

## 25.2 Surface syntax

`#hidden` is a **per-field modifier** on automaton fields. It's a
flag (no value), order-independent with `#offset` and `#access`:

```clifford
#automaton Counter {
  value:   u32;                                       // ordinary field
  scratch: u32 #hidden;                               // hidden, plain field
  status:  u32 #hidden #offset: 0x04 #access: read;   // hidden, register-block field
  cache:   [u8; 32] #hidden;                          // hidden, array field
}
```

Grammar (extension to §2.5):

```
automaton_field  := ident ':' type_expr field_attr*
field_attr       := '#offset' ':' integer_literal      // register-block
                  | '#access' ':' access_kind          // register-block
                  | '#hidden'                          // Decision #25
```

Multiple `#hidden` modifiers on the same field are a parse error
(`E0212: DuplicateClause("#hidden")`). The same one-shot rule applies
to `#offset` and `#access` — `#hidden` follows the same convention.

## 25.3 The algebraic insight (this is the whole design)

The earlier `#hidden` (Decision #9) was a *visibility-clause* system —
external annotations on effects describing what they could see. We
dropped it because `#mutates` / `#cannot_mutate` already expressed the
same thing at effect granularity, and one concept beats two.

The **re-introduced** `#hidden` is *narrower* and *algebraically
motivated*. It expresses encapsulation as a property of the field
itself, not the effect's view of the world. The slogan:

> A `#hidden` field's basis vector is **automatically orthogonal to
> everything outside the owning automaton's surface**, because the bit
> never enters outside callables' behaviour multivectors at all.

Restated as a property of the orthogonality engine's input: outside
callables literally cannot reference a hidden field, so the resolver
never produces an `actual_writes` entry containing it for those
callables, so its basis vector never appears in their behaviour blade,
so `outer_product(outside, owning)` cannot collapse on the hidden bit.

This is the **trivial-orthogonality case** of the §7.4 check. There's
no special algebra, no engine pass, no special bit type — the bit just
isn't there for outsiders to refer to. Encapsulation by construction.

The design slogan is the user's framing, captured verbatim in
`DECISIONS.md` Decision #25:

> "Encapsulation is 'the bit isn't there for outsiders to refer to' —
> trivial orthogonality by construction."

## 25.4 Visibility table

The exact rule is one row of the visibility table in §3 point 6a:

| Caller context                                       | Owns the automaton? | Field is `#hidden`? | Result        |
|------------------------------------------------------|---------------------|---------------------|---------------|
| `#transition` of automaton `A`, references `A.f`     | yes                 | yes                 | **OK**        |
| `#transition` of automaton `A`, references `A.f`     | yes                 | no                  | **OK**        |
| `#transition` of automaton `B`, references `A.f`     | no                  | yes                 | **E0407**     |
| `#transition` of automaton `B`, references `A.f`     | no                  | no                  | OK (if `B`'s `#mutates` permits — checked elsewhere) |
| `#effect` `#mutates: [A]`, references `A.f`          | no                  | yes                 | **E0407**     |
| `#effect` `#mutates: [A]`, references `A.f`          | no                  | no                  | OK            |
| `#interrupt` `#mutates: [A]`, references `A.f`       | no                  | yes                 | **E0407**     |
| `@fn` body, references `A.f`                         | no                  | yes                 | **E0407**     |

The table reduces to a single predicate: a hidden field of `A` is
accessible from this position if and only if the position is *inside
a `#transition` of `A`*. Everything else is `E0407`.

Note in particular row 5: even an `#effect` that declares
`#mutates: [Counter]` cannot touch `Counter`'s hidden fields. The
`#mutates` clause grants automaton-level access (which fields the
effect may write to), not hidden-field access (which fields are even
visible). The two are independent dimensions per Decision #25.

## 25.5 Why a transition of automaton `A`, and not "any callable that
declares `#mutates: [A]`"?

This is the design decision that makes `#hidden` algebraically clean,
and it's worth pausing on. There are three plausible answers:

1. **Owning-transition only.** What we picked. Hidden fields are
   visible only from the automaton's own transitions.
2. **Owning automaton + any `#mutates: [A]` effect.** Effects in
   `#mutates: [A]` can see the hidden bits as a "trusted insider."
3. **Any code with a `#mutates: [A, with_hidden]` opt-in.** Add an
   explicit "I want hidden access" marker.

We picked (1) because it's the only choice where the algebraic-trivial
property holds *without any flag tracking on effects*. With (2) or (3)
the engine has to know which effects opted into hidden access, then
the basis assignment differs per effect, then we're tracking visibility
state in the engine — exactly what (1) avoids.

(2) and (3) are also weaker as encapsulation: any new effect dropped
into the file with `#mutates: [Counter]` would silently gain access
to `Counter`'s internals. (1) makes the access surface *grep-able*:
the only places `Counter.scratch` can appear are inside `Counter`'s
own `#transition` blocks. You can audit the surface with one grep,
and `cliffordc audit --hidden Counter` reports the same set.

## 25.6 What this enables

- **Implementation hiding for register-block automata.** A UART
  driver's internal `#hidden` parity-error counter that no other
  code should see. Other automata can still observe what the UART
  exposes (the public RX-byte buffer); the parity counter is
  implementation detail.
- **Caches and scratch buffers** per automaton without polluting
  the global mutation analysis. The cache lives, the wedge-product
  check sees it for the owning transitions only, no risk of an
  external effect accidentally racing on it because no external
  effect can name it.
- **Cleaner `cliffordc audit` reports.** Hidden fields don't appear
  in cross-automaton dependency graphs; they're internal vertices
  with no outgoing edges to other automata.

## 25.7 Worked example: a UART RX driver with a hidden parity-error counter

```clifford
#automaton Uart {
  // Public: the byte queue consumers read from.
  rx_head:   usize;
  rx_tail:   usize;
  rx_buffer: [u8; 64];

  // Hidden: parity-error counter. Other code MUST NOT see this —
  // it's diagnostic state owned by the UART's IRQ + transitions
  // exclusively. If exposed, it would make Uart's read surface
  // larger than its abstract behaviour warrants.
  parity_errors: u32 #hidden;

  #transition rx_byte_received {
    // Update head, write buffer, account for parity error if set.
    Uart.rx_head     = (Uart.rx_head + 1usize) % 64usize;
    // Hidden field write: legal because we're inside Uart's transition.
    Uart.parity_errors = Uart.parity_errors + 1u32;
  }
}

#interrupt USART1_IRQHandler() #mutates: [Uart] #priority: HIGH {
  // Legal: USART1_IRQHandler doesn't reference Uart.parity_errors directly;
  // it dispatches to Uart.rx_byte_received which can.
  #> rx_byte_received();
}

// Suppose a logger were dropped into the file:
#effect log_uart_stats() #mutates: [Uart] {
  // ATTEMPT: read the parity error count for logging.
  // ─→ E0407: `Uart.parity_errors` is `#hidden`; only `Uart`'s own
  //          `#transition`s may access it (referenced at byte 1234)
  let _e := Uart.parity_errors;
  return;
}
```

The error stops the build before any binary is produced. The diagnostic
names the field by source identifier (`Uart.parity_errors`), names the
visibility rule (`only Uart's own #transition`s may access it`), and
points to the byte offset of the offending reference. Everything a
user needs to fix the problem is in the message; nothing requires
reading the compiler source.

## 25.8 Implementing `#hidden` from this chapter

The implementation cost is small (~50 LoC) — perfectly reasonable to
write yourself for practice. Here's the full plan:

### 25.8.1 Lexer

Add one new token:

```rust
// crates/lexer/src/lib.rs
pub enum TokenKind {
    // ... existing variants ...
    /// `#hidden` per-field encapsulation modifier (Decision #25).
    KwHashHidden,
    // ... existing variants ...
}
```

Recognise the keyword in the `#`-prefixed-identifier branch:

```rust
"hidden" => TokenKind::KwHashHidden,
```

Add `#hidden` to the `all_imperative_sigil_forms` test alongside
`#offset`, `#access` etc.

### 25.8.2 AST

Extend the `AutomatonField` struct with a `hidden: bool` flag:

```rust
// crates/ast/src/lib.rs
pub struct AutomatonField {
    pub name: String,
    pub ty: TypeExpr,
    pub offset: Option<String>,
    pub access: Option<AccessMode>,
    pub kind: FieldKind,           // existing — Private (v0.1) vs future Shared (v0.7)
    pub hidden: bool,              // ← Decision #25
    pub span: Span,
}
```

The `kind: FieldKind` axis (Decision #21) is *orthogonal* to the
`hidden: bool` axis (Decision #25). A field can be Private + non-hidden,
Private + hidden, and (in v0.7+) Shared + non-hidden. (Shared + hidden
is technically representable but would be a strange combination — your
shared lock-protected mutator is also an implementation secret? — and
the v0.7 work can decide whether to permit or reject it.)

### 25.8.3 Parser

Extend `parse_automaton_field` to accept `#hidden` as a third
field-meta clause, in any order with `#offset` and `#access`:

```rust
let mut hidden = false;
loop {
    match self.peek().kind {
        TokenKind::KwHashOffset => { /* existing */ }
        TokenKind::KwHashAccess => { /* existing */ }
        TokenKind::KwHashHidden => {
            if hidden {
                return Err(ParseError::DuplicateClause {
                    clause: "#hidden",
                    at: self.peek().span.start,
                });
            }
            self.advance();
            hidden = true;
        }
        _ => break,
    }
}
// ... emit AutomatonField { ..., hidden, ... }
```

### 25.8.4 Resolver

Build a side table: per automaton, the set of `#hidden` field names.

```rust
// crates/resolve/src/lib.rs
struct AutomatonMeta {
    fields: HashMap<String, HashSet<String>>,        // existing
    hidden_fields: HashMap<String, HashSet<String>>, // ← Decision #25
    transitions: HashMap<String, HashMap<String, Span>>, // existing
}
```

Extend `require_field` to check hidden visibility *after* confirming
the field exists:

```rust
fn require_field(&mut self, automaton: &str, field: &str, at: Span) {
    let Some(field_set) = self.automaton_meta.fields.get(automaton) else {
        return;
    };
    if !field_set.contains(field) {
        // ... E0405 UnknownField, return ...
    }
    let is_hidden = self.automaton_meta.hidden_fields.get(automaton)
        .is_some_and(|hs| hs.contains(field));
    if !is_hidden { return; }
    // Hidden — only accessible from a transition of the same automaton.
    let inside_owning = self.enclosing.as_ref()
        .and_then(|e| e.transition_of.as_deref())
        .is_some_and(|t| t == automaton);
    if !inside_owning {
        self.errors.push(ResolveError::HiddenFieldNotAccessible { /* ... */ });
    }
}
```

The whole check is six lines past the `is_hidden` early return.
That's it — no engine machinery, no propagation through
`#mutates`, no special handling in the GA pass.

### 25.8.5 Tests

The test surface is small:

- Parser: field with `#hidden`; field without (default false); `#hidden`
  in any order with `#offset` and `#access`; duplicate `#hidden`
  rejected; multiple fields, mixed hidden.
- Resolver: hidden accessible from owning transition (positive control);
  hidden inaccessible from `#effect #mutates: [A]`; from another
  automaton's transition; from `@fn`; non-hidden remains accessible
  (negative control); E0407 distinct from E0405; hidden array indexed
  write inside `#mutate` block from owning transition.

The full test set is ~190 lines of Rust and exercises every cell of
the visibility table in §25.4.

## 25.9 What `#hidden` is NOT

Three things `#hidden` deliberately does *not* do, with rationale:

- **It does not change the field's GA basis assignment for the owning
  automaton's transitions.** Inside the owning transitions, hidden
  fields enter `actual_writes` and the §7.4 wedge product exactly like
  any other field. They contribute basis vectors; transitions touching
  the same hidden field race against each other normally. Only
  *outside* callables don't see the bit.
- **It does not interact with `@fn` purity.** A `@fn` body still
  cannot reference automaton state (Decision #1's pure/imperative
  boundary applies regardless of `#hidden`); the addition is that
  even an `@fn` that *could* in principle name a path-position
  reference can't break encapsulation.
- **It does not replace `#mutates` / `#cannot_mutate`.** Those are
  effect-level capabilities (which automata an effect can write to);
  `#hidden` is a field-level visibility (which fields outside callables
  can name). Both axes coexist and are checked independently.

## 25.10 Cross-references

- `DECISIONS.md` Decision #25 — the lock entry with the full design
  rationale and the user's algebraic-trivial-orthogonality framing.
- Spec §2.5 — grammar amendment.
- Spec §3 point 6a — parser-behavior amendment.
- Spec §7 — orthogonality engine (this chapter's whole point is that
  §7 needs *no* changes).
- Decision #9 (the original dropped `#hidden` / `#visible` system) and
  Decision #16 (`#impl Interface for Automaton`) — context for why the
  re-introduction is narrower.
- Book Ch. 21 (Decision #21) — explains `FieldKind::Private` /
  `FieldKind::Shared`, the *other* per-field axis (orthogonal to
  `#hidden`).
- Book Ch. 30 (the orthogonality theorem) — the §7.4 wedge product
  whose collapse-or-not behaviour is what `#hidden` exploits.
- Book Ch. 43 (firmware patterns) — the SPSC ring buffer worked
  example in §39.1 demonstrates the outside-callable wedge-product
  collapse that `#hidden` would prevent for any field marked
  `#hidden`.
