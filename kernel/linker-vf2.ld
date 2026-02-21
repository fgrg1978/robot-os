/*
 * Linker script for Robot OS — StarFive VisionFive 2 (JH7110)
 *
 * JH7110 memory layout with U-Boot + OpenSBI:
 *   0x40000000 - 0x401FFFFF : OpenSBI firmware (2 MiB, reserved)
 *   0x40200000 - ...        : Kernel (loaded here by U-Boot)
 *   DDR size: 4 GiB (0x40000000 .. 0x140000000) on 4 GB board
 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

BOOT_STACK_SIZE = 64K;

MEMORY
{
    RAM (rwx) : ORIGIN = 0x40200000, LENGTH = 128M
}

SECTIONS
{
    . = 0x40200000;

    .text.boot : ALIGN(4K)
    {
        *(.text.boot)
    } > RAM

    .text : ALIGN(4K)
    {
        *(.text .text.*)
    } > RAM

    .rodata : ALIGN(4K)
    {
        *(.rodata .rodata.*)
    } > RAM

    .data : ALIGN(4K)
    {
        *(.data .data.*)
        *(.sdata .sdata.*)
    } > RAM

    .bss : ALIGN(4K)
    {
        _bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(8);
        _bss_end = .;
    } > RAM

    . = ALIGN(4K);
    _kernel_end = .;

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
