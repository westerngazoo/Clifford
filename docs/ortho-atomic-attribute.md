# Behaviour notes: `#atomic: interrupt_critical`

Status: **v0.2-ε** (shipped 2026-05-08) — verifier ✓, codegen ✓ (Cortex-M)
Spec source: `docs/CLIFFORD_SPEC.md` §6.6, §7.2
Crates: `clifford-ortho`, `clifford-codegen`
Companion: [`@sequential` behaviour notes](./ortho-sequential-attribute.md)

This document describes how `#atomic: interrupt_critical;` on
`#effect` / `#interrupt` / `#transition` interacts with the
`clifford-ortho` verifier and `clifford-codegen` lowering — what it
does at compile time, what it does at runtime, and what's still
out of scope.

## tl;dr

`#atomic: interrupt_critical;` on a callable asserts that **the body
executes with all maskable interrupts disabled**. The §7.2 verifier
suppresses orthogonality pairs between an atomic callable and any
`#interrupt` (v0.2-δ). Codegen emits inline-asm `cpsid i` at body
entry and `cpsie i` at every `ret` exit on Cortex-M — making the
verifier's safety claim hold at runtime (v0.2-ε). The two slices
together close what v0.2-δ deliberately documented as a soundness
gap; today the contract is tight.

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

## The runtime contract (v0.2-ε)

The verifier proves "this program is race-free *given* that the
runtime actually masks interrupts during the atomic body."
Codegen is responsible for producing the masking instructions.
v0.2-ε ships the codegen side: the actual `cpsid i` / `cpsie i`
inline-asm pair surrounds every `#atomic: interrupt_critical`
body's body block with the cleanup happening before every `ret`.

The IR emitted today (Cortex-M target):

```llvm
define i32 @drain_total() {
entry:
  fence acquire
  call void asm sideeffect "cpsid i", ""() ; #atomic: interrupt_critical entry (mask all maskable interrupts)
  %tmp.0 = getelementptr %struct.Telemetry, %struct.Telemetry* @Telemetry.state, i32 0, i32 1
  %tmp.1 = load i32, i32* %tmp.0
  ; ... body ...
  ; (slice 9 tag write, if any)
  ; (Decision #22 release fence, if any) — fence ordering BEFORE unmask
  call void asm sideeffect "cpsie i", ""() ; #atomic: interrupt_critical exit (unmask)
  ret i32 ...
}
```

The order at exit matters and is enforced by the codegen
sequencing in `emit_exit_fence_if_pending`:

1. **State-tag write** (slice 9) — the new state value lands in
   memory.
2. **Release / SeqCst fence** (Decision #22) — publishes prior
   writes so any subsequent observer (including the
   about-to-be-unmasked interrupt) sees a consistent state.
3. **`cpsie i`** (this slice) — re-enables interrupts.
4. **`ret`** — return to caller.

Reversing 2 and 3 would be a real bug — an interrupt could fire
between the unmask and the fence completion, observing partial
state.

**Target portability.** v0.2-ε MVP wires only Cortex-M
(`cpsid i` / `cpsie i`). Other targets need different sequences:

| Target | Mask | Unmask |
|---|---|---|
| Cortex-M (thumbv7m-none-eabi etc.) | `cpsid i` | `cpsie i` |
| x86_64 | `cli` (privileged) | `sti` |
| RISC-V | `csrrci x0, mstatus, 8` | `csrrsi x0, mstatus, 8` |

For now codegen always emits the Cortex-M form. A future
`cliffordc compile --target` slice will switch on the requested
triple and surface a structured `NotYetImplemented` for
unsupported targets. **`#atomic: interrupt_critical` programs
built without `--target=thumbv7m-none-eabi` (or compatible)
today will produce IR that clang rejects on non-ARM targets.**

**Other `#atomic` kinds (rejected by codegen today):**

- `#atomic: multicore_critical` — reserved for Decision #21
  (v0.7+) shared-field locking. Codegen surfaces
  `NotYetImplemented`.
- `#atomic: <custom>` — codegen has no way to know what masking
  semantics to emit; surfaces `NotYetImplemented`.

These rejections are intentional: silently producing wrong code
would be unsafe.

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
- Verifier: `verify` consults each node's `is_atomic_critical`
  flag and skips the pair when the other side is an
  `#interrupt`. The flag is collected from the AST during the
  node-collection phase.
- Codegen (v0.2-ε):
  - `Emitter::emit_atomic_entry_mask` emits the entry-mask asm
    and queues the matching unmask.
  - `Emitter::emit_atomic_exit_unmask_if_pending` emits the
    unmask asm at every `ret` site, called from
    `emit_exit_fence_if_pending` AFTER the release fence so the
    fence's publication completes before any pending IRQ can
    observe partial state.
  - `Emitter::pending_atomic_exit_unmask: bool` is the queue
    flag, reset per function.

## Tests

`crates/ortho/src/lib.rs` (verifier side, v0.2-δ):

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

`crates/codegen/src/lib.rs` (codegen side, v0.2-ε):

- `atomic_interrupt_critical_emits_cpsid_at_body_start` — entry
  mask emitted.
- `atomic_interrupt_critical_emits_cpsie_before_ret` — exit
  unmask in the right position.
- `atomic_emits_balanced_pair_per_function` — exactly one
  entry + one exit per atomic body.
- `atomic_interacts_correctly_with_release_fence` — exit
  ordering: tag write < release fence < cpsie < ret.
- `non_atomic_effect_emits_no_cpsid_or_cpsie` — sanity that
  non-atomic bodies don't get wrapped.
- `atomic_on_interrupt_emits_wrapping_too` — `#interrupt` with
  `#atomic` gets the wrapping.
- `atomic_multicore_critical_is_not_yet_implemented` —
  v0.7+ deferred kind surfaces a structured error.
- `atomic_custom_kind_is_not_yet_implemented` — user-defined
  kinds rejected to prevent silent unsafety.
