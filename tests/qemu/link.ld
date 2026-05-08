/*
 * link.ld — minimal Cortex-M3 linker script for QEMU lm3s6965evb.
 *
 * Memory map matches the Stellaris LM3S6965 reference (and what
 * QEMU emulates with `-M lm3s6965evb`):
 *   FLASH  0x00000000 .. 0x0003FFFF  (256 KB)  read-execute
 *   SRAM   0x20000000 .. 0x2000FFFF  (64 KB)   read-write
 *
 * Layout:
 *   .vectors  -> FLASH at 0x00000000     vector table
 *   .text     -> FLASH                   code
 *   .rodata   -> FLASH                   read-only data
 *   .data     -> SRAM, loaded from FLASH initialized data
 *   .bss      -> SRAM                    zero-initialized data
 *   _stack_top = top of SRAM (stack grows down per ARM AAPCS)
 */

ENTRY(Reset_Handler)

MEMORY
{
    FLASH (rx)  : ORIGIN = 0x00000000, LENGTH = 256K
    SRAM  (rwx) : ORIGIN = 0x20000000, LENGTH = 64K
}

SECTIONS
{
    /* Vector table MUST be at 0x00000000 — Cortex-M reads
     * SP and reset PC from offsets 0 and 4 on power-up. */
    .vectors : {
        KEEP(*(.vectors))
    } > FLASH

    .text : {
        *(.text*)
    } > FLASH

    .rodata : {
        *(.rodata*)
    } > FLASH

    /* .data is initialized at boot by Reset_Handler copying
     * from FLASH (`_data_load_start`) to SRAM (`_data_start`).
     * `> SRAM AT > FLASH` is the canonical way to ask the linker
     * to place the section's run-time address in SRAM and its
     * load-time image in FLASH; we then capture the load address
     * via `LOADADDR(.data)` for the startup copy. */
    .data : {
        _data_start = .;
        *(.data*)
        _data_end = .;
    } > SRAM AT > FLASH
    _data_load_start = LOADADDR(.data);

    /* .bss is zeroed at boot by Reset_Handler. */
    .bss : {
        _bss_start = .;
        *(.bss*)
        *(COMMON)
        _bss_end = .;
    } > SRAM

    /* Stack grows down from the top of SRAM. */
    _stack_top = ORIGIN(SRAM) + LENGTH(SRAM);

    /* Discard linker-generated sections we don't need on bare metal. */
    /DISCARD/ : {
        *(.ARM.attributes)
        *(.ARM.exidx*)
        *(.note*)
        *(.comment)
    }
}
