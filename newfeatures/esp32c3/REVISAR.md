# Problemas conocidos del port ESP32-C3 — leer antes de retomarlo

Nota de revisión escrita al retirar el soporte (2026-08-18). Todo lo de aquí se
descubrió con el código delante, no es especulación. Ordenado por lo que más
cuesta resolver.

---

## 1. Bloqueantes de compilación

### 1.1 `AtomicU64` no existe en RV32 — **rompe el primer crate**

```
error[E0432]: unresolved import `core::sync::atomic::AtomicU64`
  --> crates/common/src/wcet.rs
```

RV32IMAC no tiene atómicos de 64 bits. `crates/common/src/wcet.rs` usa `AtomicU64`
nueve veces (`min_cycles`, `max_cycles`, `total_cycles`, `start`, …).

Opciones: bajar a `AtomicU32` (pierde rango en contadores de ciclos, que en un
core a 160 MHz desbordan en ~27 s), o usar `portable-atomic` con emulación por
sección crítica (coste en cada acceso).

### 1.2 Profundidad desconocida

Como falla en `robot_os_common`, que es de los primeros del grafo, **nunca se ha
visto qué hay detrás**. Puede ser un error más o cincuenta. Presupuestar
descubrimiento, no solo arreglo.

---

## 2. El port nunca fue funcionalmente completo

Esto es lo más importante y lo menos evidente: los stubs RV32 **nunca se
compilaron** (el target por defecto es `riscv64imac`), así que no eran código
probado — eran huecos.

Estas syscalls devolvían `-1` incondicionalmente en RV32:

| Syscall | Implicación |
|---|---|
| `sys_fork`, `sys_fork_cow` | sin creación de procesos |
| `sys_exec`, `sys_execpath` | sin carga de binarios |
| `sys_mmap`, `sys_munmap` | sin mapeo de memoria |
| `sys_alloc_demand` | sin reserva perezosa |
| `sys_brk` | sin heap de usuario |

Y en `crates/sched/src/lib.rs` eran stubs vacíos: `copy_from_user` / `copy_to_user`
(copias identidad sin validar nada), `exec_user`, `take_pending_exec`,
`sret_to_user`, `ExecContext`.

**Conclusión honesta: en ESP32-C3 esto no era un kernel multiproceso.** Era un
ejecutivo monotarea con drivers. Si el objetivo al retomarlo es paridad de
funciones, el trabajo real es mucho mayor que arreglar la compilación.

---

## 3. Problemas de diseño, no de código

### 3.1 La herencia de prioridad por spin no puede funcionar en un solo hart

`MAX_CPUS = 1` en esp32c3. `PiMutex` es un mutex **de espera activa**: el waiter
de mayor prioridad nunca cede la CPU, así que el owner boosteado no llega a
ejecutarse nunca y la contención cuelga. No es un bug arreglable en `pi_mutex.rs`
— la solución es reconstruirlo sobre `WaitQueue`.

Aplica igual en RV64 si dos tareas en contienda comparten hart, así que conviene
resolverlo de todos modos.

### 3.2 Sin MMU, el modelo de seguridad es otro

`NO_MMU=y` obligatorio. Se caen COW, demand paging, vdso y el aislamiento
usuario/kernel por tablas de página. `copy_from_user` no puede validar nada:
en RV32 era literalmente `copy_nonoverlapping` sin comprobar el puntero. Cualquier
razonamiento de seguridad hecho para RV64 **no se traslada**.

### 3.3 Arranque en M-mode sin OpenSBI

Camino de arranque completamente distinto: `boot_esp32c3.S`, `trap_entry_esp32c3.S`,
`context_switch_esp32c3.S` (los tres en `asm/` de esta carpeta), más 13 funciones
CSR de M-mode y sus constantes `MSTATUS_*`/`MIE_*` en `arch-riscv64/src/csr.rs`.
Es una segunda personalidad del kernel, no una variante menor.

---

## 4. Presupuesto de recursos, muy justo

- **400 KiB SRAM** reales. `RAM_SIZE` estaba puesto a 1 MiB como sobreestimación
  que el linker recortaba con el layout plano.
- `FALLBACK_MEM_SIZE = 384 * 1024`
- `STACK_SIZE = 2 * 1024` por tarea (frente al valor por defecto en RV64)
- `MAX_TASKS = 8` (frente a 64)
- `PAGE_SIZE` propio, distinto del de RV64
- WiFi: `crates/drivers/src/wifi.rs` era esencialmente un stub. `WIFI_READY` solo
  se ponía a `true` dentro del bloque esp32c3, así que en cualquier otro build el
  driver ya reportaba `Off` siempre. No había pila WiFi real.
- Sin `rdtime`: `procfs` sustituía el uptime por 0.

---

## 5. Deuda de proceso — la causa raíz

**No estaba en CI.** `tools/ci_check.sh` cubre `default`, `no-ml`, `no-mmu`, `vf2`,
`k1`. Nunca esp32c3. Por eso se pudrió sin que nadie lo notara, y el toolchain
(`riscv32imac-unknown-none-elf`) ni siquiera estaba instalado en la máquina de
desarrollo.

**Si se retoma, entra en CI el mismo día o vuelve a pasar.** No es una
recomendación de estilo: es exactamente lo que ocurrió.

---

## 6. Trampas al retomar

### 6.1 Dos cosas distintas se llaman ESP32-C3

El **chip acompañante** sigue vivo y en producción: placa externa en el UART1 de
la VisionFive 2 que hace de relé transparente WiFi/TCP hacia el brain.

```
VF2 ──UART1 (TX/RX/GND)──→ ESP32-C3 ──WiFi/TCP──→ brain server
```

Vive en `crates/drivers/src/uart_bridge.rs` (gated en `feature = "vf2"`),
`crates/drivers/src/platform.rs`, y su firmware en `tools/esp32_bridge/`.
**No tocarlo.** Al retirar el port llegué a moverlo por error.

### 6.2 Buscar por `esp32`, nunca por `esp32c3`

El patrón sin guion no casa con `ESP32-C3`. Así se coló un bug real durante la
retirada: en `crates/shell/src/lib.rs` el display de plataforma OTA tenía
`_ => "ESP32-C3"` como *fallback* del match, de modo que al quitar el ID 3
cualquier byte de plataforma desconocido se habría etiquetado como ESP32-C3.

### 6.3 El compilador no valida la mitad del trabajo

Los stubs RV32 no se compilan con el target por defecto. Quedarse con un stub
`-1` en vez de la implementación real da **build verde igualmente** — firmas
idénticas, variante muerta que nunca se type-checkea. Al reintroducirlos, la
verificación tiene que ser textual o con el target RV32 realmente instalado.

### 6.4 Formato de cable OTA

Se retiró el ID de plataforma **3**. Los IDs 0/1/2 (qemu/vf2/k1) no cambiaron.
Reintroducirlo exige tocar a la vez el kernel (`crates/ota/src/pure.rs`,
`crates/shell/src/lib.rs`) y las dos herramientas (`tools/ota_send.py`,
`tools/fleet_ota_deploy.py`), o los binarios se rechazarán.

---

## Cómo empezar

El estado íntegro previo a la retirada está en el commit `be0c148`:

```
git diff be0c148 -- crates/arch-riscv64/src/csr.rs
```

Orden sugerido: (1) arreglar `wcet.rs`, (2) instalar el target, (3) compilar y
catalogar de verdad la profundidad del problema **antes** de prometer plazos,
(4) meterlo en CI, (5) recuperar las ramas condicionales crate a crate.
