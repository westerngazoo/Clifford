// traffic_classifier.cl — slice 13 / if-else demo
//
// A classifier effect that returns one of three traffic levels
// based on the running byte count from the telemetry automaton:
//   0 = idle      (no bytes seen)
//   1 = light     (1..=99 bytes)
//   2 = moderate  (100..=999 bytes)
//   3 = heavy     (1000+ bytes)
//
// Demonstrates:
//   - statement-form `if` with no else (the early-return style)  (slice 13)
//   - `else if` chain                                            (slice 13)
//   - integer comparison ops `==`, `<`                           (slice 13)
//   - field-access in condition position                         (slice 3)
//   - multi-state automaton with state tracking                  (slice 9)
//   - `Auto@state` read in a branch                              (slice 9)
//
// Compile:  cliffordc compile examples/traffic_classifier.cl
// Output:   examples/traffic_classifier.ll  (~1.5 KB of IR)


#automaton Telemetry {
  #states: [Idle, Active];
  bytes_total: u32;

  #transition record -> Active {
    Telemetry.bytes_total += 1u32;
  }
}


// Three-way classifier using early returns in if-blocks.
//
// Each `if cond { return X; }` lowers to a conditional branch:
// false-path skips the return and falls through to the next
// check. The final `return` is the catch-all for "all guards
// failed."

#effect classify() -> u8 #mutates: [Telemetry] {
  if Telemetry.bytes_total == 0u32 {
    return 0u8;
  }
  if Telemetry.bytes_total < 100u32 {
    return 1u8;
  }
  if Telemetry.bytes_total < 1000u32 {
    return 2u8;
  }
  return 3u8;
}


// Same logic written with if / else if chain — produces a more
// nested CFG but the same observable behavior. The `else if`
// form is a single statement-form `if` with a synthetic else
// block, recursively.

#effect classify_chained() -> u8 #mutates: [Telemetry] {
  if Telemetry.bytes_total == 0u32 {
    return 0u8;
  } else if Telemetry.bytes_total < 100u32 {
    return 1u8;
  } else if Telemetry.bytes_total < 1000u32 {
    return 2u8;
  } else {
    return 3u8;
  }
}


// Conditional update of a local accumulator. Demonstrates
// slice-12 (`let mut`) interacting with slice-13 (`if`).

@fn clamp_to_8(x: u32) -> u32 {
  let mut result: u32 = x;
  if result > 8u32 {
    result = 8u32;
  }
  return result;
}
