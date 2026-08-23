# RFC-0007: AI Runtime — Model Bundle, Capability-Isolated Inference

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

PHANES exposes a first-class **AI runtime** that loads, isolates, and
executes ML / VLA / world-model inference workloads. Each model is
distributed as a signed **Model Bundle** containing weights, expected
sensor / actuator capabilities, safety constraints, and metadata. The
kernel verifies the signature, instantiates the model in a dedicated
task with exactly the caps it requires, and dispatches inference to
the appropriate backend (scalar / RVV / NPU / accelerator). Replacing
a model is an OTA-grade atomic operation with rollback.

This RFC defines the runtime; RFC-0008 covers the per-platform
accelerator drivers; RFC-0011 covers the bundle signing chain.

## Motivation

The robotics + AV market in 2026 has shifted decisively towards
**foundation models for control** (RT-2, OpenVLA, π0, NVIDIA Isaac
Cosmos / GR00T, Pi-0.5). A robot OS that doesn't host these natively
is a non-starter for new product lines.

But hosting them well requires more than `mmap` + `libonnxruntime`:

- **Safety isolation.** A loaded VLA model is third-party code (often
  large, often opaque). It needs to be confined: only the cameras and
  actuators it was deployed for, never anything else.
- **OTA semantics.** Models are large (50–500 MB) and update on a
  separate cadence from the kernel. They need atomic + rollback +
  signed delivery.
- **Hardware acceleration.** RK3588 NPU is 100× faster than CPU for
  CNN / quantised inference. Without NPU integration, the OS is
  uncompetitive.
- **Capability composition.** A multi-model setup (perception VLM +
  policy VLA + safety classifier) needs distinct cap-sets per model,
  not one giant trust boundary.
- **Foundation-model ecosystem.** Bundle format compatible with
  GGUF, ONNX, OpenVLA's checkpoint, NVIDIA's TensorRT engines.

## Detailed design

### Model Bundle file format

A Model Bundle is a single file `*.MBL` (Model Bundle, signed) on
the FAT32 partition. Conceptually:

```
┌────────────────────────────────────────────┐
│ Header (256 bytes)                         │
│   magic "MBL\0"                            │
│   bundle_version (u32)                     │
│   model_id (16 B UUID)                     │
│   format ("gguf" | "onnx" | "tensorrt"     │
│           | "raw_int8" | "rust_native")    │
│   hash_sha256 (32 B)                       │
│   manifest_offset (u32)                    │
│   manifest_size (u32)                      │
│   weights_offset (u32)                     │
│   weights_size (u32)                       │
├────────────────────────────────────────────┤
│ Manifest (TOML, ~1–4 KB) — see schema      │
├────────────────────────────────────────────┤
│ Weights (binary, format-dependent)         │
└────────────────────────────────────────────┘
```

