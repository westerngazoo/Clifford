# Behaviour notes: `@snapshot Auto.field`

Status: **v0.2-ζ** (shipped 2026-05-08)
Spec source: `docs/CLIFFORD_SPEC.md` §7.2 closing note 3
Decision source: `docs/DECISIONS.md` Decision #24 + ADR 0004
Crates: `clifford-effect`, `clifford-codegen`
Companions:
- [`#atomic` behaviour notes](./ortho-atomic-attribute.md)
- [`@sequential` behaviour notes](./ortho-sequential-attribute.md)

This document describes how `@snapshot Auto.field` interacts with
the `clifford-ortho` verifier and `clifford-codegen` lowering in
v0.2-ζ — what it does, what it doesn't, when to use it instead of
`#atomic`, and the type-level restriction that keeps the
soundness claim tight.

## tl;dr

`@snapshot Auto.field` reads the field's current value into an
*owned, immutable copy* at the snapshot site. The §7.2 verifier
excludes the read from `actual_reads` so it does not pair against
any concurrent write. Codegen lowers it to the same single
GEP+load (or volatile load on register-block fields) that
`Auto.field` produces — the snapshot semantic is upstream of
codegen.

`@snapshot` is restricted to **primitive single-word fields**
(`u8`/`u16`/`u32`/`u64`/`i8`/`i16`/`i32`/`i64`/`bool`); compound
types (struct, array) would tear at the load level and break the
race-freedom guarantee. Compound snapshots surface a structured
`NotYetImplemented` error.

## Why this works

Per spec §7.2 closing note 3:

> The snapshot-and-decide pattern eliminates the issue inside `@fn`.
> When pure code reads from automaton state, it does so by
> constructing a snapshot value at a single point in the calling
> effect, then operating on the snapshot. The snapshot is owned,
> immutable, and not subject to in-place mutation — no read-write
> race possible.

The chain of reasoning:

1. The user writes `let v = @snapshot Auto.field;`. Codegen emits a
   single `load` instruction.
2. On every supported target, single-word loads of aligned data
   are *atomic at the instruction level* — the racer either
   observes the pre-write value or the post-write value, never a
   torn intermediate state.
3. After the snapshot, the SSA value `v` is owned by the caller.
   It cannot be mutated by anyone else; subsequent operations
   read from `v`, not from `Auto.field`.
4. Therefore the read race that v0.2-β's graded check would have
   flagged against a concurrent writer is, at the hardware level,
   benign — the snapshot got *some* valid value of the field, and
   that's all the user asked for.

The user takes responsibility for the load atomicity by writing
`@snapshot`. The verifier trusts the annotation; codegen enforces
the type-level precondition (primitive only) so the trust is
sound.

## When to use `@snapshot` vs. `#atomic` vs. `@sequential`

| Pattern | Use when | Cost | Restriction |
|---|---|---|---|
| `@snapshot Auto.field` | Reading ONE primitive field from foreground while ISRs may write it | One load, no interrupt latency | Primitive types only |
| `#atomic: interrupt_critical;` | Reading or writing MULTIPLE fields atomically | Masks all maskable interrupts for the body's duration | Bumps interrupt latency |
| `@sequential(A, B);` | User asserts two automatons never run concurrently (NVIC priority, scheduler ordering) | Free | Trusted; user-introduced bug if false |

For the SPSC consumer-side case (single u32 read from foreground
while an ISR increments it), `@snapshot` is the right choice —
cheaper, no interrupt-latency hit. For multi-field consistency
(reading three counters that must all be from the same ISR
boundary), `#atomic` is the right choice. For external-knowledge
non-concurrency (two automatons on different scheduler tasks),
`@sequential` is the right choice.

## Surface syntax

```clifford
// Foreground reads a per-source counter via @snapshot.
#effect drain_total() -> u32 #mutates: [Telemetry] $ [Acquire] {
  return @snapshot Telemetry.bytes_uart1 + @snapshot Telemetry.bytes_uart2;
}

// Inside a transition, @snapshot Self.field is allowed.
#automaton C { v: u32;
  #transition observe { let _x: u32 = @snapshot Self.v; return; }
}

// The compound case is rejected at codegen.
#automaton T { buf: [u8; 64]; }
#effect bad() #mutates: [T] {
  let _x: [u8; 64] = @snapshot T.buf;
  // error[codegen]: NotYetImplemented (@snapshot of non-primitive field …)
}
```

## What `@snapshot` covers

- **Read on a foreground callable** that races against an
  interrupt's write to the same primitive field. The most common
  use case; resolves with no runtime cost beyond a normal load.
- **Read on a transition**, including `@snapshot Self.field`. The
  `Self` resolution mirrors the regular `Self.field` read path.
- **Read inside arithmetic / control flow**: `@snapshot a +
  @snapshot b`, `if @snapshot v > 0 { ... }`. Each snapshot is an
  independent atomic load.
- **Multiple snapshots of the same field**: each is an
  independent atomic read; the values may differ (different
  instants of observation) and that's by design.

