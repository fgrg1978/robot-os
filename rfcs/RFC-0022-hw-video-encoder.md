# RFC-0022: Hardware Video Encoder (JH7110 H.264/H.265)

> **Status:** partially implemented (cam-ring SPSC + 10 tests landed; the `VideoEncoder` trait, `NoOpEncoder` and all JH7110/Wave420L code are NOT built)  
> **Authors:** Fernando Rodriguez
> **Created:** 2026-05-24
> **Last updated:** 2026-08-20
> **Supersedes:** —
> **Superseded by:** —


> **Status audit 2026-08-20.** Corrected from `implemented`. Verified against
> the tree: `crates/cam-ring/` and its 10 tests are real, but `VideoEncoder`,
> `NoOpEncoder`, `Wave420` and any H.264/H.265 logic appear **nowhere outside
> this document** — the only encoder in the tree is the unrelated baseline JPEG
> compressor at `crates/drivers/src/csi.rs:316`, and `cam-ring` is not even a
> dependency of `kernel/Cargo.toml`. The design text below is left as written;
> it describes intended work, not shipped work.

## Summary

Replace raw frame transmission over the multi-stream link (RFC-0021) with
H.264 (and optionally H.265) encoded video using the on-chip hardware
encoder present in the StarFive JH7110 SoC (VisionFive 2 target).  This
RFC defines the `VideoEncoder` trait, the two-ring data path, the stubbed
`NoOpEncoder` for QEMU development, and marks the register-level details
that require the JH7110 TRM to fill in.

## Motivation

Raw 1080p YUV420 at 30 fps produces ≈ 93 MB/s — well above the 100 MB/s
budget and leaving no headroom for other streams.  The JH7110 silicon
H.264 encoder (Wave420L IP block) delivers typical 10–20 : 1 compression,
reducing camera bandwidth to ≈ 5–10 MB/s.

Encoding in kernel space avoids a round-trip copy through a userspace
encoder process and keeps latency deterministic (no scheduling jitter from
a user-mode encoder thread).

## Detailed design

### `VideoEncoder` trait

```rust
/// Error type for encoder operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderError {
    /// No hardware encoder available (QEMU / unsupported SoC).
    NotAvailable,
    /// Input frame size does not match the configured resolution.
    BadFrameSize,
    /// Output buffer too small to hold the encoded slice.
    OutputBufferTooSmall,
    /// Encoder IP block reported a hardware fault.
    HardwareFault,
    /// TODO: map JH7110 Wave420L error codes to specific variants.
}

/// Synchronous frame encoder.  Implementations may block until the
/// hardware encoder finishes; an async variant is a future extension.
pub trait VideoEncoder {
    /// Encode one raw frame (YUV420 or NV12) into `out_buf`.
    ///
    /// `input_frame` must be exactly `width * height * 3 / 2` bytes.
    ///
    /// Returns the number of bytes written to `out_buf` on success.
    fn encode(&mut self, input_frame: &[u8], out_buf: &mut [u8])
        -> Result<usize, EncoderError>;

    /// Return the encoder's configured output format.
    fn format(&self) -> VideoFormat;

    /// Signal end-of-stream; flush any buffered frames.
    fn flush(&mut self) -> Result<(), EncoderError>;
}

/// Compressed video output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    /// H.264 Annex-B byte stream (start codes 0x00 0x00 0x00 0x01).
    H264AnnexB,
    /// H.264 AVCC (length-prefixed, for MP4 container).
    H264Avcc,
    /// H.265 / HEVC Annex-B byte stream.
    H265AnnexB,
    /// Raw copy (NoOpEncoder — QEMU/dev only).
    Raw,
}
```

### Two-ring data path

