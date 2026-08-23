# RFC-0008: Multi-Platform Support (RV64 + ARM + x86_64)

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

PHANES targets three CPU families from Phase 1: RISC-V 64 (RV64GC,
RV64IMAC), ARM (Cortex-A53/A55/A72/A76 application + Cortex-R52/R82
real-time), and x86_64 (for development hosts and select embedded
servers). Each architecture is a top-level Cargo target; SoC-specific
detail lives in `crates/arch/<arch>/` and `crates/drivers/<sub>/<soc>/`
following the modular pattern (RFC-0002). Sv39 / Sv48 (RV64), VMSAv8-64
(ARM64), and IA-32e (x86_64) are the active paging modes per arch.

## Motivation

Single-architecture systems don't become reference. Linux, Zephyr,
and seL4 are all multi-arch from before they became dominant. Multi-
platform from Phase 1 forces the abstraction quality that makes
porting to the *next* arch trivial — and lets us reach mass markets
without choosing a horse:

- **RISC-V** (VF2, K1, future SiFive Performance, Tenstorrent) — open
  ISA, momentum in robotics + edge AI hardware. Asia-led market.
- **ARM Cortex-A** (i.MX, Rockchip, NVIDIA Jetson, Qualcomm) —
  dominant SBC + automotive infotainment.
- **ARM Cortex-R** (Renesas RH850 R-Car, Infineon Aurix-R, NXP S32R)
  — automotive RT cores, AUTOSAR Adaptive's home turf.
- **x86_64** — dev host platform and the Tesla / Waymo / Zoox high-
  end ECU market.

A robot vendor choosing PHANES doesn't have to bet on one arch. An
automotive customer can run the same kernel on the body controller
(Cortex-R) and the ADAS compute (Cortex-A or x86_64) — with the same
RFCs and verification artefacts.

## Detailed design

### Architecture abstraction layers

```
crates/arch/
├── api/                         ← arch-independent traits
│   └── src/lib.rs               ← trait Csr, trait Mmu, trait Vec, trait Cpu, trait Boot
├── riscv64/
│   ├── Cargo.toml
│   ├── src/lib.rs               ← Sv39 / Sv48 impls; CSR access
│   └── asm/
│       ├── boot.S               ← OpenSBI handoff
│       ├── boot_noopensbi.S     ← Direct M-mode boot
│       ├── trap_entry.S
│       └── context_switch.S
├── arm64/
│   ├── Cargo.toml
│   ├── src/lib.rs               ← VMSAv8-64; SCR_EL3, TTBR0/1_EL1
│   └── asm/
│       ├── boot.S               ← TF-A handoff at EL2 → EL1
│       ├── trap_entry.S         ← VBAR_EL1 vector table
│       └── context_switch.S
├── arm-r/                       ← Cortex-R, MPU-only no MMU
│   ├── Cargo.toml
│   ├── src/lib.rs
│   └── asm/...
└── x86_64/
    ├── Cargo.toml
    ├── src/lib.rs               ← IA-32e, MSRs, GDT/IDT
    └── asm/
        ├── boot.S               ← multiboot2 handoff
        ├── trap_entry.S
        └── context_switch.S
```

### Common arch trait (`crates/arch/api/src/lib.rs`)

```rust
pub trait Cpu {
    type CsrSet;          // arch-specific CSR identifiers
    type Reg;             // word size

    fn hart_id() -> usize;
    fn enable_interrupts();
    fn disable_interrupts();
    fn read_cycle_counter() -> u64;
    fn read_time_counter() -> u64;
    fn fence_full();
    fn fence_i();          // instruction-cache sync
    fn wfi();              // wait for interrupt
}

pub trait Mmu {
    type PageTable;
    type PteFlags;

    fn create_pagetable() -> Result<Self::PageTable, MmuErr>;
    fn map(pt: &mut Self::PageTable, va: usize, pa: usize, flags: Self::PteFlags)
        -> Result<(), MmuErr>;
    fn unmap(pt: &mut Self::PageTable, va: usize) -> Result<(), MmuErr>;
    fn translate(pt: &Self::PageTable, va: usize) -> Option<usize>;
    fn switch(pt: &Self::PageTable, asid: u16);
    fn flush_tlb_va(va: usize, asid: u16);
    fn flush_tlb_all();
}

pub trait Boot {
    /// Boot ROM hand-off entry point.
    fn entry();

    /// Per-hart secondary entry point.
    fn secondary_entry(hart_id: usize);
}
```

The kernel `crate` calls these traits via `arch::cpu::*`. Per-arch
cargo features select which `arch/<arch>/` crate is linked.

### Cargo features

```toml
# kernel/Cargo.toml
[features]
arch-rv64 = ["robot_os_arch_riscv64"]
arch-arm64 = ["robot_os_arch_arm64"]
arch-arm-r = ["robot_os_arch_arm_r"]
arch-x86_64 = ["robot_os_arch_x86_64"]

# Each arch implies one default-config; SoC features compose on top:
qemu     = ["arch-rv64", "uart-ns16550a", ...]
vf2      = ["arch-rv64", "soc-jh7110", ...]
k1       = ["arch-rv64", "soc-spacemit-k1", ...]
imx8mp   = ["arch-arm64", "soc-imx8mp", ...]
rk3588   = ["arch-arm64", "soc-rk3588", ...]
jetson   = ["arch-arm64", "soc-tegra-orin", ...]
ecu-r52  = ["arch-arm-r", "soc-rcar-r52", ...]
host-x86 = ["arch-x86_64", "soc-pc", ...]
```

Exactly one `arch-*` feature is active per build (validated in
`build.rs`).

