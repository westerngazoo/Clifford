// firmware_smoke.cl — the canonical end-to-end firmware smoke test.
//
// Compiled by cliffordc to LLVM IR, linked with `harness.c` + `startup.c`,
// and run on the QEMU lm3s6965evb Cortex-M3 board with ARM semihosting.
// The harness verifies that each function returns the expected value
// and exits with code 0 on success or 1 on failure.
//
// Why these specific functions: each one exercises a distinct slice of
// the compiler so a regression in any one surface fails the test:
//
//   - answer()            slice 1     integer return + arithmetic
//   - clamp(x)            slice 13    if + comparison
//   - sum_to(n)           slice 11+   sigma loop + let-mut accumulator
//   - bit_count(x)        slice 13    if + sigma + bitwise + shift
//   - smoke_audit_poke(p) slices 20+  #audit chain end-to-end:
//                                       marker → validate_* call →
//                                       branch-on-result → unsafe op
//   - smoke_loads()       slice 42    @snapshot of the auto-included
//   - smoke_stores()      slice 42    counting ShadowSanitizer counters

@fn answer() -> i32 {
  return 42i32;
}

@fn clamp(x: u32) -> u32 {
  if x > 100u32 {
    return 100u32;
  }
  return x;
}

@fn sum_to(n: u32) -> u32 {
  let mut total: u32 = 0u32;
  sigma i in 0u32..n {
    total = total + i;
  }
  return total;
}

@fn bit_count(x: u32) -> u32 {
  // Count the set bits in `x` using sigma + shift + bitwise-and.
  // For `x == 0xFFu32` (8 bits set) this returns 8.
  let mut count: u32 = 0u32;
  sigma i in 0u32..32u32 {
    if ((x >> i) & 1u32) == 1u32 {
      count = count + 1u32;
    }
  }
  return count;
}


// ─── Slice 45: end-to-end #audit smoke test (Decision #18) ────────
//
// Exercises the full v0.2 audit chain on real Cortex-M3 hardware:
//
//   1. The cliffordc CLI sees `#audit` in this source and auto-
//      includes `clifford::audit` (slice 40), bringing in the
//      `PointerAuditor` interface and the counting ShadowSanitizer.
//   2. Codegen marks `smoke_audit_poke` as audit-active because its
//      `#mutates: [SmokeAudited]` clause names an `#audit`
//      automaton (slices 20–22).
//   3. Each unsafe primitive in the body emits the slice-21 marker,
//      a validate_* call with real ptr+size (slices 41+43), and the
//      slice-44 branch-on-result with a trap+unreachable on the
//      false path.
//   4. The counting ShadowSanitizer (slice 42) bumps its loads /
//      stores counter on every validate_* call and always returns
//      true, so the trap path is unreachable in this test — but the
//      IR is emitted, link-resolves, and survives the round-trip
//      through clang+lld onto the LM3S6965 image.
//
// `SmokeAudited` is field-less because the audit-context propagation
// (slices 22+26) keys off `#mutates: [...]` membership, not field
// access. A field-less audited automaton is the minimum surface that
// activates audit at the effect's call sites — which is exactly what
// we want to exercise.

#audit #automaton SmokeAudited { }

#effect smoke_audit_poke(p: &u32) #mutates: [SmokeAudited] {
  // Read-modify-write through `p`. The `#unchecked_load` and
  // `#unchecked_store` each get the full slice-44 wrap shape.
  // The harness passes a pointer into a static RAM buffer so the
  // operation is safe regardless of validate_*'s return value.
  let cur: u32 = #unchecked_load<u32>(p);
  #unchecked_store<u32>(p, cur + 1u32);
  return;
}

@fn smoke_loads() -> u32 $ [Readable] {
  // Single-i32 read of @ShadowSanitizer.state.loads (idx 2).
  // Reflects the total count of audited loads since boot — the
  // harness diffs this counter across the call to smoke_audit_poke.
  return @snapshot ShadowSanitizer.loads;
}

@fn smoke_stores() -> u32 $ [Readable] {
  // Symmetric to smoke_loads — reads idx 3 (stores) of the
  // ShadowSanitizer state struct.
  return @snapshot ShadowSanitizer.stores;
}
