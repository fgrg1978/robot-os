# Plan: Robot Autónomo con Brain remoto (macOS + LM Studio)

## Arquitectura final (multi-robot)

El sistema soporta múltiples tipos de robot (ruedas, drone, humanoide) con la
misma capa de inteligencia. Solo cambian las capas de policy y hardware.

```
                     UNIVERSAL (no cambia por tipo de robot)
┌─────────────────────────────────────────────────────────────────┐
│  macOS (LM Studio + robot-brain)                                │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ VLM: Qwen2.5-VL-7B (entender imagen)                     │  │
│  │ LLM: Qwen3-30B-A3B (decidir acción)                      │  │
│  │ Task Planner: prompt libre → secuencia de skills          │  │
│  │ Skill Runner: state machine + loops + detect triggers     │  │
│  │ Modes: seguridad, patrulla, explorar (presets)            │  │
│  │ Notifications: pushover, telegram, email, webhook         │  │
│  │ Ground Station: telemetría + debug                        │  │
│  └──────────────────────┬────────────────────────────────────┘  │
│                         │                                       │
│              ROBOT-SPECIFIC (cambia por tipo)                   │
│  ┌──────────────────────▼────────────────────────────────────┐  │
│  │ Policy Translator:                                        │  │
│  │   wheeled.py  → diff drive (speed_l, speed_r)             │  │
│  │   drone.py    → attitude (throttle, roll, pitch, yaw)     │  │
│  │   humanoid.py → gait + joint angles (IK solver)           │  │
│  └──────────────────────┬────────────────────────────────────┘  │
│                         │ TCP (WiFi mesh)                       │
└─────────────────────────┼───────────────────────────────────────┘
                          │ ActuatorCmd (genérico: type + N channels)
┌─────────────────────────┼───────────────────────────────────────┐
│  VisionFive 2 / FCU     │                                       │
│  ┌──────────────────────▼────────────────────────────────────┐  │
│  │ USERSPACE (ELF, U-mode Sv39)                              │  │
│  │ ┌──────────┐ ┌──────────┐ ┌──────────────┐               │  │
│  │ │ brain    │ │ camera   │ │ reflex       │               │  │
│  │ │ client   │ │ streamer │ │ (local avoid)│               │  │
│  │ └────┬─────┘ └────┬─────┘ └──────┬───────┘               │  │
│  │      │ syscalls    │              │                        │  │
│  ├──────┼─────────────┼──────────────┼────────────────────────┤  │
│  │ KERNEL (Robot OS)                                          │  │
│  │  Actuadores (motores/ESC/servos), IMU, Sensors, Safety     │  │
│  │  Ethernet (Cadence MACB) / WiFi, TCP/IP stack              │  │
│  │  Channels, Watchdog, PMP                                   │  │
│  └────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

---

## Repos

### Repo 1: `riscv_robot_os_rust` (existente — se modifica)
Kernel bare-metal + userspace binarios.

### Repo 2: `robot-brain` (nuevo — Python, corre en macOS)
Servidor AI que conecta con LM Studio y el robot.

---

## PLAN DETALLADO

### ═══════════════════════════════════════════════════
### REPO 1: riscv_robot_os_rust — Cambios necesarios
### ═══════════════════════════════════════════════════

---

## Fase P — Net stack sobre Ethernet real (VF2)

**Problema**: el net stack (crates/net) solo funciona con VirtIO (QEMU).
En VF2 real, el transporte es Cadence MACB Ethernet (crates/drivers/src/eth.rs).
El net stack necesita abstraer el transporte.

### P1 — Abstracción de transporte de red
```
crates/net/src/transport.rs (NEW):
    pub trait NetTransport {
        fn send(frame: &[u8]) -> Result<(), ()>;
        fn poll_recv(buf: &mut [u8]) -> usize;
        fn get_mac() -> [u8; 6];
        fn is_ready() -> bool;
    }
```

Cambios:
- `crates/net/src/lib.rs`: net_init() detecta plataforma:
  - QEMU → VirtIO net (como ahora)
  - VF2 → Cadence MACB (eth.rs)
  - Ambos exponen la misma interfaz de send/poll_recv
- `net_poll()` llama al transporte correcto según feature gate
- El resto del stack (ARP, IP, TCP, socket) no cambia

### P2 — DHCP funcional
```
crates/net/src/dhcp.rs (ya existe, completar):
    dhcp_discover() → obtener IP del router WiFi mesh
```
- VF2 conectado por Ethernet al mesh WiFi (cable o bridge)
- DHCP necesario para obtener IP dinámica en la red del mesh
- Alternativa: IP estática configurada en CONFIG.INI

**Archivos a modificar**:
- `crates/net/src/lib.rs` — routing de transporte
- `crates/net/src/dhcp.rs` — completar DHCP
- `crates/drivers/src/eth.rs` — ya funcional, solo integrar
- `kernel/src/main.rs` — init eth en VF2 antes de net_init

**Dependencias**: ninguna (puede empezar ya)
**Estimado**: ~200 líneas nuevas

---

## Fase Q — Userspace runtime (libsys)

**Problema**: el userspace actual solo tiene un hello.S de prueba.
Un brain client necesita: syscall wrappers, memoria dinámica, string formatting,
y un loop de ejecución continuo.

### Q1 — Syscall wrappers en Rust (userspace library)
```
userspace/libsys/ (NEW — biblioteca estática para userspace ELFs)
  src/lib.rs:
    #![no_std]
    // Syscall inline asm wrappers
    pub fn sys_write(fd: u32, buf: &[u8]) -> i32;
    pub fn sys_read(fd: u32, buf: &mut [u8]) -> i32;
    pub fn sys_open(path: &[u8], flags: u32) -> i32;
    pub fn sys_close(fd: i32) -> i32;
    pub fn sys_socket(domain: u32, stype: u32, proto: u32) -> i32;
    pub fn sys_connect(fd: i32, addr: &SockAddr) -> i32;
    pub fn sys_send(fd: i32, data: &[u8]) -> i32;
    pub fn sys_recv(fd: i32, buf: &mut [u8]) -> i32;
    pub fn sys_yield();
    pub fn sys_exit(code: i32) -> !;
    pub fn sys_sleep(ms: u32);
    pub fn sys_brk(addr: usize) -> usize;
    pub fn sys_sensor_read(sensor_id: u32, buf: &mut [u8]) -> i32;
    pub fn sys_motor_speed(id: u32, speed: u32) -> i32;
    pub fn sys_getpid() -> u32;
    pub fn sys_uptime() -> u64;

  src/alloc.rs:
    // Simple bump allocator over brk()
    pub struct BrkAllocator;
    // Implements GlobalAlloc — userspace processes can use alloc::vec, etc.

  src/fmt.rs:
    // Minimal formatting (write to fd 1)
    pub fn print(s: &str);
    pub fn println(s: &str);

  src/net.rs:
    // Higher-level TCP helpers
    pub fn tcp_connect(ip: [u8; 4], port: u16) -> Result<i32, i32>;
    pub fn tcp_send_all(fd: i32, data: &[u8]) -> Result<(), i32>;
    pub fn tcp_recv_all(fd: i32, buf: &mut [u8]) -> Result<usize, i32>;
```

### Q2 — Mejoras al scheduler para daemons
```
crates/sched/src/scheduler.rs:
  - sys_sleep() cambiado de busy-wait a yield-based:
    task.wake_at = clint::get_time() + ms * 10_000;
    task.state = TaskState::Sleeping;
    (scheduler salta tasks con wake_at > now)
  - Per-process FD table (mover de KERNEL_FD_TABLE global a Task struct)
  - Task priority (2 niveles: RT=0, Normal=1) — RT siempre primero
```

### Q3 — Userspace binary: brain client
```
userspace/brain/ (NEW — ELF que corre en U-mode)
  src/main.rs:
    #![no_std]
    #![no_main]
    use libsys::*;

    fn main() -> ! {
        // 1. Conectar al servidor macOS via TCP
        let fd = tcp_connect(SERVER_IP, SERVER_PORT)?;

        loop {
            // 2. Leer sensores via syscall
            let imu = sys_sensor_read(SENSOR_IMU, &mut buf);
            let odom = sys_sensor_read(SENSOR_ODOM, &mut buf);
            let enc = sys_sensor_read(SENSOR_ENCODER, &mut buf);

            // 3. Empaquetar SensorPacket
            let pkt = SensorPacket { imu, odom, enc, timestamp };

            // 4. Enviar a macOS
            tcp_send_all(fd, &pkt.to_bytes());

            // 5. Recibir BrainCmd
            if let Ok(n) = tcp_recv_all(fd, &mut cmd_buf) {
                if let Some(cmd) = BrainCmd::from_bytes(&cmd_buf[..n]) {
                    // 6. Aplicar comando motor via syscall
                    sys_motor_speed(0, cmd.speed_l);
                    sys_motor_speed(1, cmd.speed_r);
                }
            }

            // 7. Yield (no burn CPU)
            sys_yield();
        }
    }
```

### Q4 — Userspace binary: reflex daemon (obstacle avoidance local)
```
userspace/reflex/ (NEW — ELF que corre en U-mode)
  src/main.rs:
    fn main() -> ! {
        loop {
            let range = sys_sensor_read(SENSOR_RANGEFINDER, &mut buf);
            if range_mm < SAFETY_THRESHOLD_MM {
                // Override: parar motores inmediatamente
                sys_motor_speed(0, 0);
                sys_motor_speed(1, 0);
                // Notificar al brain (via IPC o flag)
            }
            sys_yield();
        }
    }
```

**Archivos a crear**:
- `userspace/libsys/` (nuevo crate, no workspace member — target userspace)
- `userspace/brain/` (nuevo binario ELF)
- `userspace/reflex/` (nuevo binario ELF)

**Archivos a modificar**:
- `crates/sched/src/scheduler.rs` — sleep no-busy, priority
- `crates/sched/src/task.rs` — +wake_at, +priority, +fd_table
- `crates/syscall/src/handlers.rs` — sys_sleep yield-based, per-process FD

**Dependencias**: Fase P (para TCP connect desde userspace)
**Estimado**: ~600 líneas nuevas

---

## Fase R — Protocolo binario brain↔robot (multi-robot)

**Problema**: definir el formato exacto de los mensajes entre VF2 y macOS.
El protocolo debe soportar diferentes tipos de robot (ruedas, drone, humanoide)
sin cambiar el header ni la capa de transporte.

### R1 — Protocolo compartido
```
Formato de paquete (simple, sin overhead):

  ┌──────┬──────┬──────────┬─────────┐
  │ MAGIC│ TYPE │ LEN (u16)│ PAYLOAD │
  │ 2B   │ 1B   │ 2B       │ 0-1400B │
  └──────┴──────┴──────────┴─────────┘
  + CRC-8 al final (1 byte)

  MAGIC = 0xBR (0x42, 0x52)

  Tipos Robot → Server (0x01-0x7F):
    0x01 SENSOR_PACKET:
      Header común (siempre presente, 38 bytes):
        timestamp_ms:  u64
        battery_mv:    u16
        accel_mg:      [i32; 3]   (IMU siempre presente)
        gyro_mdps:     [i32; 3]

      Payload por robot_type (varía):
        Wheeled (type=0, +22 bytes = 60 total):
          odom_dist_mm:    i32
          odom_hdg_cdeg:   i32
          encoder_l:       i64
          range_front_mm:  u16
          range_right_mm:  u16
          (nota: encoder_r se deduce de odom + encoder_l si necesario)

        Drone (type=1, +26 bytes = 64 total):
          baro_pa:         i32     (presión barométrica)
          mag_ut:          [i16;3] (magnetómetro)
          gps_lat_deg7:    i32     (lat × 10^7)
          gps_lon_deg7:    i32     (lon × 10^7)
          gps_alt_cm:      i32     (altitud en cm)
          sonar_down_mm:   u16     (distancia al suelo)

        Humanoid (type=2, +variable):
          num_joints:      u8
          joint_angles:    [i16; num_joints]  (centidegrees)
          foot_pressure_l: u16
          foot_pressure_r: u16

    0x02 CAMERA_FRAME:
      width:  u16
      height: u16
      format: u8   (0=grayscale, 1=jpeg)
      data:   [u8; width*height]  (o JPEG comprimido)

    0x03 STATUS:
      robot_type: u8  (0=wheeled, 1=drone, 2=humanoid)
      mode:       u8
      tasks_ok:   u8
      canary_ok:  u8
      uptime_s:   u32

  Tipos Server → Robot (0x80-0xFF):
    0x80 ACTUATOR_CMD (reemplaza VELOCITY_CMD — genérico):
      actuator_type: u8   (0=diff_drive, 1=quad_rotor, 2=humanoid, 3=ackermann)
      num_channels:  u8   (2 para ruedas, 4 para drone, N para humanoide)
      flags:         u8   (bit 0: emergency_stop, bit 1: alert)
      channels:      [i16; num_channels]  (valores por canal, LE)

      Ejemplos:
        Ruedas:    type=0, n=2, ch=[60, 60]                    = 7 bytes
        Drone:     type=1, n=4, ch=[1400, 1400, 1400, 1400]    = 11 bytes
        Humanoide: type=2, n=20, ch=[...joint angles]           = 43 bytes

    0x81 MODE_CMD:
      mode: u8  (0=idle, 1=patrol, 2=navigate, 3=manual, 4=security)

    0x82 WAYPOINT_CMD:
      lat_deg7:  i32
      lon_deg7:  i32
      alt_cm:    i32  (0 para robots terrestres)
      speed_cms: u16

    0x83 CONFIG_CMD:
      key: [u8; 24]
      val: [u8; 16]
```

Compatibilidad:
- `ACTUATOR_CMD` con type=0, n=2 es idéntico funcionalmente a `VELOCITY_CMD`
- El brain client en VF2 envía `robot_type` en STATUS para que el server sepa qué policy usar
- El server carga el policy translator correcto al recibir el primer STATUS

Este protocolo se implementa en:
- `userspace/libsys/src/protocol.rs` — lado robot (Rust, no_std)
- `robot-brain/protocol.py` — lado macOS (Python)

**Dependencias**: ninguna (es definición de formato)
**Estimado**: ~180 líneas por lado

---

## Fase S — Soporte de sensor reads desde userspace

**Problema**: el brain client necesita leer IMU, odom, encoders, rangefinder
desde userspace. Los syscalls SYS_SENSOR_READ (330) existen pero no están
implementados.

### S1 — Implementar SYS_SENSOR_READ
```
crates/syscall/src/handlers.rs:
  pub fn sys_sensor_read(sensor_id: u64, buf_ptr: u64, buf_len: u64) -> i64 {
      match sensor_id {
          0 => { // IMU
              let data = robot_os_imu::imu_read_scaled();
              // copy ImuData to user buf
          }
          1 => { // Odometry
              let (dist, hdg) = robot_os_robot::odom_get();
              // copy to user buf
          }
          2 => { // Encoders
              let (l, r) = robot_os_robot::encoder_read();
              // copy to user buf
          }
          3 => { // Rangefinder
              let range = robot_os_drivers::rangefinder::...;
              // copy to user buf
          }
          _ => -1,
      }
  }
```

### S2 — Channel read desde userspace
```
Alternativa más elegante:
  SYS_IPC_SEND/RECV ya existen (100-107)
  Brain client puede leer channels publicados por kernel tasks:
    - CH_IMU, CH_ATTITUDE, CH_GPS, CH_ODOM
  Requiere: exponer channels como IPC endpoints legibles desde U-mode
```

**Archivos a modificar**:
- `crates/syscall/src/handlers.rs` — implementar sys_sensor_read
- Posiblemente `crates/channel/src/lib.rs` — read desde userspace

**Dependencias**: ninguna
**Estimado**: ~100 líneas

---

## Fase T — Camera streaming

**Problema**: el brain client necesita capturar frames de cámara y enviarlos
al servidor macOS para procesamiento por VLM.

### T1 — CSI capture real (VF2)
```
crates/drivers/src/csi.rs:
  - Ya tiene stubs para JH7110 ISP
  - Implementar: csi_capture() real para VF2
  - Frame buffer: 320x240 grayscale = 75 KB
  - Captura vía ISP DMA → buffer en memoria → syscall read
```

### T2 — JPEG compresión ligera (opcional)
```
crates/camera/src/jpeg.rs (NEW):
  - JPEG baseline encoder mínimo (solo grayscale)
  - 320x240 raw (75KB) → JPEG (~10-15KB)
  - Reduce ancho de banda WiFi de 75KB×10Hz=750KB/s a ~150KB/s
  - Alternativa: enviar raw si el bandwidth del mesh lo permite
```

### T3 — Syscall para camera read
```
  SYS_SENSOR_READ(sensor_id=4, buf, len):
    - Trigger capture
    - Copy frame to user buffer
    - Return frame size
```

**Dependencias**: Fase S (sensor read syscall)
**Estimado**: ~300 líneas (sin JPEG), ~500 con JPEG

---

## Fase U — Optimizaciones del kernel para este caso de uso

### U1 — Net polling task
```
kernel/src/main.rs:
  fn net_poll_task(_: usize) {
      loop {
          robot_os_net::net_poll();
          task_yield();
      }
  }
```
- Actualmente net_poll() se llama desde timer interrupt y shell
- Un task dedicado a ~1000 Hz mejora latencia TCP significativamente
- Necesario para que el brain client tenga TCP responsivo

### U2 — TCP buffer aumentado
```
crates/net/src/tcp.rs:
  - Aumentar TCP window de 1460 a 4096+ bytes
  - Permitir múltiples segmentos en vuelo
  - Necesario para enviar camera frames (10-75 KB)
```

### U3 — Task priority (RT vs Normal)
```
crates/sched/src/scheduler.rs:
  - 2 niveles: RT (0) y Normal (1)
  - RT tasks: motor control, sensor read, net poll
  - Normal tasks: brain client, shell, telemetry
  - RT siempre se ejecuta antes que Normal
  - Simple: dos listas, schedule RT primero
```

### U4 — Userspace ELF auto-launch
```
kernel/src/main.rs:
  - Al boot, después de montar FAT32:
    - Buscar /fat/BRAIN.ELF, /fat/REFLEX.ELF
    - Si existen, cargar y ejecutar en U-mode automáticamente
    - Configurable via CONFIG.INI: autorun=brain,reflex
```

**Dependencias**: Fase Q (scheduler changes)
**Estimado**: ~200 líneas

---

## Fase W — USB WiFi (conexión inalámbrica nativa en VF2)

**Problema**: la VF2 no tiene WiFi integrado y todo el entorno es WiFi mesh.
No hay cable Ethernet posible. Se necesita WiFi via USB dongle.

**Estado actual**: `crates/drivers/src/usb.rs` ya tiene:
- xHCI init completo (halt, reset, wait CNR, port scan)
- Lectura de HCSPARAMS1 (MaxSlots, MaxPorts)
- Port status/control (CCS, PED)
- Device table estática (8 devices)
- `usb_init()`, `usb_scan()`, `usb_info()` funcionales en VF2

**Falta**: todo lo que va desde "detecté un dispositivo USB" hasta "tengo WiFi".

### W1 — USB Core: enumeración de dispositivos (~800 líneas)
```
crates/drivers/src/usb_core.rs (NEW):
  Estado actual: xHCI detecta puertos con dispositivos conectados (CCS bit)
  Falta:

  1. Device Context Base Address Array (DCBAAP)
     - Allocar array de 64-bit pointers (MaxSlots + 1)
     - Escribir en DCBAAP register (ya definido, offset 0x30)

  2. Command Ring
     - Ring buffer de TRBs (Transfer Request Blocks) — 16 bytes cada uno
     - Escribir base en CRCR register (ya definido, offset 0x38)
     - Tipos: Enable Slot, Address Device, Configure Endpoint

  3. Transfer Rings (per-endpoint)
     - Ring buffer para Control/Bulk/Interrupt transfers
     - Cada endpoint tiene su propio ring

  4. Event Ring
     - Ring donde xHCI notifica completions
     - Poll-based (sin IRQ, como el resto del kernel)

  5. Enumeración USB:
     usb_enumerate(port) → UsbDevice {vid, pid, class, subclass}
       a. Enable Slot Command → obtener slot_id
       b. Address Device Command → asignar dirección USB
       c. GET_DESCRIPTOR (Device) → leer vid, pid, class
       d. GET_DESCRIPTOR (Config) → leer interfaces/endpoints
       e. SET_CONFIGURATION → activar dispositivo

  Tipos de transfer necesarios:
     - Control Transfer: setup packets (GET_DESCRIPTOR, SET_CONFIG, etc.)
     - Bulk Transfer: datos WiFi (TX/RX frames)
```

### W2 — USB WiFi class driver: RTL8188EU (~1500 líneas)
```
crates/drivers/src/usb_wifi.rs (NEW):
  Target: Realtek RTL8188EU — el chip USB WiFi más simple y documentado.
  Dongles comunes: TP-Link TL-WN725N ($5), muchos genéricos.
  VID:PID = 0x0BDA:0x8179 (y variantes)

  El RTL8188EU es el más viable porque:
  - Firmware NO necesario (fullmac, toda la lógica en hardware)   ← INCORRECTO para la mayoría
  - Protocolo USB bien documentado (driver Linux: rtl8xxxu)
  - Hay implementaciones bare-metal de referencia

  CORRECCIÓN: RTL8188EU SÍ necesita firmware upload.
  Alternativa más simple: RTL8188CUS (firmware en ROM).

  Flujo del driver:
  1. Detectar dispositivo (vid=0x0BDA, pid en lista conocida)
  2. Upload firmware (si necesario) via bulk OUT
  3. Configurar registros MAC via vendor-specific control transfers
  4. Configurar BB (baseband) y RF registers
  5. Habilitar RX/TX queues (bulk endpoints)

  API pública:
    usb_wifi_init() -> bool           // detecta y configura dongle
    usb_wifi_scan() -> ScanResults    // escanear APs disponibles
    usb_wifi_connect(ssid, pass)      // asociar + autenticar
    usb_wifi_send(frame: &[u8])       // enviar 802.11 frame
    usb_wifi_recv(buf: &mut [u8])     // recibir 802.11 frame
    usb_wifi_is_connected() -> bool
