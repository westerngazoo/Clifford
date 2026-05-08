/*
 * harness.c — invokes each Clifford-generated function and reports
 * the first failure via ARM semihosting SYS_EXIT_EXTENDED.
 *
 * Linked with `firmware_smoke.ll` (compiled via clang
 * --target=thumbv7m-none-eabi) and `startup.c` to produce a
 * self-contained Cortex-M3 ELF that runs on QEMU lm3s6965evb.
 *
 * Exit code 0  -> all checks passed.
 * Exit code N  -> check N failed (1-indexed).
 */

#include <stdint.h>

extern int32_t answer(void);
extern uint32_t clamp(uint32_t x);
extern uint32_t sum_to(uint32_t n);
extern uint32_t bit_count(uint32_t x);

void semihost_exit(int code);

int main(void) {
    /* 1. answer() should return 42. */
    if (answer() != 42) { semihost_exit(1); }

    /* 2. clamp(50) should pass through. */
    if (clamp(50u) != 50u) { semihost_exit(2); }

    /* 3. clamp(200) should clamp to 100. */
    if (clamp(200u) != 100u) { semihost_exit(3); }

    /* 4. sum_to(0) is 0 (empty range). */
    if (sum_to(0u) != 0u) { semihost_exit(4); }

    /* 5. sum_to(10) is 0+1+...+9 = 45. */
    if (sum_to(10u) != 45u) { semihost_exit(5); }

    /* 6. bit_count(0) is 0. */
    if (bit_count(0u) != 0u) { semihost_exit(6); }

    /* 7. bit_count(0xFF) is 8 (eight set bits). */
    if (bit_count(0xFFu) != 8u) { semihost_exit(7); }

    /* 8. bit_count(0xFFFFFFFF) is 32 (all bits set). */
    if (bit_count(0xFFFFFFFFu) != 32u) { semihost_exit(8); }

    /* All checks passed. */
    semihost_exit(0);
    return 0; /* unreachable */
}
