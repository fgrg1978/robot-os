//! PHANES B2.boot — minimal x86_64 boot binary (Multiboot1).
//!
//! Run with:  make qemu-x86_64-hello
//!
//! Boots on `qemu-system-x86_64 -M q35`, transitions from 32-bit
//! protected mode to 64-bit long mode using identity-mapped
//! paging (one 1 GiB region via 2 MiB pages), then prints two
//! lines via the COM1 serial port (0x3F8) and halts.
//!
//! # Boot sequence
//!
//! 1. QEMU sees Multiboot1 magic in the ELF, drops us into
//!    32-bit protected mode with `EAX = 0x2BADB002`, `EBX =
//!    multiboot_info_ptr`.
//! 2. `_start` (32-bit) zeroes the page-table area, builds PML4
//!    → PDP → PD with 512 × 2 MiB identity-mapped entries
//!    covering [0, 1 GiB), enables PAE, sets EFER.LME, enables
//!    paging in CR0, far-jumps via a 64-bit GDT into long mode.
//! 3. `long_mode_start` sets up segment selectors, switches to
//!    the 64-bit stack, and calls `rust_main`.
//! 4. `rust_main` prints via COM1, then returns; trampoline
//!    parks on `hlt`.

#![no_std]
#![no_main]

use core::arch::global_asm;

global_asm!(r#"
// Explicit Intel syntax; rust-lld's assembler accepts AT&T by
// default so we set it once at the top and reset at the bottom.
.intel_syntax noprefix

// ── Multiboot1 header (NOT PVH).
//
// STATUS 2026-05-17: STILL DOES NOT BOOT on macOS QEMU 10.1
// — SeaBIOS comes up instead of our kernel.  Parked, not
// blocking; see `rfcs/item-2-kernel-cross-arch-plan.md` for
// the actual critical path.
//
// What we tried in this session (all failed silently):
//   1. PVH  (XEN_ELFNOTE_PHYS32_ENTRY note in PT_NOTE segment).
//      Reaches `x86_load_linux()` (pc_memory_init calls it
//      unconditionally for `-kernel`) but `pcmc->pvh_enabled`
//      is false on default q35.
//   2. Multiboot2 — plain `-kernel` doesn't recognise it; GRUB
//      only.
//   3. Multiboot1 with 64-bit ELF — explicit reject:
//      "Cannot load x86-64 image, give a 32bit one."
//   4. Multiboot1 with the binary post-processed to ELFCLASS32
//      + EM_386 via `llvm-objcopy -O elf32-i386`.  Magic
//      verified at file offset 0x1000 (within the 8 KiB scan
//      window), Linux HdrS at 0x202 is zero (so not mis-
//      classified as Linux) — yet still falls through to BIOS.
//
// QEMU 10.1 has no `multiboot*` trace events, so the next
// debug session needs a 30-line known-good mb1 asm hello world
// to confirm whether the loader works at all on this install
// before tweaking ours.  Layout per multiboot1 spec §3:
//   magic    u32 = 0x1BADB002
//   flags    u32 = 0  (no module / mem-info request)
//   checksum u32 = -(magic + flags) = 0xE4524FFE
.section .multiboot, "a"
.balign 4
multiboot_header:
    .long 0x1BADB002              // magic
    .long 0x00000000              // flags
    .long 0xE4524FFE              // checksum (-(magic + flags))

// ── 32-bit boot trampoline ─────────────────────────────────────
.section .text.boot
.globl _start
.code32
_start:
    cli

    // Save Multiboot info pointer for rust_main (RDI on 64-bit).
    mov esi, ebx

    // 32-bit stack at the top of the boot stack area.
    lea esp, [_boot_stack_top]

    // Zero the page-table region: PML4 + PDP + PD = 3 × 4 KiB.
    lea edi, [_pml4]
    xor eax, eax
    mov ecx, 3072    // 3 × 4096 / 4 dwords
    rep stosd

    // PML4[0] -> PDP, present + writable.
    lea edi, [_pml4]
    lea eax, [_pdp]
    or  eax, 0x03
    mov [edi], eax

    // PDP[0] -> PD, present + writable.
    lea edi, [_pdp]
    lea eax, [_pd]
    or  eax, 0x03
    mov [edi], eax

    // PD[0..512] -> identity-mapped 2 MiB pages, present + writable
    // + PS (large page).
    lea edi, [_pd]
    mov eax, 0x00000083   // base 0, P|RW|PS
    mov ecx, 512
2:
    mov [edi], eax
    mov dword ptr [edi + 4], 0
    add eax, 0x00200000   // next 2 MiB physical
    add edi, 8
    loop 2b

    // CR3 = PML4 phys.
    lea eax, [_pml4]
    mov cr3, eax

    // CR4.PAE = 1.
    mov eax, cr4
    or  eax, 0x20
    mov cr4, eax

    // EFER.LME = 1 (MSR 0xC0000080).
    mov ecx, 0xC0000080
    rdmsr
    or  eax, 0x100
    wrmsr

    // CR0.PG | CR0.PE.
    mov eax, cr0
    or  eax, 0x80000001
    mov cr0, eax

    // Load 64-bit GDT + far-jump to 64-bit code segment. LLVM's
    // assembler doesn't accept the `jmp seg:offset` immediate
    // form, so we synthesise it: push the new CS, push the
    // target EIP, then `retf` pops them into CS:EIP.
    lgdt [_gdt_ptr]
    lea  eax, [_long_mode_start]
    push 0x08              // new CS
    push eax               // new EIP
    retf

// ── 64-bit entry ───────────────────────────────────────────────
.code64
_long_mode_start:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    // 64-bit stack (same physical bytes as the 32-bit one).
    lea rsp, [_boot_stack_top]

    // Zero .bss (loader doesn't do this for us).
    lea rdi, [__bss_start]
    lea rcx, [__bss_end]
    sub rcx, rdi
    xor rax, rax
    rep stosb

    // Pass the multiboot info pointer as the first arg (RDI).
    mov rdi, rsi

    call rust_main

_park:
    hlt
    jmp _park

// ── GDT: null, 64-bit code (RX, L=1), 64-bit data (RW). ────────
// `.globl` for both because rust-lld refuses R_X86_64_64
// relocations against local symbols. The `lgdt` operand uses a
// 32-bit base (4 bytes) because we're still in `.code32` when
// we load it — that's the legal 6-byte form in 32-bit protected
// mode and it avoids the 64-bit-absolute relocation issue.
.section .rodata
.align 8
.globl _gdt
.globl _gdt_end
_gdt:
    .quad 0
    .quad 0x00AF9A000000FFFF    // code: P|S|X|R, L=1, D=0
    .quad 0x00AF92000000FFFF    // data: P|S|W
_gdt_end:

.align 4
.globl _gdt_ptr
_gdt_ptr:
    .word _gdt_end - _gdt - 1
    .long _gdt

// ── Page tables + boot stack live in .bss. ─────────────────────
.section .bss
.align 4096
.globl _pml4
.globl _pdp
.globl _pd
_pml4: .skip 4096
_pdp:  .skip 4096
_pd:   .skip 4096

.align 16
.globl _boot_stack_bottom
.globl _boot_stack_top
_boot_stack_bottom: .skip 16384
_boot_stack_top:

// Reset for any subsequent global_asm! blocks or inline asm
// that expects AT&T (the default).
.att_syntax
"#);

// ──────────────────────────────────────────────────────────────────────────
// COM1 (0x3F8) — minimal 16550 UART for serial console.
// ──────────────────────────────────────────────────────────────────────────

const COM1_DATA: u16 = 0x3F8;
const COM1_LSR:  u16 = 0x3FD;
const COM1_LSR_THRE: u8 = 1 << 5; // Transmitter Holding Register Empty.

#[inline]
fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") val,
            options(nostack, preserves_flags),
        );
    }
}