```
CSI capture task
        │  cam_capture() → raw YUV frame
        ▼
FrameRing<RAW_N, RAW_SZ> (raw frames)
        │  peek_read → encoder_task
        ▼
encoder_task: VideoEncoder::encode(raw_frame, encoded_buf)
        │
        ▼
FrameRing<ENC_N, ENC_SZ> (encoded frames)
        │  peek_read → tx_task
        ▼
multi_stream::wrap(STREAM_CAMERA_N, encoded_frame, tcp_buf)
        │
        ▼
TCP socket → brain
```

Ring sizing constants (named, no magic numbers):

- `CAM_RAW_RING_N: usize = 4` — four raw frame slots (≈ 4 × 3 MB = 12 MB).
- `CAM_ENC_RING_N: usize = 8` — eight encoded frame slots (allow burst
  of I-frames without stalling the encoder).
- `CAM_RAW_SLOT_SZ: usize = 3_110_400` — 1080p YUV420 (1920 × 1080 × 1.5).
- `CAM_ENC_SLOT_SZ: usize = 524_288` — 512 KiB max encoded frame (covers
  1080p I-frame at typical bitrates; TODO: confirm with JH7110 TRM).

### `NoOpEncoder` (QEMU / development stub)

```rust
pub struct NoOpEncoder;

impl VideoEncoder for NoOpEncoder {
    fn encode(&mut self, input_frame: &[u8], out_buf: &mut [u8])
        -> Result<usize, EncoderError>
    {
        if out_buf.len() < input_frame.len() {
            return Err(EncoderError::OutputBufferTooSmall);
        }
        let n = input_frame.len();
        out_buf[..n].copy_from_slice(input_frame);
        Ok(n)
    }

    fn format(&self) -> VideoFormat { VideoFormat::Raw }

    fn flush(&mut self) -> Result<(), EncoderError> { Ok(()) }
}
```

`NoOpEncoder` copies raw bytes into the encoded ring so the entire
data path (ring → encoder → multi-stream → TCP → brain) is exercisable
without hardware.

### JH7110 Wave420L driver sketch

The JH7110 integrates a Chips&Media Wave420L multi-standard video encoder
IP block.  The kernel driver shape will be:

```rust
pub struct Jh7110Encoder {
    /// MMIO base of the Wave420L instance.
    /// TODO: Confirm from JH7110 TRM section [TODO: section number].
    mmio_base: usize,
    width:  u16,
    height: u16,
    format: VideoFormat,
}
```

**Known register offsets** (Wave420L product datasheet, where available):
All addresses below are TODO pending access to the JH7110 TRM.

| Register         | Offset  | Purpose                                     |
|------------------|---------|---------------------------------------------|
| `W4_CMD_INSTANCE` | TODO   | Command queue instance select               |
| `W4_CMD_TYPE`    | TODO    | Command type (INIT / ENC_PIC / FLUSH)       |
| `W4_RET_SUCCESS` | TODO    | Return register (0 = success)               |
| `W4_BS_START`    | TODO    | Bitstream buffer start address (PA)         |
| `W4_BS_SIZE`     | TODO    | Bitstream buffer size in bytes              |
| `W4_PIC_SIZE`    | TODO    | Encoded picture dimensions (width << 16 | height) |
| `W4_SRC_FORMAT`  | TODO    | Input pixel format (NV12, YUV420P, etc.)    |
| `W4_SRC_STRIDE`  | TODO    | Source luma stride in bytes                 |
| `W4_SRC_ADDR_Y`  | TODO    | Source luma plane physical address          |
| `W4_SRC_ADDR_CB` | TODO    | Source Cb plane physical address            |
| `W4_SRC_ADDR_CR` | TODO    | Source Cr plane physical address            |
| `W4_IRQ_STATUS`  | TODO    | IRQ status / clear                          |

**Clock and reset gates** (JH7110 System CRU):

- Clock: `CLK_VEN_SRC` → TODO: index in JH7110 TRM Table [TODO].
- Reset: `RSTN_VEN` → TODO: bit in SYSCRG reset register [TODO].

