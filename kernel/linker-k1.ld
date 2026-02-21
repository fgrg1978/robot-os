/*
 * Linker script for Robot OS — SpacemiT K1 (BananaPi BPI-F3)
 *
 * K1 memory layout with U-Boot + OpenSBI:
 *   0x00000000 - 0x001FFFFF : OpenSBI firmware (2 MiB, reserved)
 *   0x00200000 - ...        : Kernel (standard RISC-V load offset)
 *   DDR size: 4-16 GiB from 0x00000000 (LPDDR4X)
 *
 * 8 CPU cores (NUM_CPUS=8 in platform.rs).
 * K1 has native RVV 1.0 (VLEN=256); --features k1 enables RVV code paths.
 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

BOOT_STACK_SIZE = 64K;

MEMORY
{
    RAM (rwx) : ORIGIN = 0x00200000, LENGTH = 256M
}

SECTIONS
{
    . = 0x00200000;

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