```

### W3 — WiFi 802.11 stack (~2000 líneas)
```
crates/drivers/src/wifi_stack.rs (NEW):
  Capa de gestión WiFi (management frames):

  1. Scan:
     - Enviar Probe Request en cada canal (1-13)
     - Parsear Probe Response / Beacon frames
     - Extraer: SSID, BSSID, canal, RSSI, security type

  2. Associate:
     - Authentication frame (Open System: 2 frames)
     - Association Request → Response
     - Parsear AID (Association ID)

  3. WPA2-PSK (CCMP):
     a. Derivar PMK: PBKDF2-SHA1(password, ssid, 4096, 32)
     b. 4-Way Handshake (EAPOL):
        - Msg 1: AP → STA (ANonce)
        - Msg 2: STA → AP (SNonce + MIC)
        - Msg 3: AP → STA (GTK + MIC)
        - Msg 4: STA → AP (ACK)
     c. Derivar PTK = PRF-384(PMK, ANonce, SNonce, MAC_AP, MAC_STA)
     d. Instalar claves TK (temporal key) para CCMP

  4. CCMP (AES-128-CCM):
     - Cifrar/descifrar data frames
     - Necesita AES-128 en software (~200 líneas)

  5. Conversión 802.11 ↔ Ethernet:
     - RX: 802.11 frame → strip headers → Ethernet frame → net stack
     - TX: Ethernet frame → add 802.11 headers → USB bulk OUT
```

### W4 — Crypto mínimo para WPA2 (~500 líneas)
```
crates/drivers/src/crypto.rs (NEW):
  - AES-128 encrypt/decrypt (tablas S-box, ~150 líneas)
  - AES-CCM mode (CCMP usa CCM, ~100 líneas)
  - SHA-1 + HMAC-SHA1 (~150 líneas)
  - PBKDF2-SHA1 (~50 líneas)
  - PRF-384 para PTK derivation (~50 líneas)

  Todo no_std, zero alloc, constant-time donde posible.
  No necesita ser criptográficamente perfecto para dev/test,
  pero sí funcional con APs WPA2-PSK reales.
```

### W5 — Integración con net stack (~100 líneas)
```
crates/net/src/lib.rs:
  net_init() detecta:
    - QEMU → VirtIO net
    - VF2 + USB WiFi → usb_wifi as transport
    - VF2 + Ethernet → Cadence MACB

  El net stack (ARP, IP, TCP, socket) no cambia.
  Solo el transporte de frames cambia:
    VirtIO send/recv  →  usb_wifi_send/recv
```

### Resumen de esfuerzo Fase W

| Sub-fase | Líneas | Dependencia |
|----------|--------|-------------|
| W1: USB Core (enumeración) | ~800 | usb.rs existente |
| W2: RTL8188 driver | ~1500 | W1 |
| W3: WiFi 802.11 stack | ~2000 | W2 |
| W4: Crypto (AES/SHA1/WPA2) | ~500 | Ninguna |
| W5: Net stack integration | ~100 | W3 + Fase P1 |
| **Total** | **~5000** | |

### Alternativa rápida: ESP32 bridge (Fase W-alt)

Si W resulta demasiado largo, la alternativa es un ESP32-C3 ($3) como bridge:

```
VF2 ──UART1 (3 cables)──→ ESP32-C3 ──WiFi──→ mesh ──→ macOS
```

| Sub-fase | Líneas | Dónde |
|----------|--------|-------|
| W-alt1: ESP32 firmware (bridge UART↔TCP) | ~300 | ESP-IDF/Arduino |
| W-alt2: VF2 UART1 ↔ brain protocol | ~200 | kernel + userspace |
| **Total alternativa** | **~500** | |

El plan soporta ambas rutas. La decisión depende de si prefieres:
- **USB WiFi (W)**: solución integrada, un solo board, más complejo
- **ESP32 bridge (W-alt)**: rápido, barato, probado, requiere hardware extra

---

### ═══════════════════════════════════════════════════
### REPO 2: robot-brain — Nuevo repositorio (macOS)
### ═══════════════════════════════════════════════════

## Estructura

```
robot-brain/
├── requirements.txt
├── config.yaml              ← configuración (IPs, modelos, modes, notificaciones)
├── server.py                ← servidor TCP principal
├── protocol.py              ← parser/builder del protocolo binario
├── perception/
│   ├── __init__.py
│   └── vision.py            ← interfaz con LM Studio (VLM)
├── planner/
│   ├── __init__.py
│   ├── decide.py            ← interfaz con LM Studio (LLM decisor táctico)
│   ├── skills.py            ← definición de skills primitivos del robot
│   ├── modes.py             ← presets (seguridad, patrulla, explorar, custom)
│   └── task_planner.py      ← LLM descompone prompt libre → secuencia de skills
├── executor/
│   ├── __init__.py
│   └── skill_runner.py      ← state machine: ejecuta skills en secuencia/loop
├── policy/
│   ├── __init__.py          ← carga translator según robot.type
│   ├── actions.py           ← parse acción textual (common)
│   ├── wheeled.py           ← skill → ActuatorCmd diff drive (2 ch)
│   ├── drone.py             ← skill → ActuatorCmd quad rotor (4 ch)
│   └── humanoid.py          ← skill → ActuatorCmd joint angles (N ch)
├── notifications.py         ← pushover, telegram, email, webhook
├── api.py                   ← HTTP API para control remoto (start/stop modes)
├── monitor/
│   ├── __init__.py
│   └── dashboard.py         ← terminal UI (telemetría live)
└── tests/
    ├── test_protocol.py
    └── test_policy.py
```

---

## Fase V — Modes, Skills y Task Planner (callers de alto nivel)

**Problema**: el usuario quiere dar instrucciones de alto nivel como "activar seguridad"
o "escanea la casa y detecta intrusos" y que el robot las ejecute autónomamente,
incluyendo loops continuos (toda la noche) y notificaciones al detectar eventos.

### V1 — Skill Library (planner/skills.py, ~100 líneas)
```
Skills universales (todos los robots):

UNIVERSAL_SKILLS = {
    "STOP":         "Stop all actuators immediately",
    "WAIT":         "Wait N seconds (actuators off, save battery)",
    "SCAN_360":     "Rotate/pan 360° scanning with VLM in steps",
    "INVESTIGATE":  "Approach detected object slowly (20% speed)",
    "ALERT":        "Stop, send notification with description + image",
    "TRACK":        "Follow detected object maintaining safe distance",
}

Skills por tipo de robot:

WHEELED_SKILLS = {
    "FORWARD":      "Move forward N cm",
    "BACKWARD":     "Move backward N cm",
    "TURN_LEFT":    "Turn left N degrees (in place)",
    "TURN_RIGHT":   "Turn right N degrees (in place)",
    "NAVIGATE_TO":  "Drive to named location using visual landmarks (2D)",
    "FOLLOW_WALL":  "Follow wall on left/right side at distance",
}

DRONE_SKILLS = {
    "TAKEOFF":      "Take off to N meters altitude",
    "LAND":         "Land at current position",
    "HOVER":        "Hold position and altitude",
    "FLY_TO":       "Fly to location (3D: x, y, z)",
    "ORBIT":        "Circle around point at radius R meters",
    "ASCEND":       "Gain N meters altitude",
    "DESCEND":      "Lose N meters altitude",
    "YAW_LEFT":     "Rotate left N degrees (in place, hovering)",
    "YAW_RIGHT":    "Rotate right N degrees (in place, hovering)",
    "RETURN_HOME":  "Fly back to takeoff point and land",
}

HUMANOID_SKILLS = {
    "WALK_TO":      "Walk to named location",
    "GRAB":         "Grab object with hand (left/right)",
    "RELEASE":      "Release held object",
    "LOOK_AT":      "Turn head toward direction/object",
    "CROUCH":       "Lower body (duck under obstacle)",
    "STAND":        "Stand upright from any position",
    "WAVE":         "Wave hand (greeting gesture)",
    "POINT":        "Point at object/direction",
    "OPEN_DOOR":    "Reach handle, turn, push/pull door",
    "PICK_UP":      "Bend down and pick up object from floor",
}

Se carga según config robot.type:
  skills = UNIVERSAL_SKILLS | TYPE_SKILLS[config.robot.type]

Cada skill tiene:
  - name, description (para que el LLM las conozca)
  - parameters: {name, type, default}
  - estimated_duration_s (para planning)
  - requires_vlm: bool (SCAN_360 sí, WAIT no)

El task_planner incluye SOLO los skills del tipo activo en su system prompt.
Así el LLM nunca genera "TAKEOFF" para un robot con ruedas.
```

### V2 — Mode Presets (planner/modes.py, ~80 líneas)
```
Modos predefinidos que NO requieren LLM para planificar:

MODES = {
    "seguridad": {
        "description": "Vigilancia continua. Escanea y alerta si detecta personas.",
        "plan": [SCAN_360, WAIT(30)],   # se repite en loop
        "loop": true,
        "detect": ["person", "open_door", "fire", "movement"],
        "on_detect": ["notify", "alert"],
        "schedule": "always",           # o "22:00-06:00"
    },
    "patrulla": {
        "description": "Recorre waypoints en loop escaneando en cada uno.",
        "plan": [
            NAVIGATE_TO("A"), SCAN_360,
            NAVIGATE_TO("B"), SCAN_360,
            NAVIGATE_TO("C"), SCAN_360,
        ],
        "loop": true,
        "detect": ["person", "obstacle"],
        "on_detect": ["notify"],
    },
    "explorar": {
        "description": "Exploración libre. LLM decide cada paso.",
        "plan": "llm",  # usa task_planner para generar plan dinámico
        "loop": false,
    },
    "volver_base": {
        "description": "Regresa al punto de inicio.",
        "plan": [NAVIGATE_TO("home")],
        "loop": false,
    },
}

Uso:
  python brain.py --mode seguridad
  HTTP: POST /api/mode {"mode": "seguridad"}
  Telegram: /seguridad

Un modo custom se puede crear por prompt libre:
  "escanea la casa y detecta intrusos"
  → task_planner descompone → plan dinámico
```

### V3 — Task Planner (planner/task_planner.py, ~60 líneas)
```
Para prompts libres que NO encajan en un preset.

System prompt al LLM:
  "You are a robot task planner. The robot has these skills:
   {SKILLS con descriptions}

   The robot knows these locations: {locations from config}

   Decompose the user's request into a sequence of skills.
   Output ONLY a JSON array:
   [
     {"skill": "NAVIGATE_TO", "args": {"location": "kitchen"}},
     {"skill": "SCAN_360", "args": {}},
     ...
   ]

   If the task should repeat, add {"skill": "LOOP", "args": {}}"

Ejemplo:
  Input:  "escanea la casa y detecta intrusos"
  Output: [
    {"skill": "NAVIGATE_TO", "args": {"location": "kitchen"}},
    {"skill": "SCAN_360", "args": {}},
    {"skill": "NAVIGATE_TO", "args": {"location": "living_room"}},
    {"skill": "SCAN_360", "args": {}},
    {"skill": "NAVIGATE_TO", "args": {"location": "bedroom"}},
    {"skill": "SCAN_360", "args": {}},
    {"skill": "NAVIGATE_TO", "args": {"location": "entry"}},
    {"skill": "LOOP", "args": {}}
  ]

Ejemplo 2:
  Input:  "ve a la cocina y quédate vigilando 1 hora"
  Output: [
    {"skill": "NAVIGATE_TO", "args": {"location": "kitchen"}},
    {"skill": "SCAN_360", "args": {}},
    {"skill": "WAIT", "args": {"seconds": 60}},
    {"skill": "SCAN_360", "args": {}},
    {"skill": "WAIT", "args": {"seconds": 60}},
    ... (x30 para cubrir 1 hora)
  ]
```

### V4 — Skill Runner (executor/skill_runner.py, ~150 líneas)
```
State machine que ejecuta un plan (secuencia de skills):

class SkillRunner:
    state: IDLE | RUNNING | PAUSED | ALERT
    current_plan: list[SkillStep]
    current_step: int
    loop: bool
    detect_triggers: list[str]

    async def run(plan, robot_conn, vlm, llm, notifier):
        while True:
            step = plan[current_step]

            if step.skill == "SCAN_360":
                for angle in [0, 90, 180, 270]:
                    send(TURN_RIGHT 90)
                    await wait_completion(~2s)
                    frame = await get_camera_frame()
                    scene = vlm.describe(frame, "Describe. Any person/threat?")

                    # Chequear triggers
                    for trigger in detect_triggers:
                        if trigger in scene.lower():
                            await notifier.alert(trigger, scene, frame)
                            # LLM decide: investigar o continuar
                            action = llm.decide(scene, sensors,
                                f"Detected {trigger}. Investigate or continue?")
                            if "INVESTIGATE" in action:
                                send(FORWARD 20)  # acercar lento
                                # nueva foto + nueva decisión
                            break

            elif step.skill == "NAVIGATE_TO":
                target = step.args["location"]
                while not arrived(target):
                    frame = await get_camera_frame()
                    scene = vlm.describe(frame, f"Navigate to {target}")
                    action = llm.decide(scene, sensors, f"reach {target}")
                    cmd = to_velocity_cmd(action)
                    send(cmd)
                    await asyncio.sleep(0.5)

            elif step.skill == "WAIT":
                seconds = step.args.get("seconds", 30)
                send(STOP)
                await asyncio.sleep(seconds)

            elif step.skill == "FORWARD":
                speed = step.args.get("speed", 50)
                send(FORWARD speed)
                await asyncio.sleep(step.args.get("duration", 2))

            # ... demás skills

            current_step += 1
            if current_step >= len(plan):
                if loop:
                    current_step = 0  # restart plan
                else:
                    break  # plan completado

Control:
  - pause() → PAUSED (motors stop, plan remembers position)
  - resume() → RUNNING (continúa donde estaba)
  - abort() → IDLE (motors stop, plan cleared)
  - change_mode(new_mode) → abort current + start new
```

**Dependencias**: server.py, perception/vision.py, planner/decide.py, policy/actions.py
**Estimado**: ~340 líneas nuevas (4 archivos)

---

## Fase X — Notificaciones y Control Remoto

**Problema**: el robot debe avisar al usuario cuando detecta algo (persona, puerta
abierta, batería baja) y el usuario debe poder controlar el robot remotamente.

### X1 — Notificaciones (notifications.py, ~100 líneas)
```
class Notifier:
    backends: list[NotifyBackend]  # configurados en config.yaml

    async def alert(trigger, description, image_bytes=None):
        message = f"ROBOT ALERT: {trigger}\n{description}\n{timestamp}"
        for backend in backends:
            await backend.send(message, image_bytes)

Backends implementados:

1. Pushover (recomendado para alertas críticas):
   - 1 HTTP POST a api.pushover.net/1/messages.json
   - Soporta: texto, imagen adjunta, prioridad (0-2), sonido custom
   - Prioridad 2 = emergency: suena hasta que el usuario confirme
   - Coste: $5 una vez (licencia app)
   - ~20 líneas de código

2. Telegram Bot (recomendado para control bidireccional):
   - sendMessage: POST a api.telegram.org/bot{token}/sendMessage
   - sendPhoto: POST con multipart/form-data (adjunta imagen)
   - getUpdates: polling para recibir comandos del usuario
   - Gratis, bidireccional
   - ~30 líneas send + ~30 líneas polling

3. Email (SMTP):
   - smtplib estándar de Python
   - Gmail con app password o Amazon SES
   - Latencia mayor (5-30s)
   - ~25 líneas

4. Webhook genérico:
   - POST JSON a URL configurable
   - Para: Home Assistant, IFTTT, Node-RED, custom
   - ~10 líneas
```

### X2 — Telegram Bot bidireccional (en notifications.py, +50 líneas)
```
Permite control remoto del robot desde Telegram:

Comandos entrantes (usuario → bot → robot):
  /seguridad          → activar modo seguridad
  /patrulla           → activar modo patrulla
  /stop               → parar motores, pausar modo
  /status             → batería, uptime, modo actual, posición
  /foto               → captura y envía foto actual
  /modo <prompt>      → prompt libre ("escanea la cocina")
  /investigar         → respuesta a alerta: acercarse
  /ignorar            → respuesta a alerta: continuar
  /volver             → volver a base

Flujo interactivo:
  Bot  → Usuario: "ALERTA: Persona detectada en hallway [foto]"
  Bot  → Usuario: "¿Qué hago? /investigar /ignorar /alarma"
  Usuario → Bot: /investigar
  Bot  → Usuario: "Acercándome... [nueva foto]"
  Bot  → Usuario: "Es el gato. Continuando patrulla."

Implementación:
  - asyncio task separado que hace getUpdates polling cada 2s
  - Comandos se parsean y envían al SkillRunner:
    "/seguridad" → runner.change_mode("seguridad")
    "/stop" → runner.abort()
    "/foto" → get latest frame → telegram.sendPhoto()
```

### X3 — HTTP API para control (api.py, ~80 líneas)
```
API REST mínima para control desde cualquier cliente:

POST /api/mode          {"mode": "seguridad"}
POST /api/prompt        {"prompt": "escanea la cocina"}
POST /api/stop          {}
GET  /api/status        → {mode, battery, uptime, odom, last_alert}
GET  /api/frame         → imagen JPEG actual
POST /api/notify/test   → enviar notificación de prueba

Implementación: aiohttp server corriendo en paralelo al TCP server.
Puerto configurable (default 8080).

Uso:
  curl -X POST localhost:8080/api/mode -d '{"mode": "seguridad"}'
  curl localhost:8080/api/status
```

### Cambios en config.yaml para Fases V + X
```yaml
# --- NUEVO: modes ---
modes:
  seguridad:
    skills: [SCAN_360, WAIT]
    loop: true
    scan_interval_s: 30
    detect: [person, open_door, fire, movement]
    on_detect: [notify, alert]
    schedule: always

  patrulla:
    skills: [NAVIGATE_TO, SCAN_360]
    waypoints: [A, B, C]
    loop: true
    detect: [person, obstacle]
    on_detect: [notify]

  explorar:
    planner: llm
    loop: false

locations:
  home:
    x_mm: 0
    y_mm: 0
  kitchen:
    x_mm: 3000
    y_mm: 1500
  living_room:
    x_mm: 5000
    y_mm: 0
  bedroom:
    x_mm: 5000
    y_mm: 3000

# --- NUEVO: notifications ---
notifications:
  pushover:
    enabled: false
    user_key: ""
    api_token: ""
    priority: 1
    sound: siren
    attach_image: true

  telegram:
    enabled: false
    bot_token: ""
    chat_id: ""
    commands: true       # habilitar control bidireccional

  email:
    enabled: false
    smtp_host: smtp.gmail.com
    smtp_port: 587
    username: ""
    password: ""          # app password, no contraseña real
    to: ""

  webhook:
    enabled: false
    url: ""
    headers: {}

# --- NUEVO: api ---
api:
  enabled: true
  port: 8080
```

**Dependencias**: Fase V depende de los componentes base ya existentes.
Fase X no depende de nada (solo HTTP requests).
**Estimado**: ~280 líneas nuevas (notifications.py + api.py + telegram polling)

---

## Fase Y — Abstracción Multi-Robot (ruedas, drone, humanoide)

**Problema**: la base actual asume un robot con 2 ruedas (differential drive).
Para soportar drones y humanoides, las capas de protocolo, policy y config
deben abstraer el tipo de actuador y sensores.

### Y1 — ActuatorCmd genérico (protocol.py refactor, ~30 líneas)
```
Actual:   VelocityCmd(speed_l: i32, speed_r: i32, flags: u8)  → solo 2 ruedas
Nuevo:    ActuatorCmd(actuator_type: u8, channels: list[int], flags: u8)

actuator_type:
  0 = diff_drive   → 2 channels: [speed_l, speed_r]
  1 = quad_rotor   → 4 channels: [motor1, motor2, motor3, motor4]
                      (o mejor: [throttle, roll, pitch, yaw] normalizados)
  2 = humanoid     → N channels: [joint_0_cdeg, joint_1_cdeg, ...]
  3 = ackermann    → 2 channels: [speed, steer_angle]

Wire format (pkt type 0x80):
  actuator_type: u8
  num_channels:  u8
  flags:         u8
  channels:      [i16; num_channels]  (little-endian)

Retrocompatible: ActuatorCmd(type=0, channels=[60,60]) ≡ VelocityCmd(60,60)
```

### Y2 — Policy Translators per tipo (~150 líneas, 3 archivos)
```
policy/__init__.py:
  def get_translator(robot_type: str) -> PolicyTranslator:
      if robot_type == "wheeled":  return WheeledPolicy()
      if robot_type == "drone":    return DronePolicy()
      if robot_type == "humanoid": return HumanoidPolicy()

policy/wheeled.py (refactor del actions.py actual):
  class WheeledPolicy(PolicyTranslator):
    def translate(skill, args, sensors) -> ActuatorCmd:
      if skill == "FORWARD":
        speed = min(args.get("speed", 60), max_speed)
        return ActuatorCmd(type=0, channels=[speed, speed])
      if skill == "TURN_RIGHT":
        s = args.get("degrees", 45) * max_speed // 90
        return ActuatorCmd(type=0, channels=[s, -s])
      if skill == "STOP":
        return ActuatorCmd(type=0, channels=[0, 0])

policy/drone.py:
  class DronePolicy(PolicyTranslator):
    def translate(skill, args, sensors) -> ActuatorCmd:
      if skill == "TAKEOFF":
        return ActuatorCmd(type=1, channels=[hover_thr, 0, 0, 0])
      if skill == "HOVER":
        # PID sobre altitud actual vs deseada
        thr = altitude_pid(sensors.baro, target_alt)
        return ActuatorCmd(type=1, channels=[thr, 0, 0, 0])
      if skill == "FLY_TO":
        # PID sobre posición → roll/pitch/yaw commands
        thr, roll, pitch, yaw = position_controller(
            current=sensors.gps, target=args["position"])
        return ActuatorCmd(type=1, channels=[thr, roll, pitch, yaw])
      if skill == "LAND":
        return ActuatorCmd(type=1, channels=[descend_thr, 0, 0, 0])

      Nota: el PID de actitud real corre en el kernel (RT),
      el drone.py solo envía setpoints (desired attitude).

policy/humanoid.py:
  class HumanoidPolicy(PolicyTranslator):
    def translate(skill, args, sensors) -> ActuatorCmd:
      if skill == "WALK_TO":
        # Genera secuencia de joint angles (gait pattern)
        joints = gait_generator(step_phase, direction)
        return ActuatorCmd(type=2, channels=joints)
      if skill == "GRAB":
        # Inverse kinematics: posición objeto → ángulos brazo
        joints = ik_solver(args["hand"], args["object_pos"])
        return ActuatorCmd(type=2, channels=joints)
      if skill == "LOOK_AT":
        neck_pan, neck_tilt = look_direction(args["direction"])
        return ActuatorCmd(type=2, channels=[neck_pan, neck_tilt])

      Nota: IK y gait pueden ser pesados — si es necesario se
      mueven al LLM o a un servicio separado. Para servos simples
      (12-DOF hobby humanoid) el cálculo es trivial.
