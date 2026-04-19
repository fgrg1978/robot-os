# Robot OS — Roadmap

## Vision

Robot OS bare-metal sobre RISC-V como plataforma de referencia para vehiculos
autonomos (drones, rovers, coches) con arquitectura **robot-servidor**:

- **Robot (RISC-V)**: control RT, sensores, actuadores, safety — latencia <1ms
- **Server (x86/GPU)**: percepcion pesada, planning, VLA, SLAM — latencia 50-200ms
- **Link**: WiFi/Ethernet, protocolo binario existente (OBS_MAGIC/ACT_MAGIC)

```
  SERVER (x86/GPU)                    ROBOT (RISC-V bare-metal)
 +--------------------+    WiFi/    +---------------------------+
 | Perception (YOLO)  | ---------> | L0: emergency-stop (IMU)  |
 | Path Planning      | <--------- | L1: avoid-obstacle (local)|
 | VLA Model          |  50-200ms  | L2: remote-vla (TCP)      |
 | SLAM / Mapping     |            | L3: explore (wander)      |
 | Ground Station UI  |            |                           |
 +--------------------+            | RT Motor Control (PID)    |
                                   | Attitude Estimation       |
                                   | Sensor Fusion (IMU+Baro)  |
                                   | ESC/PWM Output            |
                                   | Safety (WDT, PMP, canary) |
                                   +---------------------------+
```

La razon de esta arquitectura: un SoC RISC-V (VF2/K1) no tiene GPU ni NPU
suficiente para correr YOLO o VLAs de 1B+ params a 10 Hz. Pero SI puede
controlar motores a 1 kHz, fusionar IMU a 1 kHz, y ejecutar MLPs ligeros
para safety a 100 Hz. El servidor aporta cerebro, el robot aporta reflejos.

---

## Estado actual (completado)

### Kernel base (Phases 1-18)
- [x] UART, memory manager (PMM+VMM Sv39), trap handling, PLIC, CLINT
- [x] Cooperative scheduler, SMP (4 harts), task_create/yield/exit
- [x] FAT32 R/W, VFS, ramfs
- [x] TCP/IP stack (Ethernet/ARP/IPv4/ICMP/UDP/TCP/sockets)
- [x] User-space (ELF loader, syscalls, U-mode)
- [x] GPIO, PWM, I2C, SPI drivers
- [x] PID controller, motor abstraction
- [x] ML runtime (MLP f32, GGUF parser, Q4_0/Q8_0 dequant)
- [x] RVV 1.0 SIMD (context save/restore per-task)
- [x] Camera virtual + perception pipeline
- [x] MotorCmd IPC, RT motor task, deliberative task
- [x] RMLP dynamic model loading (FAT32 + OTA hot-swap)
- [x] Stack canaries + system watchdog
- [x] Persistent config (CONFIG.INI, 22 keys, zero-touch boot)

### Hardware ports (Phases A-B, 10)
- [x] QEMU virt (RV64GC, VirtIO)
- [x] StarFive VisionFive 2 (JH7110, RV64GC, no RVV)
- [x] SpacemiT K1 (RV64GCV, RVV 1.0 VLEN=256)

### Behavior + Sensors (Phases D-G2)
- [x] PMP memory protection (6 TOR regions)
- [x] Hardware watchdog (DesignWare WDT)
- [x] Scheduler Hz configurable (10-10000 Hz)
- [x] IMU driver MPU-6050 (I2C, calibration offsets)
- [x] Barometer BMP280
- [x] Feature gates (no-ml, no-mmu, esp32c3)
- [x] Subsumption behavior engine (L0-L3)
- [x] VLA remote protocol (TCP, OBS/ACT/GOAL packets)
- [x] Persistent state recovery (first-boot defaults, full subsystem apply)

### Middleware + Sensors + Flight (Phases H-L)
- [x] Channel<T> generic pub/sub middleware (H1)
- [x] MotorCmd migrated to Channel<T> — behavior_task → CH_MOTOR_CMD → rt_motor_task (H3)
- [x] AHRS complementary filter — integer-only attitude estimation (I1)
- [x] GPS driver — NMEA parser, simulated for QEMU (I2)
- [x] GPS yaw correction — course-over-ground fuses into AHRS yaw when moving (I3)
- [x] Flight controller — mixer (QuadX/+/Hex/Octo), cascaded PID, ESC driver (J)
- [x] RC input + failsafe — SBUS/PPM driver, flight modes, failsafe chain (K)
- [x] Telemetry protocol — binary robot↔server, CRC-8, UDP telemetry task (L)
- [x] Ground station — Python terminal UI, live telemetry display, command sending (L2)

