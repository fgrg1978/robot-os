# Robot OS — Roadmap

## Vision

Robot OS bare-metal on RISC-V as reference platform for autonomous vehicles
(drones, rovers, cars) with **robot-server** architecture:

- **Robot (RISC-V)**: RT control, sensors, actuators, safety — latency <1ms
- **Server (x86/GPU)**: heavy perception, planning, VLA, SLAM — latency 50-200ms
- **Link**: WiFi/Ethernet, existing binary protocol (OBS_MAGIC/ACT_MAGIC)

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

The reason for this architecture: a RISC-V SoC (VF2/K1) has no GPU or NPU
sufficient to run YOLO or 1B+ param VLAs at 10 Hz. But it CAN control motors
at 1 kHz, fuse IMU at 1 kHz, and run lightweight MLPs for safety at 100 Hz. The
server provides the brain, the robot provides the reflexes.

---

## Current Status (Completed)

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
- [x] Feature gates (no-ml, no-mmu)
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

### ESP32-C3 build target (Phase O, aparcado)
- [x] WiFi driver stub — crates/drivers/src/wifi.rs, station mode, UDP send/recv (O1+O2)
- Build target `esp32c3` removed from tree on 2026-08-18 (never compiled,
  never in CI); see `newfeatures/esp32c3/REVISAR.md`

---

## Phase H — Channel<T> Middleware

**Goal**: replace ad-hoc IPC (SpinLock<MotorCmd>) with generic typed channels,
foundation to decouple all modules.

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
- Zero-alloc, no heap, only requires `T: Copy`
- `seq` allows detecting new data without content comparison
- `ts` allows generic watchdog

### H2 — Predefined Channels
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
- Each module reads/writes channels without knowing who is on the other side
- behavior_task reads CH_IMU + CH_PERCEPTION, writes CH_MOTOR_CMD
- rt_motor_task reads CH_MOTOR_CMD, writes PWM

### H3 — Migrate Existing Modules
- `robot::MOTOR_CMD` SpinLock → `CH_MOTOR_CMD` Channel
- `behavior_task` → reads channels instead of calling directly
- `rt_motor_task` → reads CH_MOTOR_CMD
- Maintain backward compat via re-exports

---

## Phase I — Attitude Estimation (Drone)

**Goal**: fuse IMU + barometer to estimate orientation and altitude,
minimum requirement for stable drone flight.

### I1 — Complementary Filter
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
- alpha = 0.98 (trust gyro short-term, accel corrects drift)
- Altitude: barometric formula `alt = 44330 * (1 - (P/P0)^0.1903)` in integers
- No trigonometry: use approximate atan2 (CORDIC or lookup table 256 entries)
- Publishes to CH_ATTITUDE at 500-1000 Hz

### I2 — GPS Driver (UART NMEA)
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
- NMEA parser: $GPGGA (position), $GPRMC (velocity/heading)
- Integer parsing (no float): "4807.038" = 48 deg + 07.038 min = 48_1173000 deg7
- Publishes to CH_GPS at 1-10 Hz (depends on receiver)

### I3 — Fuse IMU + GPS (Extended Complementary)
- Position hold: GPS corrects dead-reckoning drift
- Heading: magnetometer or GPS course-over-ground
- For drone: no need for full Kalman; complementary is sufficient
  for stabilized flight and position hold

---

## Phase J — Flight Controller

**Goal**: control 4+ brushless motors via ESC/PWM for stable
multirotor flight.

### J1 — ESC/PWM Output (4 Channels)
```
crates/drivers/src/esc.rs (NEW):
    esc_init(channels: &[u8])       // PWM channels for motors
    esc_arm()                        // ESC arming sequence
    esc_set_throttle(ch: u8, pct: u16)  // 0-1000 (0.0%-100.0%)
    esc_disarm()
```
- PWM at 400 Hz (standard ESC) or 32 kHz (DShot, future)
- Mapping: pct 0 = 1000us pulse, pct 1000 = 2000us pulse
- Safety: if CH_ATTITUDE.age > 50ms → immediate disarm

### J2 — Mixer (Multirotor Geometry)
```
crates/flight/ (NEW crate robot_os_flight)
  src/mixer.rs:
    pub enum FrameType { QuadX, QuadPlus, Hex, Octo }
    mixer_update(throttle: i32, roll: i32, pitch: i32, yaw: i32)
      -> [u16; MAX_MOTORS]   // throttle per motor (0-1000)
```
QuadX Table (most common):
```
  Motor 1 (front-right): +throttle -roll +pitch -yaw
  Motor 2 (rear-left):   +throttle +roll -pitch -yaw
  Motor 3 (front-left):  +throttle +roll +pitch +yaw
  Motor 4 (rear-right):  +throttle -roll -pitch +yaw
```

### J3 — Flight PID (Rate + Angle)
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
- Inner loop (rate PID): 1000 Hz, reads gyro directly
- Outer loop (angle PID): 250-500 Hz, reads estimated attitude
- Alt hold PID: 50 Hz, reads CH_BARO
- All PIDs configurable via CONFIG.INI (Phase G2 already supports)

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

