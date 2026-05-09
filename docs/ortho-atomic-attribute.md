# Behaviour notes: `#atomic: interrupt_critical` in v0.2-δ

Status: **v0.2-δ** (shipped 2026-05-08) — verifier complete; codegen runtime wrapping deferred
Spec source: `docs/CLIFFORD_SPEC.md` §6.6, §7.2
Crate: `clifford-ortho` (`crates/ortho/src/lib.rs`)
Companion: [`@sequential` behaviour notes](./ortho-sequential-attribute.md)

This document describes how the `#atomic: interrupt_critical;` clause
on `#effect` / `#interrupt` / `#transition` interacts with the
`clifford-ortho` verifier in v0.2-δ — what it does, what it doesn't,
and the **runtime soundness gap** between the verifier (which trusts
the attribute) and codegen (which doesn't yet emit the masking
instructions).

## tl;dr

`#atomic: interrupt_critical;` on a callable asserts that **the body
executes with all maskable interrupts disabled**. The §7.2 verifier
suppresses orthogonality pairs between an atomic callable and any
`#interrupt`. As of v0.2-δ, codegen emits a comment marker but does
NOT yet emit `cpsid i` / `cpsie i` (or the equivalent on other
targets) — so a binary built today does not actually mask interrupts.
**This is a documented gap that a future slice closes.**

## What the verifier does

When the engine pairs two concurrency nodes for the §7.2 graded
check:

1. `can_concur` (§7.3) decides whether they could physically overlap.
2. `is_pair_sequential` (Decision #11 / `@sequential` behaviour doc)
   skips pairs the user has asserted as serialised.
3. **(v0.2-δ) Atomic suppression:** if either side carries
   `#atomic: interrupt_critical` AND the other side is an
   `#interrupt`, the pair is skipped.
4. Otherwise the wedge-product check fires.

The atomic suppression is asymmetric across callable kinds:

| Pair | Atomic suppresses? | Why |
|---|---|---|
| `#atomic effect` × `#interrupt` | ✅ | Effect's body masks the interrupt |
| `#atomic interrupt` × `#interrupt` | ✅ | Atomic interrupt masks the other on entry |
| `#atomic effect` × `#effect` | ❌ | Foreground-thread serialisation handles it (§7.3 already returns `false` for can_concur) |
| `#atomic effect` × `@fn` | ❌ | Same — foreground serialisation |
| `#atomic transition` × `#interrupt` | ❌ (today) | Transitions aren't direct concurrency nodes per §7.3 — their writes propagate via `actual_writes` into their callers; the attribute is parsed but not yet consumed by the verifier on transitions |

The **transition-side gap** is intentional for v0.2-δ. Transitions
are leaves of `#>` chains; their atomicity propagates indirectly
(if an interrupt's body invokes a `#> tick()` whose transition is
`#atomic: interrupt_critical`, the interrupt gets the masking for
that call site). Modelling this transitively requires call-site-
aware atomicity tracking, which is its own slice.

## The runtime gap (v0.2-δ → v0.2-ε)

The verifier proves "this program is race-free *given* that the
runtime actually masks interrupts during the atomic body." Codegen
is responsible for producing the masking instructions. v0.2-δ ships
with the verifier side complete and codegen as a stub:

```llvm
define i32 @drain_total() {
entry:
  fence acquire
  ; #atomic: interrupt_critical (runtime wrapping deferred to a future slice; see CHANGELOG)
  %tmp.0 = getelementptr %struct.Telemetry, %struct.Telemetry* @Telemetry.state, i32 0, i32 1
  %tmp.1 = load i32, i32* %tmp.0
  ; ... body ...
  ret i32 ...
}
```

The marker comment is emitted at the start of every `#atomic`
callable's `entry:` block. Tools that post-process the IR can find
and act on these markers; tools that consume the IR for execution
(clang, llc, qemu) silently strip them.

**What this means in practice:**

- Verifier: "This program is safe (assuming `#atomic` works at
  runtime)."
- Built binary: races at runtime if the `#atomic` body runs
  concurrently with an interrupt.
- Mitigation: on aligned 32-bit Cortex-M targets, single-word
  loads/stores are atomic at the instruction level — so the
  *theoretical* race is benign for word-sized fields. The
  *multi-field* consistency case (the actual motivation for
  `#atomic`) remains unsafe until the runtime wrapping lands.

**The CHANGELOG calls this out explicitly.** Don't rely on
`#atomic` for true cross-context safety until the runtime
wrapping ships.

## The closing slice (v0.2-ε, planned)

When the codegen wrapping lands, the IR will look like:

```llvm
define i32 @drain_total() {
entry:
  fence acquire
  call void asm sideeffect "cpsid i", ""()  ; mask interrupts
  ; ... body ...
  call void asm sideeffect "cpsie i", ""()  ; unmask
  ret i32 ...
}
```

