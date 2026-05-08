// buffer_init_sigma.cl — sigma loop demo (slice 11)
//
// Initialize a 64-byte ring-buffer's storage and counters using
// the new `sigma i in lo..hi { … }` bounded-iteration construct.
// The loop variable carries an implicit `bounded<lo, hi>` refinement
// type per §5.8 (the bounds tracker doesn't elide checks yet, but
// the typing side has the information ready for when it lands).
//
// This sample exercises:
//   - sigma loop with literal half-open range          (slice 11)
//   - sigma loop with dynamic bound (effect parameter) (slice 11)
//   - indexed field write inside a sigma body          (slice 5)
//   - mutation sugar `+=` inside a sigma body          (slice 3)
//   - multi-state automaton + state-tag tracking       (slice 9)
//   - register-block volatile MMIO write               (slice 4)
//
// Compile:  cliffordc compile examples/buffer_init_sigma.cl
// Output:   examples/buffer_init_sigma.ll  (~2-3 KB of IR)


// ─── Hardware: a UART for sending the cleared marker ────────────────

#automaton Uart {
  #address: 0x4000_4000;
  tx_data: u32 #offset: 0x00;
}


// ─── Telemetry buffer with state ────────────────────────────────────

#automaton RingBuffer {
  #states: [Uninitialized, Ready];

  // 64-byte storage. Initialized to zero in the `boot` transition.
  storage: [u8; 64];

  // How many bytes have been zeroed so far. Demonstrates the loop
  // variable threading into a counter.
  zeroed: u32;

  // Boot: zero the storage AND tally the writes. Transition to
  // Ready when the loop completes. The Release fence on the
  // transition makes the initialization visible to subsequent
  // consumers (e.g. an interrupt that reads from `storage`).
  #transition boot -> Ready $ [Release] {
    sigma i in 0u32..64u32 {
      #mutate RingBuffer { storage[i] = 0u8 };
      RingBuffer.zeroed += 1u32;
    }
    // After the sigma loop falls through, the transition's
    // implicit ret + tag-write + release fence happens automatically.
  }
}


// ─── Effect using a dynamic loop bound ──────────────────────────────
//
// Send `count` copies of byte 0x55 ('U' for "Uart") to the TX
// register, demonstrating sigma over an effect-parameter bound.

#effect spam_marker(count: u32) #mutates: [Uart] {
  sigma i in 0u32..count {
    Uart.tx_data = 85u32;   // 0x55
  }
  return;
}


// ─── Effect using an inclusive bound ────────────────────────────────
//
// Demonstrates `..=` lowering (compare opcode is `ule` instead of
// `ult`). Inclusive ranges are useful for "process indices 1
// through N inclusive" patterns.

#effect blast_marker_inclusive() #mutates: [Uart] {
  sigma i in 1u32..=8u32 {
    Uart.tx_data = 65u32;   // 'A'
  }
  return;
}


// ─── Local mut accumulator inside a sigma loop (slice 12) ───────────
//
// Sum the integers from -5 to 5 inclusive. Demonstrates:
//   - signed-range lowering (`icmp sle`, `add nsw`)            (slice 11)
//   - local mut accumulator with `let mut` + reassignment      (slice 12)
//   - load-add-store of the local on every iteration           (slice 12)

#effect sum_signed_range() -> i32 #mutates: [] {
  let mut total: i32 = 0i32;
  sigma i in -5i32..=5i32 {
    total = total + i;
  }
  return total;
}
