/*
 * startup.c — minimal Cortex-M3 boot for QEMU lm3s6965evb.
 *
 * Provides the vector table at 0x00000000 (first two slots are SP
 * + Reset_Handler, the rest point at a default infinite-loop), a
 * Reset_Handler that zeroes .bss, copies .data from flash to RAM,
 * and calls main(). Also provides the semihosting exit primitive
 * the harness uses to report results back to QEMU.
 *
 * Spec: ARM v7-M Architecture Reference Manual + Stellaris
 * LM3S6965 datasheet (Cortex-M3 with 256 KB flash / 64 KB SRAM at
 * the standard Cortex-M memory map). QEMU emulates this board out
 * of the box with `-M lm3s6965evb`.
 */

#include <stdint.h>

/* Symbols defined by the linker script. */
extern uint32_t _stack_top;
extern uint32_t _data_load_start;
extern uint32_t _data_start;
extern uint32_t _data_end;
extern uint32_t _bss_start;
extern uint32_t _bss_end;

extern int main(void);

void Reset_Handler(void);
void Default_Handler(void);

/* ARM semihosting SYS_EXIT_EXTENDED (0x20) is the Cortex-M-friendly
 * way to terminate the QEMU session with a chosen exit code. The
 * older SYS_EXIT (0x18) doesn't carry a code on Cortex-M.
 *
 * Reference: ARM Semihosting for AArch32 / AArch64, section 5.1.4. */
#define SYS_EXIT_EXTENDED 0x20
#define ADP_Stopped_ApplicationExit 0x20026

void semihost_exit(int code) {
    volatile uint32_t args[2];
    args[0] = ADP_Stopped_ApplicationExit;
    args[1] = (uint32_t)code;
    __asm__ volatile (
        "mov r0, %[op]\n"
        "mov r1, %[block]\n"
        "bkpt 0xab\n"
        :
        : [op]"r"(SYS_EXIT_EXTENDED), [block]"r"(args)
        : "r0", "r1", "memory"
    );
    /* Should not return; loop defensively if QEMU keeps running. */
    while (1) { __asm__ volatile ("wfi"); }
}

/* Reset handler: initialize .bss and .data, then call main(). If
 * main returns, exit with its return code via semihosting. */
void Reset_Handler(void) {
    /* Zero .bss. */
    uint32_t *p = &_bss_start;
    while (p < &_bss_end) { *p++ = 0; }

    /* Copy .data from flash to RAM. */
    uint32_t *src = &_data_load_start;
    uint32_t *dst = &_data_start;
    while (dst < &_data_end) { *dst++ = *src++; }

    int rc = main();
    semihost_exit(rc);
}

/* Default handler for any unimplemented vector — wfi loop. */
void Default_Handler(void) {
    while (1) { __asm__ volatile ("wfi"); }
}

/* Cortex-M3 vector table. Slot [0] is the initial SP value, slot
 * [1] is the Reset_Handler entry point, slots [2..15] are system
 * exception handlers. We point unused slots at Default_Handler. */
__attribute__((section(".vectors"), used))
const void * const vectors[] = {
    (const void *)&_stack_top,  /*  0: Initial Stack Pointer    */
    (const void *)Reset_Handler, /*  1: Reset                    */
    Default_Handler,             /*  2: NMI                      */
    Default_Handler,             /*  3: HardFault                */
    Default_Handler,             /*  4: MemManage                */
    Default_Handler,             /*  5: BusFault                 */
    Default_Handler,             /*  6: UsageFault               */
    0, 0, 0, 0,                  /*  7-10: Reserved              */
    Default_Handler,             /* 11: SVCall                   */
    Default_Handler,             /* 12: Debug Monitor            */
    0,                           /* 13: Reserved                 */
    Default_Handler,             /* 14: PendSV                   */
    Default_Handler,             /* 15: SysTick                  */
};
