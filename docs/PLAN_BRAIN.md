# Plan: Autonomous Robot with Remote Brain (macOS + LM Studio)

## Final Architecture (multi-robot)

The system supports multiple robot types (wheeled, drone, humanoid) with the
same intelligence layer. Only the policy and hardware layers change.

```
                     UNIVERSAL (unchanged by robot type)
┌─────────────────────────────────────────────────────────────────┐
│  macOS (LM Studio + robot-brain)                                │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ VLM: Qwen2.5-VL-7B (understand images)                   │  │
│  │ LLM: Qwen3-30B-A3B (decide actions)                       │  │
│  │ Task Planner: free prompt → sequence of skills            │  │
│  │ Skill Runner: state machine + loops + detect triggers     │  │
│  │ Modes: security, patrol, explore (presets)                │  │
│  │ Notifications: pushover, telegram, email, webhook         │  │
│  │ Ground Station: telemetry + debug                         │  │
│  └──────────────────────┬────────────────────────────────────┘  │
│                         │                                       │
│              ROBOT-SPECIFIC (changes by type)                   │
│  ┌──────────────────────▼────────────────────────────────────┐  │
│  │ Policy Translator:                                        │  │
│  │   wheeled.py  → diff drive (speed_l, speed_r)             │  │
│  │   drone.py    → attitude (throttle, roll, pitch, yaw)     │  │
│  │   humanoid.py → gait + joint angles (IK solver)           │  │
│  └──────────────────────┬────────────────────────────────────┘  │
│                         │ TCP (WiFi mesh)                       │
└─────────────────────────┼───────────────────────────────────────┘
                          │ ActuatorCmd (generic: type + N channels)
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
│  │  Actuators (motors/ESC/servos), IMU, Sensors, Safety       │  │
│  │  Ethernet (Cadence MACB) / WiFi, TCP/IP stack              │  │
│  │  Channels, Watchdog, PMP                                   │  │
│  └────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

---

## Repos

### Repo 1: `riscv_robot_os_rust` (existing — modified)
Bare-metal kernel + userspace binaries.

### Repo 2: `robot-brain` (new — Python, runs on macOS)
AI server that connects with LM Studio and the robot.

---

## DETAILED PLAN

### ═══════════════════════════════════════════════════
### REPO 1: riscv_robot_os_rust — Required Changes
### ═══════════════════════════════════════════════════

---

## Phase P — Net stack over real Ethernet (VF2)

**Problem**: the net stack (crates/net) only works with VirtIO (QEMU).
On real VF2, the transport is Cadence MACB Ethernet (crates/drivers/src/eth.rs).
The net stack needs to abstract the transport.

### P1 — Network transport abstraction
```
crates/net/src/transport.rs (NEW):
    pub trait NetTransport {
        fn send(frame: &[u8]) -> Result<(), ()>;
        fn poll_recv(buf: &mut [u8]) -> usize;
        fn get_mac() -> [u8; 6];
        fn is_ready() -> bool;
    }
```

Changes:
- `crates/net/src/lib.rs`: net_init() detects platform:
  - QEMU → VirtIO net (as now)
  - VF2 → Cadence MACB (eth.rs)
  - Both expose the same send/poll_recv interface
- `net_poll()` calls the correct transport based on feature gate
- Rest of stack (ARP, IP, TCP, socket) unchanged

### P2 — Functional DHCP
```
crates/net/src/dhcp.rs (already exists, complete it):
    dhcp_discover() → get IP from WiFi mesh router
```
- VF2 connected by Ethernet to WiFi mesh (cable or bridge)
- DHCP needed to get dynamic IP on mesh network
- Alternative: static IP configured in CONFIG.INI

**Files to modify**:
- `crates/net/src/lib.rs` — transport routing
- `crates/net/src/dhcp.rs` — complete DHCP
- `crates/drivers/src/eth.rs` — already functional, just integrate
- `kernel/src/main.rs` — init eth on VF2 before net_init

**Dependencies**: none (can start now)
**Estimate**: ~200 new lines

---

## Phase Q — Userspace runtime (libsys)

**Problem**: current userspace only has a test hello.S.
A brain client needs: syscall wrappers, dynamic memory, string formatting,
and a continuous execution loop.

### Q1 — Syscall wrappers in Rust (userspace library)
```
userspace/libsys/ (NEW — static library for userspace ELFs)
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

### Q2 — Scheduler improvements for daemons
```
crates/sched/src/scheduler.rs:
  - sys_sleep() changed from busy-wait to yield-based:
    task.wake_at = clint::get_time() + ms * 10_000;
    task.state = TaskState::Sleeping;
    (scheduler skips tasks with wake_at > now)
  - Per-process FD table (move from global KERNEL_FD_TABLE to Task struct)
  - Task priority (2 levels: RT=0, Normal=1) — RT always first
```

### Q3 — Userspace binary: brain client
```
userspace/brain/ (NEW — ELF running in U-mode)
  src/main.rs:
    #![no_std]
    #![no_main]
    use libsys::*;

    fn main() -> ! {
        // 1. Connect to macOS server via TCP
        let fd = tcp_connect(SERVER_IP, SERVER_PORT)?;

        loop {
            // 2. Read sensors via syscall
            let imu = sys_sensor_read(SENSOR_IMU, &mut buf);
            let odom = sys_sensor_read(SENSOR_ODOM, &mut buf);
            let enc = sys_sensor_read(SENSOR_ENCODER, &mut buf);

            // 3. Pack SensorPacket
            let pkt = SensorPacket { imu, odom, enc, timestamp };

            // 4. Send to macOS
            tcp_send_all(fd, &pkt.to_bytes());

            // 5. Receive BrainCmd
            if let Ok(n) = tcp_recv_all(fd, &mut cmd_buf) {
                if let Some(cmd) = BrainCmd::from_bytes(&cmd_buf[..n]) {
                    // 6. Apply motor command via syscall
                    sys_motor_speed(0, cmd.speed_l);
                    sys_motor_speed(1, cmd.speed_r);
                }
            }

            // 7. Yield (don't burn CPU)
            sys_yield();
        }
    }
```

### Q4 — Userspace binary: reflex daemon (local obstacle avoidance)
```
userspace/reflex/ (NEW — ELF running in U-mode)
  src/main.rs:
    fn main() -> ! {
        loop {
            let range = sys_sensor_read(SENSOR_RANGEFINDER, &mut buf);
            if range_mm < SAFETY_THRESHOLD_MM {
                // Override: stop motors immediately
                sys_motor_speed(0, 0);
                sys_motor_speed(1, 0);
                // Notify brain (via IPC or flag)
            }
            sys_yield();
        }
    }
```

**Files to create**:
- `userspace/libsys/` (new crate, not workspace member — target userspace)
- `userspace/brain/` (new ELF binary)
- `userspace/reflex/` (new ELF binary)

**Files to modify**:
- `crates/sched/src/scheduler.rs` — sleep non-busy, priority
- `crates/sched/src/task.rs` — +wake_at, +priority, +fd_table
- `crates/syscall/src/handlers.rs` — sys_sleep yield-based, per-process FD

**Dependencies**: Phase P (for TCP connect from userspace)
**Estimate**: ~600 new lines

---

## Phase R — Binary protocol brain↔robot (multi-robot)

**Problem**: define the exact message format between VF2 and macOS.
The protocol must support different robot types (wheeled, drone, humanoid)
without changing the header or transport layer.

### R1 — Shared protocol
```
Packet format (simple, no overhead):

  ┌──────┬──────┬──────────┬─────────┐
  │ MAGIC│ TYPE │ LEN (u16)│ PAYLOAD │
  │ 2B   │ 1B   │ 2B       │ 0-1400B │
  └──────┴──────┴──────────┴─────────┘
  + CRC-8 at end (1 byte)

  MAGIC = 0xBR (0x42, 0x52)

  Types Robot → Server (0x01-0x7F):
    0x01 SENSOR_PACKET:
      Common header (always present, 38 bytes):
        timestamp_ms:  u64
        battery_mv:    u16
        accel_mg:      [i32; 3]   (IMU always present)
        gyro_mdps:     [i32; 3]

      Payload by robot_type (varies):
        Wheeled (type=0, +22 bytes = 60 total):
          odom_dist_mm:    i32
          odom_hdg_cdeg:   i32
          encoder_l:       i64
          range_front_mm:  u16
          range_right_mm:  u16
          (note: encoder_r inferred from odom + encoder_l if needed)

        Drone (type=1, +26 bytes = 64 total):
          baro_pa:         i32     (barometric pressure)
          mag_ut:          [i16;3] (magnetometer)
          gps_lat_deg7:    i32     (lat × 10^7)
          gps_lon_deg7:    i32     (lon × 10^7)
          gps_alt_cm:      i32     (altitude in cm)
          sonar_down_mm:   u16     (distance to ground)

        Humanoid (type=2, +variable):
          num_joints:      u8
          joint_angles:    [i16; num_joints]  (centidegrees)
          foot_pressure_l: u16
          foot_pressure_r: u16

    0x02 CAMERA_FRAME:
      width:  u16
      height: u16
      format: u8   (0=grayscale, 1=jpeg)
      data:   [u8; width*height]  (or compressed JPEG)

    0x03 STATUS:
      robot_type: u8  (0=wheeled, 1=drone, 2=humanoid)
      mode:       u8
      tasks_ok:   u8
      canary_ok:  u8
      uptime_s:   u32

  Types Server → Robot (0x80-0xFF):
    0x80 ACTUATOR_CMD (replaces VELOCITY_CMD — generic):
      actuator_type: u8   (0=diff_drive, 1=quad_rotor, 2=humanoid, 3=ackermann)
      num_channels:  u8   (2 for wheels, 4 for drone, N for humanoid)
      flags:         u8   (bit 0: emergency_stop, bit 1: alert)
      channels:      [i16; num_channels]  (values per channel, LE)

      Examples:
        Wheels:    type=0, n=2, ch=[60, 60]                    = 7 bytes
        Drone:     type=1, n=4, ch=[1400, 1400, 1400, 1400]    = 11 bytes
        Humanoid:  type=2, n=20, ch=[...joint angles]           = 43 bytes

    0x81 MODE_CMD:
      mode: u8  (0=idle, 1=patrol, 2=navigate, 3=manual, 4=security)

    0x82 WAYPOINT_CMD:
      lat_deg7:  i32
      lon_deg7:  i32
      alt_cm:    i32  (0 for ground robots)
      speed_cms: u16

    0x83 CONFIG_CMD:
      key: [u8; 24]
      val: [u8; 16]
```

Compatibility:
- `ACTUATOR_CMD` with type=0, n=2 is functionally identical to `VELOCITY_CMD`
- Brain client on VF2 sends `robot_type` in STATUS so server knows which policy to use
- Server loads the correct policy translator on receiving the first STATUS

This protocol is implemented in:
- `userspace/libsys/src/protocol.rs` — robot side (Rust, no_std)
- `robot-brain/protocol.py` — macOS side (Python)

**Dependencies**: none (is format definition)
**Estimate**: ~180 lines per side

---

## Phase S — Sensor read support from userspace

**Problem**: the brain client needs to read IMU, odom, encoders, rangefinder
from userspace. The SYS_SENSOR_READ (330) syscalls exist but are not yet
implemented.

### S1 — Implement SYS_SENSOR_READ
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

### S2 — Channel read from userspace
```
More elegant alternative:
  SYS_IPC_SEND/RECV already exist (100-107)
  Brain client can read channels published by kernel tasks:
    - CH_IMU, CH_ATTITUDE, CH_GPS, CH_ODOM
  Requires: expose channels as IPC endpoints readable from U-mode
```

**Files to modify**:
- `crates/syscall/src/handlers.rs` — implement sys_sensor_read
- Possibly `crates/channel/src/lib.rs` — read from userspace

**Dependencies**: none
**Estimate**: ~100 lines

---

## Phase T — Camera streaming

**Problem**: the brain client needs to capture camera frames and send them
to the macOS server for VLM processing.

### T1 — Real CSI capture (VF2)
```
crates/drivers/src/csi.rs:
  - Already has stubs for JH7110 ISP
  - Implement: real csi_capture() for VF2
  - Frame buffer: 320x240 grayscale = 75 KB
  - Capture via ISP DMA → buffer in memory → syscall read
```

### T2 — Lightweight JPEG compression (optional)
```
crates/camera/src/jpeg.rs (NEW):
  - Minimal JPEG baseline encoder (grayscale only)
  - 320x240 raw (75KB) → JPEG (~10-15KB)
  - Reduces WiFi bandwidth from 75KB×10Hz=750KB/s to ~150KB/s
  - Alternative: send raw if mesh bandwidth allows
```

### T3 — Syscall for camera read
```
  SYS_SENSOR_READ(sensor_id=4, buf, len):
    - Trigger capture
    - Copy frame to user buffer
    - Return frame size
```

**Dependencies**: Phase S (sensor read syscall)
**Estimate**: ~300 lines (without JPEG), ~500 with JPEG

---

## Phase U — Kernel optimizations for this use case

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
- Currently net_poll() is called from timer interrupt and shell
- A dedicated task at ~1000 Hz significantly improves TCP latency
- Necessary for brain client to have responsive TCP

### U2 — Increased TCP buffer
```
crates/net/src/tcp.rs:
  - Increase TCP window from 1460 to 4096+ bytes
  - Allow multiple segments in flight
  - Necessary for sending camera frames (10-75 KB)
```

### U3 — Task priority (RT vs Normal)
```
crates/sched/src/scheduler.rs:
  - 2 levels: RT (0) and Normal (1)
  - RT tasks: motor control, sensor read, net poll
  - Normal tasks: brain client, shell, telemetry
  - RT always executes before Normal
  - Simple: two lists, schedule RT first
```

### U4 — Userspace ELF auto-launch
```
kernel/src/main.rs:
  - On boot, after mounting FAT32:
    - Search for /fat/BRAIN.ELF, /fat/REFLEX.ELF
    - If exist, load and execute in U-mode automatically
    - Configurable via CONFIG.INI: autorun=brain,reflex
```

**Dependencies**: Phase Q (scheduler changes)
**Estimate**: ~200 lines

---

## Phase W — USB WiFi (native wireless on VF2)

**Problem**: VF2 has no built-in WiFi and the entire environment is WiFi mesh.
Ethernet cable not possible. WiFi via USB dongle needed.

**Current state**: `crates/drivers/src/usb.rs` already has:
- Complete xHCI init (halt, reset, wait CNR, port scan)
- HCSPARAMS1 reading (MaxSlots, MaxPorts)
- Port status/control (CCS, PED)
- Static device table (8 devices)
- `usb_init()`, `usb_scan()`, `usb_info()` functional on VF2

**Missing**: everything from "detected a USB device" to "I have WiFi".

### W1 — USB Core: device enumeration (~800 lines)
```
crates/drivers/src/usb_core.rs (NEW):
  Current state: xHCI detects ports with connected devices (CCS bit)
  Missing:

  1. Device Context Base Address Array (DCBAAP)
     - Allocate array of 64-bit pointers (MaxSlots + 1)
     - Write to DCBAAP register (already defined, offset 0x30)

  2. Command Ring
     - TRB ring buffer (Transfer Request Blocks) — 16 bytes each
     - Write base to CRCR register (already defined, offset 0x38)
     - Types: Enable Slot, Address Device, Configure Endpoint

  3. Transfer Rings (per-endpoint)
     - Ring buffer for Control/Bulk/Interrupt transfers
     - Each endpoint has its own ring

  4. Event Ring
     - Ring where xHCI notifies completions
     - Poll-based (no IRQ, like rest of kernel)

  5. USB Enumeration:
     usb_enumerate(port) → UsbDevice {vid, pid, class, subclass}
       a. Enable Slot Command → get slot_id
       b. Address Device Command → assign USB address
       c. GET_DESCRIPTOR (Device) → read vid, pid, class
       d. GET_DESCRIPTOR (Config) → read interfaces/endpoints
       e. SET_CONFIGURATION → activate device

  Transfer types needed:
     - Control Transfer: setup packets (GET_DESCRIPTOR, SET_CONFIG, etc.)
     - Bulk Transfer: WiFi data (TX/RX frames)
```

### W2 — USB WiFi class driver: RTL8188EU (~1500 lines)
```
crates/drivers/src/usb_wifi.rs (NEW):
  Target: Realtek RTL8188EU — the simplest and most documented USB WiFi chip.
  Common dongles: TP-Link TL-WN725N ($5), many generic ones.
  VID:PID = 0x0BDA:0x8179 (and variants)

  RTL8188EU is most viable because:
  - Firmware NOT required (fullmac, all logic in hardware)   ← INCORRECT for most
  - Well-documented USB protocol (Linux driver: rtl8xxxu)
  - Bare-metal reference implementations exist

  CORRECTION: RTL8188EU DOES require firmware upload.
  Simpler alternative: RTL8188CUS (firmware in ROM).

  Driver flow:
  1. Detect device (vid=0x0BDA, pid in known list)
  2. Upload firmware (if needed) via bulk OUT
  3. Configure MAC registers via vendor-specific control transfers
  4. Configure BB (baseband) and RF registers
  5. Enable RX/TX queues (bulk endpoints)

  Public API:
    usb_wifi_init() -> bool           // detect and configure dongle
    usb_wifi_scan() -> ScanResults    // scan available APs
    usb_wifi_connect(ssid, pass)      // associate + authenticate
    usb_wifi_send(frame: &[u8])       // send 802.11 frame
    usb_wifi_recv(buf: &mut [u8])     // receive 802.11 frame
    usb_wifi_is_connected() -> bool
```

### W3 — WiFi 802.11 stack (~2000 lines)
```
crates/drivers/src/wifi_stack.rs (NEW):
  WiFi management layer (management frames):

  1. Scan:
     - Send Probe Request on each channel (1-13)
     - Parse Probe Response / Beacon frames
     - Extract: SSID, BSSID, channel, RSSI, security type

  2. Associate:
     - Authentication frame (Open System: 2 frames)
     - Association Request → Response
     - Parse AID (Association ID)

  3. WPA2-PSK (CCMP):
     a. Derive PMK: PBKDF2-SHA1(password, ssid, 4096, 32)
     b. 4-Way Handshake (EAPOL):
        - Msg 1: AP → STA (ANonce)
        - Msg 2: STA → AP (SNonce + MIC)
        - Msg 3: AP → STA (GTK + MIC)
        - Msg 4: STA → AP (ACK)
     c. Derive PTK = PRF-384(PMK, ANonce, SNonce, MAC_AP, MAC_STA)
     d. Install TK (temporal key) for CCMP

  4. CCMP (AES-128-CCM):
     - Encrypt/decrypt data frames
     - Needs AES-128 in software (~200 lines)

  5. 802.11 ↔ Ethernet conversion:
     - RX: 802.11 frame → strip headers → Ethernet frame → net stack
     - TX: Ethernet frame → add 802.11 headers → USB bulk OUT
```

