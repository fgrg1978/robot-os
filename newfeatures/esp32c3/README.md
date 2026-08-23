# ESP32-C3 (RV32) — soporte aparcado

Este directorio guarda los artefactos exclusivos del port a ESP32-C3. El soporte
se retiró del árbol principal de forma deliberada; **no está roto por accidente,
está aparcado**. Aquí queda lo necesario para retomarlo sin arqueología.

> **Antes de retomarlo, lee [REVISAR.md](REVISAR.md)** — inventario de los
> problemas conocidos, incluido el más importante: este port nunca fue
> funcionalmente completo (sin fork/exec/mmap en RV32).

## Por qué se retiró

El port llevaba mucho tiempo sin compilar, y nadie lo notó porque **no estaba en
CI** (`tools/ci_check.sh` cubre `default`, `no-ml`, `no-mmu`, `vf2`, `k1`) ni en
la secuencia de verificación local. El toolchain ni siquiera estaba instalado en
la máquina de desarrollo.

Fallo concreto, reproducido antes de retirarlo:

```
error[E0432]: unresolved import `core::sync::atomic::AtomicU64`
  --> crates/common/src/wcet.rs
```

RV32IMAC no tiene atómicos de 64 bits, y `crates/common/src/wcet.rs` usaba
`AtomicU64` nueve veces. La rotura es anterior al commit `2c0163d`.

Como fallaba en el **primer crate del workspace**, nunca se supo cuánto más había
roto detrás. Revivirlo implica arreglar `wcet.rs` y luego descubrir el resto.

## Qué hay aquí

| Fichero | Origen |
|---|---|
| `linker-esp32c3.ld` | `kernel/linker-esp32c3.ld` |
| `asm/boot_esp32c3.S` | `kernel/src/entry/riscv64/asm/` |
| `asm/trap_entry_esp32c3.S` | `kernel/src/entry/riscv64/asm/` |
| `asm/context_switch_esp32c3.S` | `kernel/src/entry/riscv64/asm/` |
| `esp32c3.config` | `defconfigs/esp32c3.config` |

Movidos con `git mv`, así que `git log --follow` sigue funcionando sobre ellos.

## Cuidado: dos cosas distintas se llaman "ESP32-C3"

Lo aparcado aquí es **el target de compilación RV32** — el kernel corriendo *en*
un ESP32-C3.

No confundir con el **chip acompañante**, que sigue vivo y en uso: la VisionFive 2
habla por UART1 con una placa ESP32-C3 externa que actúa de relé transparente
WiFi/TCP hacia el servidor brain.

```
VF2 ──UART1 (TX/RX/GND)──→ ESP32-C3 ──WiFi/TCP──→ brain server
```

Ese camino es código RV64 vivo en `crates/drivers/src/uart_bridge.rs`, gated en
`feature = "vf2"`, y su firmware sigue en `tools/esp32_bridge/`. **No tocar
ninguno de los dos al retomar este port.**

## Qué NO está aquí

Todo lo demás era **condicional en línea**, no ficheros separables: unas 199
ocurrencias de `esp32c3` repartidas por 40 ficheros compartidos, más 32 stubs
`cfg(target_pointer_width = "32")`. Eso no se puede mover a una carpeta; se
eliminó del árbol.

Distribución de las ramas eliminadas:

| Patrón | Ocurrencias |
|---|---|
| `cfg(not(feature = "esp32c3"))` | 85 |
| `cfg(feature = "esp32c3")` | 67 |
| `cfg(not(any(feature = "vf2", feature = "k1", feature = "esp32c3")))` | 7 |
| `cfg(not(any(feature = "no-mmu", feature = "esp32c3")))` | 5 |

Concentración: `arch-riscv64/csr.rs` (38), `kernel/main.rs` (32),
`drivers/uart.rs` (16), `arch-riscv64/trap.rs` (12).

También se retiraron las declaraciones de la feature `esp32c3` en 11 `Cargo.toml`,
los símbolos `BOARD_ESP32C3` de `Kconfig.platform` / `Kconfig.arch` /
`Kconfig.timing`, su manejo en `crates/limits/build.rs`, y el target `esp32c3`
del `Makefile`.

## Cómo recuperarlo

El estado completo previo a la retirada está en el commit `be0c148`. Para ver
exactamente qué se quitó de un fichero concreto:

```
git diff be0c148 -- crates/arch-riscv64/src/csr.rs
```

Para revivir el port en serio:

1. Arreglar `crates/common/src/wcet.rs` — `AtomicU64` no existe en RV32. Usar
   `AtomicU32`, o `portable-atomic` con la feature de emulación por critical
   section.
2. `rustup target add riscv32imac-unknown-none-elf`.
3. Recuperar las ramas condicionales del diff contra `be0c148`, crate a crate.
4. Compilar e ir descubriendo lo que rompa detrás del primer error.
5. **Añadirlo a `tools/ci_check.sh` el mismo día.** Sin CI se vuelve a pudrir en
   semanas; es exactamente lo que pasó la primera vez.