```

### Y3 — SensorPacket genérico (protocol.py, ~40 líneas)
```
Header común (38 bytes, todos los robots):
  timestamp_ms:  u64
  battery_mv:    u16
  accel_mg:      [i32; 3]
  gyro_mdps:     [i32; 3]

Payload extensible (varía por robot_type):
  Wheeled:  encoders, odom, rangefinders
  Drone:    barometer, magnetometer, GPS, sonar
  Humanoid: joint angles, foot pressure

El server detecta robot_type del primer STATUS packet y
usa el parser correcto para SensorPacket.
```

### Y4 — Config per-robot-type (~20 líneas en config.yaml)
```yaml
robot:
  type: wheeled            # wheeled | drone | humanoid
  listen_port: 9000

  # Solo se usa la sección del tipo activo:
  wheeled:
    wheel_base_mm: 200
    max_speed: 80
    encoder_ticks_per_m: 1000

  drone:
    num_motors: 4
    motor_layout: x          # x | +
    hover_throttle: 1400     # PWM value for hover
    max_altitude_m: 50
    max_tilt_deg: 35
    pid:
      roll:  [1.0, 0.01, 0.5]
      pitch: [1.0, 0.01, 0.5]
      yaw:   [0.5, 0.01, 0.2]
      alt:   [2.0, 0.1, 1.0]

  humanoid:
    num_joints: 20
    servo_bus: i2c           # i2c | serial | can
    joint_limits:            # min/max per joint (safety)
      - [-90, 90]
      - [-45, 45]
      # ...
    gait_style: walk         # walk | shuffle | crawl
```

### Y5 — Kernel: Actuator Abstraction (Repo 1, ~100 líneas)
```
El brain client en VF2 recibe ActuatorCmd y llama al actuador correcto:

crates/robot/src/actuator.rs (NEW):
  pub enum ActuatorType { DiffDrive, QuadRotor, Humanoid, Ackermann }

  pub fn actuator_apply(cmd: &ActuatorCmd) {
      match cmd.actuator_type {
          DiffDrive => {
              motor_set(0, cmd.channels[0]);
              motor_set(1, cmd.channels[1]);
          }
          QuadRotor => {
              // 4 ESC outputs (ya existe esc.rs con 8 canales PWM)
              for i in 0..4 { esc_set(i, cmd.channels[i]); }
          }
          Humanoid => {
              // Bus de servos (I2C PCA9685 o serial)
              for i in 0..cmd.num_channels {
                  servo_set(i, cmd.channels[i]);
              }
          }
      }
  }

Ya existe en el kernel:
  - motor_set() → 2 motores DC (ruedas)
  - esc_set()   → 8 canales ESC PWM 400Hz (crates/drivers/src/esc.rs)
  - i2c_write() → para bus de servos PCA9685

Solo falta: actuator_apply() como dispatcher + servo driver si se usa humanoide.
```

### Resumen Fase Y

| Sub-fase | Dónde | Líneas | Depende de |
|----------|-------|--------|-----------|
| Y1: ActuatorCmd genérico | protocol.py | ~30 | Nada |
| Y2: Policy translators (×3) | policy/*.py | ~150 | Y1 |
| Y3: SensorPacket genérico | protocol.py | ~40 | Nada |
| Y4: Config per-type | config.yaml | ~20 | Nada |
| Y5: Kernel actuator_apply | actuator.rs | ~100 | esc.rs (ya existe) |
| **Total** | | **~340** | |

Nota: en la primera versión solo se implementa wheeled.py (el robot actual).
drone.py y humanoid.py se implementan cuando se tenga el hardware correspondiente.
La abstracción existe para no tener que refactorear después.

---

## ═══════════════════════════════════════════════════
## FASES FUTURAS (escalan la base a producción/campo)
## ═══════════════════════════════════════════════════

Estas fases NO bloquean las anteriores. Se implementan cuando la base (P-Y)
funcione end-to-end. Cubren: safety per-type, comunicación long-range,
autonomía offline, misiones GPS, implementos, logging, flota, y buses industriales.

---

## Fase AG — Safety Profiles per Robot Type

**Problema CRÍTICO**: el sistema actual asume que `motor_stop()` es siempre
la respuesta segura. Esto es FALSO para drones, humanoides y vehículos.

- Drone: `motor_stop()` = caída libre → destruido
- Humanoide: joints congelados = cae de cara → dañado
- Coche a 60km/h: motor off = sin dirección asistida → peligroso
- Solo ruedas: stop = seguro

Cada tipo de robot necesita su propia **secuencia de failsafe** para cada
tipo de fallo. No es un "flag" — es una máquina de estados.

### AG1 — Safety Profile abstraction (kernel + robot-brain, ~200 líneas)
```
Cada robot_type define su SafetyProfile:

crates/robot/src/safety.rs (NEW):
  pub enum FailsafeEvent {
      WatchdogTimeout,     // sin comandos durante N ms
      LinkLost,            // perdió conexión con brain
      BatteryLow,          // bajo umbral mínimo
      BatteryCritical,     // bajo umbral crítico (aterrizar YA)
      ObstacleDetected,    // sensor de proximidad
      ImuFailure,          // lecturas IMU inválidas o frozen
      GpsLost,             // sin fix GPS
      MotorFailure(u8),    // motor N no responde
      TiltExceeded,        // inclinación peligrosa
      GeofenceViolation,   // fuera de zona permitida
      EStopUser,           // usuario presionó emergency stop
  }

  pub enum FailsafeAction {
      Stop,                // cortar actuadores (solo seguro en ruedas)
      Hover,               // mantener posición (drone)
      ControlledDescent,   // bajar gradualmente (drone)
      Land,                // aterrizaje completo (drone)
      ReturnToHome,        // volver al punto de inicio
      Crouch,              // posición baja estable (humanoide)
      SitDown,             // sentarse (humanoide)
      BrakeGradual,        // frenar progresivamente (vehículo)
      PullOver,            // orillarse + parar (vehículo)
      KillMotors,          // ÚLTIMO recurso: cortar todo
      LockJoints,          // bloquear articulaciones (humanoide, ya agachado)
      HazardLights,        // luces de emergencia (vehículo)
      Alert,               // notificar al operador
      ContinueMission,     // no hacer nada, seguir (si es seguro)
  }

  pub trait SafetyProfile {
      fn failsafe(&self, event: FailsafeEvent, state: &RobotState) -> &[FailsafeAction];
      fn is_critical(&self, event: FailsafeEvent) -> bool;
      fn battery_reserve(&self) -> u16;    // mV para llegar a home
      fn max_tilt_deg(&self) -> u16;       // antes de emergency
      fn watchdog_timeout_ms(&self) -> u32;
  }
```

### AG2 — Wheeled Safety Profile (kernel, ~40 líneas)
```
crates/robot/src/safety_wheeled.rs (NEW):
  Simple — stop es siempre seguro.

  impl SafetyProfile for WheeledSafety {
    fn failsafe(&self, event, state) -> &[FailsafeAction] {
      match event {
        WatchdogTimeout     => &[Stop, Alert],
        LinkLost            => &[Stop, Alert],   // o ContinueMission si GPS
        BatteryLow          => &[Stop, Alert],
        BatteryCritical     => &[Stop, Alert],
        ObstacleDetected    => &[Stop],
        ImuFailure          => &[Stop, Alert],
        GpsLost             => &[ContinueMission],  // no crítico para ruedas indoor
        MotorFailure(_)     => &[Stop, Alert],
        GeofenceViolation   => &[Stop, ReturnToHome],
        EStopUser           => &[Stop],
        _                   => &[Stop],
      }
    }
    fn battery_reserve(&self) -> u16 { 6500 }  // mV
    fn max_tilt_deg(&self) -> u16 { 45 }       // volcado
    fn watchdog_timeout_ms(&self) -> u32 { 3000 }
  }
```

### AG3 — Drone Safety Profile (kernel, ~120 líneas)
```
crates/robot/src/safety_drone.rs (NEW):
  Drone NUNCA hace motor_stop() excepto como último recurso (kill switch).
  La secuencia siempre es: Hover → acción correctiva → Land si necesario.

  impl SafetyProfile for DroneSafety {
    fn failsafe(&self, event, state) -> &[FailsafeAction] {
      match event {
        WatchdogTimeout => &[Hover, Alert],
          // Mantener posición. Si timeout persiste 10s → ControlledDescent → Land

        LinkLost => match state.mission_loaded {
          true  => &[ContinueMission],     // tiene misión GPS → seguir
          false => &[Hover, ReturnToHome],  // no tiene misión → volver
        },
          // Hover 30s esperando reconexión. Si no → RTH automático

        BatteryLow => &[ReturnToHome, Alert],
          // Calcular si queda energía para llegar a home.
          // Si no → Land en posición actual.

        BatteryCritical => &[Land, Alert],
          // Aterrizaje inmediato donde esté. No hay opción.

        ObstacleDetected => &[Hover],
          // Hover + VLM/LLM decide: subir, rodear, o esperar.
          // Si no hay brain → ascender 5m y continuar.

        ImuFailure => &[ControlledDescent, Land, Alert],
          // Sin IMU no puede estabilizarse. Bajar lento y aterrizar.
          // Usar GPS para posición, barómetro para altitud.

        GpsLost => &[Hover, Alert],
          // Mantener altitud y actitud via IMU/baro.
          // No navegar — esperar fix o land.

        MotorFailure(n) => {
          // Depende de cuántos motores quedan:
          // Quad: 1 fallo → redistribuir thrust (possible con algunos frames)
          //        → si no puede → ControlledDescent lo más lento posible
          // Hexa: 1 fallo → compensar fácilmente
          // Nota: requiere mixer aware del estado de cada motor
          if can_compensate(n, state) {
            &[Alert, ContinueMission]
          } else {
            &[ControlledDescent, KillMotors]  // kill al tocar suelo
          }
        },

        TiltExceeded => &[KillMotors],
          // >60° de tilt = ya está cayendo. Cortar motores para
          // evitar daño al impacto (hélices girando = más daño).
          // Este es el ÚNICO caso donde KillMotors es correcto.

        GeofenceViolation => &[Hover, ReturnToHome, Alert],
          // Frenar, hover, volver dentro de la zona.

        EStopUser => &[ControlledDescent, Land],
          // NO kill motors. Descenso controlado.
          // Double-tap E-Stop → KillMotors (override consciente).
      }
    }

    fn battery_reserve(&self) -> u16 {
      // Suficiente para volver a home + 60s hover + aterrizar
      // Se calcula dinámicamente basado en distancia a home
      self.rth_battery_estimate(state.distance_to_home)
    }
    fn max_tilt_deg(&self) -> u16 { 60 }
    fn watchdog_timeout_ms(&self) -> u32 { 500 }  // mucho más estricto
  }

Secuencias de aterrizaje:
  ControlledDescent:
    1. Reducir throttle gradualmente (-50 cm/s)
    2. Mantener nivel (roll=0, pitch=0)
    3. Monitorear sonar_down para detectar suelo
    4. Al tocar → cortar motores

  ReturnToHome:
    1. Ascender a altitud safe (configurable, ej: 30m)
    2. Rotar hacia home
    3. Volar en línea recta a home
    4. Descender sobre home
    5. Land
    6. Motor off

  Hover:
    - PID mantiene posición GPS + altitud barométrica
    - Timeout configurable antes de siguiente acción
```

### AG4 — Humanoid Safety Profile (kernel, ~100 líneas)
```
crates/robot/src/safety_humanoid.rs (NEW):
  Humanoide NUNCA congela joints instantáneamente (cae).
  Secuencia: reducir velocidad → posición estable → lock.

  impl SafetyProfile for HumanoidSafety {
    fn failsafe(&self, event, state) -> &[FailsafeAction] {
      match event {
        WatchdogTimeout     => &[Crouch, LockJoints, Alert],
        LinkLost            => &[Stop, Crouch, Alert],     // stop = dejar de caminar
        BatteryLow          => &[SitDown, LockJoints, Alert],
        BatteryCritical     => &[Crouch, LockJoints, Alert],
        ObstacleDetected    => &[Stop],                    // stop walking, stay standing
        ImuFailure          => &[Crouch, LockJoints, Alert],
        MotorFailure(n) => {
          // Depende de qué joint falló:
          // Pierna → Crouch inmediato (no puede caminar)
          // Brazo → Stop walking, mantener balance
          // Cuello → continuar (no crítico)
          if is_leg_joint(n) { &[Crouch, LockJoints, Alert] }
          else { &[Stop, Alert] }
        },
        TiltExceeded => &[BreakFall, Crouch],
          // Posición de break-fall (brazos protegen cabeza/torso)
        EStopUser => &[Crouch, LockJoints],
        _ => &[Crouch, LockJoints],
      }
    }
    fn max_tilt_deg(&self) -> u16 { 30 }   // mucho menos que drone
    fn watchdog_timeout_ms(&self) -> u32 { 1000 }
  }

Secuencias:
  Crouch:
    1. Flexionar rodillas gradualmente (-5°/step a 100Hz)
    2. Bajar centro de gravedad
    3. Mantener balance via IMU feedback
    4. Resultado: posición baja y estable

  SitDown:
    1. Crouch primero
    2. Flexionar más hasta sentarse
    3. LockJoints en posición sentada

  BreakFall:
    1. Detectar dirección de caída
    2. Extender brazos para amortiguar
    3. Proteger cabeza/torso
    4. Post-caída: evaluar daño, intentar levantarse o LockJoints

  LockJoints:
    - Todos los servos a hold position
    - Torque alto para mantener posición
    - Solo después de estar en posición estable
```

### AG5 — Vehicle Safety Profile (kernel, ~80 líneas)
```
crates/robot/src/safety_vehicle.rs (NEW):
  Vehículo (coche/tractor) NUNCA corta motor a velocidad.

  impl SafetyProfile for VehicleSafety {
    fn failsafe(&self, event, state) -> &[FailsafeAction] {
      match event {
        WatchdogTimeout => if state.speed > 0 {
          &[BrakeGradual, HazardLights, Alert]
        } else {
          &[Stop, HazardLights, Alert]
        },

        LinkLost => if state.speed > 0 {
          &[BrakeGradual, PullOver, HazardLights, Alert]
        } else {
          &[Stop, Alert]
        },

        BatteryLow          => &[PullOver, Stop, HazardLights, Alert],
        ObstacleDetected    => &[BrakeGradual],
        ImuFailure          => &[BrakeGradual, PullOver, Alert],
        GpsLost             => &[BrakeGradual, Stop, Alert],
        GeofenceViolation   => &[BrakeGradual, Stop, Alert],
        EStopUser           => &[BrakeGradual, HazardLights],
          // NO frenazo instantáneo (puede volcar tractor, bloquear ruedas)
          // Frenado ABS-like: máxima deceleración segura
        _ => &[BrakeGradual, Stop],
      }
    }
    fn watchdog_timeout_ms(&self) -> u32 {
      // Más estricto a mayor velocidad:
      if state.speed > 3000 { 500 }    // >30km/h → 500ms
      else if state.speed > 0 { 2000 } // moviendo → 2s
      else { 5000 }                    // parado → 5s
    }
  }

Secuencias:
  BrakeGradual:
    - Deceleración máxima segura (configurable, ej: 3 m/s²)
    - Si tiene ABS: modular frenado por rueda
    - Mantener dirección recta (o seguir curva actual)

  PullOver:
    1. Reducir velocidad gradualmente
    2. Si tiene GPS + mapa: buscar arcén/borde de campo
    3. Si no: seguir recto y frenar
    4. Stop cuando speed=0
    5. HazardLights on
    6. Engage parking brake

  HazardLights:
    - GPIO toggle a frecuencia fija (intermitente)
    - Se activa SIEMPRE en emergencia para vehículos
```

### AG6 — Safety state machine (kernel, ~150 líneas)
```
crates/robot/src/safety_fsm.rs (NEW):
  La safety NO es un flag. Es una máquina de estados que ejecuta
  la secuencia de failsafe paso a paso.

  pub struct SafetyFSM {
      profile: &dyn SafetyProfile,
      state: SafetyState,
      current_sequence: &[FailsafeAction],
      current_step: usize,
      event_source: FailsafeEvent,
      timer_ticks: u64,
  }

  pub enum SafetyState {
      Normal,              // todo OK, operación normal
      Responding(event),   // ejecutando secuencia de failsafe
      Stabilized,          // failsafe completado, esperando intervención
      Override,            // operador tomó control manual
  }

  impl SafetyFSM {
      // Llamado desde rt_safety_task a cada tick:
      pub fn tick(&mut self, sensors: &SensorState, actuators: &mut ActuatorState) {
          match self.state {
              Normal => {
                  // Chequear todos los posibles eventos de fallo
                  for event in check_all_events(sensors) {
                      let actions = self.profile.failsafe(event, sensors);
                      self.enter_responding(event, actions);
                      break;  // un fallo a la vez, prioridad por orden
                  }
              }
              Responding(_) => {
                  let action = self.current_sequence[self.current_step];
                  let done = execute_action(action, sensors, actuators);
                  if done {
                      self.current_step += 1;
                      if self.current_step >= self.current_sequence.len() {
                          self.state = Stabilized;
                      }
                  }
              }
              Stabilized => {
                  // Mantener estado actual. Solo sale con:
                  // - Operador override (E-STOP release)
                  // - Brain reconecta y envía RESUME
                  // - Auto-recovery si el evento se resuelve (ej: GPS fix recovered)
              }
              Override => { /* operador manual, safety monitorea pero no actúa */ }
          }
      }
  }

Kernel integration:
  - rt_safety_task: nueva task RT, prioridad máxima
  - Corre a cada tick del scheduler (100-1000 Hz)
  - Tiene override sobre CUALQUIER ActuatorCmd del brain
  - Si safety.state != Normal, ignora comandos del brain
  - Solo el operador (E-STOP release) o auto-recovery salen del failsafe
```

### AG7 — Battery reserve calculation (kernel + robot-brain, ~80 líneas)
```
Cada tipo de robot necesita calcular cuánta batería reservar:

Ruedas:    mínimo = floor (no necesita reserva para "aterrizar")
Drone:     reserva = energía para RTH + 60s hover + aterrizaje
Humanoide: reserva = energía para sentarse + mantener joints locked 30min
Vehículo:  reserva = energía para frenar + hazard lights 30min