### Perception + Navigation (Phases M-N)
- [x] Rangefinder sensors — ultrasonic (HC-SR04) + ToF (VL53L0X), simulated for QEMU (M1)
- [x] MIPI CSI-2 camera driver — simulated test patterns, register stubs VF2/K1 (M2)
- [x] Server-side perception — tools/perception_server.py, stub YOLO detection (M3)
- [x] Navigation stack — waypoints, mission, pure-pursuit guidance, occupancy grid (N)
- [x] Proximity channel — sensor_ahrs_task publishes ProximityData at ~20 Hz (M+N)
- [x] Full SLAM server — tools/slam_server.py, occupancy grid, pose correction, ASCII map (N3)

### ESP32-C3 Companion (Phase O)
- [x] WiFi driver stub — crates/drivers/src/wifi.rs, station mode, UDP send/recv (O1+O2)

---

## Fase H — Channel<T> Middleware

**Objetivo**: reemplazar los IPC ad-hoc (SpinLock<MotorCmd>) con channels
tipados genéricos, fundamento para desacoplar todos los módulos.

### H1 — Channel<T> core
```
crates/channel/ (NEW crate robot_os_channel)
  src/lib.rs:
    pub struct Channel<T: Copy> {
        data: SpinLock<T>,
        seq:  AtomicU64,      // monotonic sequence number
        ts:   AtomicU64,      // CLINT timestamp of last publish
    }
    Channel::new(default: T)
    Channel::publish(&self, val: T)
    Channel::read(&self) -> (T, u64, u64)   // (data, seq, timestamp)
    Channel::age_ticks(&self) -> u64
    Channel::seq(&self) -> u64
```
- Zero-alloc, no heap, solo requiere `T: Copy`
- `seq` permite detectar datos nuevos sin comparar contenido
- `ts` permite watchdog generico

### H2 — Channels predefinidos
```
crates/channels/ (NEW crate robot_os_channels)
  static CH_MOTOR_CMD:  Channel<MotorCmd>
  static CH_IMU:        Channel<ImuData>
  static CH_ODOM:       Channel<OdomPose>
  static CH_BARO:       Channel<BaroData>
  static CH_ATTITUDE:   Channel<Attitude>     // roll, pitch, yaw
  static CH_PERCEPTION: Channel<Obstacles>
  static CH_PLAN:       Channel<Trajectory>
  static CH_RC_INPUT:   Channel<RcInput>      // remote control
  static CH_GPS:        Channel<GpsPosition>
```
- Cada modulo lee/escribe channels sin saber quien esta al otro lado
- behavior_task lee CH_IMU + CH_PERCEPTION, escribe CH_MOTOR_CMD
- rt_motor_task lee CH_MOTOR_CMD, escribe PWM

### H3 — Migrar modulos existentes
- `robot::MOTOR_CMD` SpinLock → `CH_MOTOR_CMD` Channel
- `behavior_task` → lee channels en vez de llamar directamente
- `rt_motor_task` → lee CH_MOTOR_CMD
- Mantener backward compat via re-exports

---

## Fase I — Attitude Estimation (dron)

**Objetivo**: fusion IMU + barometro para estimar orientacion y altitud,
requisito minimo para que un dron vuele estable.

### I1 — Complementary filter
```
crates/ahrs/ (NEW crate robot_os_ahrs)
  src/lib.rs:
    pub struct Attitude {
        roll_cdeg:  i32,   // centi-degrees
        pitch_cdeg: i32,
        yaw_cdeg:   i32,
        alt_cm:     i32,   // centimetres (baro-based)
    }
    ahrs_update(imu: &ImuData, baro_pa: u32, dt_us: u32) -> Attitude
```
- Complementary filter: `angle = alpha * (angle + gyro*dt) + (1-alpha) * accel_angle`
- alpha = 0.98 (confiar mas en gyro a corto plazo, accel corrige drift)
- Altitud: barometric formula `alt = 44330 * (1 - (P/P0)^0.1903)` en enteros
- Sin trigonometria: usar atan2 aproximado (CORDIC o lookup table 256 entries)
- Publica en CH_ATTITUDE a 500-1000 Hz

