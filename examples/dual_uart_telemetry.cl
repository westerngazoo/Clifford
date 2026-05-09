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
// The foreground drain side uses `#atomic: interrupt_critical;`
// (v0.2-δ) to mask all interrupts during the body. The §7.2
// graded engine sees the attribute and suppresses the read-write
// race pair against the ISR producers — the masked body cannot
// be preempted, so the read is serialised with the ISR's
// load-modify-store.
//
// **Soundness gap as of v0.2-δ (documented):** the verifier
// trusts `#atomic` for race-freedom reasoning, but codegen does
// NOT yet emit the runtime wrapping (`cpsid i` / `cpsie i` on
// Cortex-M). The IR carries a comment marker; the actual
// interrupt-disable instructions land in a follow-up slice. A
// binary built today with `#atomic` does NOT mask interrupts
// at runtime. The verifier's safety proof IS valid for the
// program as written; the runtime gap is purely on the
// emission side.
//
// Earlier drafts of this sample also shared `bytes_total` and
// `last_byte` between producers — both the v0.1 (write-write) and
// v0.2-β (read-write) verifiers rejected that. The current shape
// — strictly disjoint per-source fields + atomic-wrapped drain
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


// ─── Foreground drain (v0.2-δ: #atomic: interrupt_critical) ─────────
//
// `drain_total` returns the SUM across both per-source counters,
// computed at read time. The Acquire fence pairs with the Release
// on every producer transition (publication ordering).
//
// `#atomic: interrupt_critical;` makes the body atomic with
// respect to the producers — the §7.2 verifier suppresses the
// read-write race pair, and (when codegen support lands) the
// runtime will mask interrupts for the body's duration.

#effect drain_total() -> u32 #mutates: [Telemetry] #atomic: interrupt_critical; $ [Acquire] {
  return Telemetry.bytes_uart1 + Telemetry.bytes_uart2;
}

// Same pattern for the per-source byte snapshots.
#effect drain_last_uart1() -> u8 #mutates: [Telemetry] #atomic: interrupt_critical; $ [Acquire] {
  return Telemetry.last_byte_uart1;
}

#effect drain_last_uart2() -> u8 #mutates: [Telemetry] #atomic: interrupt_critical; $ [Acquire] {
  return Telemetry.last_byte_uart2;
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
