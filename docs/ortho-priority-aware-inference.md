# Behaviour notes: `#priority`-aware concurrency inference

Status: **v0.2-η** (shipped 2026-05-08)
Spec source: `docs/CLIFFORD_SPEC.md` §2.5, §7.3
Crate: `clifford-ortho` (`crates/ortho/src/lib.rs`)
Companions:
- [`#atomic` behaviour notes](./ortho-atomic-attribute.md)
- [`@sequential` behaviour notes](./ortho-sequential-attribute.md)
- [`@snapshot` behaviour notes](./snapshot-attribute.md)

## tl;dr

The §7.3 concurrency inference now consults each `#interrupt`'s
declared `#priority: …` clause. Two interrupts at the **same**
priority cannot preempt each other on Cortex-M's NVIC (they
process via tail-chaining, no nested vectoring) — so the
verifier suppresses the pair. Different-priority interrupts
still take the standard wedge check: the higher-priority one
can preempt the lower-priority one.

This reduces the conservative false-positive rate on realistic
firmware that uses one priority level per interrupt class —
the most common pattern.

## The matrix

| Pair | Priority | v0.2-η suppresses? |
|---|---|---|
| `#interrupt A` × `#interrupt B` | both `LOW` | ✅ |
| `#interrupt A` × `#interrupt B` | both `MEDIUM` | ✅ |
| `#interrupt A` × `#interrupt B` | both `HIGH` | ✅ |
| `#interrupt A` × `#interrupt B` | both `Numeric("3")` | ✅ |
| `#interrupt A` × `#interrupt B` | both `Numeric("4_2") = "42"` | ✅ (canonicalised) |
| `#interrupt A` × `#interrupt B` | `LOW` vs `HIGH` | ❌ (different — preemption possible) |
| `#interrupt A` × `#interrupt B` | `Numeric("3")` vs `Numeric("5")` | ❌ |
| `#interrupt A` × `#interrupt B` | `HIGH` vs `Numeric("0")` | ❌ (mixed kinds — conservative) |
| `#effect E` × `#interrupt I` | (any) | ❌ (priority is interrupt-only) |
| `@fn F` × `#interrupt I` | (any) | ❌ (priority is interrupt-only) |

The mixed-kind rule (last `❌`) is deliberately conservative.
On Cortex-M's NVIC `HIGH` may map to numeric priority 0, but
v0.2-η doesn't know the target's priority encoding. Treating
mixed kinds as different priorities errs on the side of
flagging a potential race rather than silently suppressing it.
A future target-aware slice can refine this.

## Comparison rules

- `Low` / `Medium` / `High` are structural — case-sensitive
  enum equality.
- `Numeric(s1)` / `Numeric(s2)` compare by canonicalised text
  (whitespace and `_` separators stripped). So `Numeric("4_2")
  == Numeric("42")` and `Numeric("3") == Numeric("3")`.
- We don't parse the integer because the numeric range is
  target-specific (Cortex-M3 supports 0–7, Cortex-M4 supports
  0–15, RISC-V supports more). String-canonical comparison
  is sufficient for the match-or-not decision.

## Why `same priority ⇒ not concurrent`

Cortex-M's NVIC documentation (ARM v7-M Architecture Reference
Manual, B1.5):

> When the processor is executing an exception handler at
> priority N, only an exception with a priority strictly higher
> than N (numerically lower) can preempt the running handler.
> Interrupts at the same or lower priority remain pending.

Translating to the spec's §7.3 concurrency model:

- An interrupt at priority N is currently executing.
- Another interrupt at priority N becomes pending (set in NVIC
  ISER).
- The pending interrupt cannot fire until the running handler
  returns.
- Therefore the two handlers' bodies execute **strictly
  sequentially** — no concurrency, no race.

This holds for every Cortex-M variant we target. It also
holds for RISC-V's PLIC under the same priority discipline
(when configured for level-priority preemption).

## Sound vs. complete

v0.2-η is **sound** (it doesn't accept programs the spec would
reject) because:

- Same-priority interrupts on NVIC really do execute
  sequentially. Suppressing the pair is therefore a real
  observation about the hardware, not a trust assertion.
- Mixed-kind comparisons stay conservative — we never assume
  `HIGH == Numeric("0")` even when the target encoding makes it
  true.

It is NOT **complete** — the inference doesn't catch all the
cases where the user knows two interrupts can't run
concurrently. The escape hatches for those:

