// dual_uart_telemetry.cl — multi-producer telemetry, ISR side
//
// Two UART RX interrupts (the producers) record incoming bytes into a
// shared `Telemetry` automaton. Each ISR touches *strictly disjoint*
// fields — `bytes_uartN` / `last_byte_uartN` — so the §7 GA
// orthogonality engine proves no write-write race.
//
// What this sample exercises (across slices 1-13):
//   - register-block automatons + volatile MMIO reads       (slice 4)
//   - multi-state automaton + state-tag dispatch            (slice 9)
//   - transition `-> Dest` writes the destination tag       (slice 9)
//   - $ [Release] memory-ordering fence on each tally       (Decision #22)
//   - interrupt `section ".interrupts"` for vector-table    (slice 4)
//   - cross-callable transition mangling (`#> tally_uartN`) (slice 4)
//   - mutation sugar `+=` and `=`                           (slice 3)
//   - integer cast (volatile-read u32, store low byte u8)   (slice 7)
//
// The foreground drain side uses `@snapshot Auto.field`
// (v0.2-ζ, Decision #24 / ADR 0004) to take owned copies of the
// per-source counters. The §7.2 graded engine excludes
// `@snapshot` reads from `actual_reads` because the
// snapshot-and-decide pattern (spec §7.2 closing note 3) makes
// each read a single atomic load on aligned 32-bit hardware —
// the racer either sees the old or the new value, never torn.
//
// `@snapshot` is the lighter-weight alternative to
// `#atomic: interrupt_critical;` for the SPSC consumer-side
// case. Both are spec-supported routes:
//   - `#atomic`: masks interrupts for the duration; useful for
//     multi-field consistency across non-atomic operations.
//   - `@snapshot`: per-read atomic copy; cheaper, no interrupt
//     latency cost, but only safe for primitive-typed fields.
//
// We use `@snapshot` here because each drain effect reads
// exactly one (or two) primitive counters — single-word reads
// on aligned u32s.
//
// Earlier drafts of this sample also shared `bytes_total` and
// `last_byte` between producers — both the v0.1 (write-write) and
// v0.2-β (read-write) verifiers rejected that. The current shape
// — strictly disjoint per-source fields + @snapshot-wrapped drain
// — is what the engine proves race-free.
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


// ─── Foreground drain (v0.2-ζ: @snapshot Auto.field) ────────────────
//
// `drain_total` returns the SUM across both per-source counters
// using `@snapshot` to take owned copies. The Acquire fence
// pairs with the Release on every producer transition
// (publication ordering); `@snapshot` makes each individual
// read race-free with the ISR's load-modify-store.

#effect drain_total() -> u32 #mutates: [Telemetry] $ [Acquire] {
  return @snapshot Telemetry.bytes_uart1 + @snapshot Telemetry.bytes_uart2;
}

// Same pattern for the per-source byte snapshots.
#effect drain_last_uart1() -> u8 #mutates: [Telemetry] $ [Acquire] {
  return @snapshot Telemetry.last_byte_uart1;
}

#effect drain_last_uart2() -> u8 #mutates: [Telemetry] $ [Acquire] {
  return @snapshot Telemetry.last_byte_uart2;
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
