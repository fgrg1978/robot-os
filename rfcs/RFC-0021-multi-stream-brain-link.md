# RFC-0021: Multi-Stream Brain–Kernel Link

> **Status:** implemented (lib + 15 tests landed; kernel TCP wire-up landed, gated default-off)  
> **Authors:** Fernando Rodriguez
> **Created:** 2026-05-24
> **Last updated:** 2026-08-20
> **Supersedes:** —
> **Superseded by:** —


> **Status audit 2026-08-20.** The "kernel TCP wire-up pending" caveat was
> stale. `kernel/src/main.rs` calls `robot_os_multi_stream::unwrap` on the RX
> demux path (~3301, outermost: strip `[stream_id][len]`) and wraps on TX
> (~3290). Gated by `CFG_MULTI_STREAM`, which defaults off — built and wired,
> but not on by default.

## Summary

Replace the single-channel brain↔kernel TCP connection with a lightweight
stream-ID-prefixed multiplexed framing layer.  All logical streams
(control, camera-N, LIDAR, audio) travel over a single TCP socket.
The wire format is specified in `crates/multi-stream/src/lib.rs`; this RFC
records the rationale, semantics, and migration plan.

## Motivation

The current `crates/behavior/src/lib.rs` runs a single TCP socket that
carries every packet type: sensor readings, camera frames, status updates,
and actuator commands.  As throughput requirements grow (S1/S6 target:
100 MB/s peak per robot) camera and LIDAR byte streams dominate, and there
is no way to give control-plane packets priority or to route them
independently.

Three approaches were considered:

| Approach | Pros | Cons |
|----------|------|------|
| Multiple TCP connections (one per stream) | Strong isolation; each stream has its own send buffer | N extra file descriptors per robot; N extra NAT entries; brain-side complexity multiplies with robot count; reconnection must be coordinated across sockets |
| UDP + custom reliability | Lower latency; natural packet boundaries | Re-implements TCP reliability; QUIC overhead on a constrained kernel |
| Single-socket multiplexing (this RFC) | One file descriptor per robot; one NAT entry; control plane unaffected by camera backpressure; trivial routing on brain side | Head-of-line blocking on a single socket (see Unresolved Questions) |

For a kernel serving 1–16 robots over a local Ethernet segment a single
multiplexed TCP socket is the right trade-off: the kernel avoids allocating
multiple TCP control blocks per robot, the brain-side router dispatches by
stream-id prefix rather than by port, and control packets can be drained
even when a camera ring is paused.

## Detailed design

### Wire format

Each frame on the byte stream:

```
+──────────────+──────────────────+────────────────────────────────────────+
│  STREAM_ID   │  LEN             │  PAYLOAD                               │
│  1 byte      │  2 bytes LE      │  LEN bytes                             │
+──────────────+──────────────────+────────────────────────────────────────+
```

- `STREAM_ID` — logical stream selector (see stream-id table below).
- `LEN` — payload length, unsigned 16-bit little-endian.  Maximum `65535`
  bytes.  Named constant `MAX_PAYLOAD_LEN = 65535` in the crate.
- `PAYLOAD` — raw bytes for the inner protocol on this stream.

Total overhead: 3 bytes per frame (`HEADER_LEN = 3`).

### Stream ID allocation

| Value         | Constant               | Use                                           |
|---------------|------------------------|-----------------------------------------------|
| `0x00`        | `STREAM_CONTROL`       | Existing `brain_protocol` (sensors, actuators, status) |
| `0x10..=0x1F` | `STREAM_CAMERA_BASE`..`STREAM_CAMERA_LAST` | Up to 16 camera streams |
| `0x20`        | `STREAM_LIDAR`         | LIDAR point-cloud (future)                   |
| `0x21`        | `STREAM_AUDIO`         | Audio capture/playback (future)              |
| `0x22..=0xFF` | —                      | Reserved                                      |

Camera stream index `n` (0-based) maps to stream-id `0x10 + n` via
`camera_stream_id(n)`.

### Back-pressure semantics

- **Per-stream logical queues.** Each stream-id has its own send queue on
  the brain side.  If the camera-0 ring fills (consumer is too slow),
  only camera-0 frames are held back; `STREAM_CONTROL` traffic is
  unaffected.

- **Control-plane priority.** The kernel's transmit loop MUST drain
  `STREAM_CONTROL` frames before camera frames when both are ready.
  Camera streams SHOULD be interleaved round-robin with control frames
  at configurable ratio (e.g. 1 control : 4 camera).

