// crc32.cl — pure-functional CRC-32 (IEEE 802.3 / Ethernet / zip / png)
//
// Demonstrates that Clifford is not an embedded-only language.
// This file uses ZERO `#`-layer constructs:
//   - no #automaton / no shared state
//   - no #effect / no #interrupt / no #transition
//   - no register-block MMIO / no ARM-specific primitives
//
// What it IS: three pure `@fn`s that link against any host program
// (Linux/macOS/Windows/embedded — the IR is target-agnostic). A
// caller seeds with `crc32_init()`, folds bytes through
// `crc32_byte(crc, byte)`, then calls `crc32_finalize(crc)` to get
// the reported value.
//
// Algorithm: CRC-32/ISO-HDLC. Reflected polynomial 0xEDB88320 (the
// LSB-first form of the canonical 0x04C11DB7). Reflected input,
// reflected output, all-ones init, final XOR with all-ones — the
// most common variant, used by gzip, png, zip, Ethernet FCS.
//
// Test vectors (host-verifiable):
//   crc32_finalize(crc32_init())                              == 0x00000000
//   crc32 of empty string                                     == 0x00000000
//   crc32 of "a"          (one byte 'a' = 0x61)               == 0xE8B7BE43
//   crc32 of "123456789"  (the canonical CRC-32 test vector)  == 0xCBF43926
//
// Compile:  cliffordc compile examples/crc32.cl
// Output:   examples/crc32.ll  (~1 KB of pure-functional IR)
//
// What this exercises across slices:
//   - @fn purity                              (slice 1)
//   - integer arithmetic + casts (`as u32`)   (slices 1, 7)
//   - `let mut` + assignment                  (slice 12)
//   - sigma loops                             (slice 11)
//   - if / else                               (slice 13)
//   - comparison + bitwise + shift            (slice 13)


/// Initial CRC seed: all-ones (0xFFFFFFFF). Standard CRC-32 starts
/// here so leading zero bytes affect the final value.
@fn crc32_init() -> u32 {
  return 0xFFFFFFFFu32;
}

/// Fold one byte into the running CRC. Reflected variant: process
/// LSB first using polynomial 0xEDB88320. Eight shift-and-conditional-
/// xor steps per byte.
@fn crc32_byte(crc: u32, byte: u8) -> u32 {
  let mut c: u32 = crc ^ (byte as u32);
  sigma bit in 0u32..8u32 {
    if (c & 1u32) == 1u32 {
      c = (c >> 1u32) ^ 0xEDB88320u32;
    } else {
      c = c >> 1u32;
    }
  }
  return c;
}

/// Finalize: XOR with all-ones to produce the reported CRC.
@fn crc32_finalize(crc: u32) -> u32 {
  return crc ^ 0xFFFFFFFFu32;
}

/// Convenience: compute CRC-32 of the canonical test vector
/// "123456789" (9 ASCII bytes 0x31..0x39). Returns 0xCBF43926.
///
/// This wraps init + 9 byte-folds + finalize in one entry point a
/// host harness can call to verify the implementation without
/// needing to manage the loop state itself.
@fn crc32_test_vector() -> u32 {
  let mut c: u32 = crc32_init();
  c = crc32_byte(c, 49u8);   // '1' = 0x31
  c = crc32_byte(c, 50u8);   // '2'
  c = crc32_byte(c, 51u8);   // '3'
  c = crc32_byte(c, 52u8);   // '4'
  c = crc32_byte(c, 53u8);   // '5'
  c = crc32_byte(c, 54u8);   // '6'
  c = crc32_byte(c, 55u8);   // '7'
  c = crc32_byte(c, 56u8);   // '8'
  c = crc32_byte(c, 57u8);   // '9'
  return crc32_finalize(c);
}
