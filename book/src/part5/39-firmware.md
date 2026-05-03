# Chapter 39: Embedded firmware patterns

> **Status:** Partial. The producer/consumer pattern is fully written; subsequent firmware patterns (timer-driven event loops, hardware register-block automata, interrupt nesting) land in book v0.4.

This chapter walks through *real* embedded patterns in Clifford — the kind of code you'd ship on a microcontroller. Each section pairs the pattern with what the v0.1 GA orthogonality engine accepts and what it rejects, so you can see the language's safety claim become a concrete tool.

## 39.1 The SPSC ring buffer (UART RX → main loop)

The canonical embedded test case. A UART RX interrupt produces bytes; the main loop consumes them. The producer and consumer share state. Done wrong, this is the bug class that ships in real firmware and gets caught — sometimes — months after release. Done right, it's lock-free and provable.

This section shows both versions and traces what `cliffordc` does with each. **It also demonstrates both safety pillars from spec §7.0.1** — procedural mutation safety, and parallel verification by exhaustive pairwise check.

### Version A: the naive design (engine rejects)

The "obvious" first design — a ring buffer with a `count` field that both sides update:

```clifford
#automaton UartRx {
  data:  [u8; 64];
  head:  usize;
  tail:  usize;
  count: usize;     // <-- the bug, not yet visible
}

#interrupt UART_RX_IRQ() #priority: HIGH #mutates: [UartRx] {
  // Producer side: append a byte
  if UartRx.count < 64usize {
    UartRx.data[UartRx.head] = #volatile_load<u8>(uart_rbr_register);
    UartRx.head  = (UartRx.head + 1usize) % 64usize;
    UartRx.count = UartRx.count + 1usize;
  }
}

#effect consume_byte() -> u8 #mutates: [UartRx] {
  // Consumer side: pop a byte
  if UartRx.count == 0usize {
    return 0u8;
  }
  let b: u8 = UartRx.data[UartRx.tail];
  UartRx.tail  = (UartRx.tail + 1usize) % 64usize;
  UartRx.count = UartRx.count - 1usize;
  return b;
}
```

What `cliffordc` does (trace through every phase):

```
=== Phase 2: clifford-effect / extract_mutation_profiles ===
  UART_RX_IRQ  → actual_writes = {UartRx.data, UartRx.head, UartRx.count}
  consume_byte → actual_writes = {UartRx.tail, UartRx.count}

=== Phase 3: clifford-ortho ===

basis assignment (sorted by automaton, then field):
  bit 0: UartRx.count
  bit 1: UartRx.data
  bit 2: UartRx.head
  bit 3: UartRx.tail

per-callable behavior blades:
  blade(UART_RX_IRQ)  = 0b0111   (count + data + head)
  blade(consume_byte) = 0b1001   (count + tail)

concurrency matrix:
  one pair: (UART_RX_IRQ, consume_byte)   ← interrupt × effect

orthogonality check:
  outer_product(0b0111, 0b1001):
    0b0111 & 0b1001 = 0b0001 ≠ 0   ← collapse!
    → None
  shared bits = 0b0001 → bit 0 → UartRx.count

=== Diagnostic ===
error[E0520]: orthogonality violation between `#interrupt UART_RX_IRQ`
              and `#effect consume_byte`: both write `UartRx.count`
```

**Compile-time. Source line numbers. The bug never ships.** The producer/consumer race on `count` is the same shape as the bug that took down the Therac-25 (interaction between operator-key buffering and beam-mode setup), the same shape as half the kernel concurrency bugs lockdep catches in production after years of running. In Clifford the typechecker says no, before the binary is built.

### Version B: SPSC lock-free (engine accepts)

The textbook fix: derive `count` from `head` and `tail` rather than storing it. Producer writes only `head` and `data`; consumer writes only `tail`. They share *no* write basis vectors:

```clifford
#automaton UartRx {
  data: [u8; 64];
  head: usize;       // producer-only write; consumer reads
  tail: usize;       // consumer-only write; producer reads
  // no count: derived as (head - tail) mod 64
}

#interrupt UART_RX_IRQ() #priority: HIGH #mutates: [UartRx] {
  let next_head: usize = (UartRx.head + 1usize) % 64usize;
  if next_head != UartRx.tail {                      // not full
    UartRx.data[UartRx.head] = #volatile_load<u8>(uart_rbr_register);
    UartRx.head = next_head;
  }
  // else: drop (overflow). A real impl might count drops in a separate
  // automaton mutated only by the producer.
}

#effect consume_byte() -> u8 #mutates: [UartRx] {
  if UartRx.tail == UartRx.head {                    // empty
    return 0u8;
  }
  let b: u8 = UartRx.data[UartRx.tail];
  UartRx.tail = (UartRx.tail + 1usize) % 64usize;
  return b;
}
```

The engine's view:

```
=== Phase 2 ===
  UART_RX_IRQ  → actual_writes = {UartRx.data, UartRx.head}
  consume_byte → actual_writes = {UartRx.tail}