- **`@sequential(A, B);`** when the non-concurrency comes from
  scheduler ordering, not NVIC priority.
- **`#atomic: interrupt_critical;`** when an effect needs to
  mask interrupts during a critical section.
- **`@snapshot Auto.field`** when the issue is a single
  primitive read.

## What changed in concrete terms

Before v0.2-η:

```clifford
#automaton C { v: u32; }

// Two ISRs at the SAME priority writing the same field.
#interrupt USART1_IRQ() #mutates: [C] #priority: HIGH { C.v += 1u32; }
#interrupt USART2_IRQ() #mutates: [C] #priority: HIGH { C.v += 1u32; }

// → error[ortho]: E0520 between USART1_IRQ and USART2_IRQ
//   (false positive: NVIC processes them sequentially)
```

After v0.2-η:

```clifford
// Same source; now compiles cleanly.
// USART1_IRQ and USART2_IRQ both at HIGH → priority match
// → pair suppressed → no E0520.
```

This matches what real firmware does: many drivers put
related ISRs on the same priority specifically so they don't
preempt each other.

## What this does NOT change

- **Different-priority pairs**: still flagged. `HIGH × LOW`
  → can preempt → wedge check applies.
- **Effect × interrupt pairs**: priority is interrupt-only;
  effects on the foreground thread can be preempted by any
  interrupt regardless of priority.
- **Atomic / snapshot / sequential overrides**: continue to
  apply on top of priority. An `#atomic` body skips the IRQ
  pair check before priority comparison even runs.
- **Same-automaton concurrency**: Decision #5 still says
  automatons are inherently sequential within themselves;
  unaffected.

## When to use what

For two interrupts that race:

1. **Are they at the same priority on the same target?**
   → declare both with the same `#priority: …` clause.
   v0.2-η suppresses the pair automatically.
2. **Different priorities, but the user knows the scheduler
   guarantees non-concurrency?**
   → `@sequential(AutomatonOfA, AutomatonOfB);` (Decision #11).
3. **Different priorities, must read a multi-field consistent
   snapshot?**
   → `#atomic: interrupt_critical;` on the consumer side.
4. **Different priorities, single primitive field read?**
   → `@snapshot Auto.field` (lighter than `#atomic`).
5. **Different priorities, real race?**
   → restructure: per-source fields, lock-free ring buffer,
   etc.

## Implementation references

- `collect_interrupt_priorities(program) -> HashMap<String, PriorityLevel>`
  walks `Item::Interrupt` items and records each by name.
- `priorities_indicate_no_preemption(a, b) -> bool` is the
  comparison rule. Structural for `Low`/`Medium`/`High`,
  canonical-text for `Numeric(_)`, conservative `false` for
  mixed kinds.
- `verify` consults the map only when both sides of a pair
  are `ConcurrencyNode::Interrupt`. Other shapes take the
  standard path.

## Tests

`crates/ortho/src/lib.rs` (`v0.2-η` suite):

- `same_priority_interrupts_do_not_concur` — the canonical
  win.
- `different_priority_interrupts_still_violate` — sanity that
  the rule is priority-conditional.
- `medium_vs_medium_also_suppressed` — not HIGH-specific.
- `numeric_priorities_compare_by_canonical_text`.
- `different_numeric_priorities_still_violate`.
- `mixed_kinds_conservatively_treated_as_concurrent` — the
  HIGH vs Numeric("0") corner case.
- `priority_suppression_does_not_apply_to_effect_interrupt_pair`.
- `priorities_indicate_no_preemption_helper_smoke` — direct
  unit tests on every pair.

## Forward references

- **Target-aware priority normalisation**: a future slice
  could parse `--target` and map symbolic priorities (`HIGH`)
  to the target's numeric encoding, allowing mixed-kind
  comparisons. v0.2-η doesn't have this; mixed kinds stay
  conservative.
- **Priority-band suppression**: even different-priority pairs
  could be analysed (e.g. on Cortex-M, only the LOWER
  priority can be preempted; the higher-priority handler is
  never racing with the lower one in the "could be paused"
  sense). The spec's §7.3 model is symmetric; refining to
  the asymmetric NVIC reality would let us suppress more
  pairs but requires more careful analysis.
- **NMI**: non-maskable interrupts can preempt anything,
  including same-priority handlers. v0.2-η treats every
  declared `#interrupt` uniformly; the NMI case is currently
  outside the proof boundary and documented in
  `clifford-check`.