### W4 — Minimal crypto for WPA2 (~500 lines)
```
crates/drivers/src/crypto.rs (NEW):
  - AES-128 encrypt/decrypt (S-box tables, ~150 lines)
  - AES-CCM mode (CCMP uses CCM, ~100 lines)
  - SHA-1 + HMAC-SHA1 (~150 lines)
  - PBKDF2-SHA1 (~50 lines)
  - PRF-384 for PTK derivation (~50 lines)

  All no_std, zero alloc, constant-time where possible.
  Doesn't need to be cryptographically perfect for dev/test,
  but functional with real WPA2-PSK APs.
```

### W5 — Net stack integration (~100 lines)
```
crates/net/src/lib.rs:
  net_init() detects:
    - QEMU → VirtIO net
    - VF2 + USB WiFi → usb_wifi as transport
    - VF2 + Ethernet → Cadence MACB

  The net stack (ARP, IP, TCP, socket) unchanged.
  Only frame transport changes:
    VirtIO send/recv  →  usb_wifi_send/recv
```

### Phase W effort summary

| Sub-phase | Lines | Dependency |
|----------|-------|-----------|
| W1: USB Core (enumeration) | ~800 | existing usb.rs |
| W2: RTL8188 driver | ~1500 | W1 |
| W3: WiFi 802.11 stack | ~2000 | W2 |
| W4: Crypto (AES/SHA1/WPA2) | ~500 | None |
| W5: Net stack integration | ~100 | W3 + Phase P1 |
| **Total** | **~5000** | |

### Quick alternative: ESP32 bridge (Phase W-alt)

If W proves too long, the alternative is an ESP32-C3 ($3) as bridge:

```
VF2 ──UART1 (3 cables)──→ ESP32-C3 ──WiFi──→ mesh ──→ macOS
```

| Sub-phase | Lines | Where |
|----------|-------|-------|
| W-alt1: ESP32 firmware (UART↔TCP bridge) | ~300 | ESP-IDF/Arduino |
| W-alt2: VF2 UART1 ↔ brain protocol | ~200 | kernel + userspace |
| **Total alternative** | **~500** | |

The plan supports both routes. Decision depends on whether you prefer:
- **USB WiFi (W)**: integrated solution, single board, more complex
- **ESP32 bridge (W-alt)**: fast, cheap, proven, requires extra hardware

---

### ═══════════════════════════════════════════════════
### REPO 2: robot-brain — New repository (macOS)
### ═══════════════════════════════════════════════════

## Structure

```
robot-brain/
├── requirements.txt
├── config.yaml              ← configuration (IPs, models, modes, notifications)
├── server.py                ← main TCP server
├── protocol.py              ← binary protocol parser/builder
├── perception/
│   ├── __init__.py
│   └── vision.py            ← LM Studio interface (VLM)
├── planner/
│   ├── __init__.py
│   ├── decide.py            ← LM Studio interface (tactical LLM decider)
│   ├── skills.py            ← robot primitive skills definition
│   ├── modes.py             ← presets (security, patrol, explore, custom)
│   └── task_planner.py      ← LLM decomposes free prompt → skill sequence
├── executor/
│   ├── __init__.py
│   └── skill_runner.py      ← state machine: executes skills in sequence/loop
├── policy/
│   ├── __init__.py          ← loads translator per robot.type
│   ├── actions.py           ← parse textual action (common)
│   ├── wheeled.py           ← skill → ActuatorCmd diff drive (2 ch)
│   ├── drone.py             ← skill → ActuatorCmd quad rotor (4 ch)
│   └── humanoid.py          ← skill → ActuatorCmd joint angles (N ch)
├── notifications.py         ← pushover, telegram, email, webhook
├── api.py                   ← HTTP API for remote control (start/stop modes)
├── monitor/
│   ├── __init__.py
│   └── dashboard.py         ← terminal UI (live telemetry)
└── tests/
    ├── test_protocol.py
    └── test_policy.py
```

---

## Phase V — Modes, Skills and Task Planner (high-level callers)

**Problem**: user wants to give high-level instructions like "activate security"
or "scan the house and detect intruders" and have the robot execute them autonomously,
including continuous loops (all night) and notifications on detecting events.

### V1 — Skill Library (planner/skills.py, ~100 lines)
```
Universal skills (all robots):

UNIVERSAL_SKILLS = {
    "STOP":         "Stop all actuators immediately",
    "WAIT":         "Wait N seconds (actuators off, save battery)",
    "SCAN_360":     "Rotate/pan 360° scanning with VLM in steps",
    "INVESTIGATE":  "Approach detected object slowly (20% speed)",
    "ALERT":        "Stop, send notification with description + image",
    "TRACK":        "Follow detected object maintaining safe distance",
}

Skills by robot type:

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

Loaded per config robot.type:
  skills = UNIVERSAL_SKILLS | TYPE_SKILLS[config.robot.type]

Each skill has:
  - name, description (so LLM knows them)
  - parameters: {name, type, default}
  - estimated_duration_s (for planning)
  - requires_vlm: bool (SCAN_360 yes, WAIT no)

The task_planner includes ONLY skills from active type in its system prompt.
This way LLM never generates "TAKEOFF" for a wheeled robot.
```

### V2 — Mode Presets (planner/modes.py, ~80 lines)
```
Predefined modes that do NOT require LLM to plan:

MODES = {
    "security": {
        "description": "Continuous surveillance. Scan and alert if persons detected.",
        "plan": [SCAN_360, WAIT(30)],   # repeats in loop
        "loop": true,
        "detect": ["person", "open_door", "fire", "movement"],
        "on_detect": ["notify", "alert"],
        "schedule": "always",           # or "22:00-06:00"
    },
    "patrol": {
        "description": "Traverse waypoints in loop scanning at each.",
        "plan": [
            NAVIGATE_TO("A"), SCAN_360,
            NAVIGATE_TO("B"), SCAN_360,
            NAVIGATE_TO("C"), SCAN_360,
        ],
        "loop": true,
        "detect": ["person", "obstacle"],
        "on_detect": ["notify"],
    },
    "explore": {
        "description": "Free exploration. LLM decides each step.",
        "plan": "llm",  # uses task_planner to generate dynamic plan
        "loop": false,
    },
    "return_home": {
        "description": "Return to starting point.",
        "plan": [NAVIGATE_TO("home")],
        "loop": false,
    },
}

Usage:
  python brain.py --mode security
  HTTP: POST /api/mode {"mode": "security"}
  Telegram: /security

Custom mode can be created via free prompt:
  "scan the house and detect intruders"
  → task_planner decomposes → dynamic plan
```

### V3 — Task Planner (planner/task_planner.py, ~60 lines)
```
For free prompts that do NOT fit in a preset.

System prompt to LLM:
  "You are a robot task planner. The robot has these skills:
   {SKILLS with descriptions}

   The robot knows these locations: {locations from config}

   Decompose the user's request into a sequence of skills.
   Output ONLY a JSON array:
   [
     {"skill": "NAVIGATE_TO", "args": {"location": "kitchen"}},
     {"skill": "SCAN_360", "args": {}},
     ...
   ]

   If the task should repeat, add {"skill": "LOOP", "args": {}}"

Example:
  Input:  "scan the house and detect intruders"
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

Example 2:
  Input:  "go to the kitchen and watch for 1 hour"
  Output: [
    {"skill": "NAVIGATE_TO", "args": {"location": "kitchen"}},
    {"skill": "SCAN_360", "args": {}},
    {"skill": "WAIT", "args": {"seconds": 60}},
    {"skill": "SCAN_360", "args": {}},
    {"skill": "WAIT", "args": {"seconds": 60}},
    ... (x30 to cover 1 hour)
  ]
```

### V4 — Skill Runner (executor/skill_runner.py, ~150 lines)
```
State machine that executes a plan (skill sequence):

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

                    # Check triggers
                    for trigger in detect_triggers:
                        if trigger in scene.lower():
                            await notifier.alert(trigger, scene, frame)
                            # LLM decides: investigate or continue
                            action = llm.decide(scene, sensors,
                                f"Detected {trigger}. Investigate or continue?")
                            if "INVESTIGATE" in action:
                                send(FORWARD 20)  # approach slowly
                                # new photo + new decision
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

            # ... other skills

            current_step += 1
            if current_step >= len(plan):
                if loop:
                    current_step = 0  # restart plan
                else:
                    break  # plan completed

Control:
  - pause() → PAUSED (motors stop, plan remembers position)
  - resume() → RUNNING (continues from where it was)
  - abort() → IDLE (motors stop, plan cleared)
  - change_mode(new_mode) → abort current + start new
```

**Dependencies**: server.py, perception/vision.py, planner/decide.py, policy/actions.py
**Estimate**: ~340 new lines (4 files)

---

## Phase X — Notifications and Remote Control

**Problem**: robot must alert user when detecting something (person, open door, low battery)
and user must be able to control robot remotely.

### X1 — Notifications (notifications.py, ~100 lines)
```
class Notifier:
    backends: list[NotifyBackend]  # configured in config.yaml

    async def alert(trigger, description, image_bytes=None):
        message = f"ROBOT ALERT: {trigger}\n{description}\n{timestamp}"
        for backend in backends:
            await backend.send(message, image_bytes)

Backends implemented:

1. Pushover (recommended for critical alerts):
   - 1 HTTP POST to api.pushover.net/1/messages.json
   - Supports: text, attached image, priority (0-2), custom sound
   - Priority 2 = emergency: sounds until user confirms
   - Cost: $5 once (app license)
   - ~20 lines of code

2. Telegram Bot (recommended for bidirectional control):
   - sendMessage: POST to api.telegram.org/bot{token}/sendMessage
   - sendPhoto: POST with multipart/form-data (attach image)
   - getUpdates: polling to receive user commands
   - Free, bidirectional
   - ~30 lines send + ~30 lines polling

3. Email (SMTP):
   - Standard Python smtplib
   - Gmail with app password or Amazon SES
   - Higher latency (5-30s)
   - ~25 lines

4. Generic webhook:
   - POST JSON to configurable URL
   - For: Home Assistant, IFTTT, Node-RED, custom
   - ~10 lines
```

### X2 — Bidirectional Telegram Bot (in notifications.py, +50 lines)
```
Enables remote control of robot from Telegram:

Incoming commands (user → bot → robot):
  /security           → activate security mode
  /patrol             → activate patrol mode
  /stop               → stop motors, pause mode
  /status             → battery, uptime, current mode, position
  /photo              → capture and send current photo
  /mode <prompt>      → free prompt ("scan the kitchen")
  /investigate        → alert response: approach
  /ignore             → alert response: continue
  /home               → return to base

Interactive flow:
  Bot  → User: "ALERT: Person detected in hallway [photo]"
  Bot  → User: "What should I do? /investigate /ignore /alarm"
  User → Bot: /investigate
  Bot  → User: "Approaching... [new photo]"
  Bot  → User: "It's the cat. Continuing patrol."

Implementation:
  - Separate asyncio task doing getUpdates polling every 2s
  - Commands parsed and sent to SkillRunner:
    "/security" → runner.change_mode("security")
    "/stop" → runner.abort()
    "/photo" → get latest frame → telegram.sendPhoto()
```

### X3 — HTTP API for control (api.py, ~80 lines)
```
Minimal REST API for control from any client:

POST /api/mode          {"mode": "security"}
POST /api/prompt        {"prompt": "scan the kitchen"}
POST /api/stop          {}
GET  /api/status        → {mode, battery, uptime, odom, last_alert}
GET  /api/frame         → current JPEG image
POST /api/notify/test   → send test notification

Implementation: aiohttp server running in parallel with TCP server.
Configurable port (default 8080).

Usage:
  curl -X POST localhost:8080/api/mode -d '{"mode": "security"}'
  curl localhost:8080/api/status
```

### Config changes for Phases V + X
```yaml
# --- NEW: modes ---
modes:
  security:
    skills: [SCAN_360, WAIT]
    loop: true
    scan_interval_s: 30
    detect: [person, open_door, fire, movement]
    on_detect: [notify, alert]
    schedule: always

  patrol:
    skills: [NAVIGATE_TO, SCAN_360]
    waypoints: [A, B, C]
    loop: true
    detect: [person, obstacle]
    on_detect: [notify]

  explore:
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

# --- NEW: notifications ---
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
    commands: true       # enable bidirectional control

  email:
    enabled: false
    smtp_host: smtp.gmail.com
    smtp_port: 587
    username: ""
    password: ""          # app password, not real password
    to: ""

  webhook:
    enabled: false
    url: ""
    headers: {}

# --- NEW: api ---
api:
  enabled: true
  port: 8080
```

**Dependencies**: Phase V depends on already existing base components.
Phase X depends on nothing (only HTTP requests).
**Estimate**: ~280 new lines (notifications.py + api.py + telegram polling)

---

## Phase Y — Multi-Robot Abstraction (wheeled, drone, humanoid)

**Problem**: current base assumes a 2-wheel robot (differential drive).
To support drones and humanoids, protocol, policy, and config layers
must abstract the actuator and sensor types.

### Y1 — Generic ActuatorCmd (protocol.py refactor, ~30 lines)
```
Current:  VelocityCmd(speed_l: i32, speed_r: i32, flags: u8)  → wheels only
New:      ActuatorCmd(actuator_type: u8, channels: list[int], flags: u8)

actuator_type:
  0 = diff_drive   → 2 channels: [speed_l, speed_r]
  1 = quad_rotor   → 4 channels: [motor1, motor2, motor3, motor4]
                      (or better: [throttle, roll, pitch, yaw] normalized)
  2 = humanoid     → N channels: [joint_0_cdeg, joint_1_cdeg, ...]
  3 = ackermann    → 2 channels: [speed, steer_angle]

Wire format (pkt type 0x80):
  actuator_type: u8
  num_channels:  u8
  flags:         u8
  channels:      [i16; num_channels]  (little-endian)

Backward compatible: ActuatorCmd(type=0, channels=[60,60]) ≡ VelocityCmd(60,60)
```

### Y2 — Policy Translators per type (~150 lines, 3 files)
```
policy/__init__.py:
  def get_translator(robot_type: str) -> PolicyTranslator:
      if robot_type == "wheeled":  return WheeledPolicy()
      if robot_type == "drone":    return DronePolicy()
      if robot_type == "humanoid": return HumanoidPolicy()

policy/wheeled.py (refactor from current actions.py):
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
        # PID on current vs desired altitude
        thr = altitude_pid(sensors.baro, target_alt)
        return ActuatorCmd(type=1, channels=[thr, 0, 0, 0])
      if skill == "FLY_TO":
        # PID on position → roll/pitch/yaw commands
        thr, roll, pitch, yaw = position_controller(
            current=sensors.gps, target=args["position"])
        return ActuatorCmd(type=1, channels=[thr, roll, pitch, yaw])
      if skill == "LAND":
        return ActuatorCmd(type=1, channels=[descend_thr, 0, 0, 0])

      Note: real attitude PID runs on kernel (RT),
      drone.py only sends setpoints (desired attitude).

policy/humanoid.py:
  class HumanoidPolicy(PolicyTranslator):
    def translate(skill, args, sensors) -> ActuatorCmd:
      if skill == "WALK_TO":
        # Generate joint angles sequence (gait pattern)
        joints = gait_generator(step_phase, direction)
        return ActuatorCmd(type=2, channels=joints)
      if skill == "GRAB":
        # Inverse kinematics: object position → arm angles
        joints = ik_solver(args["hand"], args["object_pos"])
        return ActuatorCmd(type=2, channels=joints)
      if skill == "LOOK_AT":
        neck_pan, neck_tilt = look_direction(args["direction"])
        return ActuatorCmd(type=2, channels=[neck_pan, neck_tilt])

      Note: IK and gait can be heavy — if needed they
      move to LLM or separate service. For simple servos
      (12-DOF hobby humanoid) calculation is trivial.
```

### Y3 — Generic SensorPacket (protocol.py, ~40 lines)
```
Common header (38 bytes, all robots):
  timestamp_ms:  u64
  battery_mv:    u16
  accel_mg:      [i32; 3]
  gyro_mdps:     [i32; 3]

Extensible payload (varies by robot_type):
  Wheeled:  encoders, odom, rangefinders
  Drone:    barometer, magnetometer, GPS, sonar
  Humanoid: joint angles, foot pressure

Server detects robot_type from first STATUS packet and
uses correct parser for SensorPacket.
```

### Y4 — Config per-robot-type (~20 lines in config.yaml)
```yaml
robot:
  type: wheeled            # wheeled | drone | humanoid
  listen_port: 9000

  # Only the section for active type is used:
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

### Y5 — Kernel: Actuator Abstraction (Repo 1, ~100 lines)
```
Brain client on VF2 receives ActuatorCmd and calls correct actuator:

crates/robot/src/actuator.rs (NEW):
  pub enum ActuatorType { DiffDrive, QuadRotor, Humanoid, Ackermann }

  pub fn actuator_apply(cmd: &ActuatorCmd) {
      match cmd.actuator_type {
          DiffDrive => {
              motor_set(0, cmd.channels[0]);
              motor_set(1, cmd.channels[1]);
          }
          QuadRotor => {
              // 4 ESC outputs (already exists esc.rs with 8 PWM channels)
              for i in 0..4 { esc_set(i, cmd.channels[i]); }
          }
          Humanoid => {
              // Servo bus (I2C PCA9685 or serial)
              for i in 0..cmd.num_channels {
                  servo_set(i, cmd.channels[i]);
              }
          }
      }
  }

Already exists in kernel:
  - motor_set() → 2 DC motors (wheels)
  - esc_set()   → 8 ESC PWM channels 400Hz (crates/drivers/src/esc.rs)
  - i2c_write() → for PCA9685 servo bus

