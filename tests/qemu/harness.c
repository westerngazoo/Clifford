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

/* Slice 45 — Decision #18 audit chain end-to-end. */
extern void smoke_audit_poke(uint32_t *p);
extern uint32_t smoke_loads(void);
extern uint32_t smoke_stores(void);

/* Static RAM buffer for the audited read-modify-write. Lives in
 * .bss (zero-initialized by Reset_Handler), so its initial value
 * is 0 and smoke_audit_poke leaves it at 1. `volatile` prevents
 * any optimizer from eliding the round-trip — we want clang to
 * emit the actual load+store that the audit chain wraps. */
static volatile uint32_t smoke_buffer;

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

    /* Slice 45: snapshot the ShadowSanitizer counters, run one
     * audited read-modify-write, then verify the counters bumped
     * by exactly one each and the buffer was actually written.
     * This proves the slice 41+43+44 chain (validate_* call,
     * branch-on-result, audit.ok block continuation) reaches the
     * underlying unsafe op at runtime and that the counting
     * Sanitizer (slice 42) observes both sides. */
    uint32_t loads_before  = smoke_loads();
    uint32_t stores_before = smoke_stores();
    smoke_audit_poke((uint32_t *)&smoke_buffer);
    uint32_t loads_after   = smoke_loads();
    uint32_t stores_after  = smoke_stores();

    /* 9. The audited #unchecked_load incremented the loads counter. */
    if (loads_after - loads_before != 1u) { semihost_exit(9); }

    /* 10. The audited #unchecked_store incremented the stores counter. */
    if (stores_after - stores_before != 1u) { semihost_exit(10); }

    /* 11. The store actually landed: buffer went 0 → 1. Proves
     *     the audit.ok block continued into the real unsafe op
     *     and didn't get short-circuited by the trap path. */
    if (smoke_buffer != 1u) { semihost_exit(11); }

    /* All checks passed. */
    semihost_exit(0);
    return 0; /* unreachable */
}