- **Camera ring stall.** When `FrameRing::claim_write()` returns `None`
  (ring full), the kernel drops the incoming CSI frame and increments a
  `cam_ring_drops` diagnostic counter.  The brain observes a gap in the
  frame sequence and can request an I-frame if using H.264.

### API (source of truth: `crates/multi-stream/src/lib.rs`)

```rust
/// Wrap a payload into a multiplexed frame written to `out`.
pub fn wrap(stream_id: u8, inner_bytes: &[u8], out: &mut [u8])
    -> Result<usize, WrapError>;

/// Parse a multiplexed frame from a byte slice.
pub fn unwrap(frame: &[u8]) -> Option<(u8, usize, &[u8])>;
```

`unwrap` returns `None` for frames shorter than `HEADER_LEN` and for
length-extension frames where `LEN > frame.len() - HEADER_LEN`.

### Integration data path

```
CSI capture → cam_capture_task
                │
                ▼
        FrameRing (raw)  ← claim_write / commit_write
                │
                ▼
        encoder_task (NoOpEncoder for QEMU, JH7110Encoder for hardware)
                │
                ▼
        FrameRing (encoded)
                │
                ▼
        tx_task: multi_stream::wrap(STREAM_CAMERA_N, encoded_frame, tcp_buf)
                │
                ▼
        TCP socket → brain
                │
        brain receives: multi_stream::unwrap → route by stream_id
                │
    STREAM_CONTROL → existing brain_protocol parser
    STREAM_CAMERA_N → video decoder / GMM / motion pipeline
```

## Migration plan

1. **Phase A — add framing layer (this RFC).** Implement `crates/multi-stream`
   (done).  No changes to `crates/behavior/` or brain server yet.

2. **Phase B — brain-side prefix parser.** Add a thin demultiplexer to
   `robot-brain/server.py`: read 3-byte header, dispatch by stream-id.
   `STREAM_CONTROL` goes to the existing `handle_packet` loop unchanged.

3. **Phase C — kernel wraps existing packets.** Modify
   `crates/behavior/src/lib.rs` to wrap all outgoing packets with
   `multi_stream::wrap(STREAM_CONTROL, ...)` and strip the prefix on
   incoming actuator commands.  At this point the wire format changes;
   brain must be updated first (Phase B deployed first).

4. **Phase D — camera stream.** Wire `FrameRing` → encoder → `wrap` →
   TCP for camera stream-ids, per RFC-0022.

Phases A–B are non-breaking.  Phase C is a coordinated cut-over; fleet
robots must be updated atomically (use OTA slot swap from RFC-0011).

## Drawbacks

- Adds 3 bytes overhead per frame.  At 100 MB/s this is ≈ 0.5 % overhead
  for 600-byte average payload size — negligible.
- Head-of-line blocking: a large camera frame stalls control bytes until
  the write() syscall returns.  See Unresolved Questions.
- The brain-side demultiplexer must be updated before Phase C; a protocol
  version mismatch silently corrupts the stream.  Consider adding a magic
  prefix or version negotiation in Phase B.

## Rationale and alternatives

QUIC would solve head-of-line blocking at the cost of a full QUIC
implementation in the kernel.  The kernel already has a working TCP/IP
stack (`crates/net/`) and will not gain QUIC before hardware arrives in
July 2026.  QUIC is noted as a future option.

## Prior art

- HTTP/2 stream multiplexing (similar header structure but more complex).
- RTSP interleaved binary data (RFC 2326 §10.12): 1-byte magic + 1-byte
  channel + 2-byte length — very close to this design.
- gRPC over HTTP/2 (5-byte framing).

## Unresolved questions

1. **Head-of-line blocking.** A single large camera frame (e.g. 1 MB I-frame)
   will block control bytes for the duration of one `write()` syscall.  Options:
   a) Fragment camera frames into `MAX_FRAG_BYTES = 16384` chunks tagged with a
      fragment flag; b) Move to QUIC (post-July 2026); c) Accept it for now and
      document the worst-case latency jitter.
2. **Version negotiation.** Should the first frame on the socket be a
   `STREAM_CONTROL` hello-packet advertising the multi-stream version, or is
   the existing brain-protocol HELLO packet sufficient?
3. **Per-stream priority on the kernel side.** Should the kernel tx-loop use
   a weighted-fair-queue scheduler across stream IDs, or is simple round-robin
   (with control priority) sufficient?

## Future possibilities

- Per-stream encryption key derivation (extend RFC-0019 to stream granularity).
- QUIC migration at Phase 3 if head-of-line blocking proves problematic.
- Stream-id `0x30..0x3F` for IMU/GPS high-rate telemetry.