## Phase K — RC Input + Safety (Drone)

**Goal**: receive remote control commands and flight failsafes.

### K1 — RC Receiver Driver
```
crates/drivers/src/rc.rs (NEW):
    // SBUS (serial, 100K baud, inverted) or PPM (timer capture)
    pub struct RcInput {
        channels: [u16; 16],  // 1000-2000us per channel
        rssi:     u8,
        failsafe: bool,
    }
    rc_init(mode: RcMode)    // SBUS or PPM
    rc_read() -> Option<RcInput>
```
- SBUS: 25 bytes, 100000 baud, 8E2 (inverted) — most common in drones
- Standard mapping: CH1=roll, CH2=pitch, CH3=throttle, CH4=yaw, CH5=mode

### K2 — Flight Modes
```
crates/flight/src/modes.rs:
    pub enum FlightMode {
        Manual,       // RC direct to mixer (rate PID only)
        Stabilize,    // RC = target angle, angle+rate PID
        AltHold,      // Stabilize + altitude PID
        PosHold,      // AltHold + GPS position PID
        Auto,         // Follow server waypoints
        RTL,          // Return To Launch (failsafe)
        Land,         // Controlled descent
    }
```
- Manual/Stabilize: no server needed — pure local flight
- PosHold/Auto: need GPS + optionally server
- RTL/Land: automatic failsafe if link is lost

### K3 — Failsafe Chain
```
Priority (higher wins):
  1. HW watchdog timeout           → disarm (motors OFF)
  2. Attitude estimation failure   → level + descend
  3. RC link loss (>1s)            → RTL or Land
  4. Server link loss (>3s)        → switch to PosHold
  5. Low battery (future)          → RTL
  6. Geofence violation (future)   → RTL
```
- Integrates with L0 (emergency-stop) from existing behavior engine
- Stack canaries + system WDT already cover kernel crash case

---

## Phase L — Server Protocol + Ground Station

**Goal**: complete robot-server protocol and ground station UI.

### L1 — Binary Protocol v2
Existing protocol (OBS_MAGIC/ACT_MAGIC/GOAL_MAGIC) is extended:
```
Robot → Server Packets:
  TELEM (10 Hz):  attitude, position, battery, mode, channels
  SENSOR (5 Hz):  IMU raw, baro, GPS, ultrasonic distances
  STATUS (1 Hz):  task health, canary status, config summary

Server → Robot Packets:
  CMD_ATTITUDE:   target roll/pitch/yaw/throttle (for Auto mode)
  CMD_WAYPOINT:   lat/lon/alt + speed (mission upload)
  CMD_MODE:       switch flight mode
  CMD_CONFIG:     update config key remotely
  CMD_ARM/DISARM: arm/disarm motors
  CMD_MODEL:      OTA model update (already exists)

Header (8 bytes): magic[4] + length[2] + type[1] + seq[1]
Checksum: CRC-8 at end (1 byte)
```

### L2 — Server Daemon (Python/Rust)
```
server/
  ground_station.py:    # or Rust binary
    - Receives telemetry via UDP
    - Shows map + attitude + state in terminal/web
    - Sends commands (waypoints, mode, arm)
    - Integrates with VLA model for autonomous planning
    - Logging to disk (replay)
```

### L3 — Telemetry Task in Kernel
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

## Phase M — Real Perception

**Goal**: perception with real sensors, robot/server split.

### M1 — Proximity Sensors (On-board, No Server)
```
crates/drivers/src/ultrasonic.rs:  // HC-SR04 or similar
    us_init(trig: u8, echo: u8)
    us_read_mm() -> Option<u32>    // 20-4000mm

crates/drivers/src/tof.rs:        // VL53L0X (I2C)
    tof_init(bus: u8, addr: u8)
    tof_read_mm() -> Option<u16>   // 0-2000mm
```
- These run on the robot at 10-50 Hz
- Feed L1 (avoid-obstacle) directly, no server

### M2 — Real Camera Driver (VF2/K1)
```
crates/drivers/src/csi.rs:
    csi_init(width: u16, height: u16, format: PixFmt)
    csi_capture(buf: &mut [u8]) -> usize
```
- VF2: MIPI CSI-2 via JH7110 ISP
- K1: MIPI CSI-2 via SpacemiT ISP
- Frame buffer: 320x240 grayscale = 75KB (fits in heap)

### M3 — Server-Side Perception
```
Flow:
  Robot captures frame → compresses (simple JPEG or raw) → UDP to server
  Server runs YOLO/segmentation → extracts obstacles
  Server sends CMD_OBSTACLES to robot
  Robot incorporates in CH_PERCEPTION → behavior engine L1/L2
```
- Robot does NOT run YOLO — only captures and transmits
- Network latency (20-50ms WiFi) acceptable for planning
- Local safety (ultrasonic + IMU) independent of server

---

## Phase N — Path Planning + SLAM

**Goal**: autonomous navigation with map and route planning.

### N1 — Occupancy Grid (Server)
```
server/slam/:
    - 2D Grid: 100x100 cells, 10cm/cell = 10m x 10m local map
    - Updated with perception data (obstacles)
    - Sent to robot as compressed bitmap (1.25 KB)
```

