// dual_uart_telemetry.cl — multi-producer telemetry buffer
//
// Two UART RX interrupts (the producers) record incoming bytes into a
// shared telemetry automaton. The foreground task (the consumer)
// reads the totals and the most-recently-seen byte. The cross-context
// handoff is correct by virtue of:
//
// 1. Decision #22 traits (publication ordering):
//      producers: $ [Release] on each tally transition  →  fence release
//      consumer:  $ [Acquire] on the drain effect       →  fence acquire
//
// 2. Spec §7 GA orthogonality (write-write race freedom):
//      The two ISRs touch *strictly disjoint* fields. Earlier drafts
//      of this sample shared `bytes_total` and `last_byte` between
//      producers — the §7 verifier (correctly) rejected that with
//      E0520. Splitting into per-source `bytes_uartN` /
//      `last_byte_uartN` makes the wedge-product non-zero and the
//      program race-free under the engine's restricted Cl(0,0,n)
//      algebra.
//
// The Release on the producer side publishes the new state-tag plus
// every store that came before it; the Acquire on the consumer side
// guarantees those stores are visible by the time `drain` reads them.
// This is the standard one-way handoff — equivalent to the
// publish-then-acquire pattern in a SPSC ring buffer, scaled to two
// producers because the two UART ISRs touch disjoint counters.
//
// What this sample exercises end-to-end (across slices 1-10):
//   - register-block automatons + volatile MMIO reads          (slice 4)
//   - multi-state automaton + state-tag dispatch               (slice 9)
//   - transition `-> Dest` writes the destination tag          (slice 9)
//   - acquire / release fences via $ [...] traits              (Decision #22)
//   - interrupt `section ".interrupts"` for vector-table       (slice 4)
//   - cross-callable transition mangling (`#> tally_uart1`)    (slice 4)
//   - mutation sugar `+=` and `=`                              (slice 3)
//   - integer cast (volatile-read of u32, store the low byte)  (slice 7)
//
// What's missing (and the comments below flag): there's no `if` /
// `match` yet, so we can't branch on `Telemetry@state` to drop bytes
// when the slot is "full". The design here is "totalizer + most
// recent" — a deliberate fit for the current language surface.
//
// Compile:  cliffordc compile examples/dual_uart_telemetry.cl
// Output:   examples/dual_uart_telemetry.ll  (~2-3 KB of IR)


// ─── Hardware: two UART register blocks ─────────────────────────────

#automaton Uart1 {
  #address: 0x4000_4000;
  rx_data: u32 #offset: 0x00;
  status:  u32 #offset: 0x18;
}

#automaton Uart2 {
  #address: 0x4000_5000;
  rx_data: u32 #offset: 0x00;
  status:  u32 #offset: 0x18;
}


// ─── Telemetry: multi-state automaton ───────────────────────────────
//
// State machine:
//
//      ┌────── tally_uart{1,2} ──────┐
//      ▼                              │
//   Empty                          NonEmpty
//      │                              ▲
//      └────── (start_up) ────────────┘
//
// The two `tally_*` transitions both move us into `NonEmpty`. Once
// in `NonEmpty` we stay there forever in this sample — there's no
// "drained → Empty" transition because the consumer doesn't need to
// signal anything back to the ISRs (telemetry is monotone).

#automaton Telemetry {
  #states: [Empty, NonEmpty];

  // Per-source byte counters. Each ISR writes its own counter only —
  // strictly disjoint to satisfy the §7 GA orthogonality engine.
  // The ISRs may preempt each other (different vector entries); a
  // shared `bytes_total` field would race even at the same priority
  // on multi-core targets, so we keep the counters separate and let
  // the consumer sum them.
  bytes_uart1: u32;
  bytes_uart2: u32;

  // Per-source most-recently-received byte. Splitting these keeps
  // the ISRs strictly disjoint; a single shared `last_byte` would
  // be a write-write race the §7 verifier (correctly) rejects.
  last_byte_uart1: u8;
  last_byte_uart2: u8;

  // Producer 1: invoked from USART1_IRQ. Touches ONLY the uart1
  // counter and uart1 last_byte. The Release fence publishes the
  // new state-tag plus all the field stores before this
  // transition's ret returns to the ISR prologue.
  #transition tally_uart1 -> NonEmpty $ [Release] {
    Telemetry.bytes_uart1 += 1u32;
    Telemetry.last_byte_uart1 = (Uart1.rx_data as u8);
  }

  // Producer 2: invoked from USART2_IRQ. Touches ONLY the uart2
  // counter and uart2 last_byte. Strictly disjoint from tally_uart1's
  // write set per §7 — `wedge(behavior(IRQ1), behavior(IRQ2)) ≠ 0`.
  #transition tally_uart2 -> NonEmpty $ [Release] {
    Telemetry.bytes_uart2 += 1u32;
    Telemetry.last_byte_uart2 = (Uart2.rx_data as u8);
  }
}


// ─── Foreground consumer ────────────────────────────────────────────
//
// `drain_total` returns the SUM across both per-source counters,
// computed at read time. The Acquire fence pairs with the Release
// on every producer transition, so any byte tallied before this
// call is guaranteed to be reflected in the sum.

#effect drain_total() -> u32 #mutates: [Telemetry] $ [Acquire] {
  return Telemetry.bytes_uart1 + Telemetry.bytes_uart2;
}

// Read the most recent byte from each source. The consumer chooses
// which is "newer" by checking the corresponding counter (or by
// pre-deciding which UART to drain).
#effect drain_last_uart1() -> u8 #mutates: [Telemetry] $ [Acquire] {
  return Telemetry.last_byte_uart1;
}

#effect drain_last_uart2() -> u8 #mutates: [Telemetry] $ [Acquire] {
  return Telemetry.last_byte_uart2;
}

// Read which state we're in. Useful for "have we seen anything yet?"
// — without `if` we can't branch on this in source, but the consumer
// can compare it against the integer tag on the host side (Empty=0,
// NonEmpty=1).
#effect drain_state() -> u32 #mutates: [Telemetry] $ [Acquire] {
  return Telemetry@state;
}


// ─── Interrupt vectors ──────────────────────────────────────────────
//
// Each ISR delegates to its tally transition. The proc-call lowers
// to a mangled `Telemetry_tally_uart{1,2}` symbol; codegen routes
// the call automatically because the resolver knows the callee is
// a transition.

#interrupt USART1_IRQ() #mutates: [Telemetry] #priority: HIGH {
  #> tally_uart1();
}

#interrupt USART2_IRQ() #mutates: [Telemetry] #priority: HIGH {
  #> tally_uart2();
}