=== Phase 3 ===

basis assignment:
  bit 0: UartRx.data
  bit 1: UartRx.head
  bit 2: UartRx.tail

blades:
  blade(UART_RX_IRQ)  = 0b011   (data + head)
  blade(consume_byte) = 0b100   (tail)

orthogonality check:
  outer_product(0b011, 0b100):
    0b011 & 0b100 = 0b000 = 0   ← no collapse!
    → Some(0b111)

✓ orthogonal. Compiles cleanly.
```

**The engine accepts this design** because the producer and consumer write disjoint fields. The lock-free SPSC pattern from Linux's `kfifo` and Lamport's 1983 paper is *checkable* in Clifford, not merely conventional.

### What the engine deliberately doesn't catch

Version B has two **read-write** races at the field level that v0.1 doesn't catch:

1. Producer writes `data[head]`; consumer reads `data[tail]`. At v0.1's field-level granularity, the producer-write and consumer-read on `data` look like access on the same field. They *are* disjoint slots (head ≠ tail when the consumer reads), but the engine can't prove that without v0.2's planned read/write algebra.
2. Producer writes `head`; consumer reads `head`. Same situation — the value crosses cleanly because the producer's update is one cycle on aligned 32-bit targets (per spec §7.2), but the engine doesn't formally prove it.

Spec §7.2 explicitly enumerates this as v0.1's read-write deferral, with the rationale: single-field aligned writes are atomic on every target Clifford supports; multi-field consistency uses `#atomic: interrupt_critical`; the snapshot-and-decide pattern eliminates the issue inside `@fn`. Version B is correct under v0.1's model assumptions.

### What this demonstrates about §7.0.1's safety pillars

**Pillar 1 — procedural mutation safety.** Both versions exercise the structured mutation surface (`#mutate` and `Auto.field <op>= …` sugar). The engine successfully catches the race in Version A and accepts Version B. If we'd instead written either with `#unchecked_store<u8>(some_addr, value)`, the writes would go through the narrow-unsafe layer per Decision #17 — and the engine wouldn't track them. They'd appear in `cliffordc audit --list-unsafe` for review, but not in the orthogonality check. Pillar 1 holds: structured layer = proven; narrow unsafe = audited but not proven.

**Pillar 2 — parallel verification.** The engine treats `UART_RX_IRQ` and `consume_byte` as concurrent (interrupt × effect, per §7.3). The pairwise wedge-product check is decisive: Version A's race on `count` produces a zero wedge → E0520; Version B's disjoint writes produce a non-zero wedge → accepted. This is the categorical product-existence proof from Appendix B made computational. Pillar 2 holds: parallel composition is verified by the algebra, not by hope.

### Generalising

The SPSC pattern works for any single-producer single-consumer case. Multi-producer or multi-consumer scenarios need a different shape — typically a `#shared` field with a lock (Decision #21, v0.7+) or a per-producer queue (multiple SPSC instances aggregated by a single consumer). Those patterns belong in book Ch. 40 (Kernel patterns) where shared-state machinery is in scope.

For purely-bounded firmware with one producer and one consumer per channel — which is most peripheral I/O on a microcontroller — the SPSC pattern shown here is the canonical answer, and it works in v0.1 Clifford with full static guarantees on the write-side races.

## 39.2 More patterns (forthcoming)

- Timer-driven main loops with priority-ordered effects.
- Hardware register-block automata using Decision #6's `#address` / `#offset` / `#access`.
- Interrupt nesting with priority comparisons.
- The "two-phase IRQ" pattern (drain hardware in the high-priority critical section, defer software work to a low-priority task).
- DMA scatter-gather with sigma-bound proofs on buffer indices.
- Power-mode state machines with `@sequential` exclusions for sleep paths.

These ship in book v0.4 once their corresponding language features are exercised against real silicon (the v0.2 RISC-V demonstrator described in `CLAUDE.md §10`).

---

**Cross-references:**
- The two safety pillars used here are stated normatively in spec §7.0.1.
- The wedge-product check is implemented in `crates/ortho/src/lib.rs::outer_product`.
- The mutation-profile extraction (the `actual_writes` set the engine consumes) is `crates/effect/src/lib.rs::extract_mutation_profiles`.
- For the algebraic foundation, see book Ch. 24 (geometric algebra primer) and Ch. 26 (the orthogonality theorem).
- For the producer/consumer pattern's place in the categorical framing, see book Ch. 25 (two categorical layers) — the producer and consumer are two morphisms of the Kleisli category over `UartRx`'s field-tuple state.
