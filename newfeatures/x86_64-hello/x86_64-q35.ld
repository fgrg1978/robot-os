/* PHANES B2.boot — linker script for qemu-system-x86_64 -M q35.
 *
 *   We boot via Multiboot1, not PVH.  Background:
 *     - QEMU's plain `-kernel <ELF>` looks for the multiboot1 magic
 *       (0x1BADB002) in the first 8 KiB of the file; if it finds it
 *       it loads the ELF and enters at `_start` in 32-bit protected
 *       mode with EAX=0x2BADB002 / EBX=info-ptr.
 *     - PVH would have been cleaner (the kernel can stay 64-bit
 *       all the way through), but per QEMU v8.2 `hw/i386/pc.c`, the
 *       PVH loader is only reached through `xen_load_linux()`.
 *       Vanilla `-machine q35` `-kernel` never tries the PVH path,
 *       which is why a correctly-formed XEN_ELFNOTE_PHYS32_ENTRY
 *       note in our binary was ignored on both macOS QEMU 10.1 and
 *       Linux QEMU 8.2.
 *
 *   Load address is 1 MiB (0x10_0000) — historical convention,
 *   matches what Multiboot expects.
 */

ENTRY(_start)

PHDRS {
    text PT_LOAD FLAGS(7);   /* RWX — single load segment for boot */
}

SECTIONS {
    . = 0x100000;

    /* `.multiboot` first so the magic header lands at a small file
     * offset (well within the multiboot1 spec's 8 KiB window). */
    .multiboot : ALIGN(4) {
        KEEP(*(.multiboot))
    } :text

    .text : ALIGN(8) {
        KEEP(*(.text.boot))
        *(.text*)
    } :text

    .rodata : ALIGN(8) { *(.rodata*) } :text
    .data   : ALIGN(8) { *(.data*) }   :text

    .bss : ALIGN(8) {
        __bss_start = .;
        *(.bss*)
        *(COMMON)
        __bss_end = .;
    } :text

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}