### Per-arch boot path

| Arch | Stage 0 | Stage 1 | Stage 2 | Notes |
|------|---------|---------|---------|-------|
| RV64 + OpenSBI | BROM | OpenSBI (M-mode) | PHANES (S-mode) | Current path |
| RV64 + no-SBI | BROM | PHANES (M-mode → S-mode via PMP setup) | PHANES (S-mode) | `feature = "no-opensbi"` |
| ARM64 + TF-A | BROM | TF-A (EL3) | U-Boot (EL2) | PHANES enters EL1 |
| ARM-R | BROM | (none) | PHANES bare-metal | MPU-based, no virtualisation |
| x86_64 (BIOS legacy) | BIOS | GRUB (multiboot2) | PHANES (long mode) | dev host |
| x86_64 (UEFI) | UEFI firmware | UEFI app + ExitBootServices | PHANES | future |

### Phase 1 deliverables

| Deliverable | Phase 1 milestone |
|-------------|-------------------|
| `arch/api/` trait crate | week 4 |
| RV64 (existing) refactored to satisfy traits | week 8 |
| `arch/arm64/` skeleton + boots in QEMU virt-arm64 | week 16 |
| Sv39 / VMSAv8-64 abstracted under `Mmu` trait | week 20 |
| Boot reaches "[OTA] Listening" on QEMU virt-arm64 | week 24 |
| `arch/x86_64/` minimal: boots in QEMU q35, prints to UART | week 28 |
| Trap entry + context switch on all 3 archs | week 32 |
| All 5 build configs (qemu / vf2 / k1 / no-ml / no-mmu) plus 3 new (qemu-arm64 / qemu-x86_64 / qemu-arm-r) clean | week 36 |

ARM Cortex-R is Phase 2 (it's a quite different paging story — MPU
only, no MMU).

### Per-arch performance contracts

- **RV64**: WCET timer ISR ≤ 10 µs hardware, ≤ 50 ms QEMU.
- **ARM64**: WCET timer ISR ≤ 8 µs hardware (Cortex-A53 @ 1.2 GHz),
  ≤ 50 ms QEMU.
- **x86_64**: WCET timer ISR ≤ 5 µs hardware. Less RT-friendly
  (microarchitectural variance) — flagged for users; we don't
  recommend x86_64 for hard-RT.
- **ARM-R**: WCET ≤ 2 µs (Cortex-R52 @ 1 GHz, deterministic
  pipeline). The cert sweet spot.

### SoC support matrix (Phase 1–2)

| SoC | Arch | Phase | Reason |
|-----|------|-------|--------|
| QEMU virt | RV64, ARM64, x86_64 | 1 | CI baseline |
| StarFive JH7110 (VF2) | RV64 | 1 | Existing |
| SpacemiT K1 (BPI-F3) | RV64 | 1 | Existing |
| NXP i.MX 8M Plus | ARM64 | 1 | First-class HW-ROT support |
| Rockchip RK3588 | ARM64 | 2 | NPU + production silicon |
| NVIDIA Jetson Orin | ARM64 | 3 | High-end AI |
| Renesas R-Car (Cortex-R52) | ARM-R | 3 | Automotive |
| ESP32-C3 | RV32 | 3 | Microcontroller (out of MMU world) |

## Drawbacks

- **Engineering cost.** ~3 ing-months per new arch. Real but bounded.
- **Test matrix grows.** Every arch × every subsystem feature = many
  CI configurations. Mitigated by HIL CI farm (RFC-0011 supporting).
- **ARM-R has no MMU.** Some kernel paths (page tables, COW, demand
  paging) gate out via `feature = "no-mmu"`. Already partially
  supported.
- **x86_64 deviates.** Different page tables, different IRQ
  controller (APIC vs PLIC vs GIC), different boot. Feels like more
  than one arch's effort. Justified for dev host support and Tesla-
  class ECUs.

## Rationale and alternatives

**Alternative A — RV64 only.** Smaller scope. Closes the door to
ARM-dominated automotive. Rejected for strategic reasons (RFC-0001).

**Alternative B — RV64 + ARM64 only, no x86_64.** Reasonable. We
include x86_64 because: dev host (developers will want to boot in a
host VM), CI hardware (x86_64 servers are everywhere), Tesla-class
ECU customers exist.

**Alternative C — abstract everything to LLVM IR / portable Rust;
no per-arch asm.** Feasible for >90% of code but the boot + trap +
context switch must be asm (call ABI is arch-specific). We accept
~2K lines of asm per arch.

## Prior art

- **Linux kernel** `arch/` directory: same pattern at scale.
- **seL4** ports across x86, ARM, RV64. Per-arch verification done
  per-target.
- **Zephyr** `arch/` with similar abstraction.
- **Tock** has multi-arch support (Cortex-M, RV32, x86 limited).
- **Hubris** is currently ARM-only; PHANES diverges here in scope.

## Unresolved questions

- **Common allocator across archs?** Working assumption: yes. The
  `linked_list_allocator` crate (current dep) is portable.
- **Endian.** All initial targets are little-endian. We assert this
  in the type system; cross-endian support is not in scope.
- **32-bit?** RV32 (ESP32-C3) is in our future. We design pointer-
  width-agnostic where it costs nothing; full RV32 path comes Phase 3.

## Future possibilities

- **Phase 3:** RV32 / Cortex-M for true microcontroller support.
- **Phase 3:** RISC-V Hypervisor (H extension) — multi-OS hosting.
- **Phase 4:** MIPS / SPARC / POWER if specific automotive partners
  need them.
- **Phase 4:** AArch64 EL2 hypervisor mode (run guests under PHANES).
