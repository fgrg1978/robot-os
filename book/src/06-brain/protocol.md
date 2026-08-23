# Protocol

The brain ↔ kernel link uses a custom binary protocol:

```
MAGIC("BR") + TYPE(1B) + LEN(2B LE) + PAYLOAD + CRC8
```

Authenticated and integrity-protected by an HMAC-SHA256 envelope (see
`crates/behavior/src/auth_envelope.rs`).

## Packet types

| Code | Direction       | Name           |
|------|------------------|----------------|
| 0x01 | Robot → Server   | `SENSOR`       |
| 0x02 | Robot → Server   | `CAMERA`       |
| 0x03 | Robot → Server   | `STATUS`       |
| 0x04 | Robot → Server   | `OTA_ACK`      |
| 0x05 | Robot → Server   | `SENSOR_COMPACT` |
| 0x80 | Server → Robot   | `ACTUATOR`     |
| 0x81 | Server → Robot   | `MODE`         |
| 0x82 | Server → Robot   | `WAYPOINT`     |
| 0x83 | Server → Robot   | `CONFIG`       |
| 0x84 | Server → Robot   | `OTA_BEGIN`    |
| 0x85 | Server → Robot   | `OTA_CHUNK`    |
| 0x86 | Server → Robot   | `OTA_END`      |
| 0x87 | Server → Robot   | `PAYLOAD`      |
| 0x88 | Server → Robot   | `ESTOP`        |

CRC-8/MAXIM polynomial `0x31`. Little-endian.

Authoritative implementation:

- Brain side: `phanes-brain/protocol.py`
- Kernel side: `crates/behavior/src/brain_protocol.rs`

The two **must** stay in sync; CI gate `make protocol-sync-check`
asserts byte-for-byte agreement on synthetic test cases.