### N2 — Waypoint Following (Robot)
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
- Waypoints loaded from server or FAT32 (mission file)
- Pure-pursuit or L1 guidance for route following
- Runs on robot (no server needed to follow waypoints)

### N3 — Full SLAM (Server, Future)
- Visual SLAM (ORB-SLAM3 or similar) on server
- LiDAR SLAM if sensor added
- Robot sends IMU + camera frames, server maintains map
- Robot receives corrected pose via CMD_POSITION

---

## Phase O — ESP32-C3 Build Target (Micro-Drone) — PARKED

**Status (2026-08-18)**: the `esp32c3` build target (kernel running directly on
ESP32-C3, RV32IMC, 400KB RAM, for micro-drones) never compiled and was never in
CI. Removed from tree; the defconfig, linker script, boot assembly and notes on
what is needed to revive it remain in `newfeatures/esp32c3/` — see
`newfeatures/esp32c3/REVISAR.md`.

**Original goal**: support for micro-drones with ESP32-C3 (RV32IMC, 400KB RAM).

### O1 — Minimal Build (Parked)
```
cargo build --release --features esp32c3   # feature no longer exists; see newfeatures/esp32c3/
  = no-mmu + no-ml + robot_os_drivers/esp32c3
  Target: riscv32imc-unknown-none-elf
```
- Only: attitude + PID + mixer + RC + safety
- No FAT32, no TCP/IP, no ML — pure flight controller
- Config hardcoded (no INI, no disk)

### O2 — WiFi Link to Server (Parked)
- ESP32-C3 has native WiFi
- UDP telemetry to server
- Server commands via WiFi
- ESP32 has no resources for perception — all on server

---

## Summary: What Runs Where

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

## Phase Dependencies

```
Existing ─── H (channels) ─┬─ I (attitude) ─── J (flight) ─── K (RC+safety)
                            │
                            ├─ L (server protocol) ─── M (perception)
                            │                         │
                            │                         └── N (planning+SLAM)
                            │
                            └─ O (ESP32-C3, parked — see newfeatures/esp32c3/)
```

- **H is prerequisite for everything** — channels decouple modules
- **I+J+K** is the minimal flight controller (drone flies without server)
- **L** connects the server (drone flies with server)
- **M+N** are perception/planning (drone navigates autonomously)
- **O** parked (micro-drone with ESP32-C3; see `newfeatures/esp32c3/REVISAR.md`)

---

## Target Hardware for Drone

### Minimum Viable (Dev/Test)
- **FC**: SpacemiT K1 (BananaPi BPI-F3) — RV64GCV, 8 cores, RVV
- **IMU**: MPU-6050 (already supported) or ICM-42688 (better)
- **Baro**: BMP280 (already supported)
- **GPS**: u-blox NEO-M8N (UART NMEA, $15)
- **ESC**: 4x standard ESC (PWM 400 Hz)
- **RC**: FrSky SBUS receiver
- **Frame**: F450 quadcopter kit ($30)
- **Server**: laptop with WiFi (development)

### Production
- **FC**: K1 or future RISC-V with NPU
- **IMU**: ICM-42688-P (SPI, 32 kHz ODR)
- **Baro**: BMP390 (lower noise)
- **GPS**: u-blox ZED-F9P (RTK, cm precision)
- **Camera**: OV5647 or IMX219 (MIPI CSI-2)
- **Rangefinder**: TF-Luna (ToF, 12m) or VL53L1X (I2C, 4m)
- **Radio**: SiK 915MHz telemetry (long range) + WiFi (video)
- **Server**: edge box with GPU (Jetson-class or x86+GPU)

---

## Design Principles (Inherited + New)

### Inherited (Always Apply)
1. **Safety without GC**: ownership, zero panics
2. **Real determinism**: predictable latencies
3. **Close to hardware**: no unnecessary HAL
4. **Composability**: independent crates per layer

### New for Autonomous Drone
5. **Fly-first**: drone must fly without server (Stabilize mode)
6. **Degrade gracefully**: lose server → PosHold; lose GPS → Stabilize; lose IMU → disarm
7. **Split compute**: RT on robot, AI on server. The boundary is the Channel.
8. **Config-driven**: everything configurable via CONFIG.INI (PID gains, frame type, sensor buses)
9. **Test without hardware**: QEMU + simulated sensors before flying

---

## Success Metrics

| Milestone | Criterion | Phase |
|-----------|-----------|-------|
| Channels work | behavior_task migrated to channels, 0 regressions | H |
| Attitude OK | roll/pitch error < 2 deg (IMU bench test) | I |
| First hover | QuadX stable 10s in simulated QEMU | J |
| RC control | Manual + Stabilize mode with SBUS | K |
| Telemetry | Ground station sees attitude + GPS real-time | L |
| Obstacle avoid | Ultrasonic → brake at 1m (no server) | M1 |
| Auto mission | 4 waypoints in square, autonomous | N2 |
| Full auto | Server detects obstacles, replans route | M3+N |