Sidecar `*.MBL.SIG` carries the Ed25519 signature over header +
manifest + weights (RFC-0011's signing key).

### Manifest (TOML schema)

```toml
[bundle]
name        = "openvla-7b-quantized"
version     = "1.4.2"
created_at  = "2026-04-30T12:00:00Z"
author      = "PHANES Foundation / OpenVLA Team"
license     = "MIT"

[model]
type        = "vla"           # vlm | vla | classifier | policy | world_model
input_shape = "rgb:224x224x3, instr:txt[256]"
output_shape = "actions:7"    # 7-DOF arm
quantization = "int8"

[runtime]
backend     = "rvv-or-cpu"    # backend selector; see runtime backends below
min_compute = { mflops = 4_000 }   # admission control: kernel rejects if NPU/CPU below
max_memory  = { rss_mib = 96 }
inference_period_us = 50_000   # 20 Hz target
inference_budget_us = 30_000   # CBS reservation per inference

[caps_required]
# What the model expects to be granted at instantiation.
caps = [
    { kind = "channel-sub", target = "/sensors/cam_front", perm = "r" },
    { kind = "channel-pub", target = "/cmd/arm",            perm = "w" },
]

[safety]
# Optional safety constraints the kernel enforces.
max_action_magnitude = 1.0      # actions clamped before dispatch
max_consecutive_failures = 3    # if model errors 3× in a row, fall back
fallback_skill = "stop"         # what to do on fail
```

### Runtime backends

Per the modular pattern (RFC-0002):

```
crates/ml/src/
├── api.rs                 ← trait MlBackend
├── lib.rs                 ← cfg-selects active backend
├── impls/
│   ├── scalar.rs          ← #[cfg(feature = "ml-scalar")]
│   ├── rvv.rs             ← #[cfg(feature = "ml-rvv")]    (K1, future RV+V)
│   ├── npu_rk3588.rs      ← #[cfg(feature = "ml-npu-rk3588")]
│   ├── npu_hailo.rs       ← #[cfg(feature = "ml-npu-hailo")]
│   ├── npu_coral.rs       ← #[cfg(feature = "ml-npu-coral")]
│   └── tensorrt.rs        ← #[cfg(feature = "ml-tensorrt")] (Jetson)
```

Each backend implements:

```rust
pub trait MlBackend: Sync {
    fn load(bundle: &ModelBundle) -> Result<ModelHandle, MlErr>;
    fn run(handle: &ModelHandle, input: &Tensor) -> Result<Tensor, MlErr>;
    fn unload(handle: ModelHandle);
    fn caps_capability(&self) -> BackendCaps;  // mflops, mem, formats supported
}
```

A model declared `backend = "rvv-or-cpu"` causes the runtime to pick
the highest-capability available backend at boot; a model declared
`backend = "npu-hailo"` requires that backend to be present (else
admission fails).

### Model lifecycle

```text
   [unloaded]
       |
       | bundle deployed via OTA (RFC-0011 atomic)
       v
   [verified]            ← Ed25519 sig OK, hash OK
       |
       | kernel allocates inference task with declared caps
       v
   [instantiated]        ← model task in scheduler, hard-RT class
       |
       | first run() call
       v
   [running]             ← periodic inference at declared rate
       |
       | OTA replacement OR safety fault
       v
   [draining]            ← finish in-flight inference, no new work
       |
       v
   [unloaded]
```

OTA replacement uses A/B model slots (analogous to firmware A/B):

```
/fat/models/active/<model_id>.MBL    → currently used
/fat/models/staged/<model_id>.MBL    → newly received
```

After verification, `staged` is atomically renamed to `active` (the
old `active` becomes `previous` for rollback). The runtime is told
to drain old, load new. If new model crashes 3× consecutively,
runtime auto-rolls back.

### Capability-isolated inference

When the runtime instantiates a model, it:

1. Spawns a dedicated task (`model-<id>`) in the `HardRT` class with
   the budget/period from manifest.
2. Populates that task's `cap_table` with **only** the caps in
   `[caps_required]`. No others.
3. The task entry-point is the backend's `run()` loop:
   - Read input from subscribed channels (uses cap)
   - Invoke backend `run()`
   - Apply safety clamps
   - Publish output to declared channels (uses cap)

The model code never sees ambient kernel resources; it sees only its
caps. **A compromised model cannot reach beyond its declared
input/output surface.**

### Safety enforcement

The kernel applies safety clamps before dispatching a model's output:

- Numeric clamps from `[safety]` (max_action_magnitude).
- Failure counter: if `run()` returns `Err` `max_consecutive_failures`
  times, the runtime stops the model and invokes `fallback_skill`.
- Watchdog: if `run()` exceeds `inference_budget_us`, the task is
  preempted and treated as a failure for that cycle.

### Multi-model topology

A robot can host multiple models simultaneously:

```toml
# /fat/CAPS.TOML excerpt
[task.model_perception_vlm]
caps = [
    { kind = "channel-sub", target = "/sensors/cam_front", perm = "r" },
    { kind = "channel-pub", target = "/perception/scene",  perm = "w" },
]

[task.model_policy_vla]
caps = [
    { kind = "channel-sub", target = "/perception/scene",  perm = "r" },
    { kind = "channel-sub", target = "/sensors/imu",       perm = "r" },
    { kind = "channel-pub", target = "/cmd/motor",         perm = "w" },
]

[task.model_safety_classifier]
caps = [
    { kind = "channel-sub", target = "/sensors/cam_front", perm = "r" },
    { kind = "channel-pub", target = "/safety/alert",      perm = "w" },
]
```

Three models, three distinct cap-sets. The perception VLM cannot
issue motor commands; the policy VLA cannot publish safety alerts;
the safety classifier cannot move the robot. Each is mechanically
confined.

### Telemetry

Per-model metrics exposed via `proc/models/<id>/`:

- inferences_total
- inferences_failed
- avg_latency_us
- p95_latency_us
- budget_exhaustion_count
- last_error

Exposed to fleet brain via the standard telemetry protocol.

## Backend implementations — Phase 2 priorities

| Backend | Hardware | Effort | Phase |
|---------|----------|--------|-------|
| `scalar` | any CPU | already exists | 1 |
| `rvv` | K1, future RV-V | ~3 months | 2 |
| `npu_rk3588` | RK3588 NPU (6 TOPS) | ~2 months | 2 |
| `npu_hailo` | Hailo-8 (26 TOPS) | ~3 months | 2 |
| `npu_coral` | Coral USB Edge TPU | ~1 month | 2 |
| `tensorrt` | Jetson Orin family | ~4 months | 3 |
| `cuda` | desktop NVIDIA dev only | ~2 months | 3 |

## VLA-specific path

For VLA models specifically (vision → language → action), we provide:

- Tokenizer support inside the runtime (no per-model reimplement).
- Standardised image preprocessing pipeline.
- Action decoder helpers for common output shapes (joint commands,
  diff-drive, ackermann, drone motor mix).

## Drawbacks

- **Bundle format is yet another standard.** We're proposing it
  because none of the existing ones (GGUF, ONNX, TensorRT engine)
  carry the cap-required + safety metadata we need. We piggy-back
  on existing weight formats inside the bundle.
- **Backend abstraction has overhead** vs hardcoded paths. Mitigated
  by `#[inline]` and per-backend code generation.
- **Hardware support is per-vendor effort.** No way around it.

## Rationale and alternatives

**Alternative A — host-only inference.** Robot sends sensors to
cloud, cloud runs model. Latency unacceptable for control loops;
loses sovereignty. Rejected.

**Alternative B — embed `libonnxruntime` per platform.** Heavy
runtime, GC, allocator surprises. Doesn't fit kernel-space and would
require a dedicated user task with full alloc. Possible as one
backend among many; not the default.

**Alternative C (chosen) — typed bundle + capability isolation +
multi-backend trait.** Aligned with the rest of the architecture.

## Prior art

- **NVIDIA TensorRT** engine format: weights + manifest. Conceptually
  similar; closed.
- **GGUF / GGML** (`llama.cpp`): open weight format with metadata.
  Inspires the `[model]` block.
- **OpenVLA** checkpoint format: HuggingFace-style.
- **AUTOSAR Adaptive Manifest**: declarative service description.
  Inspires the manifest TOML structure.
- **WASM components**: capability-typed module format. Not directly
  applicable but informs the cap-required model.

## Unresolved questions

- **Should we support partial model reload** (e.g. swap LoRA
  adapters without reloading the whole bundle)? Working assumption:
  no in Phase 2; revisit Phase 3.
- **Multi-model orchestration scheduling** — how does the runtime
  prioritise three models with overlapping budgets? Working
  assumption: each model has its own `inference_period_us` and CBS
  reservation; the partition scheduler handles it.
- **GPU drivers in Phase 4?** Out of Phase 2 scope; relevant for
  Jetson Orin and embedded GPU SoCs.

## Future possibilities

- **Phase 3:** Runtime fine-tuning hooks (federated learning across
  fleet).
- **Phase 4:** Verified inference (formal bounds on output given
  bounded inputs) — research direction.
- **Phase 5:** Runtime model composition (chain N models into a
  pipeline, kernel mediates).
