/*
 * Linker script for Robot OS — Espressif ESP32-C3 (RV32IMC)
 *
 * ESP32-C3 memory layout:
 *   0x3FC80000 - 0x3FCE0000 : SRAM (data bus, 384 KB)
 *   0x42000000 - ...        : Flash cache (instruction bus, up to 16 MB)
 *
 * For bare-metal (no ESP-IDF), the kernel runs entirely from SRAM.
 * The bootloader loads the binary to SRAM base.
 *
 * NOTE: This is a skeleton linker script for the ESP32-C3 port.
 * Real ESP32-C3 builds require the ESP-IDF toolchain or custom bootloader.
 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

BOOT_STACK_SIZE = 8K;

MEMORY
{
    /* 384 KB SRAM accessible via data bus */
    RAM (rwx) : ORIGIN = 0x3FC80000, LENGTH = 384K
}

SECTIONS
{
    . = 0x3FC80000;

    .text.boot : ALIGN(4)
    {
        *(.text.boot)
    } > RAM

    .text : ALIGN(4)
    {
        *(.text .text.*)
    } > RAM

    .rodata : ALIGN(4)
    {
        *(.rodata .rodata.*)
    } > RAM

    .data : ALIGN(4)
    {
        *(.data .data.*)
        *(.sdata .sdata.*)
    } > RAM

    .bss : ALIGN(4)
    {
        _bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(4);
        _bss_end = .;
    } > RAM

    . = ALIGN(4);
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