#[inline]
fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") v,
            options(nostack, preserves_flags),
        );
    }
    v
}

fn com1_putc(c: u8) {
    while inb(COM1_LSR) & COM1_LSR_THRE == 0 {}
    outb(COM1_DATA, c);
}

fn com1_puts(s: &str) {
    for b in s.bytes() {
        com1_putc(b);
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Rust entry.
// ──────────────────────────────────────────────────────────────────────────

#[no_mangle]
extern "C" fn rust_main(_multiboot_info: u64) {
    com1_puts("[X86_64] hello from PHANES on q35\n");

    com1_puts("[X86_64] arch-api ARCH_ID: ");
    com1_puts(robot_os_arch_api::arch_name(
        robot_os_arch_x86_64::ARCH_ID,
    ));
    com1_puts("\n");

    let hart = robot_os_arch_x86_64::cpu::hart_id();
    com1_puts("[X86_64] hart_id() (init APIC ID): 0x");
    let nibble = (hart & 0xF) as u8;
    let hex = if nibble < 10 {
        b'0' + nibble
    } else {
        b'a' + (nibble - 10)
    };
    com1_putc(hex);
    com1_puts("\n");

    // B0.4 / B2.vec smoke — call the SSE2 dot_f32 via the
    // Vector trait. SSE2 is x86_64 ABI baseline so no probe is
    // needed; we just verify the codegen + linkage are clean.
    use robot_os_arch_api::Vector;
    let a = [1.0_f32, 2.0, 3.0, 4.0, 5.0];
    let b = [5.0_f32, 4.0, 3.0, 2.0, 1.0];
    let dot = robot_os_arch_x86_64::api_impl::X86_64_IMPL.dot_f32(&a, &b);
    com1_puts("[X86_64] Vector::dot_f32 = ");
    // Expected: 5+8+9+8+5 = 35 (== 0x23).
    let i = dot as i32;
    let nibble = (i & 0xF) as u8;
    let hi = ((i >> 4) & 0xF) as u8;
    let to_hex = |n: u8| if n < 10 { b'0' + n } else { b'a' + (n - 10) };
    com1_puts("0x");
    com1_putc(to_hex(hi));
    com1_putc(to_hex(nibble));
    com1_puts(" (expect 0x23)\n");

    com1_puts("[X86_64] parking on HLT\n");
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    com1_puts("[X86_64] PANIC\n");
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}
