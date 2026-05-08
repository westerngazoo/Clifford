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
// What this sample does NOT include (deliberately):
//   - The foreground "drain" side. v0.2-β's read-write race detector
//     (spec §7.2) correctly flags reads of `bytes_uartN` from
//     foreground code as racing against the ISR writes — even with
//     Acquire/Release publication ordering, the read of the memory
//     cell itself races with the ISR's load-modify-store (a
//     theoretical race that's benign on aligned 32-bit hardware but
//     fails the conservative §7.2 check).
//
//     A SAFE drain-side requires either:
//       (a) `#atomic: interrupt_critical` on the drain effect (CLI/STI
//           wrapper) — implementation deferred to a future slice.
//       (b) `@snapshot Auto.field` (Decision #24 / ADR 0004) to copy
//           the field into a private local before reading — parser
//           ships in v0.2-α; codegen lowering is a future slice.
//
//     Until either lands, the foreground reader pattern is:
//       - Disable interrupts manually (via inline asm, future).
//       - Read the counters.
//       - Re-enable interrupts.
//     This sample documents the producer side in isolation.
//
// Earlier drafts of this sample also shared `bytes_total` and
// `last_byte` between producers — both the v0.1 (write-write) and
// v0.2-β (read-write) verifiers rejected that. The current shape
// — strictly disjoint per-source fields — is what the engine
// proves race-free.
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