Only missing: actuator_apply() as dispatcher + servo driver if humanoid used.
```

### Phase Y summary

| Sub-phase | Where | Lines | Depends on |
|----------|-------|-------|-----------|
| Y1: Generic ActuatorCmd | protocol.py | ~30 | Nothing |
| Y2: Policy translators (×3) | policy/*.py | ~150 | Y1 |
| Y3: Generic SensorPacket | protocol.py | ~40 | Nothing |
| Y4: Config per-type | config.yaml | ~20 | Nothing |
| Y5: Kernel actuator_apply | actuator.rs | ~100 | esc.rs (already exists) |
| **Total** | | **~340** | |

Note: first version only implements wheeled.py (current robot).
drone.py and humanoid.py implemented when corresponding hardware available.
Abstraction exists to avoid refactoring later.

---

## ═══════════════════════════════════════════════════
## FUTURE PHASES (scale base to production/field)
## ═══════════════════════════════════════════════════

These phases do NOT block earlier ones. Implemented when base (P-Y)
works end-to-end. Cover: per-type safety, long-range communication,
offline autonomy, GPS missions, payloads, logging, fleet, and industrial buses.

---

## Phase AG — Safety Profiles per Robot Type

**CRITICAL Problem**: current system assumes `motor_stop()` is always
the safe response. This is FALSE for drones, humanoids, and vehicles.

- Drone: `motor_stop()` = free fall → destroyed
- Humanoid: frozen joints = falls face-first → damaged
- Car at 60km/h: motor off = no power steering → dangerous
- Wheels only: stop = safe

Each robot type needs its own **failsafe sequence** for each
failure type. Not a "flag" — it's a state machine.

### AG1 — Safety Profile abstraction (kernel + robot-brain, ~200 lines)
```
Each robot_type defines its SafetyProfile:

crates/robot/src/safety.rs (NEW):
  pub enum FailsafeEvent {
      WatchdogTimeout,     // no commands for N ms
      LinkLost,            // lost connection with brain
      BatteryLow,          // below minimum threshold
      BatteryCritical,     // below critical threshold (land NOW)
      ObstacleDetected,    // proximity sensor
      ImuFailure,          // IMU readings invalid or frozen
      GpsLost,             // no GPS fix
      MotorFailure(u8),    // motor N not responding
      TiltExceeded,        // dangerous tilt
      GeofenceViolation,   // outside allowed zone
      EStopUser,           // user pressed emergency stop
  }

  pub enum FailsafeAction {
      Stop,                // cut actuators (only safe on wheels)
      Hover,               // hold position (drone)
      ControlledDescent,   // descend gradually (drone)
      Land,                // full landing (drone)
      ReturnToHome,        // return to start point
      Crouch,              // stable low position (humanoid)
      SitDown,             // sit down (humanoid)
      BrakeGradual,        // brake progressively (vehicle)
      PullOver,            // pull over + stop (vehicle)
      KillMotors,          // LAST resort: cut everything
      LockJoints,          // lock joints (humanoid, already crouched)
      HazardLights,        // emergency lights (vehicle)
      Alert,               // notify operator
      ContinueMission,     // do nothing, continue (if safe)
  }

  pub trait SafetyProfile {
      fn failsafe(&self, event: FailsafeEvent, state: &RobotState) -> &[FailsafeAction];
      fn is_critical(&self, event: FailsafeEvent) -> bool;
      fn battery_reserve(&self) -> u16;    // mV to reach home
      fn max_tilt_deg(&self) -> u16;       // before emergency
      fn watchdog_timeout_ms(&self) -> u32;
  }
```

### AG2 — Wheeled Safety Profile (kernel, ~40 lines)
```
crates/robot/src/safety_wheeled.rs (NEW):
  Simple — stop is always safe.

  impl SafetyProfile for WheeledSafety {
    fn failsafe(&self, event, state) -> &[FailsafeAction] {
      match event {
        WatchdogTimeout     => &[Stop, Alert],
        LinkLost            => &[Stop, Alert],   // or ContinueMission if GPS
        BatteryLow          => &[Stop, Alert],
        BatteryCritical     => &[Stop, Alert],
        ObstacleDetected    => &[Stop],
        ImuFailure          => &[Stop, Alert],
        GpsLost             => &[ContinueMission],  // not critical for indoor wheels
        MotorFailure(_)     => &[Stop, Alert],
        GeofenceViolation   => &[Stop, ReturnToHome],
        EStopUser           => &[Stop],
        _                   => &[Stop],
      }
    }
    fn battery_reserve(&self) -> u16 { 6500 }  // mV
    fn max_tilt_deg(&self) -> u16 { 45 }       // rollover
    fn watchdog_timeout_ms(&self) -> u32 { 3000 }
  }
```

### AG3 — Drone Safety Profile (kernel, ~120 lines)
```
crates/robot/src/safety_drone.rs (NEW):
  Drone NEVER does motor_stop() except as last resort (kill switch).
  Sequence is always: Hover → corrective action → Land if necessary.

  impl SafetyProfile for DroneSafety {
    fn failsafe(&self, event, state) -> &[FailsafeAction] {
      match event {
        WatchdogTimeout => &[Hover, Alert],
          // Hold position. If timeout persists 10s → ControlledDescent → Land

        LinkLost => match state.mission_loaded {
          true  => &[ContinueMission],     // has GPS mission → keep going
          false => &[Hover, ReturnToHome],  // no mission → return
        },
          // Hover 30s waiting for reconnection. If none → automatic RTH

        BatteryLow => &[ReturnToHome, Alert],
          // Compute whether there is energy left to reach home.
          // If not → Land at current position.

        BatteryCritical => &[Land, Alert],
          // Immediate landing wherever it is. No choice.

        ObstacleDetected => &[Hover],
          // Hover + VLM/LLM decides: climb, go around, or wait.
          // If no brain → ascend 5m and continue.

        ImuFailure => &[ControlledDescent, Land, Alert],
          // Without IMU it cannot stabilize. Descend slowly and land.
          // Use GPS for position, barometer for altitude.

        GpsLost => &[Hover, Alert],
          // Hold altitude and attitude via IMU/baro.
          // Do not navigate — wait for fix or land.

        MotorFailure(n) => {
          // Depends on how many motors remain:
          // Quad: 1 failure → redistribute thrust (possible with some frames)
          //        → if it cannot → ControlledDescent as slow as possible
          // Hexa: 1 failure → compensate easily
          // Note: requires a mixer aware of each motor's state
          if can_compensate(n, state) {
            &[Alert, ContinueMission]
          } else {
            &[ControlledDescent, KillMotors]  // kill on touching ground
          }
        },

        TiltExceeded => &[KillMotors],
          // >60° tilt = already falling. Cut motors to
          // avoid impact damage (spinning props = more damage).
          // This is the ONLY case where KillMotors is correct.

        GeofenceViolation => &[Hover, ReturnToHome, Alert],
          // Brake, hover, return within zone.

        EStopUser => &[ControlledDescent, Land],
          // NO kill motors. Controlled descent.
          // Double-tap E-Stop → KillMotors (conscious override).
      }
    }

    fn battery_reserve(&self) -> u16 {
      // Sufficient to return home + 60s hover + land
      // Calculated dynamically based on distance to home
      self.rth_battery_estimate(state.distance_to_home)
    }
    fn max_tilt_deg(&self) -> u16 { 60 }
    fn watchdog_timeout_ms(&self) -> u32 { 500 }  // much stricter
  }

Landing sequences:
  ControlledDescent:
    1. Reduce throttle gradually (-50 cm/s)
    2. Keep level (roll=0, pitch=0)
    3. Monitor sonar_down to detect ground
    4. On touchdown → cut motors

  ReturnToHome:
    1. Ascend to safe altitude (configurable, e.g.: 30m)
    2. Rotate toward home
    3. Fly straight line to home
    4. Descend over home
    5. Land
    6. Motors off

  Hover:
    - PID maintains GPS position + barometric altitude
    - Configurable timeout before next action
```

### AG4 — Humanoid Safety Profile (kernel, ~100 lines)
```
crates/robot/src/safety_humanoid.rs (NEW):
  A humanoid NEVER freezes joints instantly (it falls).
  Sequence: reduce speed → stable position → lock.

  impl SafetyProfile for HumanoidSafety {
    fn failsafe(&self, event, state) -> &[FailsafeAction] {
      match event {
        WatchdogTimeout     => &[Crouch, LockJoints, Alert],
        LinkLost            => &[Stop, Crouch, Alert],     // stop = stop walking
        BatteryLow          => &[SitDown, LockJoints, Alert],
        BatteryCritical     => &[Crouch, LockJoints, Alert],
        ObstacleDetected    => &[Stop],                    // stop walking, stay standing
        ImuFailure          => &[Crouch, LockJoints, Alert],
        MotorFailure(n) => {
          // Depends on which joint failed:
          // Leg → immediate Crouch (cannot walk)
          // Arm → Stop walking, keep balance
          // Neck → continue (not critical)
          if is_leg_joint(n) { &[Crouch, LockJoints, Alert] }
          else { &[Stop, Alert] }
        },
        TiltExceeded => &[BreakFall, Crouch],
          // Break-fall position (arms protect head/torso)
        EStopUser => &[Crouch, LockJoints],
        _ => &[Crouch, LockJoints],
      }
    }
    fn max_tilt_deg(&self) -> u16 { 30 }   // much less than a drone
    fn watchdog_timeout_ms(&self) -> u32 { 1000 }
  }

Sequences:
  Crouch:
    1. Flex knees gradually (-5°/step at 100Hz)
    2. Lower center of gravity
    3. Keep balance via IMU feedback
    4. Result: low, stable position

  SitDown:
    1. Crouch first
    2. Flex more until sitting
    3. LockJoints in sitting position

  BreakFall:
    1. Detect fall direction
    2. Extend arms to cushion
    3. Protect head/torso
    4. Post-fall: assess damage, try to stand or LockJoints

  LockJoints:
    - All servos to hold position
    - High torque to maintain position
    - Only after in stable position
```

### AG5 — Vehicle Safety Profile (kernel, ~80 lines)
```
crates/robot/src/safety_vehicle.rs (NEW):
  Vehicle (car/tractor) NEVER cuts motor at speed.

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
          // NO instant hard braking (can roll tractor, lock wheels)
          // ABS-like braking: maximum safe deceleration
        _ => &[BrakeGradual, Stop],
      }
    }
    fn watchdog_timeout_ms(&self) -> u32 {
      // Stricter at higher speed:
      if state.speed > 3000 { 500 }    // >30km/h → 500ms
      else if state.speed > 0 { 2000 } // moving → 2s
      else { 5000 }                    // stopped → 5s
    }
  }

Sequences:
  BrakeGradual:
    - Maximum safe deceleration (configurable, e.g.: 3 m/s²)
    - If it has ABS: modulate braking per wheel
    - Keep steering straight (or follow the current curve)

  PullOver:
    1. Reduce velocity gradually
    2. If has GPS + map: find shoulder/field edge
    3. If not: go straight and brake
    4. Stop when speed=0
    5. HazardLights on
    6. Engage parking brake

  HazardLights:
    - GPIO toggle at fixed frequency (blinker)
    - ALWAYS activated in an emergency for vehicles
