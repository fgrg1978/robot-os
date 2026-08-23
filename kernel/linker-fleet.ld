/*
 * Linker script for PHANES fleet-profile builds.
 *
 * `PROFILE_FLEET` (RFC-0023 / RFC-0026) sizes static tables for
 * 4096 tasks × {channels,pipes,topics,caps} pools.  The kernel's
 * static heap buffer alone is 256 MiB (KERNEL_HEAP_SIZE), and the
 * pipe pool can reach ~16 MiB.  Adding tables + .text + .data the
 * binary lands ~300 MiB worth of in-image RAM, far above the
 * default linker.ld's 8M RAM region.
 *
 * Target hardware: gateway / edge-server boards with ≥ 1 GiB RAM
 * (RK3588 8-32 GiB, AGX Orin 32 GiB, server-class SoCs).  NOT for
 * VF2 / K1 / single-robot SBCs — use linker.ld / linker-vf2.ld /
 * linker-k1.ld for those.
 *
 * Memory layout (same OpenSBI base as default; just a bigger RAM):
 *   0x80000000 - 0x801FFFFF : OpenSBI firmware (2 MiB)
 *   0x80200000 - 0xC0000000 : Kernel (1022 MiB available)
 *
 * 1022 MiB headroom is enough for:
 *   .text  ~3 MiB
 *   .rodata ~2 MiB
 *   .data  ~16 MiB (static tables + caps pool)
 *   .bss   ~512 MiB (256 MiB heap + pipe pool + per-task buffers)
 *   stack  guard at end
 *
 * The linker.ld pattern (single RAM region, sections flow in order)
 * is preserved so build.rs / kernel/src/main.rs see the same
 * `_text_start` / `_bss_end` / `_kernel_end` / `_stack_start`
 * symbols that the default script exports.
 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

BOOT_STACK_SIZE = 64K;

MEMORY
{
    RAM (rwx) : ORIGIN = 0x80200000, LENGTH = 1022M
}

SECTIONS
{
    . = 0x80200000;

    /* Boot code first */
    _text_start = .;
    .text.boot : ALIGN(4K)
    {
        *(.text.boot)
    } > RAM

    /* All other code */
    .text : ALIGN(4K)
    {
        *(.text .text.*)
    } > RAM
    _text_end = .;

    /* Read-only data */
    _rodata_start = .;
    .rodata : ALIGN(4K)
    {
        *(.rodata .rodata.*)
    } > RAM
    _rodata_end = .;

    /* Initialized data */
    _data_start = .;
    .data : ALIGN(4K)
    {
        *(.data .data.*)
        *(.sdata .sdata.*)
    } > RAM
    _data_end = .;

    /* Uninitialized data (BSS) — fleet profile lives here in bulk. */
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
