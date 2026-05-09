// staged_pose_handoff.cl — Decision #12 demo (slice 18)
//
// The canonical "ISR builds up a multi-field update, then commits
// it atomically" pattern using `#staged #automaton`. Reads of a
// `#staged` automaton come from live state; writes are buffered
// in a shadow struct; an explicit `#flush Pose;` commits the
// shadow into live in one memcpy. Wrap the flush in
// `#atomic: interrupt_critical;` (v0.2-ε) to make the commit
// truly atomic with respect to other interrupts.
//
// This sample exercises:
//   - `#staged #automaton` modifier                            (slice 18)
//   - `#mutate Pose { … }` redirected to shadow                (slice 18)
//   - `Pose.x += 1u32` mutation sugar redirected to shadow     (slice 18)
//   - `#flush Pose;` commits shadow → live via memcpy          (slice 18)
//   - field reads still come from live state                   (slice 18)
//   - interaction with `#atomic: interrupt_critical;`          (v0.2-ε)
//
// Compile:  cliffordc compile examples/staged_pose_handoff.cl
// Output:   examples/staged_pose_handoff.ll


// ─── Pose: a 3-field state buffer the ISR fills incrementally ───────

#staged #automaton Pose {
  x: i32;
  y: i32;
  theta: i32;
}


// ─── ISR: build up a complete pose update, then commit atomically ───
//
// `#atomic: interrupt_critical;` masks IRQs around the body so the
// shadow build-up and the flush memcpy are not interleaved with any
// higher-priority handler. The flush at the end commits all three
// fields together; before the flush, no consumer reading
// `Pose.x` / `Pose.y` / `Pose.theta` from live state observes a
// half-updated pose.

#interrupt EncoderTick() #mutates: [Pose] #priority: HIGH
  #atomic: interrupt_critical;
{
  #mutate Pose { x = 100i32, y = 200i32, theta = 45i32 };
  #flush Pose;
  return;
}


// ─── Same pattern via mutation sugar — equally legal ────────────────

#interrupt EncoderTick2() #mutates: [Pose] #priority: HIGH
  #atomic: interrupt_critical;
{
  Pose.x = 1i32;
  Pose.y = 2i32;
  Pose.theta = 3i32;
  #flush Pose;
  return;
}


// ─── Consumer: reads always come from live state ────────────────────
//
// `read_x` returns `@snapshot Pose.x` from the live global, never
// the shadow. Pre-flush mutations made by an ISR are invisible to
// this reader; post-flush they appear all at once.
//
// `@fn` reads automaton state via `@snapshot` (ADR 0004); the
// @snapshot operator excludes the read from the orthogonality
// engine's race set, which is exactly what we want for "the pose
// after the most recent commit."

@fn read_x() -> i32 $ [Readable] {
  return @snapshot Pose.x;
}

@fn read_y() -> i32 $ [Readable] {
  return @snapshot Pose.y;
}

@fn read_theta() -> i32 $ [Readable] {
  return @snapshot Pose.theta;
}


// ─── A non-staged sibling for contrast ──────────────────────────────
//
// `Counter` is a plain `#automaton` — writes go to live state
// directly. Including it in the same file ensures the codegen
// output mixes the two cases cleanly: only `Pose` gets a `.shadow`
// global; `Counter` does not.

#automaton Counter {
  hits: u32;
}

#effect bump_counter() #mutates: [Counter] {
  Counter.hits += 1u32;
  return;
}