## What `@snapshot` does NOT cover

- **Compound fields.** `@snapshot` of a struct or array
  type-checks at parser level but is rejected at codegen — the
  multi-load lowering would tear under concurrent write. A
  future slice may add a `memcpy`-style snapshot inside an
  interrupt-mask scope, but that ends up being equivalent to
  `#atomic` and is therefore deferred.
- **Multi-field consistency.** Reading two fields with
  `@snapshot` does NOT guarantee they were observed at the
  same moment — between the two loads, the writer can update
  one of them. For multi-field consistency use `#atomic:
  interrupt_critical`.
- **Write protection.** `@snapshot` only annotates a READ. A
  callable that writes to an automaton field is still
  race-checked normally — it would be very wrong to silently
  exempt writes. Tested by
  `snapshot_does_not_protect_writes` in `crates/ortho`.
- **Hardware NMI.** A non-maskable interrupt can update the
  field between the snapshot's read and any subsequent
  observation. For NMI-correctness see the spec's separate
  treatment.
- **Cross-architecture atomicity contracts.** v0.2-ζ assumes
  primitive-typed loads are atomic at the instruction level,
  which holds for word-sized integers on all targets we care
  about (Cortex-M, Cortex-A, RISC-V32, RISC-V64, x86).
  Misaligned access and 64-bit access on 32-bit targets need
  more care; the type-level restriction conservatively uses
  the same set across all targets for now.

## Implementation references

### Verifier side (`crates/effect/src/lib.rs`)

The `walk_expr_for_reads` function's `Snapshot` arm is
**deliberately empty** — it walks past the snapshot without
recording a read in `actual_reads`. The arm carries a long
comment explaining the spec basis and why this is sound.

```rust
ExprKind::Snapshot { .. } => {}
```

This single empty arm is what makes the verifier accept
`@snapshot Auto.field` concurrent with `Auto.field += …`.

### Codegen side (`crates/codegen/src/lib.rs`)

- `emit_snapshot(automaton, field)` is the entry point invoked
  from the `Snapshot` arm in `emit_expr`.
- It resolves `Self` to the enclosing automaton (mirroring
  `emit_field_access`), validates that the field's IR type is in
  the primitive-load set via `is_primitive_ir_ty_for_snapshot`,
  then delegates to `emit_field_access_by_name` for the actual
  load emission.
- `emit_field_access_by_name` is the v0.2-ζ refactor that lets
  both `Auto.field` and `@snapshot Auto.field` share lowering.

### Tests

`crates/codegen/src/lib.rs` (lowering shape):
- `snapshot_lowers_to_same_load_as_field_access`
- `snapshot_on_register_block_field_emits_volatile_load`
- `snapshot_self_inside_transition_resolves_owner`
- `snapshot_compound_field_returns_e0810`
- `snapshot_on_unknown_automaton_rejected_by_resolver`
- `snapshot_inside_arithmetic_composes`

`crates/ortho/src/lib.rs` (verifier exclusion):
- `snapshot_read_does_not_trigger_race_with_concurrent_write`
- `snapshot_in_arithmetic_remains_race_free`
- `plain_read_still_triggers_race_with_snapshot_alternative`
  (the negative control)
- `snapshot_does_not_protect_writes`
- `snapshot_self_inside_transition_excluded_from_reads`

## Worked example: dual UART telemetry

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

#effect drain_total() -> u32 #mutates: [Telemetry] $ [Acquire] {
  // Two independent atomic loads, summed on already-captured
  // SSA values. No race even though the ISRs may write the
  // counters concurrently.
  return @snapshot Telemetry.bytes_uart1 + @snapshot Telemetry.bytes_uart2;
}

#interrupt USART1_IRQ() #mutates: [Telemetry] #priority: HIGH {
  #> tally_uart1();
}
#interrupt USART2_IRQ() #mutates: [Telemetry] #priority: HIGH {
  #> tally_uart2();
}
```

The full sample lives in `examples/dual_uart_telemetry.cl`.
Earlier slices used `#atomic: interrupt_critical;` for the same
pattern; v0.2-ζ swaps to `@snapshot` because the cost is lower
(no interrupt-mask) and the safety claim is identical for these
primitive single-word reads.

## Forward references

- **Compound `@snapshot`** (memcpy-snapshot inside an atomic
  scope): a future slice could lift the primitive restriction
  by emitting `cpsid i` + memcpy + `cpsie i`. That makes
  `@snapshot` equivalent in cost to `#atomic` for compound
  types, so it's effectively a syntactic shortcut.
- **`#atomic` target portability** (RISC-V `csrrci/csrrsi`):
  doesn't affect `@snapshot` — `@snapshot` lowers to a single
  `load` which works on every LLVM target.
- **`@snapshot` inside `@fn`**: the spec allows this (it's
  one of the few cases where `@fn` reads automaton state).
  v0.2-ζ supports `@fn` snapshots syntactically; the resolver
  / type checker may need additional layer-aware passes when
  ADR 0003's row-typing for `Readable` lands. Documented at
  spec §4.5.