### I2 — GPS driver (UART NMEA)
```
crates/gps/ (NEW crate robot_os_gps)
  src/lib.rs:
    pub struct GpsPosition {
        lat_deg7:  i32,    // latitude * 1e7
        lon_deg7:  i32,    // longitude * 1e7
        alt_mm:    i32,    // altitude in mm
        hdop:      u16,    // horizontal dilution * 100
        fix:       u8,     // 0=none, 1=2D, 2=3D
        sats:      u8,
    }
    gps_init(uart_bus: u8, baud: u32) -> bool
    gps_parse_nmea(buf: &[u8]) -> Option<GpsPosition>
```
- Parser NMEA: $GPGGA (posicion), $GPRMC (velocidad/rumbo)
- Integer parsing (no float): "4807.038" = 48 deg + 07.038 min = 48_1173000 deg7
- Publica en CH_GPS a 1-10 Hz (segun receptor)

### I3 — Fusion IMU + GPS (Extended Complementary)
- Position hold: GPS corrige drift de dead-reckoning
- Heading: magnetometro o GPS course-over-ground
- Para dron: no se necesita Kalman completo; complementary es suficiente
  para vuelo estabilizado y position hold

---

## Fase J — Flight Controller

**Objetivo**: controlar 4+ motores brushless via ESC/PWM para vuelo
multirotor estable.

### J1 — ESC/PWM output (4 canales)
```
crates/drivers/src/esc.rs (NEW):
    esc_init(channels: &[u8])       // PWM channels para motores
    esc_arm()                        // secuencia de armado ESC
    esc_set_throttle(ch: u8, pct: u16)  // 0-1000 (0.0%-100.0%)
    esc_disarm()
```
- PWM a 400 Hz (standard ESC) o 32 kHz (DShot, futuro)
- Mapeo: pct 0 = 1000us pulse, pct 1000 = 2000us pulse
- Safety: si CH_ATTITUDE.age > 50ms → disarm inmediato

### J2 — Mixer (geometria multirotor)
```
crates/flight/ (NEW crate robot_os_flight)
  src/mixer.rs:
    pub enum FrameType { QuadX, QuadPlus, Hex, Octo }
    mixer_update(throttle: i32, roll: i32, pitch: i32, yaw: i32)
      -> [u16; MAX_MOTORS]   // throttle per motor (0-1000)
```
Tabla QuadX (el mas comun):
```
  Motor 1 (front-right): +throttle -roll +pitch -yaw
  Motor 2 (rear-left):   +throttle +roll -pitch -yaw
  Motor 3 (front-left):  +throttle +roll +pitch +yaw
  Motor 4 (rear-right):  +throttle -roll -pitch +yaw
```

### J3 — PID de vuelo (rate + angle)
```
crates/flight/src/pid_flight.rs:
    // Cascaded PID: outer loop (angle) -> inner loop (rate)
    pub struct FlightPid {
        rate_pid:  [Pid; 3],   // roll_rate, pitch_rate, yaw_rate
        angle_pid: [Pid; 3],   // roll_angle, pitch_angle, yaw_angle
        alt_pid:   Pid,        // altitude hold
    }
    flight_pid_update(target: &FlightTarget, attitude: &Attitude,
                      gyro: &[i32;3]) -> MixerInput
```
- Inner loop (rate PID): 1000 Hz, lee gyro directamente
- Outer loop (angle PID): 250-500 Hz, lee attitude estimada
- Alt hold PID: 50 Hz, lee CH_BARO
- Todos los PID configurables via CONFIG.INI (Phase G2 ya soporta)