safety/battery.py (robot-brain, ~40 líneas):
  def battery_reserve_mv(robot_type, distance_to_home_m, altitude_m) -> int:
      if robot_type == "wheeled":
          return 6500  # fijo
      if robot_type == "drone":
          # Estimación: 100mV por km de RTH + 200mV para landing
          return 6800 + (distance_to_home_m // 10) + (altitude_m // 5)
      if robot_type == "vehicle":
          return 11000  # 12V nominal, mínimo para freno eléctrico

crates/robot/src/safety.rs (kernel, ~40 líneas):
  fn check_battery(sensors, profile) -> Option<FailsafeEvent> {
      let reserve = profile.battery_reserve();
      if sensors.battery_mv < reserve / 2 {
          Some(BatteryCritical)
      } else if sensors.battery_mv < reserve {
          Some(BatteryLow)
      } else { None }
  }
```

### AG8 — Watchdog per-type (kernel refactor, ~50 líneas)
```
Actual: watchdog_timeout_ms = 3000ms fijo para todos.

Nuevo: timeout varía por tipo Y por estado:

  Drone hovering:      500ms  (si pierde comandos, cae rápido)
  Drone en ruta:       500ms
  Drone en tierra:     5000ms (no peligroso)
  Humanoide caminando: 1000ms
  Humanoide parado:    5000ms
  Coche a >30km/h:     500ms  (a velocidad, reacción rápida)
  Coche parado:        5000ms
  Ruedas:              3000ms (como ahora)

crates/robot/src/safety.rs:
  fn dynamic_watchdog_ms(profile, state) -> u32 {
      let base = profile.watchdog_timeout_ms();
      // Más estricto si está en movimiento o en el aire
      if state.is_airborne { base / 2 }
      else if state.speed > 0 { base }
      else { base * 2 }
  }
```

### Resumen Fase AG

| Sub-fase | Dónde | Líneas | Depende de |
|----------|-------|--------|-----------|
| AG1: Safety Profile trait | kernel | ~200 | Nada |
| AG2: Wheeled safety | kernel | ~40 | AG1 |
| AG3: Drone safety | kernel | ~120 | AG1 |
| AG4: Humanoid safety | kernel | ~100 | AG1 |
| AG5: Vehicle safety | kernel | ~80 | AG1 |
| AG6: Safety FSM | kernel | ~150 | AG1 |
| AG7: Battery reserve calc | kernel + brain | ~80 | AG1 |
| AG8: Watchdog per-type | kernel | ~50 | AG1 |
| **Total** | | **~820** | |

**IMPORTANTE**: AG1 (trait) + AG2 (wheeled) + AG6 (FSM) + AG8 (watchdog) deberían
implementarse ANTES de probar cualquier tipo de robot que no sea ruedas.
AG3/AG4/AG5 se implementan cuando se tenga el hardware correspondiente,
pero la interfaz SafetyProfile debe existir desde el principio.

---

## Fase Z — Transport Abstraction (multi-link: WiFi, LoRa, RF, 4G)

**Problema**: la Fase 1 asume WiFi (alto bandwidth, corto alcance). Para campo
abierto, tractores a 5km, o drones agrícolas, necesitamos LoRa, RF o 4G.
Cada link tiene bandwidth y latencia muy distintos.

```
Link        Rango      Bandwidth     Latencia   Cámara?  Coste
WiFi        ~100m      50+ Mbps      ~5ms       Sí       $0
LoRa        2-15km     0.3-50 kbps   ~200ms     No       $10 módulo
RF 433/915  1-5km      1-100 kbps    ~50ms      No       $5 módulo
4G/LTE      Ilimitado  10+ Mbps      ~50ms      Sí       SIM mensual
Satellite   Global     2-100 kbps    ~500ms     No       $$$
```

### Z1 — Link abstraction layer (robot-brain + kernel, ~200 líneas)
```
Interfaz común para todos los links:

class TransportLink:
    async def send(data: bytes) -> bool
    async def recv(timeout_s: float) -> bytes
    def bandwidth_bps() -> int       # bandwidth disponible
    def latency_ms() -> int          # latencia estimada
    def is_connected() -> bool
    def link_quality() -> float      # 0.0-1.0 (RSSI normalizado)

Implementaciones:
  WiFiLink      → TCP socket (actual)
  LoRaLink      → serial (UART a módulo LoRa SX1276/RFM95)
  RF433Link     → serial (UART a módulo HC-12/E32)
  CellularLink  → TCP sobre PPP/QMI (módulo SIM7600/EC25)
  SatLink       → serial (Iridium/LoRa satélite)

El brain client selecciona link según config + fallback automático:
  - WiFi disponible → WiFi (full bandwidth)
  - WiFi caído → LoRa (solo telemetría + comandos)
  - LoRa caído → RF 433MHz (mínimo: heartbeat + emergency)
```

### Z2 — Bandwidth-aware protocol (robot-brain, ~100 líneas)
```
El protocolo se adapta al link activo:

WiFi mode (>1 Mbps):
  - SENSOR_PACKET a 20 Hz
  - CAMERA_FRAME a 2 Hz (JPEG, 10-75 KB)
  - ACTUATOR_CMD a 20 Hz
  - Full bidireccional

LoRa mode (<50 kbps):
  - SENSOR_PACKET_COMPACT a 1 Hz (20 bytes: timestamp, lat, lon, alt, battery, mode)
  - NO camera (imposible)
  - COMMAND_COMPACT a 0.5 Hz (8 bytes: skill_id + 3 params)
  - Robot ejecuta misión autónomamente, solo reporta estado

RF Emergency mode (<1 kbps):
  - HEARTBEAT cada 10s (4 bytes: battery + mode + GPS fix)
  - EMERGENCY_CMD: RTH (return to home), LAND, STOP

Packet types nuevos para low-bandwidth:
  0x04 SENSOR_COMPACT:
    lat_deg7: i32, lon_deg7: i32, alt_cm: i16
    battery_mv: u16, mode: u8, gps_fix: u8
    speed_cms: u16, heading_cdeg: u16
    (total: 20 bytes)

  0x84 COMMAND_COMPACT:
    skill_id: u8  (0=STOP, 1=RTH, 2=CONTINUE, 3=GOTO_WP, 4=LAND)
    param1: i16, param2: i16, param3: i16
    (total: 7 bytes)
```

### Z3 — LoRa driver (kernel, ~300 líneas)
```
crates/drivers/src/lora.rs (NEW):
  Target: Semtech SX1276/SX1278 (módulo RFM95W, $10)
  Conexión: SPI (ya soportado en kernel)

  API:
    lora_init(freq_mhz, sf, bw, power) -> bool
    lora_send(data: &[u8]) -> bool
    lora_recv(buf: &mut [u8], timeout_ms: u32) -> Option<usize>
    lora_rssi() -> i16
    lora_set_mode(mode: LoRaMode)  // sleep, standby, rx, tx

  Configuración típica campo:
    Frecuencia: 868 MHz (EU) / 915 MHz (US)
    Spreading Factor: SF7 (rápido, corto) a SF12 (lento, largo)
    Bandwidth: 125/250/500 kHz
    TX Power: 2-20 dBm

  SF7 @125kHz → ~5.5 kbps, ~2km    (OK para telemetría rápida)
  SF12@125kHz → ~0.3 kbps, ~15km   (OK para heartbeat, emergencia)
```

### Z4 — Link failover + auto-switch (~80 líneas)
```
robot-brain/transport/manager.py (NEW):
  class LinkManager:
    links: list[TransportLink]  # ordenados por prioridad

    async def send(data, priority):
      # Alta prioridad (emergency): intenta todos los links
      # Normal: usa el mejor link disponible
      for link in links:
        if link.is_connected() and link.bandwidth_bps() >= needed:
          return await link.send(data)

    async def monitor():
      # Loop continuo: chequea calidad de cada link
      # Si WiFi cae → switch a LoRa automáticamente
      # Si WiFi vuelve → switch back
      # Notifica al usuario del cambio de link

Kernel side (brain client ELF):
  - Intenta WiFi TCP connect
  - Si falla o timeout → abre UART a módulo LoRa
  - Envía SENSOR_COMPACT en vez de SENSOR_PACKET
  - Ejecuta misión offline (GPS waypoints, sin VLM)
```

### Z5 — Kernel multi-UART para LoRa/RF (Repo 1, ~100 líneas)
```
crates/drivers/src/uart.rs:
  Ya soporta UART0 (consola). Necesita:
  - UART1 init (VF2: 0x10010000, K1: 0xD4017800)
  - uart1_write()/uart1_read() para LoRa/RF module
  - Userspace: SYS_UART_WRITE/READ o mapear como fd

crates/drivers/src/spi.rs:
  Ya existe. LoRa SX1276 usa SPI.
  Solo falta: exponer SPI desde userspace si el driver LoRa corre en user mode.
```

### Resumen Fase Z

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| Z1: Link abstraction | ~200 | Nada |
| Z2: Bandwidth-aware protocol | ~100 | Z1 |
| Z3: LoRa driver (SX1276) | ~300 | SPI (ya existe) |
| Z4: Link failover | ~80 | Z1 |
| Z5: Multi-UART kernel | ~100 | UART (ya existe) |
| **Total** | **~780** | |

**Fase 1**: solo WiFi (ya planificado en W/W-alt).
**Fase 2**: añadir LoRa como backup (Z3 + Z5 + Z1).
**Fase 3**: añadir 4G si necesario (módulo SIM + PPP stack).

---

## Fase AA — GPS Missions + Geofencing

**Problema**: para drones agrícolas y tractores, la navegación no es visual
("ve a la cocina") sino GPS ("recorre estos 500 waypoints con precisión de 2cm").

### AA1 — Mission Planner (robot-brain, ~200 líneas)
```
planner/mission.py (NEW):
  Genera patrones de cobertura a partir de un área definida.

  class MissionPlanner:
    def boustrophedon(area: Polygon, row_spacing_m, direction_deg) -> list[Waypoint]:
        # Patrón zigzag (ida y vuelta) — el más común en agricultura
        # Input: polígono del campo + separación entre filas
        # Output: lista de waypoints GPS ordenados
        #
        #   →→→→→→→→→→→→→→→→→│
        #   │←←←←←←←←←←←←←←←←
        #   →→→→→→→→→→→→→→→→→│
        #   │←←←←←←←←←←←←←←←←
        #   →→→→→→→→→→→→→→→→→│

    def spiral(center: LatLon, radius_m, spacing_m) -> list[Waypoint]:
        # Espiral desde centro hacia afuera (búsqueda, fumigación circular)

    def grid(area: Polygon, spacing_m) -> list[Waypoint]:
        # Cuadrícula (cobertura fotográfica, mapping)

    def perimeter(area: Polygon) -> list[Waypoint]:
        # Solo el perímetro (inspección de cercas, vallas)

  class Waypoint:
    lat_deg7: int      # lat × 10^7 (integer, sin floats)
    lon_deg7: int      # lon × 10^7
    alt_cm: int        # altitud (0 para terrestres)
    speed_cms: int     # velocidad en este tramo
    action: str        # "navigate" | "spray_on" | "spray_off" | "photo" | "land"

  Formatos de entrada (interoperables):
    - KML/KMZ (Google Earth) → parsear polígono
    - GeoJSON → parsear polígono
    - Lista manual de coordenadas
    - Dibujar en mapa (futuro: web UI)
```

### AA2 — Geofencing (robot-brain + kernel, ~150 líneas)
```
safety/geofence.py (NEW):
  Define límites que el robot NUNCA puede cruzar.
  Se valida ANTES de enviar cualquier ActuatorCmd.

  class Geofence:
    inclusion_zones: list[Polygon]  # DEBE estar dentro de alguna
    exclusion_zones: list[Polygon]  # NUNCA puede entrar
    max_altitude_m: float           # techo (para drones)
    min_altitude_m: float           # suelo mínimo (para drones)
    max_distance_m: float           # radio máximo desde home

    def is_allowed(lat, lon, alt) -> bool:
        # 1. ¿Está dentro de alguna inclusion zone?
        # 2. ¿Está fuera de todas las exclusion zones?
        # 3. ¿Altitud dentro de límites?
        # 4. ¿Distancia a home < max_distance?

    def nearest_violation(lat, lon) -> tuple[str, float]:
        # "exclusion_zone_road", 15.3m  → para warnings

  Acciones en violación:
    WARN:    notificar, no actuar (approaching limit)
    BRAKE:   desacelerar gradualmente (entering buffer zone)
    STOP:    parar inmediatamente (at limit)
    RTH:     volver a home (beyond limit — failsafe)

  Buffer zones:
    Cada geofence tiene un buffer (ej: 10m antes del límite → BRAKE)
    Evita frenazos bruscos — desacelera progresivamente

Kernel side (safety redundante):
  crates/robot/src/geofence.rs (NEW, ~80 líneas):
    - Geofence simplificado en kernel (rectángulo + radio, sin polígonos)
    - Último recurso: si el brain client falla, el kernel hace STOP
    - Configurable via CONFIG.INI: geofence_lat_min/max, lon_min/max, radius_m
    - Chequeado en rt_motor_task antes de aplicar ActuatorCmd
```

### AA3 — GPS Waypoint Navigation (kernel + robot-brain, ~150 líneas)
```
El GPS driver ya existe: crates/gps/src/lib.rs (parser NMEA completo).
Falta: navegación de waypoint a waypoint.

planner/gps_nav.py (robot-brain, ~80 líneas):
  def navigate_waypoint(current: LatLon, target: Waypoint, heading) -> skill:
      bearing = calc_bearing(current, target)   # ángulo al target
      distance = haversine(current, target)     # distancia en metros
      turn_needed = normalize(bearing - heading)

      if distance < arrival_radius:
          return next_waypoint()
      if abs(turn_needed) > 10:
          return TURN(turn_needed)
      return FORWARD(speed=target.speed_cms)

  Nota: para tractores y drones con RTK GPS (2cm), la navegación
  por waypoints es suficientemente precisa sin VLM.
  El VLM se usa como safety overlay: "¿hay algo en el camino?"

Kernel side (crosstrack correction):
  crates/nav/src/lib.rs — ya tiene stubs de navegación.
  Añadir: crosstrack_error(pos, wp_a, wp_b) → corrección lateral
  para que el tractor/drone no se desvíe de la línea entre waypoints.
```

### AA4 — RTK GPS support (kernel, ~100 líneas)
```
Para precisión de 2cm (tractores, agricultura de precisión):

crates/gps/src/rtk.rs (NEW):
  - Parsear mensajes RTCM3 (correcciones diferenciales)
  - Input: UART desde base station o NTRIP caster (via 4G)
  - Feed RTCM al módulo GPS (ublox F9P u similar)
  - El módulo hace el cálculo RTK internamente
  - gps_fix_type() ahora reporta: NoFix | 2D | 3D | RTK_Float | RTK_Fixed

Hardware:
  - Módulo: u-blox ZED-F9P ($200) — soporta RTK
  - Base station: segunda F9P fija (o servicio NTRIP público)
  - Precisión: 2cm horizontal con RTK Fixed

Nota: NO implementar RTK en software. El módulo F9P lo hace.
Solo hay que:
  1. Recibir RTCM por un link (4G/WiFi) y pasarlo al módulo por UART
  2. Parsear el fix type mejorado del NMEA/UBX output
```

### AA5 — Headland turns (robot-brain, ~60 líneas)
```
Para tractores: al llegar al final de una fila, hacer giro automático.

planner/headland.py (NEW):
  def headland_turn(current_heading, next_row_heading, vehicle_type) -> list[skill]:
      # Para tractor (Ackermann steering, no puede girar en sitio):
      if vehicle_type == "ackermann":
          return [
              FORWARD(speed=30, duration=2),   # avanzar un poco
              TURN(next_heading, radius=3m),     # giro amplio
              FORWARD(speed=30, duration=2),   # realinear
          ]
      # Para diff drive (puede girar en sitio):
      else:
          return [TURN(next_heading)]

  Tipos de giro:
    - U-turn (180°): 2 filas adyacentes
    - Skip-turn: salta filas para reducir compactación del suelo
    - Fishtail: giro en 3 maniobras (para vehículos largos)
```

### Resumen Fase AA

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AA1: Mission planner (patterns) | ~200 | Nada |
| AA2: Geofencing | ~230 | GPS driver (ya existe) |
| AA3: GPS waypoint nav | ~150 | GPS + AA2 |
| AA4: RTK GPS | ~100 | GPS UART (ya existe) |
| AA5: Headland turns | ~60 | AA1 + AA3 |
| **Total** | **~740** | |

---

## Fase AB — Implement/Payload Abstraction

**Problema**: un robot no solo se mueve — también actúa sobre el entorno.
Un drone fumiga, un tractor siembra, un vigilante enciende un spotlight.

### AB1 — Payload abstraction (robot-brain + kernel, ~120 líneas)
```
Packet type nuevo:
  0x85 PAYLOAD_CMD:
    payload_type: u8
    channel:      u8
    value:        i16     # PWM, porcentaje, on/off, etc.
    duration_ms:  u16     # 0 = indefinido

payload_type:
  0 = GPIO on/off       (spotlight, sirena, relay)
  1 = PWM duty          (bomba de spray, variador de velocidad)
  2 = Servo angle       (gripper, release hook)
  3 = PTO (tractor)     (power take-off: on/off + RPM)
  4 = Spray section     (sección individual de barra de fumigación)

Kernel:
  crates/robot/src/payload.rs (NEW, ~60 líneas):
    pub fn payload_apply(cmd: &PayloadCmd) {
        match cmd.payload_type {
            GPIO  => gpio_set(cmd.channel, cmd.value != 0),
            PWM   => pwm_set(cmd.channel, cmd.value as u16),
            Servo => servo_set(cmd.channel, cmd.value),
            PTO   => pto_set(cmd.value as u16),
            Spray => spray_section(cmd.channel, cmd.value != 0),
        }
    }

robot-brain:
  Skills nuevos per payload:
    SPRAY_ON / SPRAY_OFF
    GRIPPER_OPEN / GRIPPER_CLOSE
    SPOTLIGHT_ON / SPOTLIGHT_OFF
    PTO_START / PTO_STOP
    RELEASE (drop payload)

  Los skills se definen en config.yaml por tipo de misión.
```

### AB2 — Spray control inteligente (robot-brain, ~80 líneas)
```
Para agricultura: ajustar caudal según velocidad y VLM.

policy/spray.py (NEW):
  def spray_rate(speed_cms, target_rate_ml_per_m2, swath_m) -> int:
      # PWM proporcional a velocidad para mantener dosis constante
      if speed_cms == 0: return 0
      flow_ml_per_s = target_rate * swath * speed_cms / 100
      return flow_to_pwm(flow_ml_per_s)

  Opcional con VLM:
    - VLM identifica "weed" vs "crop" en la imagen
    - Solo activa spray sobre maleza (precision spraying)
    - Ahorra 30-70% de producto
```

### AB3 — CAN bus driver (kernel, ~400 líneas)
```
Para tractores e implementos industriales que usan CAN/ISOBUS.

crates/drivers/src/can.rs (NEW):
  Target: controlador CAN del SoC o MCP2515 (SPI-to-CAN, $5)

  API:
    can_init(bitrate: u32) -> bool          // 250k, 500k, 1M
    can_send(id: u32, data: &[u8]) -> bool
    can_recv(buf: &mut CanFrame) -> bool
    can_set_filter(id: u32, mask: u32)

  Protocolos sobre CAN:
    - J1939 (tractores): engine RPM, PTO speed, implement control
    - ISOBUS (ISO 11783): maquinaria agrícola estándar
    - CANopen (industrial): servos, sensores, actuadores

  Nota: J1939/ISOBUS son complejos. En fase 1, solo CAN raw frames.
  El parsing de J1939 se puede hacer en robot-brain (Python).
```

### Resumen Fase AB

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AB1: Payload abstraction | ~120 | Nada |
| AB2: Spray control | ~80 | AB1 + VLM |
| AB3: CAN bus driver | ~400 | SPI (ya existe) |
| **Total** | **~600** | |

---

## Fase AC — Offline Autonomy (sin brain remoto)

**Problema**: con LoRa a 5km, no hay bandwidth para cámara ni para consultar
VLM/LLM en cada paso. El robot debe poder operar autónomamente con inteligencia
local limitada, reportando solo telemetría y aceptando comandos de alto nivel.

### AC1 — Mission preload (robot-brain → kernel, ~80 líneas)
```
Antes de ir al campo, el brain carga la misión completa en el robot:

Nuevo packet type:
  0x86 MISSION_UPLOAD:
    mission_id: u16
    num_waypoints: u16
    waypoints: [Waypoint; N]    # lat, lon, alt, speed, action

El brain client en VF2 almacena la misión en RAM (o FAT32 /fat/MISSION.BIN).
Ejecuta waypoint por waypoint sin necesidad del brain remoto.

Flujo:
  1. En casa (WiFi): brain planifica misión + envía MISSION_UPLOAD
  2. Robot va al campo (LoRa): ejecuta misión, reporta SENSOR_COMPACT
  3. Brain monitorea progreso (posición, battery, waypoint actual)
  4. Si necesario: brain envía COMMAND_COMPACT (skip waypoint, RTH, pause)
  5. Robot vuelve a WiFi range: descarga log completo + fotos
```

### AC2 — Onboard decision fallback (kernel, ~100 líneas)
```
El MLP local (Fase 14-15) ya funciona como reflex layer (L1):
  - Obstáculo → stop
  - Camino libre → forward

Para offline autonomy, añadir L1.5:
  - Si tiene GPS mission: seguir waypoints (GPS nav)
  - Si sensor detecta obstáculo: desviarse localmente, retomar ruta
  - Si batería baja: RTH automático
  - Si pierde GPS fix: STOP y esperar
  - Si pierde link: continuar misión (configurable) o RTH

crates/behavior/src/lib.rs:
  Behavior tree ya tiene L0-L3. Añadir:
  L1.5 = GPS_MISSION:
    - Prioridad entre L1 (reflex) y L2 (remote brain)
    - Si brain está connected → L2 override
    - Si brain desconectado → L1.5 ejecuta misión GPS
```

### AC3 — Data logging + deferred upload (kernel, ~150 líneas)
```
El robot graba todo localmente. Cuando vuelve a WiFi, sube el log.

crates/robot/src/logger.rs (NEW):
  Ring buffer en RAM (o FAT32 si tiene SD):
    - Sensor readings cada 100ms
    - GPS positions cada 1s
    - Camera frames cada 5s (JPEG, almacena en SD)
    - Events: mode changes, alerts, geofence warnings
    - Actuator commands sent

  Formato: binary log (timestamp + type + data), similar a MAVLink .tlog

  Al volver a WiFi:
    1. Brain detecta reconexión
    2. Robot envía LOG_AVAILABLE con tamaño
    3. Brain descarga via bulk transfer
    4. Robot borra log

  Útil para:
    - Debug (qué pasó cuando el robot estaba solo)
    - Training data (imágenes + acciones para fine-tuning futuro)
    - Compliance (registro de aplicación de productos agrícolas)
    - Mapeo (fotos geolocalizadas → ortomosaico)
```

### Resumen Fase AC

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AC1: Mission preload | ~80 | Protocol (R1) |
| AC2: Onboard decision fallback | ~100 | Behavior tree (ya existe) |
| AC3: Data logging + deferred upload | ~150 | FAT32 (ya existe) |
| **Total** | **~330** | |

---

## Fase AD — Data Logging, Replay y Analytics

**Problema**: para mejorar el sistema, necesitas ver qué hizo el robot,
reproducir situaciones, y analizar rendimiento.

### AD1 — Structured event log (robot-brain, ~100 líneas)
```
robot-brain/logging/event_log.py (NEW):
  class EventLogger:
    # Graba todo en SQLite local (no necesita server externo)
    def log_sensor(timestamp, sensor_data)
    def log_frame(timestamp, image_bytes, vlm_description)
    def log_decision(timestamp, scene, action, confidence)
    def log_actuator(timestamp, actuator_cmd)
    def log_alert(timestamp, trigger, description, image)
    def log_mode_change(timestamp, old_mode, new_mode)
    def log_link_change(timestamp, old_link, new_link)
    def log_geofence(timestamp, event_type, distance)

  Tabla: events(id, timestamp, type, data_json, image_blob)
  Indexado por timestamp + type para queries rápidos.
```

### AD2 — Mission replay (robot-brain, ~80 líneas)
```
robot-brain/logging/replay.py (NEW):
  Reproduce una misión grabada paso a paso:
    - Muestra frames con overlay de: sensores, decisión LLM, actuador
    - Permite avanzar/retroceder
    - Identifica puntos donde el robot tomó mala decisión
    - Export a video MP4 (opcional, con ffmpeg)

  Uso:
    python replay.py --mission 2026-03-15_field_A.db
    python replay.py --mission latest --speed 5x
```

### AD3 — Analytics dashboard (robot-brain, ~120 líneas)
```
robot-brain/monitor/analytics.py (NEW):
  Métricas calculadas de los logs:
    - Área cubierta vs área total (eficiencia)
    - Tiempo en movimiento vs parado
    - Battery consumption per km
    - Alertas por hora/misión
    - Crosstrack error promedio (precisión de navegación)
    - VLM/LLM latency percentiles
    - Link quality over time
    - Geofence violations count

  Output: terminal table o JSON (para integrar con Grafana si quieres)
```

### Resumen Fase AD

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AD1: Event logger | ~100 | SQLite (stdlib) |
| AD2: Mission replay | ~80 | AD1 |
| AD3: Analytics | ~120 | AD1 |
| **Total** | **~300** | |

---

## Fase AE — Fleet Management (multi-vehículo)

**Problema**: si tienes 3 drones cubriendo un campo, o 2 robots de vigilancia
en un edificio, necesitas coordinarlos.

### AE1 — Fleet server (robot-brain, ~200 líneas)
```
robot-brain/fleet/manager.py (NEW):
  class FleetManager:
    robots: dict[str, RobotConnection]  # id → connection

    def assign_areas(total_area: Polygon, num_robots) -> dict[str, Polygon]:
        # Divide el área equitativamente entre robots
        # Evita solapamiento

    def monitor_all() -> FleetStatus:
        # Estado de cada robot: position, battery, mode, mission progress
        # Alertas: robot desconectado, battery baja, geofence violation

    def relay(source_id, dest_id, data):
        # Robot A no tiene link directo al brain
        # Robot B actúa como relay: A→B→brain

    def redistribute(failed_id):
        # Robot 2 falla → redistribuir su área entre Robot 1 y 3

  Cada robot se conecta al mismo brain server con un robot_id en STATUS packet.
  El server mantiene estado separado por robot.
```

### AE2 — Multi-robot coordination protocol (~50 líneas)
```
Packet types nuevos:
  0x87 FLEET_STATUS:
    robot_id: u16
    num_robots: u8
    neighbors: [(id, rssi, distance_m); N]  # robots cercanos detectados

  0x88 FLEET_CMD:
    target_id: u16   (0xFFFF = broadcast)
    cmd_type:  u8    (0=assign_area, 1=relay_for, 2=RTH_all, 3=pause_all)
    payload:   [u8; N]
```

### Resumen Fase AE

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AE1: Fleet manager | ~200 | Server base |
| AE2: Fleet protocol | ~50 | Protocol (R1) |
| **Total** | **~250** | |

---

## Fase AF — MAVLink Bridge (interop con ecosistema existente)

**Problema**: ya existe un ecosistema enorme de autopilots (PX4, ArduPilot),
ground stations (QGroundControl, Mission Planner), y hardware (Pixhawk).
En vez de reimplementar todo, podemos hacer bridge.

### AF1 — MAVLink parser/builder (robot-brain o kernel, ~200 líneas)
```
MAVLink v2 es el estándar de facto para drones y vehículos autónomos.
Parser mínimo (no necesitamos los 300+ message types):

Messages que nos interesan:
  HEARTBEAT (0):        system alive, mode, status
  GPS_RAW_INT (24):     lat, lon, alt, fix, satellites
  ATTITUDE (30):        roll, pitch, yaw
  GLOBAL_POSITION (33): lat, lon, alt, velocities
  MISSION_ITEM (39):    waypoint upload
  COMMAND_LONG (76):    arm, disarm, takeoff, land, RTH
  STATUSTEXT (253):     text status messages

Dos usos:
  1. robot-brain habla MAVLink a un Pixhawk (hardware autopilot)
     → brain envía waypoints → Pixhawk los ejecuta → feedback
  2. robot-brain traduce nuestro protocolo a MAVLink
     → QGroundControl se conecta como ground station
     → visualización en mapa, misiones, telemetría gratis
```

### AF2 — QGroundControl compatible (robot-brain, ~100 líneas)
```
robot-brain/bridge/mavlink_bridge.py (NEW):
  Traduce nuestro protocolo ↔ MAVLink:

  SENSOR_PACKET → MAVLink GPS_RAW_INT + ATTITUDE + GLOBAL_POSITION
  ACTUATOR_CMD  ← MAVLink COMMAND_LONG (arm/disarm/goto)
  STATUS        → MAVLink HEARTBEAT
  MISSION_UPLOAD → MAVLink MISSION_ITEM sequence

  Puerto UDP 14550 (estándar MAVLink).
  QGroundControl se conecta automáticamente y muestra:
    - Posición en mapa
    - Telemetría en tiempo real
    - Mission planning visual (drag & drop waypoints)
    - Geofence editor visual

  Esto da ground station GRATIS sin implementar UI.
```

### Resumen Fase AF

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AF1: MAVLink parser | ~200 | Nada |
| AF2: QGC bridge | ~100 | AF1 + Protocol |
| **Total** | **~300** | |

---

## Fase AH — EKF State Estimation + Sensor Fusion

**Problema CRÍTICO para drones**: sin EKF, los sensores son datos crudos con ruido.
Un drone no puede hacer hover estable con IMU raw. Necesita fusión de sensores
que filtre ruido, compense bias, y combine múltiples fuentes en una sola estimación.

**Referencia**: PX4 EKF2 (24 estados, delayed fusion, error-state quaternion).

### AH1 — EKF core (kernel, ~500 líneas)
```
crates/nav/src/ekf.rs (NEW):
  Extended Kalman Filter con estado mínimo viable:

  State vector (15 estados):
    - position:     [x, y, z]        (NED, metros)
    - velocity:     [vx, vy, vz]     (NED, m/s)
    - attitude:     [q0, q1, q2, q3] (quaternion)
    - gyro_bias:    [bx, by, bz]     (rad/s, estimado online)
    - accel_bias:   [bax, bay, baz]  (m/s², estimado online)

  Matrices:
    - P[15×15]: covariance
    - Q[15×15]: process noise
    - Implementación: arrays estáticos, sin alloc

  Predict (a cada muestra IMU, ~200-1000 Hz):
    1. Integrar gyro → rotar quaternion
    2. Rotar accel a NED → integrar velocidad → integrar posición
    3. Propagar covariance P = F*P*F' + Q

  Update (cuando llega medición GPS/baro/mag):
    - GPS update: corrige position + velocity
    - Baro update: corrige altitude (z)
    - Mag update: corrige heading (yaw)
    - Innovation check: si residual > 5σ → rechazar medición (sensor fault)

  Error-state formulation:
    - No estimar quaternion directamente (singularidades)
    - Estimar error en rotación (3 ángulos pequeños)
    - Aplicar corrección al quaternion después del update

  Delayed fusion (como PX4):
    - Cada sensor tiene latencia diferente (GPS ~200ms, baro ~50ms, IMU ~1ms)
    - Buffer de IMU measurements
    - Cuando llega GPS, rewind al timestamp del GPS, apply update, re-propagate

  Todo integer donde posible:
    - Position/velocity: mm y mm/s (i32)
    - Quaternion: Q30 fixed-point (i32 con 30 bits fraccionarios)
    - Solo P matrix en f32 (necesita dinámica de rango)
```

### AH2 — Sensor calibration (kernel, ~150 líneas)
```
crates/nav/src/calibration.rs (NEW):
  Al boot, calibración automática:

  IMU:
    - Gyro bias: promedio de primeras 1000 muestras (drone quieto)
    - Accel bias: comparar con gravedad esperada [0,0,-9.81]
    - Gyro temperature compensation (tabla LUT si disponible)

  Magnetometer:
    - Hard-iron offset: centro de la esfera de muestras
    - Soft-iron scaling: elipsoide → esfera
    - Declination correction (de GPS position o config)

  Barometer:
    - Reference pressure al boot (ground level = 0m)
    - Temperatura compensation

  Nota: calibración completa (girar el drone en 8) es manual.
  Calibración básica (bias removal) es automática al boot.
```

### AH3 — Redundancia de sensores + voting (kernel, ~200 líneas)
```
crates/nav/src/redundancy.rs (NEW):
  Para drones serios: dual IMU, dual baro, dual GPS.

  Voting strategy:
    2 sensores:
      - Si ambos coinciden (dentro de tolerancia) → promediar
      - Si divergen → marcar uno como suspect, usar el otro
      - Si ambos divergen mucho → ALERT, usar EKF prediction only

    3 sensores (TMR):
      - Median voter: usar valor del medio
      - Si uno diverge de los otros dos → descartarlo automáticamente
      - Reportar sensor health en STATUS packet

  sensor_health: [SensorStatus; MAX_SENSORS]
    SensorStatus { id, type, ok: bool, last_update_ms, divergence_count }

  Integración con Safety FSM (Fase AG):
    - ImuFailure se detecta aquí (readings frozen, divergencia, NaN)
    - GpsLost se detecta aquí (no fix por >5s)
    - BaroFailure se detecta aquí (pressure reading implausible)
```

### Resumen Fase AH

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AH1: EKF core (15 estados) | ~500 | IMU + GPS (ya existen) |
| AH2: Sensor calibration | ~150 | AH1 |
| AH3: Sensor redundancy + voting | ~200 | AH1 + AG (safety) |
| **Total** | **~850** | |

---

## Fase AI — Simulación (SITL/HITL) — ANTES QUE HARDWARE

**Problema**: no puedes probar nada físico sin riesgo de romper hardware.
TODO se prueba primero en simulación. El brain server no sabe si habla con
un robot real o simulado — mismo protocolo TCP, mismo código.

**Principio**: simular PRIMERO, hardware DESPUÉS. Cada tipo de robot
tiene su simulador. Probar al menos 10 horas simuladas antes de hardware real.

### AI0 — SITL Wheeled (robot-brain, ~200 líneas) *** SEMANA 1 — ANTES DE TODO ***
```
tools/sitl/sitl_wheeled.py (NEW):
  Simulador de robot con ruedas differential drive.
  Se conecta al brain server como si fuera el VF2 real.
  CERO dependencias: solo Python stdlib + protocol.py.

  class WheeledSim:
    # Estado físico
    x_mm: int = 0             # posición X (mm)
    y_mm: int = 0             # posición Y (mm)
    theta_cdeg: int = 0       # heading (centidegrees)
    speed_l: int = 0          # velocidad motor izquierdo
    speed_r: int = 0          # velocidad motor derecho
    battery_mv: int = 8400    # batería (drena lento)
    encoder_l: int = 0        # ticks encoder izquierdo
    encoder_r: int = 0        # ticks encoder derecho

    # Parámetros del robot (Yahboom chassis 310 motors)
    wheel_base_mm: int = 142  # distancia entre ruedas
    ticks_per_m: int = 1000   # encoder ticks por metro
    max_speed: int = 80       # velocidad máxima

    def step(self, dt=0.05):
        # Modelo cinemático differential drive
        v = (self.speed_l + self.speed_r) / 2
        w = (self.speed_r - self.speed_l) * 36000 / (self.wheel_base_mm * 2)
        self.x_mm += v * cos(theta_rad) * dt
        self.y_mm += v * sin(theta_rad) * dt
        self.theta_cdeg += w * dt
        # Encoders
        self.encoder_l += self.speed_l * self.ticks_per_m * dt / 1000
        self.encoder_r += self.speed_r * self.ticks_per_m * dt / 1000
        # Battery drain simulado
        drain = (abs(self.speed_l) + abs(self.speed_r)) / 10
        self.battery_mv -= drain * dt

    def sensor_packet(self) -> bytes:
        return SensorPacket(
            timestamp_ms=time_ms(),
            accel_mg=(0, 0, 1000),           # gravedad 1g en Z
            gyro_mdps=(0, 0, self.omega()),   # rotación Z
            odom_dist_mm=self.distance(),
            odom_hdg_cdeg=self.theta_cdeg,
            encoder_l=self.encoder_l,
            encoder_r=self.encoder_r,
            range_front_mm=random(200, 5000), # obstáculo simulado
            range_right_mm=random(100, 3000),
            battery_mv=self.battery_mv,
        ).to_bytes()

  Entorno simulado:
    class SimWorld:
      walls: list[Line]         # paredes (colisión)
      obstacles: list[Circle]   # obstáculos circulares
      rooms: dict[str, Point]   # locations nombrados ("kitchen", "bedroom")

      def raycast(origin, direction) -> int:
          # Rangefinder simulado: distancia al primer obstáculo
          # Usa las walls y obstacles del mundo

      def check_collision(robot_pos, robot_radius) -> bool:
          # ¿El robot chocó con algo?

    Mundos predefinidos:
      house.yaml:    casa con 4 habitaciones, pasillos, puertas
      office.yaml:   oficina con cubículos
      field.yaml:    campo abierto con bordes
      empty.yaml:    espacio vacío (calibración)

  Servidor TCP:
    async def main():
      sim = WheeledSim()
      world = SimWorld.load("house.yaml")

      # Conectar al brain server como si fuera el VF2
      reader, writer = await asyncio.open_connection(brain_host, brain_port)
      # O: escuchar como servidor (el brain se conecta a nosotros)

      while True:
          # 1. Enviar sensor data al brain
          await send_packet(writer, SENSOR_PACKET, sim.sensor_packet())

          # 2. Recibir ActuatorCmd del brain
          pkt = await read_packet_timeout(reader, 0.05)
          if pkt and pkt.type == ACTUATOR_CMD:
              cmd = ActuatorCmd.from_bytes(pkt.payload)
              sim.speed_l = cmd.channels[0]
              sim.speed_r = cmd.channels[1]

          # 3. Step physics
          sim.step(dt=0.05)
          world.check_collision(sim)

          # 4. Cada 500ms: enviar camera frame simulado
          if frame_timer():
              frame = render_topdown(sim, world)  # vista cenital
              await send_packet(writer, CAMERA_FRAME, frame)

  Visualización (opcional, matplotlib):
    - Vista cenital del mundo con paredes
    - Posición del robot (triángulo orientado)
    - Trayectoria recorrida
    - Rangefinder rays (líneas desde robot hasta obstáculo)
    - Actualización en tiempo real

Uso:
  Terminal 1: python tools/sitl/sitl_wheeled.py --world house.yaml --viz
  Terminal 2: python server.py --mode patrulla
  → El brain patrulla una casa simulada, ve obstáculos, decide, gira, etc.

  Sin visualización (headless, para tests):
  python tools/sitl/sitl_wheeled.py --world house.yaml --headless --duration 3600
  → 1 hora de patrulla simulada, genera log para análisis
```

### AI1 — SITL Drone (robot-brain, ~300 líneas) *** PRE-DRONE ***
```
tools/sitl/sitl_drone.py (NEW):
  Modelo físico simplificado del drone.
  Se implementa cuando se vaya a trabajar con drones (post fases AH-AK).

  class DronePhysics:
    position: [x, y, z]       # metros NED
    velocity: [vx, vy, vz]
    attitude: Quaternion
    angular_vel: [p, q, r]
    motor_rpms: [float; 4]

    def step(dt, motor_commands, wind):
        forces = compute_motor_forces(motor_commands)
        forces += gravity + aerodynamic_drag + wind
        torques = compute_motor_torques(motor_commands)
        # Newton-Euler equations
        acceleration = forces / mass
        angular_accel = inertia_inv * (torques - cross(angular_vel, inertia * angular_vel))
        # Integrate
        velocity += acceleration * dt
        position += velocity * dt
        angular_vel += angular_accel * dt
        attitude = integrate_quaternion(attitude, angular_vel, dt)

  sitl_sensors.py:
    Genera datos de sensores simulados a partir del estado físico:
    - IMU: accel + gyro + noise + bias (configurable)
    - GPS: position + delay(200ms) + noise(2m) + occasional dropout
    - Baro: altitude + noise + drift
    - Mag: heading + hard-iron offset + noise
    - Rangefinder: distance to ground + noise

  Uso:
    Terminal 1: python tools/sitl/sitl_drone.py --wind 5.0
    Terminal 2: python server.py --mode patrulla
    → El brain controla un drone simulado con viento de 5 m/s
```

### AI1b — SITL Humanoid (MuJoCo, ~200 líneas) *** PRE-HUMANOID ***
```
tools/sitl/sitl_humanoid.py (NEW):
  Usa MuJoCo para física realista de contactos, balance, caídas.
  Se implementa cuando se vaya a trabajar con humanoides (post fases AO-AU).

  Requiere: pip install mujoco

  class HumanoidSim:
    model: mujoco.MjModel      # cargado de URDF/MJCF
    data: mujoco.MjData

    def step(joint_commands):
        data.ctrl[:] = joint_commands
        mujoco.mj_step(model, data)

    def sensor_packet():
        # IMU del torso
        # Joint angles actuales
        # Foot contact forces
        # Camera render

  Uso:
    python tools/sitl/sitl_humanoid.py --model humanoid_12dof.xml --viz
```

### AI2 — Integración con simuladores externos (~100 líneas)
```
Bridges para conectar simuladores 3D al brain server:

tools/bridges/ (NEW directory):

  webots_bridge.py:
    Conecta Webots al brain via protocolo TCP.
    - Lee sensores de Webots (cámara, LiDAR, IMU, GPS)
    - Convierte a SensorPacket
    - Recibe ActuatorCmd → aplica a motores Webots
    Util para: visión 3D realista (cámara renderizada), LiDAR

  gazebo_bridge.py:
    Conecta Gazebo/ROS2 al brain via protocolo TCP.
    - Suscribe a topics ROS2 (sensor_msgs, nav_msgs)
    - Convierte a nuestro formato binario
    Util para: simulaciones de flota, drones con ROS2

  Nota: estos bridges son OPCIONALES. El SITL custom es suficiente
  para el 90% del desarrollo. Los bridges se usan cuando se necesita:
    - Renderizado 3D realista (para probar VLM con imágenes reales)
    - Física de colisiones complejas
    - LiDAR simulado
    - Múltiples robots simultáneos
```

### AI3 — HITL support (kernel + tools, ~100 líneas)
```
Hardware-in-the-Loop: el kernel REAL corre en el VF2,
pero los sensores vienen del simulador en vez del hardware.

tools/hitl/hitl_bridge.py:
  - Corre modelo físico en PC
  - Envía sensor data simulada al VF2 via TCP/UART
  - Recibe ActuatorCmd del VF2
  - Verifica que el kernel real se comporta igual que SITL

Kernel support:
  CONFIG.INI: sensor_source = hardware | sitl
  Si sitl: leer sensores de un socket/UART en vez del I2C/SPI real

Orden de validación:
  1. SITL puro (PC only) → verificar lógica del brain
  2. HITL (kernel real + sensores simulados) → verificar kernel
  3. Hardware real → verificar actuadores + sensores reales
  Si algo falla en 3 que no falla en 1-2 → problema de hardware/driver
```

### AI4 — Test scenarios library (~150 líneas)
```
tools/sitl/scenarios/ (NEW):
  YAML files con escenarios de prueba por tipo de robot.

  === Wheeled scenarios (probar desde semana 1): ===

  wheeled/patrol_house.yaml:
    world: house.yaml
    mode: patrulla
    waypoints: [kitchen, living_room, bedroom, entry]
    duration: 600s
    expected: {visits_all: true, collisions: 0, battery_ok: true}

  wheeled/obstacle_avoid.yaml:
    world: obstacles.yaml
    mode: explorar
    duration: 300s
    obstacles: {count: 10, random_positions: true}
    expected: {collisions: 0}

  wheeled/security_detect.yaml:
    world: house.yaml
    mode: seguridad
    events:
      - {time: 120s, action: spawn_person, location: kitchen}
    expected: {alert_triggered: true, alert_time: <30s}

  wheeled/battery_low.yaml:
    world: house.yaml
    battery: {start: 6800, drain_rate: 100}
    expected: {returns_home: true, battery_above: 6500}

  wheeled/long_run.yaml:
    world: house.yaml
    mode: patrulla
    duration: 36000s  # 10 horas
    expected: {crashes: 0, memory_leaks: false}

  === Drone scenarios (probar pre-drone): ===

  drone/hover_test.yaml:
    initial: {position: [0,0,-10], attitude: level}
    duration: 30s
    expected: {position_error: <0.5m, attitude_error: <5deg}

  drone/wind_rejection.yaml:
    wind: {speed: 8.0, direction: 90, gusts: 3.0}
    duration: 60s
    expected: {drift: <2m}

  drone/motor_failure.yaml:
    events:
      - {time: 10s, action: kill_motor, motor: 2}
    expected: {lands_safely: true, crash: false}

  drone/battery_rth.yaml:
    initial: {position: [500,0,-30]}
    battery: {start: 7200, drain_rate: 50}
    expected: {triggers_rth: true, reaches_home: true}

  drone/geofence_test.yaml:
    geofence: {type: circle, center: [0,0], radius: 100m}
    mission: fly_to [150, 0, -30]
    expected: {stops_at_boundary: true}

  === Humanoid scenarios (probar pre-humanoid): ===

  humanoid/stand_balance.yaml:
    initial: standing
    perturbations:
      - {time: 5s, force: [30, 0, 0], duration: 0.1s}   # push forward
      - {time: 15s, force: [0, 50, 0], duration: 0.1s}   # push lateral
    expected: {falls: 0, recovers: true}

  humanoid/walk_5m.yaml:
    command: walk_forward 5m
    expected: {reaches_goal: true, falls: 0, time: <30s}

  humanoid/pick_up_object.yaml:
    object: {type: cup, position: [500, 0, 800]}
    command: grab cup
    expected: {grasped: true, falls: 0}

  === Automated runner: ===

  python tools/sitl/run_tests.py --type wheeled --all
  python tools/sitl/run_tests.py --type drone --all
  python tools/sitl/run_tests.py --type humanoid --all
  python tools/sitl/run_tests.py --all  # todos los tipos

  Output: PASS/FAIL por scenario + log detallado + métricas
```

### AI5 — Visualización + Ground Station simulada (~100 líneas)
```
tools/sitl/viz.py (NEW):
  Visualización en tiempo real del SITL (matplotlib o pygame):

  Para wheeled:
    - Vista cenital 2D del mundo
    - Robot como triángulo orientado
    - Trayectoria recorrida (línea)
    - Rangefinder rays (líneas)
    - Waypoints y locations marcados
    - Panel lateral: batería, modo, velocidad, heading

  Para drone:
    - Vista cenital 2D + vista lateral (altitud)
    - Posición 3D del drone
    - Geofence dibujado
    - Wind vector arrow
    - Panel: altitud, GPS, battery, mode

  Para humanoid:
    - Stick figure 2D (vista frontal + lateral)
    - ZMP point vs support polygon
    - CoM trajectory
    - Joint angles como barras

  Uso:
    python tools/sitl/sitl_wheeled.py --world house.yaml --viz
    # Abre ventana matplotlib con el mundo + robot moviéndose en real-time
```

### Resumen Fase AI

| Sub-fase | Líneas | Cuándo | Depende de |
|----------|--------|--------|-----------|
| **AI0: SITL Wheeled** | **~200** | **Semana 1 (YA)** | **protocol.py (ya existe)** |
| AI1: SITL Drone | ~300 | Pre-drone | Protocol + drone physics |
| AI1b: SITL Humanoid (MuJoCo) | ~200 | Pre-humanoid | MuJoCo + URDF |
| AI2: Bridges (Webots/Gazebo) | ~100 | Opcional | Simulador externo instalado |
| AI3: HITL bridge | ~100 | Pre-hardware real | AI0/AI1 + kernel |
| AI4: Test scenarios | ~150 | Con AI0 | AI0 |
| AI5: Visualización | ~100 | Con AI0 | AI0 + matplotlib |
| **Total** | **~1150** | | |

### Herramientas externas (no implementar, solo integrar si se necesita):

| Simulador | Para qué | Cuándo | Instalación |
|-----------|----------|--------|-------------|
| **ir-sim** | Navegación 2D rápida, multi-robot | Opcional | `pip install ir-sim` |
| **Webots** | 3D realista, cámara renderizada, LiDAR | Pre-drone/campo | Descarga gratuita |
| **Gazebo** | 3D industrial, ROS2, flotas | Fase AE (fleet) | Con ROS2 |
| **MuJoCo** | Humanoides, RL training, contactos | Fases AO-AU | `pip install mujoco` |
| **NVIDIA Isaac** | GPU-accelerated, digital twins | Opcional avanzado | Requiere NVIDIA GPU |

---

## Fase AJ — 3D Path Planning + Obstacle Avoidance

**Problema**: "si hay obstáculo → stop" no sirve para drones. Un drone debe
planificar rutas 3D alrededor de obstáculos (cables, árboles, edificios).

**Referencia**: PX4-Avoidance (VFH+*, octomap), Skydio (6 cámaras, mapa 3D continuo).

### AJ1 — Occupancy grid 3D (robot-brain o kernel, ~200 líneas)
```
nav/occupancy.py (robot-brain) o crates/nav/src/occupancy.rs (kernel):
  Mapa 3D del entorno como grid de celdas ocupadas/libres.

  class OccupancyGrid3D:
    resolution: float = 0.5  # metros por celda
    size: [100, 100, 20]     # 100×100×20 celdas = 50×50×10 metros
    data: bitarray            # 1 bit por celda = 25 KB

    def update_from_depth(camera_pose, depth_image):
        # Raycast desde cámara, marcar celdas como occupied/free
        for pixel in depth_image:
            point_3d = deproject(pixel, depth)
            world_point = camera_pose * point_3d
            cell = world_to_cell(world_point)
            data[cell] = OCCUPIED

    def update_from_rangefinder(position, direction, distance):
        # Single ray update (más simple)

    def is_free(x, y, z) -> bool
    def is_path_clear(start, end) -> bool  # ray check
```

### AJ2 — Path planner 3D (robot-brain, ~200 líneas)
```
nav/planner_3d.py (NEW):
  Algoritmos de planificación:

  A* 3D:
    - Grid-based, óptimo, lento en grids grandes
    - Bueno para planning global (waypoint A → B)

  RRT* (Rapidly-exploring Random Tree):
    - Sampling-based, rápido en espacios grandes
    - Bueno para entornos con muchos obstáculos
    - Probabilistically optimal

  VFH+ (Vector Field Histogram):
    - Local planner, rápido
    - Bueno para evitar obstáculos reactivamente
    - Genera "best direction" basado en histogram polar de obstáculos

  Arquitectura dual (como PX4):
    - Global planner (A*/RRT*): ruta de A a B evitando obstáculos conocidos
    - Local planner (VFH+): ajuste reactivo por obstáculos nuevos
    - Global replanning si local planner se queda atascado
```

### AJ3 — Depth perception (robot-brain, ~100 líneas)
```
perception/depth.py (NEW):
  Obtener distancia a obstáculos:

  Opción 1 — Stereo cameras:
    - Dos cámaras separadas → disparidad → depth map
    - Computacionalmente pesado, pero no necesita hardware extra

  Opción 2 — Monocular depth estimation (VLM/NN):
    - Una cámara → red neuronal estima depth
    - Modelos: MiDaS, Depth Anything V2 (open source)
    - Menos preciso pero funciona con 1 cámara
    - Puede correr en LM Studio/macOS

  Opción 3 — LiDAR/ToF sensor:
    - Hardware adicional ($50-200)
    - Más preciso y rápido
    - Intel RealSense D435i popular en drones

  Output: depth_map[H×W] → alimenta occupancy grid (AJ1)
```

### Resumen Fase AJ

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AJ1: Occupancy grid 3D | ~200 | Sensores de profundidad |
| AJ2: Path planner 3D (A*/RRT*/VFH+) | ~200 | AJ1 |
| AJ3: Depth perception | ~100 | Cámara (T1) o LiDAR |
| **Total** | **~500** | |

---

## Fase AK — Motor Mixing + Wind Compensation

**Problema**: nuestro `ActuatorCmd channels[4]` va directo a ESC.
Un sistema de vuelo real necesita una capa de mixing que convierta
comandos de actitud (roll/pitch/yaw/throttle) en PWM por motor,
y compense perturbaciones como viento.

### AK1 — Motor mixer (kernel, ~150 líneas)
```
crates/flight/src/mixer.rs (NEW o ampliar existente):
  Convierte comandos de actitud → thrust por motor.

  Configuraciones soportadas:
    QUAD_X:     [+,+,-,+], [-,+,+,+], [+,-,+,+], [-,-,-,+]
    QUAD_PLUS:  [0,+,-,+], [-,0,+,+], [0,-,+,+], [+,0,-,+]
    HEX_X:      6 motores
    OCTO_X:     8 motores

  pub fn mix(throttle: i16, roll: i16, pitch: i16, yaw: i16,
             layout: MotorLayout) -> [i16; MAX_MOTORS] {
      let mut output = [0i16; MAX_MOTORS];
      for (i, config) in layout.motors.iter().enumerate() {
          output[i] = throttle
              + roll  * config.roll_factor
              + pitch * config.pitch_factor
              + yaw   * config.yaw_factor;
          output[i] = output[i].clamp(MIN_THROTTLE, MAX_THROTTLE);
      }
      // Desaturation: si algún motor satura, reducir todos proporcionalmente
      desaturate(&mut output);
      output
  }

  Motor failure compensation:
    pub fn mix_with_failure(cmd, layout, failed_motors: u8) -> [i16; MAX_MOTORS] {
        // Recalcular mixing matrix sin los motores fallidos
        // Reducir capacidad de maniobra pero mantener vuelo
        // Si no es posible (>1 motor fallido en quad) → safety failsafe
```

### AK2 — Attitude PID controller (kernel, ~200 líneas)
```
crates/flight/src/attitude.rs (NEW o ampliar existente):
  PID de 3 ejes que corre en RT task (>200 Hz).

  pub struct AttitudePID {
      roll:  PIDController,
      pitch: PIDController,
      yaw:   PIDController,
      alt:   PIDController,
  }

  impl AttitudePID {
      pub fn update(&mut self, desired: Attitude, current: Attitude,
                    dt: f32) -> MixerInput {
          MixerInput {
              roll:     self.roll.compute(desired.roll - current.roll, dt),
              pitch:    self.pitch.compute(desired.pitch - current.pitch, dt),
              yaw:      self.yaw.compute(desired.yaw - current.yaw, dt),
              throttle: self.alt.compute(desired.alt - current.alt, dt),
          }
      }
  }

  Tuning via config:
    pid_roll  = [P, I, D, I_MAX]
    pid_pitch = [P, I, D, I_MAX]
    pid_yaw   = [P, I, D, I_MAX]
    pid_alt   = [P, I, D, I_MAX]
```

### AK3 — Wind estimation + feedforward (kernel, ~150 líneas)
```
crates/flight/src/wind.rs (NEW):
  Estima viento a partir del comportamiento del drone:

  Principio: si el drone está hover (velocidad=0) pero está inclinado,
  el tilt compensa viento. Viento ≈ f(tilt, throttle, mass).

  pub struct WindEstimator {
      wind_ned: [f32; 3],  // estimación actual [N, E, D] m/s
      alpha: f32,          // filtro exponencial (0.01-0.1)
  }

  impl WindEstimator {
      pub fn update(&mut self, accel_body: [f32;3], attitude: Quaternion,
                    velocity_ned: [f32;3], throttle: f32) {
          // Accel esperada (sin viento) = rotate(attitude, [0,0,-throttle_to_accel])
          // Accel medida = accel real
          // Diferencia = fuerza del viento / masa
          let expected = rotate(attitude, accel_from_throttle(throttle));
          let residual = accel_body - expected;
          let wind_accel = rotate_to_ned(attitude, residual);
          // Integrar para obtener wind velocity
          self.wind_ned = lerp(self.wind_ned, integrate(wind_accel), self.alpha);
      }

      pub fn feedforward(&self, attitude: Quaternion) -> MixerInput {
          // Inclinar el drone ligeramente contra el viento estimado
          // Reduce drift antes de que el PID tenga que corregir
      }
  }
```

### Resumen Fase AK

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AK1: Motor mixer | ~150 | ESC driver (ya existe) |
| AK2: Attitude PID | ~200 | EKF (AH1) |
| AK3: Wind estimation | ~150 | AH1 + AK2 |
| **Total** | **~500** | |

---

## Fase AL — Terrain Following + Smart RTH

### AL1 — Terrain following (kernel + brain, ~100 líneas)
```
Mantener altitud sobre el terreno (no sobre el mar/punto de despegue).

crates/flight/src/terrain.rs (NEW):
  pub fn terrain_follow_throttle(
      target_agl_m: f32,      // altitud deseada sobre suelo
      sonar_distance_m: f32,  // lectura sonar down
      baro_altitude_m: f32,   // altitud barométrica
      current_throttle: f32,
  ) -> f32 {
      // PID sobre (sonar_distance - target_agl)
      // Fallback a baro si sonar fuera de rango (>10m)
  }

  Uso: agricultura (spray uniforme en terreno con pendiente)
```

### AL2 — Smart RTH (robot-brain, ~150 líneas)
```
Return-to-Home que no choca con edificios.

planner/rth.py (NEW):
  def plan_rth(current_pos, home_pos, occupancy_grid, geofence, battery) -> list[Waypoint]:
      # 1. Subir a safe altitude (configurable, o max obstáculo + 10m)
      # 2. Verificar ruta directa: ¿libre de obstáculos?
      #    Sí → ruta directa
      #    No → A* 3D alrededor de obstáculos
      # 3. Verificar geofence: ¿ruta cruza zona prohibida?
      #    Sí → rodear zona prohibida
      # 4. Verificar batería: ¿suficiente para esta ruta?
      #    No → aterrizar en punto seguro más cercano
      # 5. Descender sobre home → land

  Alternativa simple (sin occupancy grid):
      # 1. Subir a safe_altitude
      # 2. Volar en línea recta a home
      # 3. Descender
      # Funciona si no hay obstáculos altos entre aquí y home
```

### Resumen Fase AL

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AL1: Terrain following | ~100 | Sonar/LiDAR + alt PID |
| AL2: Smart RTH | ~150 | AJ (path planning) |
| **Total** | **~250** | |

---

## Fase AM — SLAM + Visual Odometry

**Problema**: indoor (sin GPS) o cuando GPS no es suficientemente preciso.
SLAM construye un mapa del entorno mientras navega. Visual Odometry estima
movimiento por cambios entre frames de cámara.

### AM1 — Visual Odometry básica (robot-brain, ~200 líneas)
```
perception/visual_odom.py (NEW):
  Estima movimiento relativo entre 2 frames consecutivos.

  Flujo:
    1. Detectar features (ORB, FAST, o Harris corners)
    2. Match features entre frame N y N+1
    3. Estimar Essential matrix (5-point algorithm)
    4. Descomponer en rotación + traslación
    5. Escalar con IMU/rangefinder (monocular VO no tiene escala)

  Output: delta_pose (dx, dy, dz, droll, dpitch, dyaw) cada frame
  Se alimenta al EKF como medición adicional (complementa GPS)

  Nota: VO es computacionalmente pesado. Opciones:
    - Correr en macOS (robot-brain) con frames recibidos → latencia
    - Correr onboard con cámara local → mejor pero necesita compute
    - Usar VLM para VO approximada ("moved ~1m forward") → lento, impreciso
```

### AM2 — SLAM graph-based (robot-brain, ~300 líneas futuro)
```
Más avanzado que VO: construye mapa + optimiza posiciones pasadas.

perception/slam.py (NEW, futuro):
  Graph SLAM:
    - Nodos = poses del robot en momentos distintos
    - Edges = odometría entre poses + loop closures
    - Optimización: minimizar error total (g2o, GTSAM, o custom)
    - Output: mapa 2D/3D + trayectoria corregida

  Para nuestro caso:
    - Indoor: SLAM reemplaza GPS
    - Outdoor: SLAM complementa GPS (más preciso en entornos densos)
    - Mapa persistente: guarda mapa → lo recarga en siguiente misión

  Nota: SLAM completo es un proyecto en sí mismo. Alternativa:
    - Usar NVIDIA Isaac ROS Visual SLAM si hay Jetson
    - Usar ORB-SLAM3 (open source, C++)
    - O quedarse con VO (AM1) + GPS como first version
```

### Resumen Fase AM

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AM1: Visual Odometry | ~200 | Camera + EKF (AH1) |
| AM2: Graph SLAM | ~300 | AM1 (futuro avanzado) |
| **Total** | **~500** | |

---

## Fase AN — Testing Framework + CI

**Problema**: 2 test files manuales no escalan. PX4 tiene miles de tests.
Tesla hace regression testing continuo. Necesitamos tests automatizados.

### AN1 — Unit test suite (robot-brain, ~200 líneas)
```
tests/ (expandir):
  test_ekf.py          — EKF converge con datos sintéticos
  test_mixer.py        — motor mixing correcto para cada layout
  test_geofence.py     — point-in-polygon, buffer zones
  test_mission.py      — boustrophedon genera waypoints correctos
  test_safety.py       — cada FailsafeEvent genera acciones correctas per type
  test_wind.py         — wind estimator converge con viento simulado
  test_skills.py       — cada skill se ejecuta correctamente
  test_notifications.py — pushover/telegram mock send

  Correr: pytest tests/ -v
```

### AN2 — SITL integration tests (~100 líneas)
```
tests/integration/ (NEW):
  test_hover.py:
    - Lanzar SITL + brain
    - Enviar TAKEOFF → verificar altitud estable en 10s
    - Enviar HOVER 30s → verificar drift < 1m
    - Enviar LAND → verificar toca suelo

  test_mission.py:
    - Cargar misión de 5 waypoints
    - Ejecutar en SITL
    - Verificar todos los waypoints alcanzados

  test_failsafe.py:
    - Simular pérdida de link → verificar RTH
    - Simular battery low → verificar landing
    - Simular motor failure → verificar controlled descent

  Correr: pytest tests/integration/ --sitl
```

### AN3 — Chaos testing / fault injection (~80 líneas)
```
tools/chaos/ (NEW):
  Inyectar fallos aleatorios durante SITL para probar robustez:

  chaos_runner.py:
    Fallos inyectables:
      - GPS dropout (5-30s)
      - IMU spike (valores absurdos por 1 frame)
      - IMU frozen (misma lectura repetida)
      - Baro drift (+50m en 10s)
      - Motor degradation (80% thrust en 1 motor)
      - Link loss (5-60s)
      - Wind gust (repentino 10 m/s)
      - Camera black frame

    python tools/chaos/chaos_runner.py --duration 300 --fault-rate 0.1
    → corre 5 min de SITL con fallos aleatorios cada ~10s
    → reporta: crashes, geofence violations, safety triggers, recovery time
```

### AN4 — CI pipeline (config, ~30 líneas)
```
.github/workflows/test.yml:
  - pytest tests/                    # unit tests
  - cargo build (all feature combos) # kernel builds
  - pytest tests/integration/ --sitl # SITL tests
  - python tools/chaos/chaos_runner.py --duration 60  # quick chaos
  - Coverage report

Triggers: on push, on PR, nightly (extended chaos + all scenarios)
```

### Resumen Fase AN

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AN1: Unit test suite | ~200 | Nada |
| AN2: SITL integration tests | ~100 | AI (SITL) |
| AN3: Chaos testing | ~80 | AI (SITL) |
| AN4: CI pipeline | ~30 | AN1 + AN2 |
| **Total** | **~410** | |

---

## ═══════════════════════════════════════════════════
## FASES HUMANOID-SPECIFIC
## ═══════════════════════════════════════════════════

Estas fases son específicas para robots humanoides bípedos. No aplican a
drones ni a robots con ruedas. Se implementan cuando se tenga hardware
humanoide. Los problemas fundamentales son: caminar sin caer, manipular
objetos, y operar de forma segura cerca de personas.

---

## Fase AO — Balance + ZMP (Zero Moment Point)

**Problema CRÍTICO para humanoides**: sin balance activo, el robot cae.
Es el equivalente al EKF+PID del drone — sin esto, no funciona.

**Referencia**: ZMP es el estándar industrial. Tesla Optimus, Honda ASIMO,
Boston Dynamics Atlas todos usan variantes de ZMP + whole-body control.

### AO1 — ZMP calculator + CoM tracker (kernel, ~300 líneas)
```
crates/humanoid/src/zmp.rs (NEW):
  El ZMP es el punto donde las fuerzas de inercia+gravedad no generan
  momento de rotación. Si el ZMP sale del polígono de soporte (pies),
  el robot cae.

  pub struct BalanceState {
      com_position: [i32; 3],     // centro de masa (mm)
      com_velocity: [i32; 3],     // velocidad del CoM (mm/s)
      zmp: [i32; 2],              // Zero Moment Point (mm, plano XY)
      support_polygon: Polygon,   // convex hull de los pies en contacto
      is_stable: bool,            // zmp dentro del polígono?
      stability_margin: i32,      // distancia del ZMP al borde (mm)
  }

  pub fn compute_zmp(
      com: [i32; 3],
      com_accel: [i32; 3],       // de IMU/EKF
      foot_forces: [i32; 2],     // fuerza en cada pie (sensores)
  ) -> [i32; 2] {
      // ZMP_x = CoM_x - (CoM_z * CoM_accel_x) / (g + CoM_accel_z)
      // ZMP_y = CoM_y - (CoM_z * CoM_accel_y) / (g + CoM_accel_z)
      // Aritmética entera: escalar para evitar overflow
  }

  pub fn is_stable(zmp: [i32;2], support: &Polygon) -> bool {
      point_in_convex_hull(zmp, support)
  }

  pub fn stability_margin(zmp: [i32;2], support: &Polygon) -> i32 {
      // Distancia mínima del ZMP al borde del polígono
      // Positivo = estable, negativo = cayendo
  }

  Corre onboard a 200+ Hz. Alimenta al balance controller.
```

### AO2 — Balance controller (kernel, ~250 líneas)
```
crates/humanoid/src/balance.rs (NEW):
  Controlador que mantiene el ZMP dentro del polígono de soporte.

  pub struct BalanceController {
      pid_roll: PIDController,    // inclinación lateral
      pid_pitch: PIDController,   // inclinación frontal
      ankle_strategy: bool,       // corrección vía tobillos (perturbaciones pequeñas)
      hip_strategy: bool,         // corrección vía caderas (perturbaciones medianas)
      step_strategy: bool,        // dar un paso extra (perturbaciones grandes)
  }

  Tres estrategias de balance (como los humanos):
    1. Ankle strategy:  perturbación pequeña (<3cm ZMP error)
       → ajustar ángulo de tobillos para mover ZMP
       → rápido, sutil, no requiere mover pies

    2. Hip strategy:    perturbación mediana (3-8cm)
       → mover cadera/torso para reposicionar CoM
       → más lento, más visible

    3. Stepping strategy: perturbación grande (>8cm o ZMP fuera de polígono)
       → dar un paso en dirección de la caída
       → el más lento, pero salva de caídas grandes
       → requiere re-planificar footstep

  Push recovery:
    - Detectar empujón: aceleración lateral repentina en IMU
    - Clasificar magnitud → elegir estrategia
    - Ejecutar corrección en <100ms

  impl BalanceController {
      pub fn update(&mut self, state: &BalanceState, joints: &JointState)
          -> BalanceCorrection {
          let zmp_error = state.zmp - state.support_polygon.center();

          if abs(zmp_error) < ANKLE_THRESHOLD {
              ankle_correction(zmp_error)
          } else if abs(zmp_error) < HIP_THRESHOLD {
              hip_correction(zmp_error, joints)
          } else {
              step_correction(zmp_error, state.com_velocity)
          }
      }
  }
```

### AO3 — Foot force sensors (kernel, ~80 líneas)
```
crates/humanoid/src/foot_sensor.rs (NEW):
  Sensores de presión en cada pie — necesarios para saber:
  - ¿Qué pie está en el suelo? (stance vs swing)
  - ¿Dónde está el centro de presión? (para ZMP)
  - ¿Cuánta fuerza en cada pie? (para detectar terreno irregular)

  Hardware típico: 4 celdas de carga por pie (esquinas)
  Interface: ADC vía I2C o SPI

  pub struct FootSensor {
      force_fl: i32,  // front-left (mN)
      force_fr: i32,  // front-right
      force_bl: i32,  // back-left
      force_br: i32,  // back-right
  }

  pub fn center_of_pressure(foot: &FootSensor) -> [i32; 2] {
      // CoP_x = (force_fr + force_br - force_fl - force_bl) * foot_half_length
      //         / total_force
      // CoP_y = (force_fl + force_fr - force_bl - force_br) * foot_half_width
      //         / total_force
  }

  pub fn is_in_contact(foot: &FootSensor) -> bool {
      foot.total_force() > CONTACT_THRESHOLD
  }
```

### Resumen Fase AO

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AO1: ZMP calculator + CoM tracker | ~300 | IMU/EKF (AH1) |
| AO2: Balance controller | ~250 | AO1 |
| AO3: Foot force sensors | ~80 | I2C/ADC (ya existe) |
| **Total** | **~630** | |

---

## Fase AP — Gait Generation (cómo caminar)

**Problema**: caminar es una secuencia coordinada de 20+ joints en fases
alternas (stance/swing). No es trivial — la humanidad tardó millones de
años en evolucionar bipedalismo.

### AP1 — CPG (Central Pattern Generator) gait (kernel, ~200 líneas)
```
crates/humanoid/src/cpg.rs (NEW):
  Genera patrón rítmico de caminata usando osciladores acoplados.

  Enfoque clásico (determinístico, no necesita training):

  pub struct CPG {
      phase: f32,              // fase actual del ciclo (0-2π)
      frequency: f32,          // frecuencia de paso (Hz, típico 1-2)
      amplitude: [f32; N_JOINTS],  // amplitud de oscilación por joint
      offset: [f32; N_JOINTS],     // posición central por joint
      coupling: [[f32; N_JOINTS]; N_JOINTS],  // acoplamiento entre joints
  }

  impl CPG {
      pub fn step(&mut self, dt: f32) -> [i16; N_JOINTS] {
          self.phase += 2.0 * PI * self.frequency * dt;

          let mut angles = [0i16; N_JOINTS];
          for j in 0..N_JOINTS {
              // Cada joint oscila sinusoidalmente
              // con fase offset respecto a los demás
              let joint_phase = self.phase + self.phase_offset[j];
              let angle = self.offset[j] + self.amplitude[j] * sin(joint_phase);
              angles[j] = (angle * 100.0) as i16;  // centidegrees
          }
          angles
      }
  }

  Fases del ciclo de caminata:
    0%   - 50%:  pierna izquierda stance, pierna derecha swing
    50%  - 100%: pierna derecha stance, pierna izquierda swing

  Joints mínimos para caminar (12-DOF):
    Per leg (6): hip_yaw, hip_roll, hip_pitch, knee, ankle_pitch, ankle_roll

  Parámetros tuneables:
    step_length_mm, step_height_mm, step_frequency_hz,
    lateral_sway_mm, torso_pitch_offset_deg
```

### AP2 — Footstep planner (robot-brain, ~150 líneas)
```
planner/footstep.py (NEW):
  Decide DÓNDE poner cada pie (no solo la trayectoria del joint).

  class FootstepPlanner:
    def plan_steps(current_feet, target_position, obstacles) -> list[Footstep]:
        # Genera secuencia de footsteps desde posición actual hasta destino
        # Cada step: position(x,y), orientation(yaw), foot(L/R)
        # Evita obstáculos, respeta step_length máximo
        # Soporta: caminar recto, girar, caminar lateral, subir escalón

    class Footstep:
        x_mm: int
        y_mm: int
        z_mm: int          # para escalones
        yaw_cdeg: int
        foot: Foot          # LEFT | RIGHT
        step_type: StepType # NORMAL | TURN | LATERAL | STAIR_UP | STAIR_DOWN

  Escenarios:
    Caminar recto:  pasos alternados, misma dirección
    Girar:          pivotar sobre un pie, pasos cortos en arco
    Escaleras:      detectar escalón (sonar/VLM), ajustar step_height
    Terreno irregular: VLM identifica terreno → ajustar parámetros
```

### AP3 — RL-based gait (alternativa moderna, robot-brain, ~200 líneas)
```
policy/humanoid_rl.py (NEW):
  Alternativa a CPG: entrenar una red neuronal en MuJoCo y transferir al robot.

  Enfoque:
    1. Definir robot en MuJoCo (URDF/MJCF)
    2. Entrenar con PPO/SAC: reward = velocidad forward + penalización caída
    3. Exportar policy como ONNX o weights simples
    4. Ejecutar en robot real (sim-to-real transfer)

  Ventajas sobre CPG:
    - Aprende gaits más naturales y eficientes
    - Se adapta a terreno irregular automáticamente
    - Puede aprender get-up, correr, saltar

  Desventajas:
    - Necesita entrenamiento (horas/días de GPU)
    - Sim-to-real gap puede ser grande
    - Menos interpretable que CPG

  Referencia: Open X-Humanoid, MEVITA, rl_sar

  Implementación:
    - Entrenar en macOS (GPU o MPS)
    - Exportar policy network (~50KB de pesos)
    - Cargar en kernel como RMLP (ya tenemos model_load_bytes)
    - Ejecutar a 100 Hz: observation → policy → joint angles

  Observation vector (input):
    - IMU: roll, pitch, yaw, gyro × 3
    - Joint angles actuales: N joints
    - Joint velocidades: N joints
    - Foot contact: L, R
    - Comando: velocidad deseada (vx, vy, vyaw)

  Action vector (output):
    - Target joint angles: N joints (PD controller aplica)
```

### Resumen Fase AP

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AP1: CPG gait generator | ~200 | AO (balance) |
| AP2: Footstep planner | ~150 | AO + VLM (optional) |
| AP3: RL-based gait (alternativa) | ~200 | MuJoCo training |
| **Total** | **~550** | |

---

## Fase AQ — Inverse Kinematics + Manipulation

**Problema**: un humanoide necesita agarrar cosas, abrir puertas, manipular
objetos. Para eso necesita IK (Inverse Kinematics): dada la posición deseada
de la mano, calcular los ángulos de cada joint del brazo.

### AQ1 — IK solver (kernel o brain, ~250 líneas)
```
Analítico (rápido, exacto para cadenas conocidas):
  crates/humanoid/src/ik.rs (kernel, para RT) o
  policy/ik_solver.py (brain, para planning)

  pub struct ArmChain {
      // 7-DOF típico: shoulder(3) + elbow(1) + wrist(3)
      dh_params: [DHParam; 7],  // Denavit-Hartenberg
      joint_limits: [(i16, i16); 7],  // min/max por joint
  }

  pub fn solve_ik(
      chain: &ArmChain,
      target_pos: [i32; 3],     // posición deseada mano (mm)
      target_rot: Quaternion,    // orientación deseada mano
      current_angles: [i16; 7],  // ángulos actuales (seed)
  ) -> Option<[i16; 7]> {
      // Método: Jacobian transpose iterativo
      // O: analítico closed-form para 6-DOF arms
      // O: CycleIK (neural, más rápido para planning)

      // Iterativo:
      for _ in 0..MAX_ITERATIONS {
          let current_pos = forward_kinematics(chain, angles);
          let error = target_pos - current_pos;
          if norm(error) < TOLERANCE { return Some(angles); }
          let jacobian = compute_jacobian(chain, angles);
          let delta = jacobian_transpose(jacobian, error);
          angles += delta;
          clamp_to_limits(&mut angles, chain.joint_limits);
      }
      None  // no convergió
  }

  pub fn forward_kinematics(chain: &ArmChain, angles: &[i16]) -> [i32; 3] {
      // DH transform chain → posición del end effector
  }

  Self-collision check:
    pub fn check_self_collision(all_joints: &FullBodyState) -> bool {
        // Verificar que brazo no choca con torso, otro brazo, o piernas
        // Simplified: bounding spheres per link segment
    }
```

### AQ2 — Grasp planning (robot-brain, ~200 líneas)
```
policy/grasp.py (NEW):
  Pipeline completo de manipulación:

  class GraspPlanner:
    def plan_grasp(object_detection, depth_map) -> GraspPlan:
        # 1. Detectar objeto (VLM: "red cup on table")
        # 2. Estimar pose 6DOF del objeto (position + orientation)
        # 3. Elegir tipo de grasp:
        #    - Power grasp (objetos grandes: botellas, cajas)
        #    - Precision grasp (objetos pequeños: bolígrafos, monedas)
        #    - Hook grasp (asas, manijas de puerta)
        # 4. Calcular approach vector (desde dónde acercar la mano)
        # 5. Pre-grasp pose → reach → grasp → lift → verify

    class GraspPlan:
        pre_grasp_pose: Pose6D    # mano abierta, cerca del objeto
        grasp_pose: Pose6D        # mano en posición de agarre
        grasp_type: GraspType     # power | precision | hook
        force_target: int          # fuerza de agarre (mN)
        post_grasp: Pose6D        # levantar después de agarrar

  Verificación:
    - Después de cerrar mano → chequear force feedback
    - Si fuerza < threshold → no agarró → reintentar
    - Si fuerza > max → demasiada presión → aflojar

  Skills de manipulación:
    GRAB(object, hand)    → detect + plan + reach + grasp + lift
    PLACE(location)       → navigate + lower + release + retract
    HANDOVER(to_person)   → extend arm + wait for pull + release
    OPEN_DOOR(handle)     → reach handle + grasp + rotate + push/pull
    PUSH_BUTTON(button)   → extend finger + contact + press + retract
    POUR(container, target) → grab + tilt + pour + un-tilt + place
```

### AQ3 — Hand controller (kernel, ~100 líneas)
```
crates/humanoid/src/hand.rs (NEW):
  Control de dedos — desde simple (1-DOF gripper) hasta complejo (22-DOF Tesla).

  Niveles de complejidad:
    Nivel 1 — Gripper simple (1-DOF):
      pub fn gripper_open() / gripper_close()
      → 1 servo, open/close, suficiente para agarrar objetos simples

    Nivel 2 — Mano 5-DOF (1 servo por dedo):
      pub fn hand_set_fingers(thumb, index, middle, ring, pinky: i16)
      → power grasp, precision grasp básico

    Nivel 3 — Mano dexterous (11-22 DOF, tipo Tesla Optimus):
      pub fn hand_set_joints(joints: &[i16; N_FINGER_JOINTS])
      → tendon-driven, force feedback por dedo
      → manipulación fina (huevos, tornillos, telas)

  Force feedback:
    pub fn hand_force(finger: u8) -> i32   // mN per finger
    pub fn hand_contact(finger: u8) -> bool // contacto detectado
```

### Resumen Fase AQ

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AQ1: IK solver | ~250 | Definición de cadena (URDF/config) |
| AQ2: Grasp planning | ~200 | VLM + depth + AQ1 |
| AQ3: Hand controller | ~100 | Servo/motor driver |
| **Total** | **~550** | |

---

## Fase AR — Whole-Body Control (WBC)

**Problema**: un humanoide no mueve piernas y brazos de forma independiente.
Caminar mientras carga algo cambia el CoM. Agacharse requiere coordinar
torso + piernas + brazos. WBC coordina TODO el cuerpo como un sistema.

### AR1 — Whole-body coordinator (kernel, ~300 líneas)
```
crates/humanoid/src/wbc.rs (NEW):
  Prioridades de control (stack-of-tasks):

  pub struct WholeBodyController {
      tasks: [Task; MAX_TASKS],  // ordenados por prioridad
  }

  Prioridades (de mayor a menor):
    1. Balance (ZMP dentro de soporte)     ← NUNCA se viola
    2. Self-collision avoidance             ← NUNCA se viola
    3. Joint limits                         ← NUNCA se viola
    4. Feet contact (stance foot fijo)     ← durante caminata
    5. End-effector position (mano donde queremos)
    6. Body orientation (torso vertical)
    7. Comfort posture (posición "natural")

  Solver:
    En cada tick (~200 Hz):
    1. Computar Jacobians para todas las tasks activas
    2. Proyectar tasks de menor prioridad en el null-space de las superiores
    3. Resolver por joint velocities → integrar → joint angles
    4. Verificar limits → clamp

  Ejemplo: "agarrar vaso de la mesa mientras camina"
    - Task 1 (balance): mantiene ZMP estable
    - Task 4 (feet): sigue footstep plan
    - Task 5 (hand): mano se mueve hacia el vaso
    - Task 6 (torso): torso compensa el peso del brazo extendido
    → WBC resuelve todo simultáneamente sin conflictos

  Alternativa simple (sin null-space):
    - PD controller por joint
    - Prioridades como override: si balance requiere ankle adjustment,
      sobreescribir el target del gait generator
    - Menos elegante pero funcional para robots simples (<20 DOF)
```

### AR2 — CoM compensator (kernel, ~100 líneas)
```
crates/humanoid/src/com_compensator.rs (NEW):
  Cuando el robot carga algo o extiende un brazo, el CoM se desplaza.
  El compensator ajusta la postura del torso para mantener CoM sobre los pies.

  pub fn compensate_com(
      current_com: [i32; 3],
      support_center: [i32; 2],
      payload_mass_g: i32,
      payload_pos: [i32; 3],
  ) -> TorsoCorrection {
      // Calcular nuevo CoM incluyendo payload
      // Inclinar torso en dirección opuesta para centrar CoM
      // Limitar inclinación a ±15° (más allá es inestable)
  }

  Casos:
    - Carga en una mano → torso se inclina al lado opuesto
    - Carga sobre la cabeza → torso recto, rodillas ligeramente flexionadas
    - Empujando puerta → torso adelante, pies retrasados
```

### Resumen Fase AR

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AR1: Whole-body coordinator | ~300 | AO + AP + AQ |
| AR2: CoM compensator | ~100 | AO1 (ZMP) |
| **Total** | **~400** | |

---

## Fase AS — Fall Detection + Recovery

**Problema**: los humanoides se caen. A diferencia de drones (destruido)
o ruedas (no aplica), un humanoide puede levantarse. Necesita:
detectar caída → protegerse → levantarse.

### AS1 — Fall detector (kernel, ~80 líneas)
```
crates/humanoid/src/fall.rs (NEW):
  Detectar que el robot está cayendo ANTES de tocar el suelo.

  pub enum FallState {
      Stable,           // ZMP dentro de soporte
      Tipping,          // ZMP en el borde, recuperable
      Falling(FallDir), // irrecuperable, preparar impacto
      OnGround(Pose),   // ya en el suelo (face_down, face_up, side)
  }

  pub fn detect_fall(balance: &BalanceState, imu: &ImuData) -> FallState {
      // 1. ZMP check: si margin < 0 y velocity alta → Falling
      // 2. Tilt check: si roll o pitch > max_tilt → Falling
      // 3. Free-fall check: si accel ≈ 0 (caída libre) → Falling
      // 4. Ground check: si foot sensors = 0 y no es swing phase → Falling

      let fall_direction = if pitch > 0 { Forward }
                          else if pitch < 0 { Backward }
                          else if roll > 0 { Right }
                          else { Left };
  }

  Latencia: <10ms desde inicio de caída hasta detección.
  El balance controller tiene ~200ms para intentar recovery (stepping).
  Si no puede → switch a break-fall.
```

### AS2 — Break-fall + protective pose (kernel, ~100 líneas)
```
crates/humanoid/src/breakfall.rs (NEW):
  Cuando la caída es inevitable, minimizar daño.

  pub fn break_fall(direction: FallDir) -> [i16; N_JOINTS] {
      match direction {
          Forward => {
              // Brazos al frente, codos ligeramente flexionados
              // Cabeza girada a un lado (proteger cara)
              // Rodillas flexionadas (amortiguar)
              POSE_BREAKFALL_FORWARD
          }
          Backward => {
              // Chin to chest (proteger nuca)
              // Brazos a los lados, palmas abajo
              // Rodillas flexionadas
              POSE_BREAKFALL_BACKWARD
          }
          Left | Right => {
              // Brazo del lado de caída extendido (roll)
              // Otro brazo protege torso
              // Piernas juntas, ligeramente flexionadas
              POSE_BREAKFALL_SIDE
          }
      }
  }

  Timing:
    - Detectar caída: t=0
    - Mover a break-fall pose: t=0 a t=200ms (lo más rápido posible)
    - Impacto: t=200-500ms (depende de altura)
    - Después del impacto: evaluar daño (joint currents, IMU)
```

### AS3 — Get-up sequence (kernel + brain, ~150 líneas)
```
crates/humanoid/src/getup.rs (NEW):
  Secuencias para levantarse del suelo.

  pub fn get_up(pose: GroundPose) -> Vec<[i16; N_JOINTS]> {
      match pose {
          FaceDown => {
              // 1. Push-up con brazos
              // 2. Llevar rodillas bajo el cuerpo
              // 3. Posición cuadrúpeda
              // 4. Un pie adelante (lunge)
              // 5. Push up a standing
              GET_UP_FACE_DOWN  // secuencia de keyframes
          }
          FaceUp => {
              // 1. Girar a un lado (roll)
              // 2. Push-up lateral
              // 3. → FaceDown → secuencia anterior
              // O: sit-up → crouch → stand
              GET_UP_FACE_UP
          }
          Side => {
              // 1. Brazo inferior empuja
              // 2. Girar a FaceDown
              // 3. → secuencia FaceDown
              GET_UP_SIDE
          }
      }
  }

  Cada secuencia es una serie de keyframes interpolados.
  Se puede refinar con RL (entrenar get-up en MuJoCo).
  Post-getup: verificar balance → si estable → resume operación normal.

  Alternativa RL:
    Igual que gait (Fase AP3), entrenar policy de get-up en simulación.
    Más robusto que keyframes, se adapta a terreno irregular.
```

### Resumen Fase AS

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AS1: Fall detector | ~80 | AO (balance) + IMU |
| AS2: Break-fall + protective pose | ~100 | AS1 |
| AS3: Get-up sequence | ~150 | AS1 + AP (gait) |
| **Total** | **~330** | |

---

## Fase AT — Force/Torque Sensing + Compliance Control

**Problema**: un humanoide toca cosas y personas. Necesita saber cuánta
fuerza aplica y ser "blando" cuando interactúa. Sin esto, rompe cosas
o lastima personas.

**Referencia**: ISO 13482 (service robots), ISO 15066 (collaborative robots).

### AT1 — Joint torque sensing + monitoring (kernel, ~120 líneas)
```
crates/humanoid/src/torque.rs (NEW):
  Leer fuerza/torque en cada joint.

  Métodos:
    1. Current-based: medir corriente del motor → estimar torque
       Barato, impreciso, pero suficiente para detección de colisión.
       torque ≈ motor_current × torque_constant

    2. Strain gauge: sensor dedicado en cada joint
       Preciso, caro. Para manos y joints de contacto frecuente.

    3. Series elastic actuator (SEA): spring en el joint
       Mide deflexión del spring → torque. Usado en Atlas, Optimus.

  pub fn read_joint_torque(joint: u8) -> i32 {  // mN·m
      // Current-based (fallback):
      let current_ma = motor_current_read(joint);
      current_ma * TORQUE_CONSTANT[joint] / 1000
  }

  pub fn detect_collision(joint: u8, expected_torque: i32) -> bool {
      let actual = read_joint_torque(joint);
      let residual = (actual - expected_torque).abs();
      residual > COLLISION_THRESHOLD[joint]
  }
```

### AT2 — Impedance controller (kernel, ~150 líneas)
```
crates/humanoid/src/impedance.rs (NEW):
  En vez de control de posición rígido (ir a ángulo X exacto),
  control de impedancia: comportarse como un spring-damper.

  Si algo empuja contra el brazo del robot, el brazo CEDE
  en vez de mantener posición a toda costa.

  pub struct ImpedanceParams {
      stiffness: i32,   // K — qué tan "duro" (N/m)
      damping: i32,     // D — qué tan rápido amortigua
      inertia: i32,     // M — masa virtual
  }

  pub fn impedance_control(
      target_pos: i32,
      current_pos: i32,
      current_vel: i32,
      external_force: i32,
      params: &ImpedanceParams,
  ) -> i32 {  // torque command
      // F = M*accel + D*vel + K*(pos - target) - F_ext
      // Robot se comporta como un spring entre target y actual
      // Si external_force empuja → robot cede proporcionalmente a 1/K
      params.stiffness * (target_pos - current_pos)
      - params.damping * current_vel
      - external_force / params.inertia
  }

  Modos:
    RIGID:   K=alto, D=alto   → para tareas de precisión (tornillos)
    SOFT:    K=bajo, D=medio  → para interacción humana (entregar objeto)
    FREE:    K=0, D=bajo      → brazo se mueve libremente (teleoperation)
    LOCKED:  K=máx, D=máx     → joint bloqueado (safety)
```

### AT3 — Human proximity safety (robot-brain + kernel, ~100 líneas)
```
safety/human_proximity.py (robot-brain):
  ISO 13482 compliance: reducir velocidad y fuerza cerca de personas.

  class HumanSafetyMonitor:
    def check(person_distance_mm, robot_speed, joint_torques) -> SafetyAction:
        if person_distance < 300:    # contacto inminente
            return STOP_AND_COMPLY   # impedance mode SOFT en todos los joints
        if person_distance < 1000:   # zona cercana
            return SLOW_DOWN(max_speed=20)  # reducir a 20%
        if person_distance < 2000:   # zona de awarness
            return REDUCE_FORCE(max_torque=50)  # limitar fuerza
        return NORMAL

  Detección de persona:
    - VLM identifica personas en la imagen
    - Depth estimation da distancia
    - Alternativa: LiDAR/sonar dedicado

  Kernel side:
    - Si SafetyAction != NORMAL → limitar velocidad y torque en RT
    - Override cualquier comando que exceda los límites
    - Integrado con Safety FSM (AG4: humanoid safety)

  Límites ISO 13482 (ejemplo):
    - Fuerza máxima en contacto transitorio: 150N (pecho), 65N (cara)
    - Presión máxima: 210 N/cm² (transitorio)
    - Estos valores se configuran en config.yaml
```

### Resumen Fase AT

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AT1: Joint torque sensing | ~120 | Motor drivers |
| AT2: Impedance controller | ~150 | AT1 |
| AT3: Human proximity safety | ~100 | VLM + AT2 + AG4 |
| **Total** | **~370** | |

---

## Fase AU — Humanoid Simulation (MuJoCo)

**Problema**: probar balance, gait y manipulation sin romper hardware.
MuJoCo es el estándar para simulación de humanoides (usado por Tesla,
DeepMind, OpenAI, Berkeley). Es gratuito desde 2022 (adquirido por Google).

### AU1 — Humanoid SITL con MuJoCo (tools, ~200 líneas)
```
tools/sitl/humanoid_sim.py (NEW):
  Extiende el SITL framework (Fase AI) con simulación MuJoCo.

  Flujo:
    1. Cargar modelo MJCF/URDF del humanoide
    2. Conectar al robot-brain via protocolo TCP (como drone SITL)
    3. Loop:
       - Recibir ActuatorCmd (joint angles)
       - Aplicar al modelo MuJoCo
       - Step physics (timestep=2ms)
       - Leer sensores simulados (IMU, foot force, joint torque, camera)
       - Enviar SensorPacket al brain
    4. Render visual (opcional, para debug)

  Ventajas de MuJoCo sobre physics custom:
    - Contacto realista (soft contact, friction)
    - Tendons y actuadores modelados
    - Estable numéricamente incluso con contactos complejos
    - Usado por toda la industria → validated

  Escenarios de test humanoid:
    - Stand still → verificar balance estable
    - Walk forward 5m → verificar no cae
    - Push recovery → empujón lateral de 50N × 0.1s
    - Pick up object → reach + grasp + lift
    - Stairs → subir 3 escalones
    - Fall and get up → empujón fuerte → caer → levantarse
    - Human proximity → persona se acerca → robot reduce velocidad
```

### AU2 — RL training pipeline (tools, ~150 líneas)
```
tools/training/humanoid_rl.py (NEW):
  Pipeline para entrenar gait y manipulation con RL en MuJoCo.

  Flujo:
    1. Definir reward function (velocidad + estabilidad - energía - caídas)
    2. Entrenar con PPO (Proximal Policy Optimization) o SAC
    3. Evaluar en escenarios de test
    4. Exportar policy weights → RMLP format o ONNX
    5. Cargar en kernel (model_load_bytes, ya existe)
    6. Evaluar sim-to-real gap en HITL

  Reward function ejemplo (walking):
    reward = +1.0 * forward_velocity
           + -0.1 * energy_consumption
           + -10.0 * fall_penalty
           + +0.5 * upright_bonus
           + -0.01 * action_smoothness_penalty
           + +0.2 * foot_clearance_bonus

  Dependencies: mujoco, gymnasium, stable-baselines3 (o custom PPO)
```

### Resumen Fase AU

| Sub-fase | Líneas | Depende de |
|----------|--------|-----------|
| AU1: MuJoCo SITL | ~200 | AI (SITL framework) + MuJoCo |
| AU2: RL training pipeline | ~150 | AU1 |
| **Total** | **~350** | |

---

## Componentes existentes (ya implementados)

### server.py — Servidor TCP
```python
# Escucha conexión del robot (VF2)
# Recibe: SensorPacket, CameraFrame, Status
# Envía: VelocityCmd, ModeCmd, WaypointCmd

async def handle_robot(reader, writer):
    while True:
        pkt = await protocol.read_packet(reader)

        if pkt.type == SENSOR_PACKET:
            state.update_sensors(pkt)

        elif pkt.type == CAMERA_FRAME:
            # Enviar a VLM para descripción de escena
            description = await vision.describe(pkt.image)

            # Enviar a LLM para decisión
            action = await planner.decide(
                scene=description,
                sensors=state.sensors,
                task=state.current_task,
                odom=state.odom,
            )

            # Traducir decisión a comando motor
            cmd = policy.to_velocity_cmd(action)

            # Enviar al robot
            await protocol.send_packet(writer, cmd)
```

### perception/vision.py — Interfaz VLM (LM Studio)
```python
# Conecta al endpoint local de LM Studio
# LM Studio corre SmolVLM u otro VLM

async def describe(image: bytes) -> str:
    response = await lmstudio_client.chat(
        model="smolvlm",
        messages=[{
            "role": "user",
            "content": [
                {"type": "image", "data": base64(image)},
                {"type": "text", "text": "Describe what you see. "
                 "Focus on: obstacles, paths, people, doors, walls. "
                 "Include distances if possible."}
            ]
        }]
    )
    return response.text
```

### planner/decide.py — Interfaz LLM decisor
```python
# Usa LM Studio con un LLM (Llama 3.2, Qwen 2.5, etc.)

SYSTEM_PROMPT = """You are the brain of an autonomous robot.
You receive scene descriptions and sensor data.
You must decide the next action.

Available actions:
- FORWARD <speed 0-100>
- TURN_LEFT <degrees>
- TURN_RIGHT <degrees>
- STOP
- INVESTIGATE <direction>
- ALERT <message>

Current task: {task}
"""

async def decide(scene: str, sensors: dict, task: str, odom: dict) -> str:
    response = await lmstudio_client.chat(
        model="llama-3.2-3b",
        messages=[
            {"role": "system", "content": SYSTEM_PROMPT.format(task=task)},
            {"role": "user", "content": f"""
Scene: {scene}
IMU: accel={sensors['accel']}, gyro={sensors['gyro']}
Odom: dist={odom['dist_mm']}mm, heading={odom['heading_cdeg']/100}°
Range: front={sensors['range_front']}mm, right={sensors['range_right']}mm
Battery: {sensors['battery_mv']}mV

What is your next action?"""}
        ]
    )
    return response.text
```

### policy/actions.py — Traducción acción → comando motor
```python
import re

def to_velocity_cmd(action_text: str) -> VelocityCmd:
    action = action_text.strip().upper()

    if "STOP" in action:
        return VelocityCmd(speed_l=0, speed_r=0)

    if "FORWARD" in action:
        speed = extract_number(action, default=60)
        return VelocityCmd(speed_l=speed, speed_r=speed)

    if "TURN_RIGHT" in action:
        degrees = extract_number(action, default=45)
        # Differential drive: right turn = left fast, right slow
        intensity = min(degrees, 90) * 80 // 90
        return VelocityCmd(speed_l=intensity, speed_r=-intensity)

    if "TURN_LEFT" in action:
        degrees = extract_number(action, default=45)
        intensity = min(degrees, 90) * 80 // 90
        return VelocityCmd(speed_l=-intensity, speed_r=intensity)

    if "ALERT" in action:
        # Don't move, but flag the alert
        return VelocityCmd(speed_l=0, speed_r=0, flags=FLAG_ALERT)

    # Default: stop (safe)
    return VelocityCmd(speed_l=0, speed_r=0)
```

### config.yaml
```yaml
robot:
  listen_port: 9000           # TCP port para conexión del robot
  sensor_rate_hz: 20          # Rate esperado de sensor packets
  camera_rate_hz: 2           # Rate de camera frames
  watchdog_timeout_ms: 3000   # Si no recibe datos → alertar

lmstudio:
  host: "127.0.0.1"
  port: 1234                  # Puerto default de LM Studio
  vlm_model: "smolvlm"        # Modelo VLM para visión
  llm_model: "llama-3.2-3b"   # Modelo LLM para decisiones
  timeout_s: 10

tasks:
  default: "patrol"           # Tarea por defecto al iniciar
  patrol_waypoints:           # Puntos de patrulla
    - name: "A"
      x_mm: 0
      y_mm: 0
    - name: "B"
      x_mm: 5000
      y_mm: 0
    - name: "C"
      x_mm: 5000
      y_mm: 3000

safety:
  max_speed: 80               # Velocidad máxima (% motor)
  min_battery_mv: 6500        # Voltaje mínimo batería
  obstacle_stop_mm: 200       # Distancia mínima para parar
```

---

## ═══════════════════════════════════════════════════
## ORDEN DE EJECUCIÓN
## ═══════════════════════════════════════════════════

```
Semana 1-2: Fundamentos + Abstracción + SIMULACIÓN
├── *** AI0: SITL Wheeled (simular robot diff drive AHORA) ***
├── AI4: Test scenarios wheeled (patrol, obstacle, security, battery)
├── AI5: Visualización 2D (matplotlib, ver robot moverse)
├── P1: Net transport abstraction (VirtIO/MACB/USB-WiFi)
├── R1: Definir protocolo binario multi-robot (ActuatorCmd genérico)
├── Y1+Y3: ActuatorCmd + SensorPacket genéricos en protocol.py
├── Y4: Config per-robot-type
├── W4: Crypto (AES/SHA1) — sin dependencias, puede empezar ya
├── X1: Notificaciones (pushover/telegram) — sin dependencias, HTTP puro
└── robot-brain: scaffold repo + protocol.py ✓ HECHO
    ↑ Todo probado contra SITL wheeled. CERO hardware necesario.

Semana 3-4: Conectividad WiFi + Skills (TODO contra SITL)
├── Ruta A (USB WiFi):
│   ├── W1: USB Core enumeración
│   ├── W2: RTL8188 driver
│   └── W3: WiFi 802.11 stack + W5 integración
├── Ruta B (ESP32 bridge):
│   ├── W-alt1: ESP32 firmware (UART↔TCP bridge)
│   └── W-alt2: VF2 UART1 protocol
├── (en paralelo) Q1: libsys (syscall wrappers)
├── V1: Skill library (universal + per-type) — probado contra SITL
├── V2: Mode presets (seguridad, patrulla, explorar) — probado contra SITL
└── V3: Task planner (LLM descompone prompts libres) — probado contra SITL

Semana 5-6: Userspace + Integración brain (SITL + empezar hardware)
├── Q2: Scheduler improvements (sleep, priority)
├── S1: SYS_SENSOR_READ implementar
├── Q3: Brain client ELF
├── Q4: Reflex daemon ELF
├── V4: Skill runner (state machine + loop continuo) — probado contra SITL
├── Y2: Policy translators (wheeled.py primero, drone/humanoid stub)
├── robot-brain: server.py + policy/
├── *** Hardware chassis llega → montar + probar motores desde shell ***
└── AI3: HITL bridge (kernel real + sensores simulados para validar)

Semana 7-8: Integración end-to-end (SITL → HITL → Hardware)
├── U1: Net poll task
├── U4: Autorun userspace ELFs
├── P2: DHCP
├── X2: Telegram bot bidireccional (control remoto)
├── X3: HTTP API
├── Y5: Kernel actuator_apply() dispatcher
├── robot-brain: perception/vision.py + planner/decide.py
└── Validación: SITL → HITL → hardware real (mismos tests, 3 entornos)

Semana 9-10: Testing + Optimización + Hardware validation
├── U2: TCP buffer size
├── U3: Task priority
├── T1: CSI capture real (VF2)
├── Integrar server.py con mode manager + skill runner
├── Integration testing: SITL scenarios → QEMU → VF2 real
└── Comparar métricas SITL vs hardware (drift, latencia, battery)

Futuro cercano (cuando haya hardware de cámara):
├── T2: JPEG encoder
├── T3: Camera syscall
├── Y2: drone.py / humanoid.py policy (cuando haya hardware)
└── robot-brain: monitor/dashboard.py

Futuro — Safety + Escalabilidad (cuando la base funcione end-to-end):
│
│   REGLA: simular ANTES de hardware. Orden por tipo:
│   Drone:     AI1 (SITL drone) → AH-AK → scenarios drone → AG3 → hardware
│   Humanoid:  AI1b (SITL MuJoCo) → AO-AU → scenarios humanoid → AG4 → hardware
│   Vehículo:  AI0 adaptado → AA-AB → scenarios vehículo → AG5 → hardware
│
├── AG: Safety profiles per robot type *** ANTES de probar drone/humanoide/vehículo ***
│   └── AG1-AG8 (ver detalle en Fase AG)
├── AH: EKF State Estimation + Sensor Fusion *** CRÍTICO para drones ***
│   ├── AH1: EKF core 15 estados (kernel, corre onboard a 200+ Hz)
│   ├── AH2: Sensor calibration (gyro/accel/mag/baro)
│   └── AH3: Sensor redundancy + voting (dual IMU/GPS/baro)
├── AI: Simulación SITL/HITL
│   ├── AI1: SITL physics engine (Python, modelo de drone)
│   ├── AI2: HITL bridge (kernel real + sensores simulados)
│   └── AI3: Test scenarios library (hover, wind, motor failure, RTH)
├── AJ: 3D Path Planning + Obstacle Avoidance
│   ├── AJ1: Occupancy grid 3D
│   ├── AJ2: Path planner (A*/RRT*/VFH+)
│   └── AJ3: Depth perception (stereo/monocular/LiDAR)
├── AK: Motor Mixing + Wind Compensation
│   ├── AK1: Motor mixer (quad X/+, hex, octo, failure compensation)
│   ├── AK2: Attitude PID controller (roll/pitch/yaw/alt)
│   └── AK3: Wind estimation + feedforward
├── AL: Terrain Following + Smart RTH
│   ├── AL1: Terrain following (sonar/LiDAR down)
│   └── AL2: Smart RTH (avoid obstacles, check battery, geofence)
├── AM: SLAM + Visual Odometry
│   ├── AM1: Visual Odometry básica (indoor/GPS-denied)
│   └── AM2: Graph SLAM (futuro avanzado)
├── AN: Testing Framework + CI
│   ├── AN1: Unit test suite expandido
│   ├── AN2: SITL integration tests
│   ├── AN3: Chaos testing / fault injection
│   └── AN4: CI pipeline (GitHub Actions)
├── Z: Transport multi-link (LoRa + RF + 4G)
│   ├── Z1: Link abstraction layer
│   ├── Z2: Bandwidth-aware protocol
│   ├── Z3: LoRa driver (SX1276, SPI)
│   ├── Z4: Link failover auto-switch
│   └── Z5: Multi-UART kernel
├── AA: GPS missions + geofencing
│   ├── AA1: Mission planner (boustrophedon, spiral, grid, perimeter)
│   ├── AA2: Geofencing (inclusion/exclusion zones + buffer)
│   ├── AA3: GPS waypoint navigation
│   ├── AA4: RTK GPS (2cm precision, u-blox F9P)
│   └── AA5: Headland turns (tractores)
├── AB: Implement/payload abstraction
│   ├── AB1: Payload cmd (spray, gripper, PTO, spotlight)
│   ├── AB2: Spray control inteligente (VLM + caudal)
│   └── AB3: CAN bus driver (J1939/ISOBUS para tractores)
├── AC: Offline autonomy
│   ├── AC1: Mission preload (cargar misión completa al robot)
│   ├── AC2: Onboard decision fallback (GPS nav sin brain)
│   └── AC3: Data logging + deferred upload
├── AD: Logging, replay, analytics
│   ├── AD1: Structured event log (SQLite)
│   ├── AD2: Mission replay
│   └── AD3: Analytics dashboard
├── AE: Fleet management (multi-vehículo)
│   ├── AE1: Fleet manager (area split, relay, redistribute)
│   └── AE2: Fleet protocol
├── AF: MAVLink bridge
│   ├── AF1: MAVLink parser (v2, messages clave)
│   └── AF2: QGroundControl compatible (ground station gratis)
│
└── Humanoid-specific (cuando haya hardware humanoide):
    ├── AO: Balance + ZMP
    │   ├── AO1: ZMP calculator (kernel, RT)
    │   ├── AO2: Balance controller (kernel, PD)
    │   ├── AO3: CoM estimator + tilt recovery
    │   └── AO4: Push recovery reflexes
    ├── AP: Gait Generation
    │   ├── AP1: Gait state machine (stand/walk/run/stair)
    │   ├── AP2: Footstep planner
    │   ├── AP3: CPG oscillator + trajectory generator
    │   └── AP4: RL gait policy (MuJoCo → RMLP)
    ├── AQ: Inverse Kinematics + Manipulation
    │   ├── AQ1: IK solver (analytical 6-DOF leg)
    │   ├── AQ2: Arm IK + grasp planner
    │   └── AQ3: Collision self-check
    ├── AR: Whole-Body Control
    │   ├── AR1: Task-priority WBC
    │   └── AR2: Servo bus driver (Dynamixel/serial bus)
    ├── AS: Fall Detection + Recovery
    │   ├── AS1: Fall detector (IMU threshold + ML)
    │   ├── AS2: Impact protection (crouch pre-impact)
    │   └── AS3: Stand-up sequences (front/back/side)
    ├── AT: Force/Torque Sensing + Compliance
    │   ├── AT1: F/T sensor driver
    │   ├── AT2: Impedance controller
    │   └── AT3: Contact detection + human safety (ISO 13482)
    └── AU: Humanoid Simulation (MuJoCo)
        ├── AU1: URDF model + MuJoCo bridge
        ├── AU2: Gait training (RL in sim)
        └── AU3: Sim-to-real transfer validation
```

---

## ═══════════════════════════════════════════════════
## QUÉ HAY vs QUÉ FALTA (resumen)
## ═══════════════════════════════════════════════════

### YA EXISTE (no tocar):
| Componente | Estado |
|---|---|
| TCP/IP stack completo | Funciona sobre VirtIO (QEMU) |
| Socket syscalls (370-381) | Implementados con user-space support |
| File I/O syscalls | Implementados con copy_from/to_user |
| IPC syscalls (100-107) | Implementados |
| Motor syscalls (230-234) | Implementados |
| Sensor syscalls (330-332) | Números definidos, handlers pendientes |
| Cadence MACB Ethernet (VF2) | Driver completo con DMA rings |
| xHCI USB Host (VF2) | Init, reset, port scan, device detect — falta enumeración |
| CSI camera driver | Stubs (simulated on QEMU) |
| ELF loader + Sv39 paging | Funciona (hello.elf demostrado) |
| brk/mmap/munmap | Implementados |
| Channels (pub/sub) | Funciona (CH_MOTOR_CMD, CH_IMU, etc.) |
| Behavior engine (L0-L3) | Funciona |
| Telemetry protocol | Funciona (binary + CRC-8 + UDP) |
| Watchdog (HW + SW) | Funciona |

### FALTA (hacer):
| Componente | Fase | Prioridad | Dificultad |
|---|---|---|---|
| **SITL Wheeled (simulador)** | AI0 | **CRÍTICA (semana 1)** | **Baja** |
| **Test scenarios wheeled** | AI4 | **ALTA (semana 1)** | Baja |
| **Visualización SITL** | AI5 | ALTA (semana 1) | Baja |
| Net transport abstraction | P1 | ALTA | Baja |
| DHCP completar | P2 | Media | Media |
| Userspace syscall lib (libsys) | Q1 | ALTA | Media |
| Sleep yield-based | Q2 | ALTA | Baja |
| Task priority (RT/Normal) | Q2/U3 | Media | Baja |
| Brain client ELF | Q3 | ALTA | Media |
| Reflex daemon ELF | Q4 | Media | Baja |
| Protocolo binario brain↔robot | R1 | ALTA | Baja |
| SYS_SENSOR_READ impl | S1 | ALTA | Baja |
| CSI capture real | T1 | Media | Alta |
| JPEG encoder | T2 | Baja | Alta |
| Net poll task | U1 | ALTA | Baja |
| TCP buffer increase | U2 | Media | Baja |
| Autorun ELFs | U4 | Media | Baja |
| **USB WiFi — USB Core** | W1 | ALTA | Alta |
| **USB WiFi — RTL8188 driver** | W2 | ALTA | Alta |
| **USB WiFi — 802.11 stack** | W3 | ALTA | Muy Alta |
| **USB WiFi — Crypto (AES/WPA2)** | W4 | ALTA | Media |
| **USB WiFi — Net integration** | W5 | ALTA | Baja |
| *(alternativa)* ESP32 bridge | W-alt | ALTA | Baja |
| **Skill library (universal + per-type)** | V1 | ALTA | Baja |
| **Mode presets (seguridad, patrulla)** | V2 | ALTA | Baja |
| **Task planner (prompt libre → skills)** | V3 | ALTA | Media |
| **Skill runner (state machine + loops)** | V4 | ALTA | Media |
| **Notificaciones (pushover/telegram)** | X1 | ALTA | Baja |
| **Telegram bot bidireccional** | X2 | Media | Media |
| **HTTP API control** | X3 | Media | Baja |
| **ActuatorCmd genérico (multi-robot)** | Y1 | ALTA | Baja |
| **Policy translators per tipo** | Y2 | ALTA | Media |
| **SensorPacket genérico** | Y3 | Media | Baja |
| **Config per-robot-type** | Y4 | Media | Baja |
| **Kernel actuator_apply()** | Y5 | Media | Baja |
| **robot-brain** (Python) | — | ALTA | Media |

### FUTURO — Safety + Escalabilidad:
| Componente | Fase | Prioridad | Dificultad |
|---|---|---|---|
| **SafetyProfile trait + FSM** | AG1+AG6 | **CRÍTICA** | Media |
| **Wheeled safety (refactor)** | AG2 | **CRÍTICA** | Baja |
| **Drone safety (hover/land/RTH)** | AG3 | **CRÍTICA** (pre-drone) | Alta |
| **Humanoid safety (crouch/sit)** | AG4 | **CRÍTICA** (pre-humanoid) | Alta |
| **Vehicle safety (brake/pullover)** | AG5 | **CRÍTICA** (pre-vehicle) | Media |
| **Battery reserve per type** | AG7 | ALTA | Baja |
| **Dynamic watchdog per type** | AG8 | ALTA | Baja |
| **EKF core (15 estados, onboard)** | AH1 | **CRÍTICA** (pre-drone) | Alta |
| **Sensor calibration** | AH2 | ALTA | Media |
| **Sensor redundancy + voting** | AH3 | ALTA | Media |
| **SITL Drone** | AI1 | ALTA (pre-drone) | Media |
| **SITL Humanoid (MuJoCo)** | AI1b | ALTA (pre-humanoid) | Media |
| **Bridges (Webots/Gazebo)** | AI2 | Baja (opcional) | Baja |
| **HITL bridge** | AI3 | ALTA (pre-hardware) | Baja |
| **Occupancy grid 3D** | AJ1 | Media | Media |
| **3D path planner (A*/RRT*/VFH+)** | AJ2 | Media | Alta |
| **Depth perception** | AJ3 | Media | Media |
| **Motor mixer (quad/hex/octo)** | AK1 | **CRÍTICA** (pre-drone) | Media |
| **Attitude PID controller** | AK2 | **CRÍTICA** (pre-drone) | Media |
| **Wind estimation + feedforward** | AK3 | ALTA | Media |
| **Terrain following** | AL1 | Media | Baja |
| **Smart RTH** | AL2 | ALTA | Media |
| **Visual Odometry** | AM1 | Media | Alta |
| **Graph SLAM** | AM2 | Baja | Muy Alta |
| **Unit test suite** | AN1 | ALTA | Baja |
| **SITL integration tests** | AN2 | ALTA | Baja |
| **Chaos testing** | AN3 | Media | Baja |
| **CI pipeline** | AN4 | ALTA | Baja |
| Link abstraction (multi-transport) | Z1 | ALTA | Media |
| Bandwidth-aware protocol | Z2 | ALTA | Media |
| LoRa driver (SX1276) | Z3 | Media | Media |
| Link failover auto-switch | Z4 | Media | Media |
| Multi-UART kernel | Z5 | Baja | Baja |
| Mission planner (patterns) | AA1 | ALTA | Media |
| Geofencing (safety boundaries) | AA2 | ALTA | Media |
| GPS waypoint navigation | AA3 | ALTA | Media |
| RTK GPS (2cm precision) | AA4 | Media | Baja (hardware lo hace) |
| Headland turns (tractores) | AA5 | Baja | Baja |
| Payload abstraction | AB1 | Media | Baja |
| Spray control inteligente | AB2 | Baja | Media |
| CAN bus driver (J1939) | AB3 | Baja | Alta |
| Mission preload (offline) | AC1 | ALTA | Baja |
| Onboard decision fallback | AC2 | ALTA | Media |
| Data logging + deferred upload | AC3 | Media | Media |
| Event logger (SQLite) | AD1 | Media | Baja |
| Mission replay | AD2 | Baja | Baja |
| Analytics dashboard | AD3 | Baja | Baja |
| Fleet manager | AE1 | Baja | Media |
| Fleet protocol | AE2 | Baja | Baja |
| MAVLink parser | AF1 | Baja | Media |
| QGroundControl bridge | AF2 | Baja | Baja |

### FUTURO — Humanoid-specific:
| Componente | Fase | Prioridad | Dificultad |
|---|---|---|---|
| **ZMP calculator (kernel, RT)** | AO1 | **CRÍTICA** (pre-humanoid) | Alta |
| **Balance controller (PD)** | AO2 | **CRÍTICA** (pre-humanoid) | Alta |
| **CoM estimator + tilt recovery** | AO3 | ALTA | Media |
| **Push recovery reflexes** | AO4 | ALTA | Media |
| **Gait state machine** | AP1 | **CRÍTICA** (pre-humanoid) | Alta |
| **Footstep planner** | AP2 | ALTA | Media |
| **CPG oscillator + trajectory** | AP3 | ALTA | Alta |
| **RL gait policy (MuJoCo→RMLP)** | AP4 | Media | Muy Alta |
| **IK solver (6-DOF leg)** | AQ1 | **CRÍTICA** (pre-humanoid) | Alta |
| **Arm IK + grasp planner** | AQ2 | Media | Alta |
| **Collision self-check** | AQ3 | ALTA | Media |
| **Task-priority WBC** | AR1 | ALTA | Muy Alta |
| **Servo bus driver (Dynamixel)** | AR2 | **CRÍTICA** (pre-humanoid) | Media |
| **Fall detector (IMU + ML)** | AS1 | **CRÍTICA** (pre-humanoid) | Media |
| **Impact protection (crouch)** | AS2 | ALTA | Media |
| **Stand-up sequences** | AS3 | ALTA | Alta |
| **F/T sensor driver** | AT1 | Media | Media |
| **Impedance controller** | AT2 | ALTA | Alta |
| **Human safety (ISO 13482)** | AT3 | **CRÍTICA** (pre-humanoid) | Media |
| **URDF model + MuJoCo bridge** | AU1 | ALTA | Media |
| **Gait training RL** | AU2 | Media | Alta |
| **Sim-to-real transfer** | AU3 | Media | Alta |

### OPTIMIZACIONES RECOMENDADAS:
| Qué | Por qué | Impacto |
|---|---|---|
| Net poll task dedicado | TCP latency de ~100ms → ~1ms | Alto |
| Sleep yield-based | CPU burn 100% → yield cuando idle | Alto |
| Task priority | Motor control nunca preempted por shell | Medio |
| TCP window 4KB+ | Enviar frames sin fragmentar tanto | Medio |
| Per-process FD table | Aislamiento userspace correcto | Bajo (funcional) |
