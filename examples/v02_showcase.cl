// v02_showcase.cl — end-to-end demo of the v0.2 firmware surface.
//
// One file exercises every v0.2 language addition:
//
//   • Decision #12 — #staged automaton + #flush statement      (slices 18-19)
//   • Decision #12 — @shadow Auto.field operator               (slice 24)
//   • Decision #18 — #audit modifier + audit codegen markers   (slices 20-23, 26)
//   • Decision #14 — labelled `sigma` loops + labelled break   (slices 17, 27)
//   • Decision #6  — register-block automaton (#address)        (existing)
//   • Decision #9  — multi-state automaton + state tag          (existing)
//   • Decision #22 — Release / SeqCst memory ordering on transitions (existing)
//   • #atomic: interrupt_critical body wrapping                 (v0.2-ε)
//
// Compile:  cliffordc compile examples/v02_showcase.cl
// Output:   examples/v02_showcase.ll  (LLVM IR text)


// ─── Hardware: an audited UART register block ───────────────────────
//
// `#audit` here means every volatile access to Uart's MMIO
// registers emits a `; audit-wrap site for Uart …` IR
// marker — the future runtime auditing pass turns those into
// `PointerAuditor::validate_load/store` dispatch.

#audit #automaton Uart {
  #address: 0x4000_4000;
  tx_data: u32 #offset: 0x00;
  rx_data: u32 #offset: 0x04;
  status:  u32 #offset: 0x08;
}


// ─── Sensor pose: a #staged automaton built up by the ISR ───────────
//
// The ISR fills shadow fields one at a time, then issues a
// single `#flush Pose;` to commit the whole struct atomically
// to live state. Consumers reading `@snapshot Pose.x` from
// `@fn` only ever see post-flush state. ISR-side
// `@shadow Pose.x` peeks at the in-flight value.

#staged #automaton Pose {
  #states: [Booting, Running];
  x: i32;
  y: i32;
  theta: i32;
  samples_seen: u32;

  // Boot transition: zero everything via shadow + flush, then
  // promote to Running. The state-tag write at transition
  // exit also targets the shadow, so the flush commits both
  // the field updates AND the tag together.
  #transition boot -> Running $ [Release] {
    #mutate Pose { x = 0i32, y = 0i32, theta = 0i32, samples_seen = 0u32 };
    #flush Pose;
  }
}


// ─── Producer ISR: writes shadow, flushes ─────────────────────────────
//
// `#atomic: interrupt_critical;` masks IRQs around the body,
// so the staged updates and the flush memcpy are not
// interleaved with any other handler. The audit markers fire
// for every Uart volatile access (Uart is `#audit`).

#interrupt EncoderTick() #mutates: [Pose, Uart] #priority: HIGH
  #atomic: interrupt_critical;
{
  // Build up a fresh pose in the shadow.
  Pose.x = 100i32;
  Pose.y = 200i32;
  Pose.theta = 45i32;
  Pose.samples_seen += 1u32;

  // Acknowledge the encoder interrupt by writing the UART
  // status register (MMIO write — `audit-wrap site for Uart
  // (volatile_store)` marker fires here).
  Uart.status = 1u32;

  // Commit the shadow to live state — single memcpy.
  #flush Pose;
  return;
}


// ─── Consumer effect: scans recent poses for the first match ────────
//
// `find_first_x_above` uses a labelled `sigma 'scan` plus
// `break 'scan;` to early-exit a nested-loop search. Reads
// come from `@snapshot Pose.x` (live) so the consumer sees
// only post-flush state, never partial.

@fn find_first_x_above(threshold: i32, n: u32) -> u32 $ [Readable] {
  let mut found: u32 = n;
  // Outer loop labelled — inner break can target it.
  sigma 'scan i in 0u32..n {
    sigma _retry in 0u32..3u32 {
      if @snapshot Pose.x > threshold {
        found = i;
        break 'scan;
      }
    }
  }
  return found;
}


// ─── Diagnostic effect: peeks at pending shadow ───────────────────────
//
// Useful for "did the producer start a new pose update?" —
// reads from the shadow global (in-flight values), distinct
// from the snapshot path (live committed values).

@fn pending_x_diff() -> i32 $ [Readable] {
  let pending: i32 = @shadow Pose.x;
  let live: i32 = @snapshot Pose.x;
  return pending - live;
}


// ─── Counter (plain) — sanity that the ordinary path works too ──────
//
// Non-`#staged`, non-`#audit`. Confirms slice-21+ markers
// don't leak into ordinary callables.

#automaton Counter {
  hits: u32;
}

#effect bump_counter() #mutates: [Counter] {
  Counter.hits += 1u32;
  return;
}