### J4 — Flight task (RT loop)
```
kernel/src/main.rs:
    fn flight_control_task(_: usize) {
        loop {
            let imu = CH_IMU.read();
            let att = ahrs_update(&imu, baro, dt);
            CH_ATTITUDE.publish(att);

            let target = CH_PLAN.read();  // del servidor o RC
            let mixer_in = flight_pid_update(&target, &att, &imu.gyro);
            let motors = mixer_update(mixer_in);
            for (i, &thr) in motors.iter().enumerate() {
                esc_set_throttle(i as u8, thr);
            }
            task_yield();  // ~1000 Hz con sched_hz=1000
        }
    }
```

---

## Fase K — RC Input + Safety (dron)

**Objetivo**: recibir comandos de control remoto y failsafes de vuelo.

### K1 — RC receiver driver
```
crates/drivers/src/rc.rs (NEW):
    // SBUS (serial, 100K baud, inverted) o PPM (timer capture)
    pub struct RcInput {
        channels: [u16; 16],  // 1000-2000us per channel
        rssi:     u8,
        failsafe: bool,
    }
    rc_init(mode: RcMode)    // SBUS o PPM
    rc_read() -> Option<RcInput>
```
- SBUS: 25 bytes, 100000 baud, 8E2 (invertido) — el mas comun en drones
- Mapeo standard: CH1=roll, CH2=pitch, CH3=throttle, CH4=yaw, CH5=mode

### K2 — Flight modes
```
crates/flight/src/modes.rs:
    pub enum FlightMode {
        Manual,       // RC directo a mixer (solo rate PID)
        Stabilize,    // RC = target angle, angle+rate PID
        AltHold,      // Stabilize + altitude PID
        PosHold,      // AltHold + GPS position PID
        Auto,         // Sigue waypoints del servidor
        RTL,          // Return To Launch (failsafe)
        Land,         // Descenso controlado
    }
```
- Manual/Stabilize: no necesita servidor — vuelo local puro
- PosHold/Auto: necesita GPS + opcionalmente servidor
- RTL/Land: failsafe automatico si se pierde link

### K3 — Failsafe chain
```
Prioridad (mayor gana):
  1. HW watchdog timeout           → disarm (motores OFF)
  2. Attitude estimation failure   → level + descend
  3. RC link loss (>1s)            → RTL o Land
  4. Server link loss (>3s)        → switch a PosHold
  5. Low battery (futuro)          → RTL
  6. Geofence violation (futuro)   → RTL
```
- Integra con L0 (emergency-stop) del behavior engine existente
- Stack canaries + system WDT ya cubren el caso de crash del kernel

---

## Fase L — Server Protocol + Ground Station

**Objetivo**: protocolo completo robot-servidor y UI de ground station.

### L1 — Protocolo binario v2
El protocolo existente (OBS_MAGIC/ACT_MAGIC/GOAL_MAGIC) se extiende:
```
Paquetes Robot → Server:
  TELEM (10 Hz):  attitude, position, battery, mode, channels
  SENSOR (5 Hz):  IMU raw, baro, GPS, ultrasonic distances
  STATUS (1 Hz):  task health, canary status, config summary

Paquetes Server → Robot:
  CMD_ATTITUDE:   target roll/pitch/yaw/throttle (para Auto mode)
  CMD_WAYPOINT:   lat/lon/alt + speed (mission upload)
  CMD_MODE:       switch flight mode
  CMD_CONFIG:     update config key remotely
  CMD_ARM/DISARM: arm/disarm motors
  CMD_MODEL:      OTA model update (ya existe)

Header (8 bytes): magic[4] + length[2] + type[1] + seq[1]
Checksum: CRC-8 al final (1 byte)
```

### L2 — Server daemon (Python/Rust)
```
server/
  ground_station.py:    # o Rust binary
    - Recibe telemetria via UDP
    - Muestra mapa + actitud + estado en terminal/web
    - Envia comandos (waypoints, mode, arm)
    - Integra con VLA model para planning autonomo
    - Logging a disco (replay)
```

### L3 — Telemetry task en kernel
```
kernel/src/main.rs:
    fn telemetry_task(_: usize) {
        loop {
            let att  = CH_ATTITUDE.read();
            let gps  = CH_GPS.read();
            let mode = current_flight_mode();
            // Serialize TELEM packet
            // UDP send to server IP:port (from config)
            yield_ms(100);  // 10 Hz
        }
    }
```

---

## Fase M — Percepcion Real