```

### AG6 — Safety state machine (kernel, ~150 lines)
```
crates/robot/src/safety_fsm.rs (NEW):
  Safety is NOT a flag. It's a state machine that executes
  the failsafe sequence step by step.

  pub struct SafetyFSM {
      profile: &dyn SafetyProfile,
      state: SafetyState,
      current_sequence: &[FailsafeAction],
      current_step: usize,
      event_source: FailsafeEvent,
      timer_ticks: u64,
  }

  pub enum SafetyState {
      Normal,              // all OK, normal operation
      Responding(event),   // executing failsafe sequence
      Stabilized,          // failsafe completed, awaiting intervention
      Override,            // operator took manual control
  }

  impl SafetyFSM {
      // Called from rt_safety_task on each tick:
      pub fn tick(&mut self, sensors: &SensorState, actuators: &mut ActuatorState) {
          match self.state {
              Normal => {
                  // Check all possible failure events
                  for event in check_all_events(sensors) {
                      let actions = self.profile.failsafe(event, sensors);
                      self.enter_responding(event, actions);
                      break;  // one failure at a time, priority by order
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
                  // Maintain current state. Exits only with:
                  // - Operator override (E-STOP release)
                  // - Brain reconnects and sends RESUME
                  // - Auto-recovery if event resolves (e.g.: GPS fix recovered)
              }
              Override => { /* manual operator, safety monitors but doesn't act */ }
          }
      }
  }

Kernel integration:
  - rt_safety_task: new RT task, maximum priority
  - Runs on each scheduler tick (100-1000 Hz)
  - Has override over ANY ActuatorCmd from brain
  - If safety.state != Normal, ignores brain commands
  - Only operator (E-STOP release) or auto-recovery exit failsafe
```

### AG7 — Battery reserve calculation (kernel + robot-brain, ~80 lines)
```
Each robot type needs to calculate how much battery to reserve:

Wheels:    minimum = floor (no need to reserve for "landing")
Drone:     reserve = energy for RTH + 60s hover + landing
Humanoid:  reserve = energy to sit + keep joints locked 30min
Vehicle:   reserve = energy to brake + hazard lights 30min

safety/battery.py (robot-brain, ~40 lines):
  def battery_reserve_mv(robot_type, distance_to_home_m, altitude_m) -> int:
      if robot_type == "wheeled":
          return 6500  # fixed
      if robot_type == "drone":
          # Estimate: 100mV per km RTH + 200mV for landing
          return 6800 + (distance_to_home_m // 10) + (altitude_m // 5)
      if robot_type == "vehicle":
          return 11000  # 12V nominal, minimum for electric brake

crates/robot/src/safety.rs (kernel, ~40 lines):
  fn check_battery(sensors, profile) -> Option<FailsafeEvent> {
      let reserve = profile.battery_reserve();
      if sensors.battery_mv < reserve / 2 {
          Some(BatteryCritical)
      } else if sensors.battery_mv < reserve {
          Some(BatteryLow)
      } else { None }
  }
```

### AG8 — Watchdog per-type (kernel refactor, ~50 lines)
```
Current: watchdog_timeout_ms = 3000ms fixed for all.

New: timeout varies by type AND state:

  Drone hovering:      500ms  (if loses commands, falls fast)
  Drone en route:      500ms
  Drone on ground:     5000ms (not dangerous)
  Humanoid walking:    1000ms
  Humanoid standing:   5000ms
  Car at >30km/h:      500ms  (at speed, fast reaction)
  Car parked:          5000ms
  Wheels:              3000ms (as now)

crates/robot/src/safety.rs:
  fn dynamic_watchdog_ms(profile, state) -> u32 {
      let base = profile.watchdog_timeout_ms();
      // Stricter if moving or airborne
      if state.is_airborne { base / 2 }
      else if state.speed > 0 { base }
      else { base * 2 }
  }
```

### Phase AG summary

| Sub-phase | Where | Lines | Depends on |
|----------|-------|-------|-----------|
| AG1: Safety Profile trait | kernel | ~200 | Nothing |
| AG2: Wheeled safety | kernel | ~40 | AG1 |
| AG3: Drone safety | kernel | ~120 | AG1 |
| AG4: Humanoid safety | kernel | ~100 | AG1 |
| AG5: Vehicle safety | kernel | ~80 | AG1 |
| AG6: Safety FSM | kernel | ~150 | AG1 |
| AG7: Battery reserve calc | kernel + brain | ~80 | AG1 |
| AG8: Watchdog per-type | kernel | ~50 | AG1 |
| **Total** | | **~820** | |

**IMPORTANT**: AG1 (trait) + AG2 (wheeled) + AG6 (FSM) + AG8 (watchdog) should
be implemented BEFORE testing any robot type other than wheels.
AG3/AG4/AG5 implemented when corresponding hardware available,
but SafetyProfile interface must exist from the start.

---

## Phase Z — Transport Abstraction (multi-link: WiFi, LoRa, RF, 4G)

**Problem**: Phase 1 assumes WiFi (high bandwidth, short range). For open field,
tractors at 5km, or agricultural drones, we need LoRa, RF or 4G.
Each link has very different bandwidth and latency.

```
Link        Range      Bandwidth     Latency   Camera?  Cost
WiFi        ~100m      50+ Mbps      ~5ms      Yes      $0
LoRa        2-15km     0.3-50 kbps   ~200ms    No       $10 module
RF 433/915  1-5km      1-100 kbps    ~50ms     No       $5 module
4G/LTE      Unlimited  10+ Mbps      ~50ms     Yes      Monthly SIM
Satellite   Global     2-100 kbps    ~500ms    No       $$$
```

### Z1 — Link abstraction layer (robot-brain + kernel, ~200 lines)
```
Common interface for all links:

class TransportLink:
    async def send(data: bytes) -> bool
    async def recv(timeout_s: float) -> bytes
    def bandwidth_bps() -> int       # available bandwidth
    def latency_ms() -> int          # estimated latency
    def is_connected() -> bool
    def link_quality() -> float      # 0.0-1.0 (normalized RSSI)

Implementations:
  WiFiLink      → TCP socket (current)
  LoRaLink      → serial (UART to LoRa module SX1276/RFM95)
  RF433Link     → serial (UART to module HC-12/E32)
  CellularLink  → TCP over PPP/QMI (module SIM7600/EC25)
  SatLink       → serial (Iridium/LoRa satellite)

Brain client selects link per config + automatic fallback:
  - WiFi available → WiFi (full bandwidth)
  - WiFi down → LoRa (telemetry + commands only)
  - LoRa down → RF 433MHz (minimum: heartbeat + emergency)
```

### Z2 — Bandwidth-aware protocol (robot-brain, ~100 lines)
```
Protocol adapts to active link:

WiFi mode (>1 Mbps):
  - SENSOR_PACKET at 20 Hz
  - CAMERA_FRAME at 2 Hz (JPEG, 10-75 KB)
  - ACTUATOR_CMD at 20 Hz
  - Full bidirectional

LoRa mode (<50 kbps):
  - SENSOR_PACKET_COMPACT at 1 Hz (20 bytes: timestamp, lat, lon, alt, battery, mode)
  - NO camera (impossible)
  - COMMAND_COMPACT at 0.5 Hz (8 bytes: skill_id + 3 params)
  - Robot executes mission autonomously, reports state only

RF Emergency mode (<1 kbps):
  - HEARTBEAT every 10s (4 bytes: battery + mode + GPS fix)
  - EMERGENCY_CMD: RTH (return to home), LAND, STOP

New packet types for low-bandwidth:
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

### Z3 — LoRa driver (kernel, ~300 lines)
```
crates/drivers/src/lora.rs (NEW):
  Target: Semtech SX1276/SX1278 (RFM95W module, $10)
  Connection: SPI (already supported in kernel)

  API:
    lora_init(freq_mhz, sf, bw, power) -> bool
    lora_send(data: &[u8]) -> bool
    lora_recv(buf: &mut [u8], timeout_ms: u32) -> Option<usize>
    lora_rssi() -> i16
    lora_set_mode(mode: LoRaMode)  // sleep, standby, rx, tx

  Typical field configuration:
    Frequency: 868 MHz (EU) / 915 MHz (US)
    Spreading Factor: SF7 (fast, short) to SF12 (slow, long)
    Bandwidth: 125/250/500 kHz
    TX Power: 2-20 dBm

  SF7 @125kHz → ~5.5 kbps, ~2km    (OK for fast telemetry)
  SF12@125kHz → ~0.3 kbps, ~15km   (OK for heartbeat, emergency)
```

### Z4 — Link failover + auto-switch (~80 lines)
```
robot-brain/transport/manager.py (NEW):
  class LinkManager:
    links: list[TransportLink]  # ordered by priority

    async def send(data, priority):
      # High priority (emergency): try all links
      # Normal: use best available link
      for link in links:
        if link.is_connected() and link.bandwidth_bps() >= needed:
          return await link.send(data)

    async def monitor():
      # Continuous loop: checks quality of each link
      # If WiFi drops → switch to LoRa automatically
      # If WiFi returns → switch back
      # Notify user of link change

Kernel side (brain client ELF):
  - Attempt WiFi TCP connect
  - If fails or timeout → open UART to LoRa module
  - Send SENSOR_COMPACT instead of SENSOR_PACKET
  - Execute offline mission (GPS waypoints, no VLM)
```

### Z5 — Kernel multi-UART for LoRa/RF (Repo 1, ~100 lines)
```
crates/drivers/src/uart.rs:
  Already supports UART0 (console). Needs:
  - UART1 init (VF2: 0x10010000, K1: 0xD4017800)
  - uart1_write()/uart1_read() for LoRa/RF module
  - Userspace: SYS_UART_WRITE/READ or map as fd

crates/drivers/src/spi.rs:
  Already exists. LoRa SX1276 uses SPI.
  Only missing: expose SPI from userspace if LoRa driver runs in user mode.
```

### Phase Z summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| Z1: Link abstraction | ~200 | Nothing |
| Z2: Bandwidth-aware protocol | ~100 | Z1 |
| Z3: LoRa driver (SX1276) | ~300 | SPI (already exists) |
| Z4: Link failover | ~80 | Z1 |
| Z5: Multi-UART kernel | ~100 | UART (already exists) |
| **Total** | **~780** | |

**Phase 1**: WiFi only (already planned in W/W-alt).
**Phase 2**: Add LoRa as backup (Z3 + Z5 + Z1).
**Phase 3**: Add 4G if needed (SIM module + PPP stack).

---

## Phase AA — GPS Missions + Geofencing

**Problem**: for agricultural drones and tractors, navigation is not visual
("go to kitchen") but GPS ("traverse these 500 waypoints with 2cm precision").

### AA1 — Mission Planner (robot-brain, ~200 lines)
```
planner/mission.py (NEW):
  Generates coverage patterns from a defined area.

  class MissionPlanner:
    def boustrophedon(area: Polygon, row_spacing_m, direction_deg) -> list[Waypoint]:
        # Zigzag pattern (back and forth) — most common in agriculture
        # Input: field polygon + spacing between rows
        # Output: ordered list of GPS waypoints
        #
        #   →→→→→→→→→→→→→→→→→│
        #   │←←←←←←←←←←←←←←←←
        #   →→→→→→→→→→→→→→→→→│
        #   │←←←←←←←←←←←←←←←←
        #   →→→→→→→→→→→→→→→→→│

    def spiral(center: LatLon, radius_m, spacing_m) -> list[Waypoint]:
        # Spiral from center outward (search, circular spraying)

    def grid(area: Polygon, spacing_m) -> list[Waypoint]:
        # Grid (photographic coverage, mapping)

    def perimeter(area: Polygon) -> list[Waypoint]:
        # Perimeter only (fence/wall inspection)

  class Waypoint:
    lat_deg7: int      # lat × 10^7 (integer, no floats)
    lon_deg7: int      # lon × 10^7
    alt_cm: int        # altitude (0 for ground robots)
    speed_cms: int     # speed for this segment
    action: str        # "navigate" | "spray_on" | "spray_off" | "photo" | "land"

  Input formats (interoperable):
    - KML/KMZ (Google Earth) → parse polygon
    - GeoJSON → parse polygon
    - Manual coordinate list
    - Draw on map (future: web UI)
```

### AA2 — Geofencing (robot-brain + kernel, ~150 lines)
```
safety/geofence.py (NEW):
  Define limits robot NEVER can cross.
  Validated BEFORE sending any ActuatorCmd.

  class Geofence:
    inclusion_zones: list[Polygon]  # MUST be inside one
    exclusion_zones: list[Polygon]  # NEVER can enter
    max_altitude_m: float           # ceiling (for drones)
    min_altitude_m: float           # floor minimum (for drones)
    max_distance_m: float           # max radius from home

    def is_allowed(lat, lon, alt) -> bool:
        # 1. Is it inside some inclusion zone?
        # 2. Is it outside all exclusion zones?
        # 3. Is altitude within limits?
        # 4. Is distance to home < max_distance?

    def nearest_violation(lat, lon) -> tuple[str, float]:
        # "exclusion_zone_road", 15.3m  → for warnings

  Actions on violation:
    WARN:    notify, don't act (approaching limit)
    BRAKE:   decelerate gradually (entering buffer zone)
    STOP:    stop immediately (at limit)
    RTH:     return home (beyond limit — failsafe)

  Buffer zones:
    Each geofence has a buffer (e.g.: 10m before limit → BRAKE)
    Avoids hard braking — decelerates gradually

Kernel side (redundant safety):
  crates/robot/src/geofence.rs (NEW, ~80 lines):
    - Simplified geofence in kernel (rectangle + radius, no polygons)
    - Last resort: if brain client fails, kernel does STOP
    - Configurable via CONFIG.INI: geofence_lat_min/max, lon_min/max, radius_m
    - Checked in rt_motor_task before applying ActuatorCmd
```

### AA3 — GPS Waypoint Navigation (kernel + robot-brain, ~150 lines)
```
GPS driver already exists: crates/gps/src/lib.rs (complete NMEA parser).
Missing: waypoint-to-waypoint navigation.

planner/gps_nav.py (robot-brain, ~80 lines):
  def navigate_waypoint(current: LatLon, target: Waypoint, heading) -> skill:
      bearing = calc_bearing(current, target)   # angle to target
      distance = haversine(current, target)     # distance in meters
      turn_needed = normalize(bearing - heading)

      if distance < arrival_radius:
          return next_waypoint()
      if abs(turn_needed) > 10:
          return TURN(turn_needed)
      return FORWARD(speed=target.speed_cms)

  Note: for tractors and drones with RTK GPS (2cm), waypoint
  navigation is precise enough without VLM.
  VLM used as safety overlay: "is there something in the way?"

Kernel side (crosstrack correction):
  crates/nav/src/lib.rs — already has navigation stubs.
  Add: crosstrack_error(pos, wp_a, wp_b) → lateral correction
  so tractor/drone doesn't deviate from line between waypoints.
```

### AA4 — RTK GPS support (kernel, ~100 lines)
```
For 2cm precision (tractors, precision agriculture):

crates/gps/src/rtk.rs (NEW):
  - Parse RTCM3 messages (differential corrections)
  - Input: UART from base station or NTRIP caster (via 4G)
  - Feed RTCM to GPS module (ublox F9P or similar)
  - Module does RTK calculation internally
  - gps_fix_type() now reports: NoFix | 2D | 3D | RTK_Float | RTK_Fixed

Hardware:
  - Module: u-blox ZED-F9P ($200) — supports RTK
  - Base station: second fixed F9P (or public NTRIP service)
  - Precision: 2cm horizontal with RTK Fixed

Note: Do NOT implement RTK in software. F9P module does it.
Only need to:
  1. Receive RTCM on a link (4G/WiFi) and pass to module via UART
  2. Parse improved fix type from NMEA/UBX output
```

### AA5 — Headland turns (robot-brain, ~60 lines)
```
For tractors: at end of row, make automatic turn.

planner/headland.py (NEW):
  def headland_turn(current_heading, next_row_heading, vehicle_type) -> list[skill]:
      # For tractor (Ackermann steering, can't turn in place):
      if vehicle_type == "ackermann":
          return [
              FORWARD(speed=30, duration=2),   # advance bit
              TURN(next_heading, radius=3m),     # wide turn
              FORWARD(speed=30, duration=2),   # realign
          ]
      # For diff drive (can turn in place):
      else:
          return [TURN(next_heading)]

  Turn types:
    - U-turn (180°): 2 adjacent rows
    - Skip-turn: skip rows to reduce soil compaction
    - Fishtail: turn in 3 maneuvers (for long vehicles)
```

### Phase AA summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AA1: Mission planner (patterns) | ~200 | Nothing |
| AA2: Geofencing | ~230 | GPS driver (already exists) |
| AA3: GPS waypoint nav | ~150 | GPS + AA2 |
| AA4: RTK GPS | ~100 | GPS UART (already exists) |
| AA5: Headland turns | ~60 | AA1 + AA3 |
| **Total** | **~740** | |

---

## Phase AB — Implement/Payload Abstraction

**Problem**: robot doesn't just move — it also acts on environment.
Drone sprays, tractor seeds, security robot turns on spotlight.

### AB1 — Payload abstraction (robot-brain + kernel, ~120 lines)
```
New packet type:
  0x85 PAYLOAD_CMD:
    payload_type: u8
    channel:      u8
    value:        i16     # PWM, percentage, on/off, etc.
    duration_ms:  u16     # 0 = indefinite

payload_type:
  0 = GPIO on/off       (spotlight, siren, relay)
  1 = PWM duty          (spray pump, variable speed)
  2 = Servo angle       (gripper, release hook)
  3 = PTO (tractor)     (power take-off: on/off + RPM)
  4 = Spray section     (individual boom section)

Kernel:
  crates/robot/src/payload.rs (NEW, ~60 lines):
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
  New skills per payload:
    SPRAY_ON / SPRAY_OFF
    GRIPPER_OPEN / GRIPPER_CLOSE
    SPOTLIGHT_ON / SPOTLIGHT_OFF
    PTO_START / PTO_STOP
    RELEASE (drop payload)

  Skills defined in config.yaml per mission type.
```

### AB2 — Smart spray control (robot-brain, ~80 lines)
```
For agriculture: adjust flow rate by speed and VLM.

policy/spray.py (NEW):
  def spray_rate(speed_cms, target_rate_ml_per_m2, swath_m) -> int:
      # PWM proportional to speed to maintain constant dose
      if speed_cms == 0: return 0
      flow_ml_per_s = target_rate * swath * speed_cms / 100
      return flow_to_pwm(flow_ml_per_s)

  Optional with VLM:
    - VLM identifies "weed" vs "crop" in image
    - Spray only over weeds (precision spraying)
    - Saves 30-70% of product
```

### AB3 — CAN bus driver (kernel, ~400 lines)
```
For tractors and industrial implements using CAN/ISOBUS.

crates/drivers/src/can.rs (NEW):
  Target: SoC CAN controller or MCP2515 (SPI-to-CAN, $5)

  API:
    can_init(bitrate: u32) -> bool          // 250k, 500k, 1M
    can_send(id: u32, data: &[u8]) -> bool
    can_recv(buf: &mut CanFrame) -> bool
    can_set_filter(id: u32, mask: u32)

  Protocols on CAN:
    - J1939 (tractors): engine RPM, PTO speed, implement control
    - ISOBUS (ISO 11783): standard agricultural machinery
    - CANopen (industrial): servos, sensors, actuators

  Note: J1939/ISOBUS are complex. Phase 1: CAN raw frames only.
  J1939 parsing can be done in robot-brain (Python).
```

### Phase AB summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AB1: Payload abstraction | ~120 | Nothing |
| AB2: Spray control | ~80 | AB1 + VLM |
| AB3: CAN bus driver | ~400 | SPI (already exists) |
| **Total** | **~600** | |

---

## Phase AC — Offline Autonomy (without remote brain)

**Problem**: with LoRa at 5km, no bandwidth for camera or querying
VLM/LLM at each step. Robot must operate autonomously with limited
local intelligence, reporting only telemetry and accepting high-level commands.

### AC1 — Mission preload (robot-brain → kernel, ~80 lines)
```
Before going to field, brain loads complete mission to robot:

New packet type:
  0x86 MISSION_UPLOAD:
    mission_id: u16
    num_waypoints: u16
    waypoints: [Waypoint; N]    # lat, lon, alt, speed, action

Brain client on VF2 stores mission in RAM (or FAT32 /fat/MISSION.BIN).
Executes waypoint by waypoint without needing remote brain.

Flow:
  1. At home (WiFi): brain plans mission + sends MISSION_UPLOAD
  2. Robot goes to field (LoRa): executes mission, reports SENSOR_COMPACT
  3. Brain monitors progress (position, battery, current waypoint)
  4. If needed: brain sends COMMAND_COMPACT (skip waypoint, RTH, pause)
  5. Robot returns to WiFi range: downloads complete log + photos
```

### AC2 — Onboard decision fallback (kernel, ~100 lines)
```
Local MLP (Phase 14-15) already works as reflex layer (L1):
  - Obstacle → stop
  - Path clear → forward

For offline autonomy, add L1.5:
  - If has GPS mission: follow waypoints (GPS nav)
  - If sensor detects obstacle: deviate locally, resume route
  - If battery low: automatic RTH
  - If loses GPS fix: STOP and wait
  - If loses link: continue mission (configurable) or RTH

crates/behavior/src/lib.rs:
  Behavior tree already has L0-L3. Add:
  L1.5 = GPS_MISSION:
    - Priority between L1 (reflex) and L2 (remote brain)
    - If brain is connected → L2 override
    - If brain disconnected → L1.5 executes GPS mission
```

### AC3 — Data logging + deferred upload (kernel, ~150 lines)
```
Robot records everything locally. When returns to WiFi, uploads log.

crates/robot/src/logger.rs (NEW):
  Ring buffer in RAM (or FAT32 if has SD):
    - Sensor readings every 100ms
    - GPS positions every 1s
    - Camera frames every 5s (JPEG, stored on SD)
    - Events: mode changes, alerts, geofence warnings
    - Actuator commands sent

  Format: binary log (timestamp + type + data), similar to MAVLink .tlog

  When returns to WiFi:
    1. Brain detects reconnection
    2. Robot sends LOG_AVAILABLE with size
    3. Brain downloads via bulk transfer
    4. Robot deletes log

  Useful for:
    - Debug (what happened when robot was alone)
    - Training data (images + actions for future fine-tuning)
    - Compliance (record of agricultural product application)
    - Mapping (geolocated photos → orthomosaic)
```

### Phase AC summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AC1: Mission preload | ~80 | Protocol (R1) |
| AC2: Onboard decision fallback | ~100 | Behavior tree (already exists) |
| AC3: Data logging + deferred upload | ~150 | FAT32 (already exists) |
| **Total** | **~330** | |

---

## Phase AD — Data Logging, Replay and Analytics

**Problem**: to improve system, need to see what robot did,
replay situations, and analyze performance.

### AD1 — Structured event log (robot-brain, ~100 lines)
```
robot-brain/logging/event_log.py (NEW):
  class EventLogger:
    # Record everything in local SQLite (no external server needed)
    def log_sensor(timestamp, sensor_data)
    def log_frame(timestamp, image_bytes, vlm_description)
    def log_decision(timestamp, scene, action, confidence)
    def log_actuator(timestamp, actuator_cmd)
    def log_alert(timestamp, trigger, description, image)
    def log_mode_change(timestamp, old_mode, new_mode)
    def log_link_change(timestamp, old_link, new_link)
    def log_geofence(timestamp, event_type, distance)

  Table: events(id, timestamp, type, data_json, image_blob)
  Indexed by timestamp + type for fast queries.
```

### AD2 — Mission replay (robot-brain, ~80 lines)
```
robot-brain/logging/replay.py (NEW):
  Replays recorded mission step by step:
    - Shows frames with overlay of: sensors, LLM decision, actuator
    - Allow forward/backward
    - Identify points where robot made bad decision
    - Export to MP4 video (optional, with ffmpeg)

  Usage:
    python replay.py --mission 2026-03-15_field_A.db
    python replay.py --mission latest --speed 5x
```

### AD3 — Analytics dashboard (robot-brain, ~120 lines)
```
robot-brain/monitor/analytics.py (NEW):
  Metrics calculated from logs:
    - Area covered vs total area (efficiency)
    - Time moving vs stopped
    - Battery consumption per km
    - Alerts per hour/mission
    - Average crosstrack error (navigation precision)
    - VLM/LLM latency percentiles
    - Link quality over time
    - Geofence violations count

  Output: terminal table or JSON (for Grafana integration if needed)
```

### Phase AD summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AD1: Event logger | ~100 | SQLite (stdlib) |
| AD2: Mission replay | ~80 | AD1 |
| AD3: Analytics | ~120 | AD1 |
| **Total** | **~300** | |

---

## Phase AE — Fleet Management (multi-vehicle)

**Problem**: if have 3 drones covering a field, or 2 security robots
in a building, need to coordinate them.

### AE1 — Fleet server (robot-brain, ~200 lines)
```
robot-brain/fleet/manager.py (NEW):
  class FleetManager:
    robots: dict[str, RobotConnection]  # id → connection

    def assign_areas(total_area: Polygon, num_robots) -> dict[str, Polygon]:
        # Divide area equitably among robots
        # Avoid overlap

    def monitor_all() -> FleetStatus:
        # State of each robot: position, battery, mode, mission progress
        # Alerts: robot disconnected, battery low, geofence violation

    def relay(source_id, dest_id, data):
        # Robot A has no direct link to brain
        # Robot B acts as relay: A→B→brain

    def redistribute(failed_id):
        # Robot 2 fails → redistribute its area between Robot 1 and 3

  Each robot connects to same brain server with robot_id in STATUS packet.
  Server maintains separate state per robot.
```

### AE2 — Multi-robot coordination protocol (~50 lines)
```
New packet types:
  0x87 FLEET_STATUS:
    robot_id: u16
    num_robots: u8
    neighbors: [(id, rssi, distance_m); N]  # detected nearby robots

  0x88 FLEET_CMD:
    target_id: u16   (0xFFFF = broadcast)
    cmd_type:  u8    (0=assign_area, 1=relay_for, 2=RTH_all, 3=pause_all)
    payload:   [u8; N]
```

### Phase AE summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AE1: Fleet manager | ~200 | Base server |
| AE2: Fleet protocol | ~50 | Protocol (R1) |
| **Total** | **~250** | |

---

## Phase AF — MAVLink Bridge (interop with existing ecosystem)

**Problem**: huge ecosystem already exists with autopilots (PX4, ArduPilot),
ground stations (QGroundControl, Mission Planner), and hardware (Pixhawk).
Instead of reimplementing everything, we can make a bridge.

### AF1 — MAVLink parser/builder (robot-brain or kernel, ~200 lines)
```
MAVLink v2 is the de facto standard for drones and autonomous vehicles.
Minimal parser (we don't need 300+ message types):

Messages we care about:
  HEARTBEAT (0):        system alive, mode, status
  GPS_RAW_INT (24):     lat, lon, alt, fix, satellites
  ATTITUDE (30):        roll, pitch, yaw
  GLOBAL_POSITION (33): lat, lon, alt, velocities
  MISSION_ITEM (39):    waypoint upload
  COMMAND_LONG (76):    arm, disarm, takeoff, land, RTH
  STATUSTEXT (253):     text status messages

Two uses:
  1. robot-brain speaks MAVLink to a Pixhawk (hardware autopilot)
     → brain sends waypoints → Pixhawk executes them → feedback
  2. robot-brain translates our protocol to MAVLink
     → QGroundControl connects as ground station
     → map visualization, missions, telemetry for free
```

### AF2 — QGroundControl compatible (robot-brain, ~100 lines)
```
robot-brain/bridge/mavlink_bridge.py (NEW):
  Translates our protocol ↔ MAVLink:

  SENSOR_PACKET → MAVLink GPS_RAW_INT + ATTITUDE + GLOBAL_POSITION
  ACTUATOR_CMD  ← MAVLink COMMAND_LONG (arm/disarm/goto)
  STATUS        → MAVLink HEARTBEAT
  MISSION_UPLOAD → MAVLink MISSION_ITEM sequence

  UDP port 14550 (MAVLink standard).
  QGroundControl connects automatically and shows:
    - Position on map
    - Real-time telemetry
    - Visual mission planning (drag & drop waypoints)
    - Visual geofence editor

  This gives us a ground station for FREE without implementing UI.
```

### Phase AF summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AF1: MAVLink parser | ~200 | Nothing |
| AF2: QGC bridge | ~100 | AF1 + Protocol |
| **Total** | **~300** | |

---

## Phase AH — EKF State Estimation + Sensor Fusion

**CRITICAL problem for drones**: without EKF, sensors are raw noisy data.
A drone cannot hover stably with raw IMU. It needs sensor fusion
that filters noise, compensates bias, and combines multiple sources into single estimate.

**Reference**: PX4 EKF2 (24 states, delayed fusion, error-state quaternion).

### AH1 — EKF core (kernel, ~500 lines)
```
crates/nav/src/ekf.rs (NEW):
  Extended Kalman Filter with minimum viable state:

  State vector (15 states):
    - position:     [x, y, z]        (NED, meters)
    - velocity:     [vx, vy, vz]     (NED, m/s)
    - attitude:     [q0, q1, q2, q3] (quaternion)
    - gyro_bias:    [bx, by, bz]     (rad/s, estimated online)
    - accel_bias:   [bax, bay, baz]  (m/s², estimated online)

  Matrices:
    - P[15×15]: covariance
    - Q[15×15]: process noise
    - Implementation: static arrays, no alloc

  Predict (each IMU sample, ~200-1000 Hz):
    1. Integrate gyro → rotate quaternion
    2. Rotate accel to NED → integrate velocity → integrate position
    3. Propagate covariance P = F*P*F' + Q

  Update (when GPS/baro/mag measurement arrives):
    - GPS update: corrects position + velocity
    - Baro update: corrects altitude (z)
    - Mag update: corrects heading (yaw)
    - Innovation check: if residual > 5σ → reject measurement (sensor fault)

  Error-state formulation:
    - Don't estimate quaternion directly (singularities)
    - Estimate error in rotation (3 small angles)
    - Apply correction to quaternion after update

  Delayed fusion (like PX4):
    - Each sensor has different latency (GPS ~200ms, baro ~50ms, IMU ~1ms)
    - Buffer of IMU measurements
    - When GPS arrives, rewind to GPS timestamp, apply update, re-propagate

  All integer where possible:
    - Position/velocity: mm and mm/s (i32)
    - Quaternion: Q30 fixed-point (i32 with 30 fractional bits)
    - Only P matrix in f32 (needs dynamic range)
```

### AH2 — Sensor calibration (kernel, ~150 lines)
```
crates/nav/src/calibration.rs (NEW):
  At boot, automatic calibration:

  IMU:
    - Gyro bias: average of first 1000 samples (drone still)
    - Accel bias: compare with expected gravity [0,0,-9.81]
    - Gyro temperature compensation (LUT table if available)

  Magnetometer:
    - Hard-iron offset: center of sample sphere
    - Soft-iron scaling: ellipsoid → sphere
    - Declination correction (from GPS position or config)

  Barometer:
    - Reference pressure at boot (ground level = 0m)
    - Temperature compensation

  Note: full calibration (rotate drone in 8) is manual.
  Basic calibration (bias removal) is automatic at boot.
```

### AH3 — Sensor redundancy + voting (kernel, ~200 lines)
```
crates/nav/src/redundancy.rs (NEW):
  For serious drones: dual IMU, dual baro, dual GPS.

  Voting strategy:
    2 sensors:
      - If both agree (within tolerance) → average
      - If diverge → mark one as suspect, use other
      - If both diverge greatly → ALERT, use EKF prediction only

    3 sensors (TMR):
      - Median voter: use middle value
      - If one diverges from other two → discard automatically
      - Report sensor health in STATUS packet

  sensor_health: [SensorStatus; MAX_SENSORS]
    SensorStatus { id, type, ok: bool, last_update_ms, divergence_count }

  Integration with Safety FSM (Phase AG):
    - ImuFailure detected here (readings frozen, divergence, NaN)
    - GpsLost detected here (no fix for >5s)
    - BaroFailure detected here (pressure reading implausible)
```

### Phase AH summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AH1: EKF core (15 states) | ~500 | IMU + GPS (already exist) |
| AH2: Sensor calibration | ~150 | AH1 |
| AH3: Sensor redundancy + voting | ~200 | AH1 + AG (safety) |
| **Total** | **~850** | |

---

## Phase AI — Simulation (SITL/HITL) — BEFORE HARDWARE

**Problem**: can't test anything physical without risk of breaking hardware.
EVERYTHING is tested first in simulation. Brain server doesn't know if talking to
real robot or simulated — same TCP protocol, same code.

**Principle**: simulate FIRST, hardware AFTER. Each robot type
has its simulator. Test at least 10 simulated hours before real hardware.

### AI0 — SITL Wheeled (robot-brain, ~200 lines) *** WEEK 1 — BEFORE EVERYTHING ***
```
tools/sitl/sitl_wheeled.py (NEW):
  Simulator of wheeled robot differential drive.
  Connects to brain server as if real VF2.
  ZERO dependencies: only Python stdlib + protocol.py.

  class WheeledSim:
    # Physical state
    x_mm: int = 0             # X position (mm)
    y_mm: int = 0             # Y position (mm)
    theta_cdeg: int = 0       # heading (centidegrees)
    speed_l: int = 0          # left motor velocity
    speed_r: int = 0          # right motor velocity
    battery_mv: int = 8400    # battery (drains slow)
    encoder_l: int = 0        # left encoder ticks
    encoder_r: int = 0        # right encoder ticks

    # Robot parameters (Yahboom chassis 310 motors)
    wheel_base_mm: int = 142  # distance between wheels
    ticks_per_m: int = 1000   # encoder ticks per meter
    max_speed: int = 80       # max velocity

    def step(self, dt=0.05):
        # Differential drive kinematic model
        v = (self.speed_l + self.speed_r) / 2
        w = (self.speed_r - self.speed_l) * 36000 / (self.wheel_base_mm * 2)
        self.x_mm += v * cos(theta_rad) * dt
        self.y_mm += v * sin(theta_rad) * dt
        self.theta_cdeg += w * dt
        # Encoders
        self.encoder_l += self.speed_l * self.ticks_per_m * dt / 1000
        self.encoder_r += self.speed_r * self.ticks_per_m * dt / 1000
        # Simulated battery drain
        drain = (abs(self.speed_l) + abs(self.speed_r)) / 10
        self.battery_mv -= drain * dt

    def sensor_packet(self) -> bytes:
        return SensorPacket(
            timestamp_ms=time_ms(),
            accel_mg=(0, 0, 1000),           # gravity 1g in Z
            gyro_mdps=(0, 0, self.omega()),   # rotation Z
            odom_dist_mm=self.distance(),
            odom_hdg_cdeg=self.theta_cdeg,
            encoder_l=self.encoder_l,
            encoder_r=self.encoder_r,
            range_front_mm=random(200, 5000), # simulated obstacle
            range_right_mm=random(100, 3000),
            battery_mv=self.battery_mv,
        ).to_bytes()

  Simulated environment:
    class SimWorld:
      walls: list[Line]         # walls (collision)
      obstacles: list[Circle]   # circular obstacles
      rooms: dict[str, Point]   # named locations ("kitchen", "bedroom")

      def raycast(origin, direction) -> int:
          # Simulated rangefinder: distance to first obstacle
          # Uses the walls and obstacles of the world

      def check_collision(robot_pos, robot_radius) -> bool:
          # Did robot hit something?

    Predefined worlds:
      house.yaml:    house with 4 rooms, hallways, doors
      office.yaml:   office with cubicles
      field.yaml:    open field with edges
      empty.yaml:    empty space (calibration)

  TCP Server:
    async def main():
      sim = WheeledSim()
      world = SimWorld.load("house.yaml")

      # Connect to brain server as if real VF2
      reader, writer = await asyncio.open_connection(brain_host, brain_port)
      # Or: listen as server (brain connects to us)

      while True:
          # 1. Send sensor data to brain
          await send_packet(writer, SENSOR_PACKET, sim.sensor_packet())

          # 2. Receive ActuatorCmd from brain
          pkt = await read_packet_timeout(reader, 0.05)
          if pkt and pkt.type == ACTUATOR_CMD:
              cmd = ActuatorCmd.from_bytes(pkt.payload)
              sim.speed_l = cmd.channels[0]
              sim.speed_r = cmd.channels[1]

          # 3. Step physics
          sim.step(dt=0.05)
          world.check_collision(sim)

          # 4. Every 500ms: send simulated camera frame
          if frame_timer():
              frame = render_topdown(sim, world)  # topdown view
              await send_packet(writer, CAMERA_FRAME, frame)

  Visualization (optional, matplotlib):
    - Topdown view of world with walls
    - Robot position (oriented triangle)
    - Traversed trajectory
    - Rangefinder rays (lines from robot to obstacle)
    - Real-time update

Usage:
  Terminal 1: python tools/sitl/sitl_wheeled.py --world house.yaml --viz
  Terminal 2: python server.py --mode patrol
  → Brain patrols simulated house, sees obstacles, decides, turns, etc.

  Without visualization (headless, for tests):
  python tools/sitl/sitl_wheeled.py --world house.yaml --headless --duration 3600
  → 1 hour of simulated patrol, generates log for analysis
```

### AI1 — SITL Drone (robot-brain, ~300 lines) *** PRE-DRONE ***
```
tools/sitl/sitl_drone.py (NEW):
  Simplified physical model of drone.
  Implemented when starting work with drones (post phases AH-AK).

  class DronePhysics:
    position: [x, y, z]       # meters NED
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
    Generates simulated sensor data from physical state:
    - IMU: accel + gyro + noise + bias (configurable)
    - GPS: position + delay(200ms) + noise(2m) + occasional dropout
    - Baro: altitude + noise + drift
    - Mag: heading + hard-iron offset + noise
    - Rangefinder: distance to ground + noise

  Usage:
    Terminal 1: python tools/sitl/sitl_drone.py --wind 5.0
    Terminal 2: python server.py --mode patrol
    → Brain controls simulated drone with 5 m/s wind
```

### AI1b — SITL Humanoid (MuJoCo, ~200 lines) *** PRE-HUMANOID ***
```
tools/sitl/sitl_humanoid.py (NEW):
  Uses MuJoCo for realistic contact physics, balance, falls.
  Implemented when starting work with humanoids (post phases AO-AU).

  Requires: pip install mujoco

  class HumanoidSim:
    model: mujoco.MjModel      # loaded from URDF/MJCF
    data: mujoco.MjData

    def step(joint_commands):
        data.ctrl[:] = joint_commands
        mujoco.mj_step(model, data)

    def sensor_packet():
        # IMU of torso
        # Current joint angles
        # Foot contact forces
        # Camera render

  Usage:
    python tools/sitl/sitl_humanoid.py --model humanoid_12dof.xml --viz
```

### AI2 — Integration with external simulators (~100 lines)
```
Bridges to connect 3D simulators to brain server:

tools/bridges/ (NEW directory):

  webots_bridge.py:
    Connects Webots to brain via TCP protocol.
    - Reads Webots sensors (camera, LiDAR, IMU, GPS)
    - Converts to SensorPacket
    - Receives ActuatorCmd → applies to Webots motors
    Useful for: realistic 3D vision (rendered camera), LiDAR

  gazebo_bridge.py:
    Connects Gazebo/ROS2 to brain via TCP protocol.
    - Subscribes to ROS2 topics (sensor_msgs, nav_msgs)
    - Converts to our binary format
    Useful for: fleet simulations, drones with ROS2

  Note: these bridges are OPTIONAL. Custom SITL is sufficient
  for 90% of development. Bridges used when need:
    - Realistic 3D rendering (test VLM with real images)
    - Complex collision physics
    - Simulated LiDAR
    - Multiple simultaneous robots
```

### AI3 — HITL support (kernel + tools, ~100 lines)
```
Hardware-in-the-Loop: REAL kernel runs on VF2,
but sensors come from simulator instead of hardware.

tools/hitl/hitl_bridge.py:
  - Runs physical model on PC
  - Sends simulated sensor data to VF2 via TCP/UART
  - Receives ActuatorCmd from VF2
  - Verifies real kernel behaves same as SITL

Kernel support:
  CONFIG.INI: sensor_source = hardware | sitl
  If sitl: read sensors from socket/UART instead of real I2C/SPI

Validation order:
  1. Pure SITL (PC only) → verify brain logic
  2. HITL (real kernel + simulated sensors) → verify kernel
  3. Real hardware → verify real actuators + sensors
  If something fails in 3 but not 1-2 → hardware/driver problem
```

### AI4 — Test scenarios library (~150 lines)
```
tools/sitl/scenarios/ (NEW):
  YAML files with test scenarios by robot type.

  === Wheeled scenarios (test from week 1): ===

  wheeled/patrol_house.yaml:
    world: house.yaml
    mode: patrol
    waypoints: [kitchen, living_room, bedroom, entry]
    duration: 600s
    expected: {visits_all: true, collisions: 0, battery_ok: true}

  wheeled/obstacle_avoid.yaml:
    world: obstacles.yaml
    mode: explore
    duration: 300s
    obstacles: {count: 10, random_positions: true}
    expected: {collisions: 0}

  wheeled/security_detect.yaml:
    world: house.yaml
    mode: security
    events:
      - {time: 120s, action: spawn_person, location: kitchen}
    expected: {alert_triggered: true, alert_time: <30s}

  wheeled/battery_low.yaml:
    world: house.yaml
    battery: {start: 6800, drain_rate: 100}
    expected: {returns_home: true, battery_above: 6500}

  wheeled/long_run.yaml:
    world: house.yaml
    mode: patrol
    duration: 36000s  # 10 hours
    expected: {crashes: 0, memory_leaks: false}

  === Drone scenarios (test pre-drone): ===

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

  === Humanoid scenarios (test pre-humanoid): ===

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
  python tools/sitl/run_tests.py --all  # all types

  Output: PASS/FAIL per scenario + detailed log + metrics
```

### AI5 — Visualization + Simulated Ground Station (~100 lines)
```
tools/sitl/viz.py (NEW):
  Real-time SITL visualization (matplotlib or pygame):

  For wheeled:
    - 2D topdown view of world
    - Robot as oriented triangle
    - Traversed trajectory (line)
    - Rangefinder rays (lines)
    - Waypoints and locations marked
    - Side panel: battery, mode, velocity, heading

  For drone:
    - 2D topdown + side view (altitude)
    - 3D drone position
    - Geofence drawn
    - Wind vector arrow
    - Panel: altitude, GPS, battery, mode

  For humanoid:
    - 2D stick figure (front + side view)
    - ZMP point vs support polygon
    - CoM trajectory
    - Joint angles as bars

  Usage:
    python tools/sitl/sitl_wheeled.py --world house.yaml --viz
    # Opens matplotlib window with world + robot moving in real-time
```

### Phase AI summary

| Sub-phase | Lines | When | Depends on |
|----------|-------|------|-----------|
| **AI0: SITL Wheeled** | **~200** | **Week 1 (DONE)** | **protocol.py (exists)** |
| AI1: SITL Drone | ~300 | Pre-drone | Protocol + drone physics |
| AI1b: SITL Humanoid (MuJoCo) | ~200 | Pre-humanoid | MuJoCo + URDF |
| AI2: Bridges (Webots/Gazebo) | ~100 | Optional | External simulator installed |
| AI3: HITL bridge | ~100 | Pre-real hardware | AI0/AI1 + kernel |
| AI4: Test scenarios | ~150 | With AI0 | AI0 |
| AI5: Visualization | ~100 | With AI0 | AI0 + matplotlib |
| **Total** | **~1150** | | |

### External tools (don't implement, only integrate if needed):

| Simulator | For what | When | Installation |
|-----------|----------|------|-------------|
| **ir-sim** | Fast 2D navigation, multi-robot | Optional | `pip install ir-sim` |
| **Webots** | Realistic 3D, rendered camera, LiDAR | Pre-drone/field | Free download |
| **Gazebo** | Industrial 3D, ROS2, fleets | Phase AE (fleet) | With ROS2 |
| **MuJoCo** | Humanoids, RL training, contacts | Phases AO-AU | `pip install mujoco` |
| **NVIDIA Isaac** | GPU-accelerated, digital twins | Optional advanced | Requires NVIDIA GPU |

---

## Phase AJ — 3D Path Planning + Obstacle Avoidance

**Problem**: "if obstacle → stop" doesn't work for drones. Drone must
plan 3D routes around obstacles (cables, trees, buildings).

**Reference**: PX4-Avoidance (VFH+*, octomap), Skydio (6 cameras, continuous 3D map).

### AJ1 — 3D Occupancy grid (robot-brain or kernel, ~200 lines)
```
nav/occupancy.py (robot-brain) or crates/nav/src/occupancy.rs (kernel):
  3D map of environment as grid of occupied/free cells.

  class OccupancyGrid3D:
    resolution: float = 0.5  # meters per cell
    size: [100, 100, 20]     # 100×100×20 cells = 50×50×10 meters
    data: bitarray            # 1 bit per cell = 25 KB

    def update_from_depth(camera_pose, depth_image):
        # Raycast from camera, mark cells as occupied/free
        for pixel in depth_image:
            point_3d = deproject(pixel, depth)
            world_point = camera_pose * point_3d
            cell = world_to_cell(world_point)
            data[cell] = OCCUPIED

    def update_from_rangefinder(position, direction, distance):
        # Single ray update (simpler)

    def is_free(x, y, z) -> bool
    def is_path_clear(start, end) -> bool  # ray check
```

### AJ2 — 3D Path planner (robot-brain, ~200 lines)
```
nav/planner_3d.py (NEW):
  Planning algorithms:

  A* 3D:
    - Grid-based, optimal, slow in large grids
    - Good for global planning (waypoint A → B)

  RRT* (Rapidly-exploring Random Tree):
    - Sampling-based, fast in large spaces
    - Good for environments with many obstacles
    - Probabilistically optimal

  VFH+ (Vector Field Histogram):
    - Local planner, fast
    - Good for reactively avoiding obstacles
    - Generates "best direction" based on polar histogram of obstacles

  Dual architecture (like PX4):
    - Global planner (A*/RRT*): route from A to B avoiding known obstacles
    - Local planner (VFH+): reactive adjustment for new obstacles
    - Global replan if local planner gets stuck
```

### AJ3 — Depth perception (robot-brain, ~100 lines)
```
perception/depth.py (NEW):
  Obtain distance to obstacles:

  Option 1 — Stereo cameras:
    - Two separated cameras → disparity → depth map
    - Computationally heavy, no extra hardware needed

  Option 2 — Monocular depth estimation (VLM/NN):
    - One camera → neural network estimates depth
    - Models: MiDaS, Depth Anything V2 (open source)
    - Less precise but works with 1 camera
    - Can run in LM Studio/macOS

  Option 3 — LiDAR/ToF sensor:
    - Extra hardware ($50-200)
    - More precise and fast
    - Intel RealSense D435i popular on drones

  Output: depth_map[H×W] → feeds occupancy grid (AJ1)
```

### Phase AJ summary

| Sub-phase | Lines | Depends on |
|----------|--------|-----------|
| AJ1: Occupancy grid 3D | ~200 | Depth sensors |
| AJ2: Path planner 3D (A*/RRT*/VFH+) | ~200 | AJ1 |
| AJ3: Depth perception | ~100 | Camera (T1) or LiDAR |
| **Total** | **~500** | |

---

## Phase AK — Motor Mixing + Wind Compensation

**Problem**: our `ActuatorCmd channels[4]` goes directly to ESC.
Real flight system needs mixing layer that converts
attitude commands (roll/pitch/yaw/throttle) to PWM per motor,
and compensates disturbances like wind.

### AK1 — Motor mixer (kernel, ~150 lines)
```
crates/flight/src/mixer.rs (NEW or expand existing):
  Converts attitude commands → thrust per motor.

  Supported configurations:
    QUAD_X:     [+,+,-,+], [-,+,+,+], [+,-,+,+], [-,-,-,+]
    QUAD_PLUS:  [0,+,-,+], [-,0,+,+], [0,-,+,+], [+,0,-,+]
    HEX_X:      6 motors
    OCTO_X:     8 motors

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
      // Desaturation: if any motor saturates, reduce all proportionally
      desaturate(&mut output);
      output
  }

  Motor failure compensation:
    pub fn mix_with_failure(cmd, layout, failed_motors: u8) -> [i16; MAX_MOTORS] {
        // Recalculate mixing matrix without failed motors
        // Reduce maneuverability but maintain flight
        // If not possible (>1 motor failed in quad) → safety failsafe
```

### AK2 — Attitude PID controller (kernel, ~200 lines)
```
crates/flight/src/attitude.rs (NEW or expand existing):
  3-axis PID running on RT task (>200 Hz).

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

### AK3 — Wind estimation + feedforward (kernel, ~150 lines)
```
crates/flight/src/wind.rs (NEW):
  Estimates wind from drone behavior:

  Principle: if drone is hovering (velocity=0) but tilted,
  tilt compensates wind. Wind ≈ f(tilt, throttle, mass).

  pub struct WindEstimator {
      wind_ned: [f32; 3],  // current estimate [N, E, D] m/s
      alpha: f32,          // exponential filter (0.01-0.1)
  }

  impl WindEstimator {
      pub fn update(&mut self, accel_body: [f32;3], attitude: Quaternion,
                    velocity_ned: [f32;3], throttle: f32) {
          // Expected accel (no wind) = rotate(attitude, [0,0,-throttle_to_accel])
          // Measured accel = actual accel
          // Difference = wind force / mass
          let expected = rotate(attitude, accel_from_throttle(throttle));
          let residual = accel_body - expected;
          let wind_accel = rotate_to_ned(attitude, residual);
          // Integrate to get wind velocity
          self.wind_ned = lerp(self.wind_ned, integrate(wind_accel), self.alpha);
      }

      pub fn feedforward(&self, attitude: Quaternion) -> MixerInput {
          // Tilt drone slightly against estimated wind
          // Reduces drift before PID needs to correct
      }
  }
```

### Phase AK summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AK1: Motor mixer | ~150 | ESC driver (already exists) |
| AK2: Attitude PID | ~200 | EKF (AH1) |
| AK3: Wind estimation | ~150 | AH1 + AK2 |
| **Total** | **~500** | |

---

## Phase AL — Terrain Following + Smart RTH

### AL1 — Terrain following (kernel + brain, ~100 lines)
```
Maintain altitude above terrain (not above sea/takeoff point).

crates/flight/src/terrain.rs (NEW):
  pub fn terrain_follow_throttle(
      target_agl_m: f32,      // desired altitude above ground
      sonar_distance_m: f32,  // downward sonar reading
      baro_altitude_m: f32,   // barometric altitude
      current_throttle: f32,
  ) -> f32 {
      // PID on (sonar_distance - target_agl)
      // Fallback to baro if sonar out of range (>10m)
  }

  Usage: agriculture (uniform spray on terrain with slope)
```

### AL2 — Smart RTH (robot-brain, ~150 lines)
```
Return-to-Home that doesn't crash into buildings.

planner/rth.py (NEW):
  def plan_rth(current_pos, home_pos, occupancy_grid, geofence, battery) -> list[Waypoint]:
      # 1. Climb to safe altitude (configurable, or max obstacle + 10m)
      # 2. Check direct route: clear of obstacles?
      #    Yes → direct route
      #    No → A* 3D around obstacles
      # 3. Check geofence: does route cross prohibited zone?
      #    Yes → route around prohibited zone
      # 4. Check battery: enough for this route?
      #    No → land at nearest safe point
      # 5. Descend over home → land

  Simple alternative (no occupancy grid):
      # 1. Climb to safe_altitude
      # 2. Fly straight line to home
      # 3. Descend
      # Works if no tall obstacles between here and home
```

### Phase AL summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AL1: Terrain following | ~100 | Sonar/LiDAR + alt PID |
| AL2: Smart RTH | ~150 | AJ (path planning) |
| **Total** | **~250** | |

---

## Phase AM — SLAM + Visual Odometry

**Problem**: indoor (no GPS) or when GPS insufficiently precise.
SLAM builds map while navigating. Visual Odometry estimates
movement from camera frame changes.

### AM1 — Basic Visual Odometry (robot-brain, ~200 lines)
```
perception/visual_odom.py (NEW):
  Estimates relative movement between 2 consecutive frames.

  Flow:
    1. Detect features (ORB, FAST, or Harris corners)
    2. Match features between frame N and N+1
    3. Estimate Essential matrix (5-point algorithm)
    4. Decompose into rotation + translation
    5. Scale with IMU/rangefinder (monocular VO has no scale)

  Output: delta_pose (dx, dy, dz, droll, dpitch, dyaw) per frame
  Fed to EKF as additional measurement (complements GPS)

  Note: VO is computationally heavy. Options:
    - Run on macOS (robot-brain) with received frames → latency
    - Run onboard with local camera → better but needs compute
    - Use VLM for approximate VO ("moved ~1m forward") → slow, imprecise
```

### AM2 — Graph-based SLAM (robot-brain, ~300 lines future)
```
More advanced than VO: builds map + optimizes past positions.

perception/slam.py (NEW, future):
  Graph SLAM:
    - Nodes = robot poses at different times
    - Edges = odometry between poses + loop closures
    - Optimization: minimize total error (g2o, GTSAM, or custom)
    - Output: 2D/3D map + corrected trajectory

  For our case:
    - Indoor: SLAM replaces GPS
    - Outdoor: SLAM complements GPS (more precise in dense environments)
    - Persistent map: saves map → reloads in next mission

  Note: full SLAM is a project itself. Alternative:
    - Use NVIDIA Isaac ROS Visual SLAM if have Jetson
    - Use ORB-SLAM3 (open source, C++)
    - Or stick with VO (AM1) + GPS as first version
```

### Phase AM summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AM1: Visual Odometry | ~200 | Camera + EKF (AH1) |
| AM2: Graph SLAM | ~300 | AM1 (future advanced) |
| **Total** | **~500** | |

---

## Phase AN — Testing Framework + CI

**Problem**: 2 manual test files don't scale. PX4 has thousands of tests.
Tesla does continuous regression testing. Need automated tests.

### AN1 — Unit test suite (robot-brain, ~200 lines)
```
tests/ (expand):
  test_ekf.py          — EKF converges with synthetic data
  test_mixer.py        — correct motor mixing for each layout
  test_geofence.py     — point-in-polygon, buffer zones
  test_mission.py      — boustrophedon generates correct waypoints
  test_safety.py       — each FailsafeEvent generates correct actions per type
  test_wind.py         — wind estimator converges with simulated wind
  test_skills.py       — each skill executes correctly
  test_notifications.py — pushover/telegram mock send

  Run: pytest tests/ -v
```

### AN2 — SITL integration tests (~100 lines)
```
tests/integration/ (NEW):
  test_hover.py:
    - Launch SITL + brain
    - Send TAKEOFF → verify stable altitude in 10s
    - Send HOVER 30s → verify drift < 1m
    - Send LAND → verify touches ground

  test_mission.py:
    - Load mission with 5 waypoints
    - Execute in SITL
    - Verify all waypoints reached

  test_failsafe.py:
    - Simulate link loss → verify RTH
    - Simulate battery low → verify landing
    - Simulate motor failure → verify controlled descent

  Run: pytest tests/integration/ --sitl
```

### AN3 — Chaos testing / fault injection (~80 lines)
```
tools/chaos/ (NEW):
  Inject random faults during SITL to test robustness:

  chaos_runner.py:
    Injectable faults:
      - GPS dropout (5-30s)
      - IMU spike (absurd values for 1 frame)
      - IMU frozen (same reading repeated)
      - Baro drift (+50m in 10s)
      - Motor degradation (80% thrust on 1 motor)
      - Link loss (5-60s)
      - Wind gust (sudden 10 m/s)
      - Camera black frame

    python tools/chaos/chaos_runner.py --duration 300 --fault-rate 0.1
    → runs 5 min SITL with random faults every ~10s
    → reports: crashes, geofence violations, safety triggers, recovery time
```

### AN4 — CI pipeline (config, ~30 lines)
```
.github/workflows/test.yml:
  - pytest tests/                    # unit tests
  - cargo build (all feature combos) # kernel builds
  - pytest tests/integration/ --sitl # SITL tests
  - python tools/chaos/chaos_runner.py --duration 60  # quick chaos
  - Coverage report

Triggers: on push, on PR, nightly (extended chaos + all scenarios)
```

### Phase AN summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AN1: Unit test suite | ~200 | Nothing |
| AN2: SITL integration tests | ~100 | AI (SITL) |
| AN3: Chaos testing | ~80 | AI (SITL) |
| AN4: CI pipeline | ~30 | AN1 + AN2 |
| **Total** | **~410** | |

---

## ═══════════════════════════════════════════════════
## HUMANOID-SPECIFIC PHASES
## ═══════════════════════════════════════════════════

These phases are specific to bipedal humanoid robots. Don't apply to
drones or wheeled robots. Implemented when humanoid hardware available.
Fundamental problems: walking without falling, manipulating objects,
operating safely near people.

---

## Phase AO — Balance + ZMP (Zero Moment Point)

**CRITICAL problem for humanoids**: without active balance, robot falls.
Equivalent to drone's EKF+PID — without this, nothing works.

**Reference**: ZMP is industrial standard. Tesla Optimus, Honda ASIMO,
Boston Dynamics Atlas all use ZMP variants + whole-body control.

### AO1 — ZMP calculator + CoM tracker (kernel, ~300 lines)
```
crates/humanoid/src/zmp.rs (NEW):
  ZMP is the point where inertia+gravity forces don't generate
  rotation moment. If ZMP leaves support polygon (feet),
  robot falls.

  pub struct BalanceState {
      com_position: [i32; 3],     // center of mass (mm)
      com_velocity: [i32; 3],     // CoM velocity (mm/s)
      zmp: [i32; 2],              // Zero Moment Point (mm, XY plane)
      support_polygon: Polygon,   // convex hull of feet in contact
      is_stable: bool,            // zmp within polygon?
      stability_margin: i32,      // distance ZMP to edge (mm)
  }

  pub fn compute_zmp(
      com: [i32; 3],
      com_accel: [i32; 3],       // from IMU/EKF
      foot_forces: [i32; 2],     // force on each foot (sensors)
  ) -> [i32; 2] {
      // ZMP_x = CoM_x - (CoM_z * CoM_accel_x) / (g + CoM_accel_z)
      // ZMP_y = CoM_y - (CoM_z * CoM_accel_y) / (g + CoM_accel_z)
      // Integer arithmetic: scale to avoid overflow
  }

  pub fn is_stable(zmp: [i32;2], support: &Polygon) -> bool {
      point_in_convex_hull(zmp, support)
  }

  pub fn stability_margin(zmp: [i32;2], support: &Polygon) -> i32 {
      // Minimum distance ZMP to polygon edge
      // Positive = stable, negative = falling
  }

  Runs onboard at 200+ Hz. Feeds balance controller.
```

### AO2 — Balance controller (kernel, ~250 lines)
```
crates/humanoid/src/balance.rs (NEW):
  Controller that keeps ZMP within support polygon.

  pub struct BalanceController {
      pid_roll: PIDController,    // lateral tilt
      pid_pitch: PIDController,   // frontal tilt
      ankle_strategy: bool,       // correction via ankles (small perturbations)
      hip_strategy: bool,         // correction via hips (medium perturbations)
      step_strategy: bool,        // extra step (large perturbations)
  }

  Three balance strategies (like humans):
    1. Ankle strategy:  small perturbation (<3cm ZMP error)
       → adjust ankle angle to move ZMP
       → fast, subtle, doesn't require moving feet

    2. Hip strategy:    medium perturbation (3-8cm)
       → move hip/torso to reposition CoM
       → slower, more visible

    3. Stepping strategy: large perturbation (>8cm or ZMP outside polygon)
       → take step in direction of fall
       → slowest, but saves from large falls
       → requires replanning footstep

  Push recovery:
    - Detect push: sudden lateral acceleration on IMU
    - Classify magnitude → choose strategy
    - Execute correction in <100ms

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

### AO3 — Foot force sensors (kernel, ~80 lines)
```
crates/humanoid/src/foot_sensor.rs (NEW):
  Pressure sensors on each foot — needed to know:
  - Which foot on ground? (stance vs swing)
  - Where is center of pressure? (for ZMP)
  - How much force on each foot? (detect uneven terrain)

  Typical hardware: 4 load cells per foot (corners)
  Interface: ADC via I2C or SPI

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

### Phase AO summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AO1: ZMP calculator + CoM tracker | ~300 | IMU/EKF (AH1) |
| AO2: Balance controller | ~250 | AO1 |
| AO3: Foot force sensors | ~80 | I2C/ADC (already exists) |
| **Total** | **~630** | |

---

## Phase AP — Gait Generation (how to walk)

**Problem**: walking is coordinated sequence of 20+ joints in
alternating phases (stance/swing). Not trivial — humanity took millions
years to evolve bipedalism.

### AP1 — CPG (Central Pattern Generator) gait (kernel, ~200 lines)
```
crates/humanoid/src/cpg.rs (NEW):
  Generates rhythmic walking pattern using coupled oscillators.

  Classical approach (deterministic, needs no training):

  pub struct CPG {
      phase: f32,              // current cycle phase (0-2π)
      frequency: f32,          // step frequency (Hz, typical 1-2)
      amplitude: [f32; N_JOINTS],  // oscillation amplitude per joint
      offset: [f32; N_JOINTS],     // center position per joint
      coupling: [[f32; N_JOINTS]; N_JOINTS],  // coupling between joints
  }

  impl CPG {
      pub fn step(&mut self, dt: f32) -> [i16; N_JOINTS] {
          self.phase += 2.0 * PI * self.frequency * dt;

          let mut angles = [0i16; N_JOINTS];
          for j in 0..N_JOINTS {
              // Each joint oscillates sinusoidally
              // with phase offset relative to others
              let joint_phase = self.phase + self.phase_offset[j];
              let angle = self.offset[j] + self.amplitude[j] * sin(joint_phase);
              angles[j] = (angle * 100.0) as i16;  // centidegrees
          }
          angles
      }
  }

  Walking cycle phases:
    0%   - 50%:  left leg stance, right leg swing
    50%  - 100%: right leg stance, left leg swing

  Minimum joints for walking (12-DOF):
    Per leg (6): hip_yaw, hip_roll, hip_pitch, knee, ankle_pitch, ankle_roll

  Tunable parameters:
    step_length_mm, step_height_mm, step_frequency_hz,
    lateral_sway_mm, torso_pitch_offset_deg
```

### AP2 — Footstep planner (robot-brain, ~150 lines)
```
planner/footstep.py (NEW):
  Decide WHERE to place each foot (not just joint trajectory).

  class FootstepPlanner:
    def plan_steps(current_feet, target_position, obstacles) -> list[Footstep]:
        # Generate footstep sequence from current position to destination
        # Each step: position(x,y), orientation(yaw), foot(L/R)
        # Avoids obstacles, respects max step_length
        # Supports: straight walk, turn, lateral walk, climb step

    class Footstep:
        x_mm: int
        y_mm: int
        z_mm: int          # for stairs
        yaw_cdeg: int
        foot: Foot          # LEFT | RIGHT
        step_type: StepType # NORMAL | TURN | LATERAL | STAIR_UP | STAIR_DOWN

  Scenarios:
    Straight walk:  alternating steps, same direction
    Turn:           pivot on one foot, short arc steps
    Stairs:         detect step (sonar/VLM), adjust step_height
    Uneven terrain: VLM identifies terrain → adjust parameters
```

### AP3 — RL-based gait (modern alternative, robot-brain, ~200 lines)
```
policy/humanoid_rl.py (NEW):
  Alternative to CPG: train neural network in MuJoCo and transfer to robot.

  Approach:
    1. Define robot in MuJoCo (URDF/MJCF)
    2. Train with PPO/SAC: reward = forward velocity + fall penalty
    3. Export policy as ONNX or simple weights
    4. Run on real robot (sim-to-real transfer)

  Advantages over CPG:
    - Learns more natural and efficient gaits
    - Adapts to uneven terrain automatically
    - Can learn get-up, running, jumping

  Disadvantages:
    - Needs training (hours/days of GPU)
    - Sim-to-real gap can be large
    - Less interpretable than CPG

  Reference: Open X-Humanoid, MEVITA, rl_sar

  Implementation:
    - Train on macOS (GPU or MPS)
    - Export policy network (~50KB of weights)
    - Load in kernel as RMLP (already have model_load_bytes)
    - Run at 100 Hz: observation → policy → joint angles

  Observation vector (input):
    - IMU: roll, pitch, yaw, gyro × 3
    - Current joint angles: N joints
    - Joint velocities: N joints
    - Foot contact: L, R
    - Command: desired velocity (vx, vy, vyaw)

  Action vector (output):
    - Target joint angles: N joints (PD controller applies)
```

### Phase AP summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AP1: CPG gait generator | ~200 | AO (balance) |
| AP2: Footstep planner | ~150 | AO + VLM (optional) |
| AP3: RL-based gait (alternative) | ~200 | MuJoCo training |
| **Total** | **~550** | |

---

## Phase AQ — Inverse Kinematics + Manipulation

**Problem**: humanoid needs to grasp things, open doors, manipulate
objects. For that needs IK (Inverse Kinematics): given desired hand position,
calculate angles for each arm joint.

### AQ1 — IK solver (kernel or brain, ~250 lines)
```
Analytic (fast, exact for known chains):
  crates/humanoid/src/ik.rs (kernel, for RT) or
  policy/ik_solver.py (brain, for planning)

  pub struct ArmChain {
      // Typical 7-DOF: shoulder(3) + elbow(1) + wrist(3)
      dh_params: [DHParam; 7],  // Denavit-Hartenberg
      joint_limits: [(i16, i16); 7],  // min/max per joint
  }

  pub fn solve_ik(
      chain: &ArmChain,
      target_pos: [i32; 3],     // desired hand position (mm)
      target_rot: Quaternion,    // desired hand orientation
      current_angles: [i16; 7],  // current angles (seed)
  ) -> Option<[i16; 7]> {
      // Method: Iterative Jacobian transpose
      // Or: closed-form analytic for 6-DOF arms
      // Or: CycleIK (neural, faster for planning)

      // Iterative:
      for _ in 0..MAX_ITERATIONS {
          let current_pos = forward_kinematics(chain, angles);
          let error = target_pos - current_pos;
          if norm(error) < TOLERANCE { return Some(angles); }
          let jacobian = compute_jacobian(chain, angles);
          let delta = jacobian_transpose(jacobian, error);
          angles += delta;
          clamp_to_limits(&mut angles, chain.joint_limits);
      }
      None  // didn't converge
  }

  pub fn forward_kinematics(chain: &ArmChain, angles: &[i16]) -> [i32; 3] {
      // DH transform chain → end effector position
  }

  Self-collision check:
    pub fn check_self_collision(all_joints: &FullBodyState) -> bool {
        // Verify arm doesn't collide with torso, other arm, or legs
        // Simplified: bounding spheres per link segment
    }
```

### AQ2 — Grasp planning (robot-brain, ~200 lines)
```
policy/grasp.py (NEW):
  Complete manipulation pipeline:

  class GraspPlanner:
    def plan_grasp(object_detection, depth_map) -> GraspPlan:
        # 1. Detect object (VLM: "red cup on table")
        # 2. Estimate 6DOF pose of object (position + orientation)
        # 3. Choose grasp type:
        #    - Power grasp (large objects: bottles, boxes)
        #    - Precision grasp (small objects: pens, coins)
        #    - Hook grasp (handles, door handles)
        # 4. Calculate approach vector (from where to approach hand)
        # 5. Pre-grasp pose → reach → grasp → lift → verify

    class GraspPlan:
        pre_grasp_pose: Pose6D    # hand open, near object
        grasp_pose: Pose6D        # hand in grasp position
        grasp_type: GraspType     # power | precision | hook
        force_target: int          # grasp force (mN)
        post_grasp: Pose6D        # lift after grasping

  Verification:
    - After closing hand → check force feedback
    - If force < threshold → didn't grasp → retry
    - If force > max → too much pressure → release

  Manipulation skills:
    GRAB(object, hand)    → detect + plan + reach + grasp + lift
    PLACE(location)       → navigate + lower + release + retract
    HANDOVER(to_person)   → extend arm + wait for pull + release
    OPEN_DOOR(handle)     → reach handle + grasp + rotate + push/pull
    PUSH_BUTTON(button)   → extend finger + contact + press + retract
    POUR(container, target) → grab + tilt + pour + un-tilt + place
```

### AQ3 — Hand controller (kernel, ~100 lines)
```
crates/humanoid/src/hand.rs (NEW):
  Finger control — from simple (1-DOF gripper) to complex (22-DOF Tesla).

  Complexity levels:
    Level 1 — Simple gripper (1-DOF):
      pub fn gripper_open() / gripper_close()
      → 1 servo, open/close, enough to grasp simple objects

    Level 2 — 5-DOF hand (1 servo per finger):
      pub fn hand_set_fingers(thumb, index, middle, ring, pinky: i16)
      → power grasp, basic precision grasp

    Level 3 — Dexterous hand (11-22 DOF, like Tesla Optimus):
      pub fn hand_set_joints(joints: &[i16; N_FINGER_JOINTS])
      → tendon-driven, force feedback per finger
      → fine manipulation (eggs, screws, fabrics)

  Force feedback:
    pub fn hand_force(finger: u8) -> i32   // mN per finger
    pub fn hand_contact(finger: u8) -> bool // contact detected
```

### Phase AQ summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AQ1: IK solver | ~250 | Chain definition (URDF/config) |
| AQ2: Grasp planning | ~200 | VLM + depth + AQ1 |
| AQ3: Hand controller | ~100 | Servo/motor driver |
| **Total** | **~550** | |

---

## Phase AR — Whole-Body Control (WBC)

**Problem**: humanoid doesn't move legs and arms independently.
Walking while carrying something changes CoM. Crouching requires coordinating
torso + legs + arms. WBC coordinates ENTIRE body as system.

### AR1 — Whole-body coordinator (kernel, ~300 lines)
```
crates/humanoid/src/wbc.rs (NEW):
  Control priorities (stack-of-tasks):

  pub struct WholeBodyController {
      tasks: [Task; MAX_TASKS],  // ordered by priority
  }

  Priorities (highest to lowest):
    1. Balance (ZMP within support)     ← NEVER violated
    2. Self-collision avoidance         ← NEVER violated
    3. Joint limits                     ← NEVER violated
    4. Feet contact (stance foot fixed) ← during walking
    5. End-effector position (hand where we want)
    6. Body orientation (torso vertical)
    7. Comfort posture (natural position)

  Solver:
    Each tick (~200 Hz):
    1. Compute Jacobians for all active tasks
    2. Project lower-priority tasks into null-space of higher ones
    3. Solve for joint velocities → integrate → joint angles
    4. Check limits → clamp

  Example: "grasp glass from table while walking"
    - Task 1 (balance): keeps ZMP stable
    - Task 4 (feet): follows footstep plan
    - Task 5 (hand): hand moves toward glass
    - Task 6 (torso): torso compensates for extended arm weight
    → WBC solves everything simultaneously without conflicts

  Simple alternative (no null-space):
    - PD controller per joint
    - Priorities as override: if balance needs ankle adjustment,
      override gait generator target
    - Less elegant but functional for simple robots (<20 DOF)
```

### AR2 — CoM compensator (kernel, ~100 lines)
```
crates/humanoid/src/com_compensator.rs (NEW):
  When robot carries something or extends arm, CoM shifts.
  Compensator adjusts torso posture to keep CoM over feet.

  pub fn compensate_com(
      current_com: [i32; 3],
      support_center: [i32; 2],
      payload_mass_g: i32,
      payload_pos: [i32; 3],
  ) -> TorsoCorrection {
      // Calculate new CoM including payload
      // Tilt torso opposite direction to center CoM
      // Limit tilt to ±15° (beyond is unstable)
  }

  Cases:
    - Load on one hand → torso tilts opposite side
    - Load on head → torso straight, knees slightly flexed
    - Pushing door → torso forward, feet back
```

### Phase AR summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AR1: Whole-body coordinator | ~300 | AO + AP + AQ |
| AR2: CoM compensator | ~100 | AO1 (ZMP) |
| **Total** | **~400** | |

---

## Phase AS — Fall Detection + Recovery

**Problem**: humanoids fall. Unlike drones (destroyed)
or wheels (not applicable), humanoid can get up. Needs:
detect fall → protect → get up.

### AS1 — Fall detector (kernel, ~80 lines)
```
crates/humanoid/src/fall.rs (NEW):
  Detect robot falling BEFORE touching ground.

  pub enum FallState {
      Stable,           // ZMP within support
      Tipping,          // ZMP at edge, recoverable
      Falling(FallDir), // irrecoverable, prepare impact
      OnGround(Pose),   // already on ground (face_down, face_up, side)
  }

  pub fn detect_fall(balance: &BalanceState, imu: &ImuData) -> FallState {
      // 1. ZMP check: if margin < 0 and velocity high → Falling
      // 2. Tilt check: if roll or pitch > max_tilt → Falling
      // 3. Free-fall check: if accel ≈ 0 (free fall) → Falling
      // 4. Ground check: if foot sensors = 0 and not swing phase → Falling

      let fall_direction = if pitch > 0 { Forward }
                          else if pitch < 0 { Backward }
                          else if roll > 0 { Right }
                          else { Left };
  }

  Latency: <10ms from fall start to detection.
  Balance controller has ~200ms to attempt recovery (stepping).
  If not → switch to break-fall.
```

### AS2 — Break-fall + protective pose (kernel, ~100 lines)
```
crates/humanoid/src/breakfall.rs (NEW):
  When fall inevitable, minimize damage.

  pub fn break_fall(direction: FallDir) -> [i16; N_JOINTS] {
      match direction {
          Forward => {
              // Arms forward, elbows slightly flexed
              // Head turned to side (protect face)
              // Knees flexed (dampen)
              POSE_BREAKFALL_FORWARD
          }
          Backward => {
              // Chin to chest (protect back of neck)
              // Arms at sides, palms down
              // Knees flexed
              POSE_BREAKFALL_BACKWARD
          }
          Left | Right => {
              // Arm on fall side extended (roll)
              // Other arm protects torso
              // Legs together, slightly flexed
              POSE_BREAKFALL_SIDE
          }
      }
  }

  Timing:
    - Detect fall: t=0
    - Move to break-fall pose: t=0 to t=200ms (as fast as possible)
    - Impact: t=200-500ms (depends on height)
    - After impact: evaluate damage (joint currents, IMU)
```

### AS3 — Get-up sequence (kernel + brain, ~150 lines)
```
crates/humanoid/src/getup.rs (NEW):
  Sequences to get up from ground.

  pub fn get_up(pose: GroundPose) -> Vec<[i16; N_JOINTS]> {
      match pose {
          FaceDown => {
              // 1. Push-up with arms
              // 2. Bring knees under body
              // 3. Quadrupedal position
              // 4. One foot forward (lunge)
              // 5. Push up to standing
              GET_UP_FACE_DOWN  // sequence of keyframes
          }
          FaceUp => {
              // 1. Roll to one side
              // 2. Side push-up
              // 3. → FaceDown → previous sequence
              // Or: sit-up → crouch → stand
              GET_UP_FACE_UP
          }
          Side => {
              // 1. Lower arm pushes
              // 2. Roll to FaceDown
              // 3. → FaceDown sequence
              GET_UP_SIDE
          }
      }
  }

  Each sequence is series of interpolated keyframes.
  Can be refined with RL (train get-up in MuJoCo).
  Post-getup: check balance → if stable → resume normal operation.

  RL alternative:
    Like gait (Phase AP3), train get-up policy in simulation.
    More robust than keyframes, adapts to uneven terrain.
```

### Phase AS summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AS1: Fall detector | ~80 | AO (balance) + IMU |
| AS2: Break-fall + protective pose | ~100 | AS1 |
| AS3: Get-up sequence | ~150 | AS1 + AP (gait) |
| **Total** | **~330** | |

---

## Phase AT — Force/Torque Sensing + Compliance Control

**Problem**: humanoid touches things and people. Needs to know how much
force applies and be "soft" when interacting. Without this, breaks things
or hurts people.

**Reference**: ISO 13482 (service robots), ISO 15066 (collaborative robots).

### AT1 — Joint torque sensing + monitoring (kernel, ~120 lines)
```
crates/humanoid/src/torque.rs (NEW):
  Read force/torque on each joint.

  Methods:
    1. Current-based: measure motor current → estimate torque
       Cheap, imprecise, enough for collision detection.
       torque ≈ motor_current × torque_constant

    2. Strain gauge: dedicated sensor on each joint
       Precise, expensive. For hands and frequently-contact joints.

    3. Series elastic actuator (SEA): spring in joint
       Measure spring deflection → torque. Used on Atlas, Optimus.

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

### AT2 — Impedance controller (kernel, ~150 lines)
```
crates/humanoid/src/impedance.rs (NEW):
  Instead of rigid position control (go to angle X exactly),
  impedance control: behave like spring-damper.

  If something pushes against robot's arm, arm YIELDS
  instead of maintaining position at all costs.

  pub struct ImpedanceParams {
      stiffness: i32,   // K — how "hard" (N/m)
      damping: i32,     // D — how fast dampens
      inertia: i32,     // M — virtual mass
  }

  pub fn impedance_control(
      target_pos: i32,
      current_pos: i32,
      current_vel: i32,
      external_force: i32,
      params: &ImpedanceParams,
  ) -> i32 {  // torque command
      // F = M*accel + D*vel + K*(pos - target) - F_ext
      // Robot behaves like spring between target and actual
      // If external_force pushes → robot yields proportional to 1/K
      params.stiffness * (target_pos - current_pos)
      - params.damping * current_vel
      - external_force / params.inertia
  }

  Modes:
    RIGID:   K=high, D=high   → for precision tasks (screws)
    SOFT:    K=low, D=medium  → for human interaction (hand over object)
    FREE:    K=0, D=low       → arm moves freely (teleoperation)
    LOCKED:  K=max, D=max     → joint locked (safety)
```

### AT3 — Human proximity safety (robot-brain + kernel, ~100 lines)
```
safety/human_proximity.py (robot-brain):
  ISO 13482 compliance: reduce velocity and force near people.

  class HumanSafetyMonitor:
    def check(person_distance_mm, robot_speed, joint_torques) -> SafetyAction:
        if person_distance < 300:    # imminent contact
            return STOP_AND_COMPLY   # impedance mode SOFT on all joints
        if person_distance < 1000:   # close zone
            return SLOW_DOWN(max_speed=20)  # reduce to 20%
        if person_distance < 2000:   # awareness zone
            return REDUCE_FORCE(max_torque=50)  # limit force
        return NORMAL

  Person detection:
    - VLM identifies people in image
    - Depth estimation gives distance
    - Alternative: dedicated LiDAR/sonar

  Kernel side:
    - If SafetyAction != NORMAL → limit velocity and torque in RT
    - Override any command exceeding limits
    - Integrated with Safety FSM (AG4: humanoid safety)

  ISO 13482 limits (example):
    - Max force on transient contact: 150N (chest), 65N (face)
    - Max pressure: 210 N/cm² (transient)
    - These values configured in config.yaml
```

### Phase AT summary

| Sub-phase | Lines | Depends on |
|----------|-------|-----------|
| AT1: Joint torque sensing | ~120 | Motor drivers |
| AT2: Impedance controller | ~150 | AT1 |
| AT3: Human proximity safety | ~100 | VLM + AT2 + AG4 |
| **Total** | **~370** | |

---

## Phase AU — Humanoid Simulation (MuJoCo)

**Problem**: test balance, gait and manipulation without breaking hardware.
MuJoCo is standard for humanoid simulation (used by Tesla,
DeepMind, OpenAI, Berkeley). Free since 2022 (acquired by Google).

### AU1 — Humanoid SITL with MuJoCo (tools, ~200 lines)
```
tools/sitl/humanoid_sim.py (NEW):
  Extends SITL framework (Phase AI) with MuJoCo simulation.

  Flow:
    1. Load humanoid MJCF/URDF model
    2. Connect to robot-brain via TCP protocol (like drone SITL)
    3. Loop:
       - Receive ActuatorCmd (joint angles)
       - Apply to MuJoCo model
       - Step physics (timestep=2ms)
       - Read simulated sensors (IMU, foot force, joint torque, camera)
       - Send SensorPacket to brain
    4. Render visual (optional, for debug)

  Advantages of MuJoCo over custom physics:
    - Realistic contact (soft contact, friction)
    - Tendons and actuators modeled
    - Numerically stable even with complex contacts
    - Used across industry → validated

  Humanoid test scenarios:
    - Stand still → verify stable balance
    - Walk forward 5m → verify doesn't fall
    - Push recovery → lateral push 50N × 0.1s
    - Pick up object → reach + grasp + lift
    - Stairs → climb 3 steps
    - Fall and get up → strong push → fall → get up
    - Human proximity → person approaches → robot reduces speed
```

### AU2 — RL training pipeline (tools, ~150 lines)
```
tools/training/humanoid_rl.py (NEW):
  Pipeline to train gait and manipulation with RL in MuJoCo.

  Flow:
    1. Define reward function (velocity + stability - energy - falls)
    2. Train with PPO (Proximal Policy Optimization) or SAC
    3. Evaluate on test scenarios
    4. Export policy weights → RMLP format or ONNX
    5. Load in kernel (model_load_bytes, already exists)
    6. Evaluate sim-to-real gap in HITL

  Reward function example (walking):
    reward = +1.0 * forward_velocity
           + -0.1 * energy_consumption
           + -10.0 * fall_penalty
           + +0.5 * upright_bonus
           + -0.01 * action_smoothness_penalty
           + +0.2 * foot_clearance_bonus

  Dependencies: mujoco, gymnasium, stable-baselines3 (o custom PPO)
```

### Phase AU summary

| Sub-phase | Lines | Depends on |
|----------|--------|-----------|
| AU1: MuJoCo SITL | ~200 | AI (SITL framework) + MuJoCo |
| AU2: RL training pipeline | ~150 | AU1 |
| **Total** | **~350** | |

---

## Existing components (already implemented)

### server.py — TCP Server
```python
# Listens for robot connection (VF2)
# Receives: SensorPacket, CameraFrame, Status
# Sends: VelocityCmd, ModeCmd, WaypointCmd

async def handle_robot(reader, writer):
    while True:
        pkt = await protocol.read_packet(reader)

        if pkt.type == SENSOR_PACKET:
            state.update_sensors(pkt)

        elif pkt.type == CAMERA_FRAME:
            # Send to VLM for scene description
            description = await vision.describe(pkt.image)

            # Send to LLM for decision
            action = await planner.decide(
                scene=description,
                sensors=state.sensors,
                task=state.current_task,
                odom=state.odom,
            )

            # Translate decision to motor command
            cmd = policy.to_velocity_cmd(action)

            # Send to robot
            await protocol.send_packet(writer, cmd)
```

### perception/vision.py — VLM Interface (LM Studio)
```python
# Connects to local LM Studio endpoint
# LM Studio runs SmolVLM or other VLM

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

### planner/decide.py — LLM Decider Interface
```python
# Uses LM Studio with an LLM (Llama 3.2, Qwen 2.5, etc.)

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

### policy/actions.py — Action → motor command translation
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
  listen_port: 9000           # TCP port for robot connection
  sensor_rate_hz: 20          # Expected rate of sensor packets
  camera_rate_hz: 2           # Rate of camera frames
  watchdog_timeout_ms: 3000   # If no data received → alert

lmstudio:
  host: "127.0.0.1"
  port: 1234                  # Default LM Studio port
  vlm_model: "smolvlm"        # VLM model for vision
  llm_model: "llama-3.2-3b"   # LLM model for decisions
  timeout_s: 10

tasks:
  default: "patrol"           # Default task on startup
  patrol_waypoints:           # Patrol points
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
  max_speed: 80               # Max speed (% motor)
  min_battery_mv: 6500        # Min battery voltage
  obstacle_stop_mm: 200       # Min distance to stop
```

---

## ═══════════════════════════════════════════════════
## EXECUTION ORDER
## ═══════════════════════════════════════════════════

```
Weeks 1-2: Fundamentals + Abstraction + SIMULATION
├── *** AI0: SITL Wheeled (simulate diff drive robot NOW) ***
├── AI4: Test scenarios wheeled (patrol, obstacle, security, battery)
├── AI5: 2D Visualization (matplotlib, see robot move)
├── P1: Net transport abstraction (VirtIO/MACB/USB-WiFi)
├── R1: Define multi-robot binary protocol (generic ActuatorCmd)
├── Y1+Y3: Generic ActuatorCmd + SensorPacket in protocol.py
├── Y4: Config per-robot-type
├── W4: Crypto (AES/SHA1) — no dependencies, can start now
├── X1: Notifications (pushover/telegram) — no dependencies, pure HTTP
└── robot-brain: scaffold repo + protocol.py ✓ DONE
    ↑ All tested against SITL wheeled. ZERO hardware needed.

Weeks 3-4: WiFi Connectivity + Skills (ALL against SITL)
├── Route A (USB WiFi):
│   ├── W1: USB Core enumeration
│   ├── W2: RTL8188 driver
│   └── W3: WiFi 802.11 stack + W5 integration
├── Route B (ESP32 bridge):
│   ├── W-alt1: ESP32 firmware (UART↔TCP bridge)
│   └── W-alt2: VF2 UART1 protocol
├── (in parallel) Q1: libsys (syscall wrappers)
├── V1: Skill library (universal + per-type) — tested against SITL
├── V2: Mode presets (security, patrol, explore) — tested against SITL
└── V3: Task planner (LLM decomposes free prompts) — tested against SITL

Weeks 5-6: Userspace + Brain integration (SITL + start hardware)
├── Q2: Scheduler improvements (sleep, priority)
├── S1: Implement SYS_SENSOR_READ
├── Q3: Brain client ELF
├── Q4: Reflex daemon ELF
├── V4: Skill runner (state machine + continuous loop) — tested against SITL
├── Y2: Policy translators (wheeled.py first, drone/humanoid stub)
├── robot-brain: server.py + policy/
├── *** Hardware chassis arrives → assemble + test motors from shell ***
└── AI3: HITL bridge (real kernel + simulated sensors for validation)

Weeks 7-8: End-to-end integration (SITL → HITL → Hardware)
├── U1: Net poll task
├── U4: Autorun userspace ELFs
├── P2: DHCP
├── X2: Bidirectional Telegram bot (remote control)
├── X3: HTTP API
├── Y5: Kernel actuator_apply() dispatcher
├── robot-brain: perception/vision.py + planner/decide.py
└── Validation: SITL → HITL → real hardware (same tests, 3 environments)

Weeks 9-10: Testing + Optimization + Hardware validation
├── U2: TCP buffer size
├── U3: Task priority
├── T1: Real CSI capture (VF2)
├── Integrate server.py with mode manager + skill runner
├── Integration testing: SITL scenarios → QEMU → real VF2
└── Compare metrics SITL vs hardware (drift, latency, battery)

Near future (when camera hardware available):
├── T2: JPEG encoder
├── T3: Camera syscall
├── Y2: drone.py / humanoid.py policy (when hardware available)
└── robot-brain: monitor/dashboard.py

Future — Safety + Scalability (when base works end-to-end):
│
│   RULE: simulate BEFORE hardware. Order by type:
│   Drone:     AI1 (SITL drone) → AH-AK → drone scenarios → AG3 → hardware
│   Humanoid:  AI1b (SITL MuJoCo) → AO-AU → humanoid scenarios → AG4 → hardware
│   Vehicle:   AI0 adapted → AA-AB → vehicle scenarios → AG5 → hardware
│
├── AG: Safety profiles per robot type *** BEFORE testing drone/humanoid/vehicle ***
│   └── AG1-AG8 (see detail in Phase AG)
├── AH: EKF State Estimation + Sensor Fusion *** CRITICAL for drones ***
│   ├── AH1: EKF core 15 states (kernel, runs onboard at 200+ Hz)
│   ├── AH2: Sensor calibration (gyro/accel/mag/baro)
│   └── AH3: Sensor redundancy + voting (dual IMU/GPS/baro)
├── AI: SITL/HITL Simulation
│   ├── AI1: SITL physics engine (Python, drone model)
│   ├── AI2: HITL bridge (real kernel + simulated sensors)
│   └── AI3: Test scenarios library (hover, wind, motor failure, RTH)
├── AJ: 3D Path Planning + Obstacle Avoidance
│   ├── AJ1: 3D Occupancy grid
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
│   ├── AM1: Basic Visual Odometry (indoor/GPS-denied)
│   └── AM2: Graph SLAM (future advanced)
├── AN: Testing Framework + CI
│   ├── AN1: Expanded unit test suite
│   ├── AN2: SITL integration tests
│   ├── AN3: Chaos testing / fault injection
│   └── AN4: CI pipeline (GitHub Actions)
├── Z: Multi-link transport (LoRa + RF + 4G)
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
│   └── AA5: Headland turns (tractors)
├── AB: Implement/payload abstraction
│   ├── AB1: Payload cmd (spray, gripper, PTO, spotlight)
│   ├── AB2: Smart spray control (VLM + flow rate)
│   └── AB3: CAN bus driver (J1939/ISOBUS for tractors)
├── AC: Offline autonomy
│   ├── AC1: Mission preload (load complete mission to robot)
│   ├── AC2: Onboard decision fallback (GPS nav without brain)
│   └── AC3: Data logging + deferred upload
├── AD: Logging, replay, analytics
│   ├── AD1: Structured event log (SQLite)
│   ├── AD2: Mission replay
│   └── AD3: Analytics dashboard
├── AE: Fleet management (multi-vehicle)
│   ├── AE1: Fleet manager (area split, relay, redistribute)
│   └── AE2: Fleet protocol
├── AF: MAVLink bridge
│   ├── AF1: MAVLink parser (v2, key messages)
│   └── AF2: QGroundControl compatible (free ground station)
│
└── Humanoid-specific (when humanoid hardware available):
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
## WHAT EXISTS vs WHAT'S MISSING (summary)
## ═══════════════════════════════════════════════════

### ALREADY EXISTS (don't touch):
| Component | Status |
|---|---|
| Complete TCP/IP stack | Works over VirtIO (QEMU) |
| Socket syscalls (370-381) | Implemented with user-space support |
| File I/O syscalls | Implemented with copy_from/to_user |
| IPC syscalls (100-107) | Implemented |
| Motor syscalls (230-234) | Implemented |
| Sensor syscalls (330-332) | Numbers defined, handlers pending |
| Cadence MACB Ethernet (VF2) | Complete driver with DMA rings |
| xHCI USB Host (VF2) | Init, reset, port scan, device detect — enumeration missing |
| CSI camera driver | Stubs (simulated on QEMU) |
| ELF loader + Sv39 paging | Works (hello.elf demonstrated) |
| brk/mmap/munmap | Implemented |
| Channels (pub/sub) | Works (CH_MOTOR_CMD, CH_IMU, etc.) |
| Behavior engine (L0-L3) | Works |
| Telemetry protocol | Works (binary + CRC-8 + UDP) |
| Watchdog (HW + SW) | Works |

### MISSING (to do):
| Component | Phase | Priority | Difficulty |
|---|---|---|---|
| **SITL Wheeled (simulator)** | AI0 | **CRITICAL (week 1)** | **Low** |
| **Test scenarios wheeled** | AI4 | **HIGH (week 1)** | Low |
| **SITL Visualization** | AI5 | HIGH (week 1) | Low |
| Net transport abstraction | P1 | HIGH | Low |
| Complete DHCP | P2 | Medium | Medium |
| Userspace syscall lib (libsys) | Q1 | HIGH | Medium |
| Yield-based sleep | Q2 | HIGH | Low |
| Task priority (RT/Normal) | Q2/U3 | Medium | Low |
| Brain client ELF | Q3 | HIGH | Medium |
| Reflex daemon ELF | Q4 | Medium | Low |
| Binary protocol brain↔robot | R1 | HIGH | Low |
| SYS_SENSOR_READ impl | S1 | HIGH | Low |
| Real CSI capture | T1 | Medium | High |
| JPEG encoder | T2 | Low | High |
| Net poll task | U1 | HIGH | Low |
| TCP buffer increase | U2 | Medium | Low |
| Autorun ELFs | U4 | Medium | Low |
| **USB WiFi — USB Core** | W1 | HIGH | High |
| **USB WiFi — RTL8188 driver** | W2 | HIGH | High |
| **USB WiFi — 802.11 stack** | W3 | HIGH | Very High |
| **USB WiFi — Crypto (AES/WPA2)** | W4 | HIGH | Medium |
| **USB WiFi — Net integration** | W5 | HIGH | Low |
| *(alternative)* ESP32 bridge | W-alt | HIGH | Low |
| **Skill library (universal + per-type)** | V1 | HIGH | Low |
| **Mode presets (security, patrol)** | V2 | HIGH | Low |
| **Task planner (free prompt → skills)** | V3 | HIGH | Medium |
| **Skill runner (state machine + loops)** | V4 | HIGH | Medium |
| **Notifications (pushover/telegram)** | X1 | HIGH | Low |
| **Bidirectional Telegram bot** | X2 | Medium | Medium |
| **HTTP API control** | X3 | Medium | Low |
| **Generic ActuatorCmd (multi-robot)** | Y1 | HIGH | Low |
| **Policy translators per type** | Y2 | HIGH | Medium |
| **Generic SensorPacket** | Y3 | Medium | Low |
| **Config per-robot-type** | Y4 | Medium | Low |
| **Kernel actuator_apply()** | Y5 | Medium | Low |
| **robot-brain** (Python) | — | HIGH | Medium |

### FUTURE — Safety + Scalability:
| Component | Phase | Priority | Difficulty |
|---|---|---|---|
| **SafetyProfile trait + FSM** | AG1+AG6 | **CRITICAL** | Medium |
| **Wheeled safety (refactor)** | AG2 | **CRITICAL** | Low |
| **Drone safety (hover/land/RTH)** | AG3 | **CRITICAL** (pre-drone) | High |
| **Humanoid safety (crouch/sit)** | AG4 | **CRITICAL** (pre-humanoid) | High |
| **Vehicle safety (brake/pullover)** | AG5 | **CRITICAL** (pre-vehicle) | Medium |
| **Battery reserve per type** | AG7 | HIGH | Low |
| **Dynamic watchdog per type** | AG8 | HIGH | Low |
| **EKF core (15 states, onboard)** | AH1 | **CRITICAL** (pre-drone) | High |
| **Sensor calibration** | AH2 | HIGH | Medium |
| **Sensor redundancy + voting** | AH3 | HIGH | Medium |
| **SITL Drone** | AI1 | HIGH (pre-drone) | Medium |
| **SITL Humanoid (MuJoCo)** | AI1b | HIGH (pre-humanoid) | Medium |
| **Bridges (Webots/Gazebo)** | AI2 | Low (optional) | Low |
| **HITL bridge** | AI3 | HIGH (pre-hardware) | Low |
| **3D Occupancy grid** | AJ1 | Medium | Medium |
| **3D path planner (A*/RRT*/VFH+)** | AJ2 | Medium | High |
| **Depth perception** | AJ3 | Medium | Medium |
| **Motor mixer (quad/hex/octo)** | AK1 | **CRITICAL** (pre-drone) | Medium |
| **Attitude PID controller** | AK2 | **CRITICAL** (pre-drone) | Medium |
| **Wind estimation + feedforward** | AK3 | HIGH | Medium |
| **Terrain following** | AL1 | Medium | Low |
| **Smart RTH** | AL2 | HIGH | Medium |
| **Visual Odometry** | AM1 | Medium | High |
| **Graph SLAM** | AM2 | Low | Very High |
| **Unit test suite** | AN1 | HIGH | Low |
| **SITL integration tests** | AN2 | HIGH | Low |
| **Chaos testing** | AN3 | Medium | Low |
| **CI pipeline** | AN4 | HIGH | Low |
| Link abstraction (multi-transport) | Z1 | HIGH | Medium |
| Bandwidth-aware protocol | Z2 | HIGH | Medium |
| LoRa driver (SX1276) | Z3 | Medium | Medium |
| Link failover auto-switch | Z4 | Medium | Medium |
| Multi-UART kernel | Z5 | Low | Low |
| Mission planner (patterns) | AA1 | HIGH | Medium |
| Geofencing (safety boundaries) | AA2 | HIGH | Medium |
| GPS waypoint navigation | AA3 | HIGH | Medium |
| RTK GPS (2cm precision) | AA4 | Medium | Low (hardware does it) |
| Headland turns (tractors) | AA5 | Low | Low |
| Payload abstraction | AB1 | Medium | Low |
| Smart spray control | AB2 | Low | Medium |
| CAN bus driver (J1939) | AB3 | Low | High |
| Mission preload (offline) | AC1 | HIGH | Low |
| Onboard decision fallback | AC2 | HIGH | Medium |
| Data logging + deferred upload | AC3 | Medium | Medium |
| Event logger (SQLite) | AD1 | Medium | Low |
| Mission replay | AD2 | Low | Low |
| Analytics dashboard | AD3 | Low | Low |
| Fleet manager | AE1 | Low | Medium |
| Fleet protocol | AE2 | Low | Low |
| MAVLink parser | AF1 | Low | Medium |
| QGroundControl bridge | AF2 | Low | Low |

### FUTURE — Humanoid-specific:
| Component | Phase | Priority | Difficulty |
|---|---|---|---|
| **ZMP calculator (kernel, RT)** | AO1 | **CRITICAL** (pre-humanoid) | High |
| **Balance controller (PD)** | AO2 | **CRITICAL** (pre-humanoid) | High |
| **CoM estimator + tilt recovery** | AO3 | HIGH | Medium |
| **Push recovery reflexes** | AO4 | HIGH | Medium |
| **Gait state machine** | AP1 | **CRITICAL** (pre-humanoid) | High |
| **Footstep planner** | AP2 | HIGH | Medium |
| **CPG oscillator + trajectory** | AP3 | HIGH | High |
| **RL gait policy (MuJoCo→RMLP)** | AP4 | Medium | Very High |
| **IK solver (6-DOF leg)** | AQ1 | **CRITICAL** (pre-humanoid) | High |
| **Arm IK + grasp planner** | AQ2 | Medium | High |
| **Collision self-check** | AQ3 | HIGH | Medium |
| **Task-priority WBC** | AR1 | HIGH | Very High |
| **Servo bus driver (Dynamixel)** | AR2 | **CRITICAL** (pre-humanoid) | Medium |
| **Fall detector (IMU + ML)** | AS1 | **CRITICAL** (pre-humanoid) | Medium |
| **Impact protection (crouch)** | AS2 | HIGH | Medium |
| **Stand-up sequences** | AS3 | HIGH | High |
| **F/T sensor driver** | AT1 | Medium | Medium |
| **Impedance controller** | AT2 | HIGH | High |
| **Human safety (ISO 13482)** | AT3 | **CRITICAL** (pre-humanoid) | Medium |
| **URDF model + MuJoCo bridge** | AU1 | HIGH | Medium |
| **Gait training RL** | AU2 | Medium | High |
| **Sim-to-real transfer** | AU3 | Medium | High |

### RECOMMENDED OPTIMIZATIONS:
| What | Why | Impact |
|---|---|---|
| Dedicated net poll task | TCP latency ~100ms → ~1ms | High |
| Yield-based sleep | CPU burn 100% → yield when idle | High |
| Task priority | Motor control never preempted by shell | Medium |
| TCP window 4KB+ | Send frames without fragmentation | Medium |
| Per-process FD table | Correct userspace isolation | Low (functional) |
