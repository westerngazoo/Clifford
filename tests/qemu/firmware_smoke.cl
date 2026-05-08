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
//   - answer()            slice 1   integer return + arithmetic
//   - clamp(x)            slice 13  if + comparison
//   - sum_to(n)           slice 11+ sigma loop + let-mut accumulator
//   - bit_count(x)        slice 13  if + sigma + bitwise + shift

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
