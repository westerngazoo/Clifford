/*
 * crc32_host.c — host harness for the pure-functional crc32.cl.
 *
 * Demonstrates that Clifford-generated LLVM IR composes with
 * standard host C code — no firmware runtime, no startup, no
 * linker script. Compile with:
 *
 *     cliffordc compile examples/crc32.cl
 *     clang examples/crc32.ll examples/crc32_host.c -o crc32
 *     ./crc32
 *
 * Expected output (the three well-known CRC-32/ISO-HDLC vectors):
 *     crc32 of "123456789" = 0xcbf43926  (PASS)
 *     crc32 of "a"         = 0xe8b7be43  (PASS)
 *     crc32 of "" (empty)  = 0x00000000  (PASS)
 *
 * Exits with code 0 on all passes, 1 on any failure.
 */

#include <stdio.h>
#include <stdint.h>
#include <string.h>

extern uint32_t crc32_init(void);
extern uint32_t crc32_byte(uint32_t crc, uint8_t byte);
extern uint32_t crc32_finalize(uint32_t crc);
extern uint32_t crc32_test_vector(void);

static uint32_t crc32_string(const char *s) {
    uint32_t c = crc32_init();
    for (size_t i = 0; s[i] != '\0'; i++) {
        c = crc32_byte(c, (uint8_t)s[i]);
    }
    return crc32_finalize(c);
}

static int report(const char *label, uint32_t actual, uint32_t expected) {
    int ok = (actual == expected);
    printf("crc32 of %-12s = 0x%08x  (%s)\n",
           label, actual, ok ? "PASS" : "FAIL");
    return ok;
}

int main(void) {
    int all_passed = 1;

    /* The canonical CRC-32 test vector — every implementation
     * targeting CRC-32/ISO-HDLC must produce this value. */
    all_passed &= report("\"123456789\"",
                         crc32_test_vector(),  /* exercises the fused entry point */
                         0xcbf43926);

    /* Single-byte input — exercises crc32_init + one fold + finalize.
     * Standard CRC-32 of single byte 'a' (0x61) = 0xE8B7BE43. */
    all_passed &= report("\"a\"",
                         crc32_string("a"),
                         0xe8b7be43);

    /* Empty string — exercises the "no bytes folded" edge case.
     * crc32_finalize(crc32_init()) = 0xFFFFFFFF ^ 0xFFFFFFFF = 0. */
    all_passed &= report("\"\" (empty)",
                         crc32_string(""),
                         0x00000000);

    return all_passed ? 0 : 1;
}
