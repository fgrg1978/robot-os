/*
 * Linker script for Robot OS (Rust)
 * Target: RISC-V 64-bit (QEMU virt)
 *
 * Memory layout with OpenSBI:
 *   0x80000000 - 0x801FFFFF : OpenSBI firmware (2MB)
 *   0x80200000 - ...        : Kernel (loaded here)
 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

BOOT_STACK_SIZE = 64K;

MEMORY
{
    RAM (rwx) : ORIGIN = 0x80200000, LENGTH = 8M
}

SECTIONS
{
    . = 0x80200000;

    /* Boot code first */
    .text.boot : ALIGN(4K)
    {
        *(.text.boot)
    } > RAM

    /* All other code */
    .text : ALIGN(4K)
    {
        *(.text .text.*)
    } > RAM

    /* Read-only data */
    .rodata : ALIGN(4K)
    {
        *(.rodata .rodata.*)
    } > RAM

    /* Initialized data */
    .data : ALIGN(4K)
    {
        *(.data .data.*)
        *(.sdata .sdata.*)
    } > RAM

    /* Uninitialized data (BSS) */
    .bss : ALIGN(4K)
    {
        _bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(8);           /* ensure _bss_end is 8-byte aligned for sd-based clear */
        _bss_end = .;
    } > RAM

    . = ALIGN(4K);
    _kernel_end = .;

    /* Stack grows downward from end of RAM */
    _stack_end   = ORIGIN(RAM) + LENGTH(RAM);
    _stack_start = _stack_end - BOOT_STACK_SIZE;

    /DISCARD/ :
    {
        *(.comment)
        *(.eh_frame)
        *(.note*)
    }
}

ASSERT(_kernel_end < _stack_start, "Kernel image overflows into boot stack!")