**IRQ line**: `VPU_ENC_IRQ` → TODO: PLIC source index from JH7110
  interrupt assignment table [TODO].

**DMA descriptor format**: TODO — Wave420L uses a ring descriptor
  format; layout is in the Wave420L firmware protocol document which
  is NDA-gated.  Use C&M public Wave517 spec as approximation until
  JH7110-specific doc is obtained.

**Bitstream output format**: TODO — confirm whether JH7110 encoder
  outputs Annex-B (start-code prefixed) or AVCC (length-prefixed)
  by default; this affects brain-side decoder configuration.

**Rate control**: TODO — CBR vs VBR mode register and target-bitrate
  field location.

**I-frame interval**: TODO — GOP size register; must be tunable via
  `crates/config` INI key `[camera] gop_size`.

### Integration with crates/config

New `[camera]` INI section (extend `crates/config/src/lib.rs`):

```ini
[camera]
enabled = 1
width    = 1920
height   = 1080
fps      = 30
gop_size = 30        ; I-frame every N frames
bitrate  = 8000000   ; target bitrate in bits/s (CBR)
stream_id = 16       ; STREAM_CAMERA_BASE = 0x10 = 16
```

All values are `AtomicU32` runtime-readable with no reboot required
(except `width`/`height` which require encoder re-init).

## Drawbacks

- Driver development requires the JH7110 TRM (currently partially
  available; NDA sections cover Wave420L internals).
- The two-ring path increases static memory use by ≈ 15 MB for 1080p.
  Use `CAM_RAW_RING_N = 2` for constrained boards.
- Synchronous `encode()` blocks the encoder task for the hardware
  encoding latency (≈ 8–16 ms at 30 fps for 1080p).

## Rationale and alternatives

Software H.264 encoding (e.g. x264 or a custom RISC-V RVV port) would
consume the entire A55 core budget and still not hit real-time at 1080p.
Hardware encoding is the only viable path on the JH7110.

## Prior art

- Linux `v4l2-codec` driver for Wave420L / Wave521 on JH7110.
- StarFive VisionFive 2 SDK: `drivers/media/platform/wave5/` (GPL-2.0).
  The SDK driver provides the command protocol reference but cannot be
  reused directly (GPL, dynamic allocation, Linux-only IRQ model).

## Unresolved questions

1. **TODO: JH7110 TRM register addresses** — Wave420L MMIO base, all
   register offsets, DMA descriptor layout.  Source: JH7110 TRM
   (StarFive release, expected via public SDK or NDA).
2. **TODO: IRQ assignment** — PLIC source number for VPU encoder IRQ.
   Source: JH7110 interrupt assignment table (TRM Appendix).
3. **TODO: Clock/reset index** — `CLK_VEN_SRC` and `RSTN_VEN` bit
   positions in SYSCRG registers.  Source: JH7110 CRU register map.
4. **TODO: DMA descriptor format** — Wave420L ring descriptor fields.
   Source: Chips&Media Wave420L firmware protocol (NDA or public Wave517
   doc approximation).
5. **TODO: Bitstream output format** — Annex-B vs AVCC default mode.
   Source: JH7110 TRM or empirical test on hardware.
6. **TODO: Rate-control register layout** — CBR target bitrate field.
   Source: JH7110 TRM or Wave420L datasheet.
7. **TODO: CAM_ENC_SLOT_SZ confirmation** — 512 KiB is an estimate;
   confirm maximum encoded I-frame size at 1080p 30 fps with JH7110.
8. **Async encoding.** Should `encode()` be non-blocking (post an
   encoding job and return immediately, with a separate `poll_done()`)?
   Required if encoder latency > 1 frame period (33 ms at 30 fps).

## Future possibilities

- H.265 encoding for higher compression at the same quality.
- Encoder → direct DMA to TCP TX ring (zero intermediate copy).
- Per-stream resolution scaling (e.g. camera-0 = 1080p, camera-1 = 720p
  for a second camera viewpoint).