**Objetivo**: percepcion con sensores reales, split robot/servidor.

### M1 — Sensores de proximidad (on-board, sin servidor)
```
crates/drivers/src/ultrasonic.rs:  // HC-SR04 o similar
    us_init(trig: u8, echo: u8)
    us_read_mm() -> Option<u32>    // 20-4000mm

crates/drivers/src/tof.rs:        // VL53L0X (I2C)
    tof_init(bus: u8, addr: u8)
    tof_read_mm() -> Option<u16>   // 0-2000mm
```
- Estos corren en el robot a 10-50 Hz
- Alimentan L1 (avoid-obstacle) directamente, sin servidor

### M2 — Camera driver real (VF2/K1)
```
crates/drivers/src/csi.rs:
    csi_init(width: u16, height: u16, format: PixFmt)
    csi_capture(buf: &mut [u8]) -> usize
```
- VF2: MIPI CSI-2 via JH7110 ISP
- K1: MIPI CSI-2 via SpacemiT ISP
- Frame buffer: 320x240 grayscale = 75KB (cabe en heap)

### M3 — Server-side perception
```
Flujo:
  Robot captura frame → comprime (JPEG simple o raw) → UDP al servidor
  Servidor corre YOLO/segmentation → extrae obstaculos
  Servidor envia CMD_OBSTACLES al robot
  Robot incorpora en CH_PERCEPTION → behavior engine L1/L2
```
- El robot NO corre YOLO — solo captura y transmite
- La latencia de red (20-50ms WiFi) es aceptable para planning
- La safety local (ultrasonic + IMU) no depende del servidor

---

## Fase N — Path Planning + SLAM

**Objetivo**: navegacion autonoma con mapa y planificacion de ruta.

### N1 — Occupancy grid (servidor)
```
server/slam/:
    - Grid 2D: 100x100 celdas, 10cm/celda = 10m x 10m local map
    - Actualizado con datos de perception (obstaculos)
    - Enviado al robot como bitmap comprimido (1.25 KB)
```

### N2 — Waypoint following (robot)
```
crates/flight/src/nav.rs:
    pub struct Waypoint {
        lat_deg7: i32,
        lon_deg7: i32,
        alt_mm:   i32,
        speed_cms: u16,
        action:   WaypointAction,  // None, Hover, Land, Photo
    }
    nav_update(current: &GpsPosition, target: &Waypoint) -> FlightTarget
```
- Waypoints cargados del servidor o de FAT32 (mission file)
- Pure-pursuit o L1 guidance para seguimiento de ruta
- Corre en el robot (no necesita servidor para seguir waypoints)

### N3 — Full SLAM (servidor, futuro)
- Visual SLAM (ORB-SLAM3 o similar) en el servidor
- LiDAR SLAM si se agrega sensor
- El robot envia IMU + camera frames, el servidor mantiene el mapa
- El robot recibe pose corregida via CMD_POSITION

---

## Fase O — ESP32-C3 Companion (micro-dron)

**Objetivo**: soporte para micro-drones con ESP32-C3 (RV32IMC, 400KB RAM).

### O1 — Build minimo
```
cargo build --release --features esp32c3
  = no-mmu + no-ml + robot_os_drivers/esp32c3
  Target: riscv32imc-unknown-none-elf
```
- Feature gate ya existe (Phase F), falta driver real
- Solo: attitude + PID + mixer + RC + safety
- Sin FAT32, sin TCP/IP, sin ML — puro flight controller
- Config hardcoded (no INI, no disco)

### O2 — WiFi link a servidor
- ESP32-C3 tiene WiFi nativo
- UDP telemetria al servidor
- Comandos del servidor via WiFi
- El ESP32 no tiene recursos para percepcion — todo al servidor

---

## Resumen: que corre donde