The asm sequences are target-specific:
- **Cortex-M (`thumbv7m-none-eabi` etc.)**: `cpsid i` / `cpsie i`.
- **x86_64**: `cli` / `sti` (privileged; rarely usable).
- **RISC-V**: `csrrci x0, mstatus, 8` / `csrrsi x0, mstatus, 8`.

For v0.2-ε MVP only Cortex-M will be wired; other targets surface
a structured `NotYetImplemented` error with the user's `--target`
flag in the message.

## What `#atomic` does not cover

- **`#atomic` on `@fn`**: rejected at parse time (`@fn` is the
  pure-functional layer; `#`-prefixed clauses are imperative-side
  only). The lexer / parser doesn't allow `#atomic` on `@fn`
  syntactically.
- **Cross-thread atomicity**: `#atomic: interrupt_critical` masks
  *interrupts*, not other foreground threads on a multi-core
  target. For SMP-aware atomicity, see the reserved
  `#atomic: multicore_critical` (v0.7+ Decision #21 lock
  machinery).
- **Read-write races between two foreground callables**: these are
  already non-concurrent per §7.3 (single foreground thread), so
  `#atomic` doesn't apply.
- **Hardware NMI**: a non-maskable interrupt can preempt
  `#atomic: interrupt_critical` bodies. Most NMIs in firmware
  are reserved for fatal-error paths and don't share state with
  application code, so this is rarely a concern, but it IS a real
  hole in the safety claim. Spec §6.6 acknowledges this.

## When you should use `#atomic`

Use `#atomic: interrupt_critical;` when **all** of these are true:

1. The verifier rejects a pair `(X, Y)` with X foreground and Y
   an interrupt that touch overlapping state.
2. The body is short enough that masking interrupts for its
   duration won't break real-time deadlines on the target.
3. The alternative (split the data into per-source fields, use
   `@snapshot` to copy-then-read, restructure ownership) is
   impractical.

Do **not** use `#atomic` to silence:

- Same-automaton write-write races on the foreground. Those
  serialise on the foreground thread already; the engine
  doesn't flag them.
- NMI-correctness concerns. NMIs aren't masked by `cpsid i`;
  document the NMI contract separately.

## Worked example

```clifford
#automaton Telemetry {
  #states: [Empty, NonEmpty];
  bytes_uart1: u32;
  bytes_uart2: u32;

  #transition tally_uart1 -> NonEmpty $ [Release] {
    Telemetry.bytes_uart1 += 1u32;
  }
  #transition tally_uart2 -> NonEmpty $ [Release] {
    Telemetry.bytes_uart2 += 1u32;
  }
}

// Without `#atomic`: v0.2-β rejects this — drain_total reads
// bytes_uartN that the ISRs write.
//
// With `#atomic: interrupt_critical`: v0.2-δ accepts. The
// verifier trusts that the body runs with interrupts masked,
// so no read-write race is possible.
#effect drain_total() -> u32
    #mutates: [Telemetry]
    #atomic: interrupt_critical;
    $ [Acquire]
{
  return Telemetry.bytes_uart1 + Telemetry.bytes_uart2;
}

#interrupt USART1_IRQ() #mutates: [Telemetry] #priority: HIGH {
  #> tally_uart1();
}

#interrupt USART2_IRQ() #mutates: [Telemetry] #priority: HIGH {
  #> tally_uart2();
}
```

The full sample lives in `examples/dual_uart_telemetry.cl`.

## Implementation references

- AST: `clifford_ast::AtomicKind` enum + `atomic: Option<AtomicKind>`
  field on `EffectDecl`, `InterruptDecl`, `TransitionDecl`.
- Parser: `parse_optional_atomic_clause` recognises
  `#atomic: <kind>;` after the `#mutates` / `#priority` /
  `#cannot_mutate` / `$ [TraitList]` clauses.
- Codegen: `emit_atomic_marker_if_any` emits the
  `; #atomic: <kind> (runtime wrapping deferred …)` IR comment
  at the start of the body. **Runtime wrapping lands in v0.2-ε.**
- Verifier: `verify` consults each node's `is_atomic_critical`
  flag and skips the pair when the other side is an
  `#interrupt`. The flag is collected from the AST during the
  node-collection phase.

## Tests

`crates/ortho/src/lib.rs` contains:

- `atomic_effect_suppresses_pair_with_interrupt` — the
  canonical SPSC consumer-side fix.
- `atomic_interrupt_suppresses_pair_with_other_interrupt` — IRQ-
  vs-IRQ pair suppression.
- `atomic_does_not_suppress_pair_with_non_interrupt` — atomic
  is interrupt-specific.
- `no_atomic_means_no_suppression` — sanity that the attribute
  is what's making the first test pass.
- `atomic_with_multiple_field_writes_suppresses_all` — §7.2's
  motivation (multi-field consistency).
- `atomic_transition_is_recognised` — confirms parsing
  end-to-end (verifier handling on transitions deferred).
