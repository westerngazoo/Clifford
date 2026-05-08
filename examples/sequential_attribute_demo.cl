// sequential_attribute_demo.cl — small demo of @sequential(A, B)
//
// Shows the v0.2-γ behaviour of the @sequential attribute against
// the GA orthogonality engine. See docs/ortho-sequential-attribute.md
// for the full semantics.
//
// Compile:  cliffordc compile examples/sequential_attribute_demo.cl
// Output:   examples/sequential_attribute_demo.ll

// Two automatons that own disjoint state. Different basis bits
// mean even concurrent writes wouldn't conflict — but the user's
// real intent here is "MotorDriver runs only on the motor task,
// SensorPoll runs only on the sensor task, the runtime guarantees
// they never overlap." The @sequential attribute documents that
// invariant in source.

#automaton MotorDriver {
  speed: u32;
  direction: u32;
}

#automaton SensorPoll {
  reading: u32;
  valid:   u32;
}

// Documentary @sequential: tells the verifier (and future readers)
// that the user has external proof these two automata never run
// concurrently.
@sequential(MotorDriver, SensorPoll);

// Effects that mutate the two automatons. Without @sequential,
// these two effects already wouldn't conflict on the foreground
// thread (effect × effect → never concurrent per §7.3). The
// @sequential is documentary here.
#effect set_speed() #mutates: [MotorDriver] {
  MotorDriver.speed = 100u32;
}

#effect take_reading() #mutates: [SensorPoll] {
  SensorPoll.reading = 42u32;
  SensorPoll.valid   = 1u32;
}

// An interrupt and an effect on different automatons: would be
// flagged as concurrent per §7.3 if they touched the SAME
// automaton's fields. Here they don't, so the wedge is non-zero
// even without @sequential — but the attribute makes the
// non-concurrency guarantee explicit.
#interrupt MOTOR_TICK() #mutates: [MotorDriver] #priority: HIGH {
  MotorDriver.direction = 1u32;
}

#effect read_sensor() #mutates: [SensorPoll] {
  SensorPoll.valid = 0u32;
}