```
                     ON-BOARD (RISC-V)        SERVER (x86/GPU)
                    ┌─────────────────┐      ┌─────────────────┐
  1000 Hz           │ Attitude (AHRS) │      │                 │
                    │ Rate PID        │      │                 │
                    │ Mixer → ESC/PWM │      │                 │
  ──────────────    ├─────────────────┤      │                 │
  100-500 Hz        │ Angle PID       │      │                 │
                    │ Alt hold PID    │      │                 │
                    │ Safety checks   │      │                 │
  ──────────────    ├─────────────────┤      ├─────────────────┤
  10-50 Hz          │ Sensor read     │      │ Perception      │
                    │ Ultrasonic      │      │ (YOLO, SLAM)    │
                    │ GPS parse       │      │                 │
                    │ Telemetry TX    │      │ Path Planning   │
  ──────────────    ├─────────────────┤      ├─────────────────┤
  1-10 Hz           │ Camera capture  │      │ VLA inference   │
                    │ → transmit raw  │      │ Map update      │
                    │ Config/OTA      │      │ Ground Station  │
                    │ Logging         │      │ Mission mgmt    │
                    └─────────────────┘      └─────────────────┘
```

---

## Dependencias entre fases

```
Existente ─── H (channels) ─┬─ I (attitude) ─── J (flight) ─── K (RC+safety)
                             │
                             ├─ L (server protocol) ─── M (percepcion)
                             │                          │
                             │                          └── N (planning+SLAM)
                             │
                             └─ O (ESP32-C3, independiente)
```

- **H es prerrequisito de todo** — los channels desacoplan modulos
- **I+J+K** son el flight controller minimo (dron vuela sin servidor)
- **L** conecta el servidor (dron vuela con servidor)
- **M+N** son percepcion/planning (dron navega autonomo)
- **O** es independiente (micro-dron con ESP32-C3)

---

## Hardware objetivo para dron

### Minimo viable (dev/test)
- **FC**: SpacemiT K1 (BananaPi BPI-F3) — RV64GCV, 8 cores, RVV
- **IMU**: MPU-6050 (ya soportado) o ICM-42688 (mejor)
- **Baro**: BMP280 (ya soportado)
- **GPS**: u-blox NEO-M8N (UART NMEA, $15)
- **ESC**: 4x ESC standard (PWM 400 Hz)
- **RC**: FrSky SBUS receiver
- **Frame**: F450 quadcopter kit ($30)
- **Server**: laptop con WiFi (desarrollo)

### Produccion
- **FC**: K1 o futuro RISC-V con NPU
- **IMU**: ICM-42688-P (SPI, 32 kHz ODR)
- **Baro**: BMP390 (menor ruido)
- **GPS**: u-blox ZED-F9P (RTK, cm precision)
- **Camera**: OV5647 o IMX219 (MIPI CSI-2)
- **Rangefinder**: TF-Luna (ToF, 12m) o VL53L1X (I2C, 4m)
- **Radio**: SiK 915MHz telemetry (largo alcance) + WiFi (video)
- **Server**: edge box con GPU (Jetson-class o x86+GPU)

---

## Principios de diseno (heredados + nuevos)

### Heredados (aplican siempre)
1. **Seguridad sin GC**: ownership, zero panics
2. **Determinismo real**: latencias predecibles
3. **Hardware cercano**: sin HAL innecesario
4. **Composabilidad**: crates independientes por capas

### Nuevos para dron autonomo
5. **Fly-first**: el dron debe poder volar sin servidor (Stabilize mode)
6. **Degrade gracefully**: si pierde servidor → PosHold; si pierde GPS → Stabilize; si pierde IMU → disarm
7. **Split compute**: RT en robot, IA en servidor. La frontera es el Channel.
8. **Config-driven**: todo parametrizable via CONFIG.INI (PID gains, frame type, sensor buses)
9. **Test without hardware**: QEMU + simulated sensors antes de volar

---

## Metricas de exito

| Milestone | Criterio | Fase |
|-----------|----------|------|
| Channels work | behavior_task migrado a channels, 0 regressions | H |
| Attitude OK | roll/pitch error < 2 deg (IMU bench test) | I |
| First hover | QuadX estable 10s en QEMU simulado | J |
| RC control | Manual + Stabilize mode con SBUS | K |
| Telemetry | Ground station ve actitud + GPS en tiempo real | L |
| Obstacle avoid | Ultrasonic → frenado a 1m (sin servidor) | M1 |
| Auto mission | 4 waypoints en cuadrado, autonomo | N2 |
| Full auto | Servidor detecta obstaculos, replannea ruta | M3+N |
