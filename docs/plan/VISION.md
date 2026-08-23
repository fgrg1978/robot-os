# Vision

> **The verifiable Rust microkernel with capability-typed IPC,
> hierarchical multi-policy scheduler, native AI runtime, and a
> complete safety + cybersecurity certification path —
> for robots and autonomous vehicles.**

## What we are building

An open-source operating system that fills a hole nobody else fills:

- **Open-source** — Apache 2.0, Foundation-hosted, real governance.
- **Verifiable** — TLA+ models, Kani+Loom CI, eventual seL4-style proofs
  on the IPC + scheduler core.
- **Hard real-time** — multi-policy scheduler (FIFO / EDF / RR / CFS /
  Sporadic) with adaptive partitioning, automotive-grade.
- **Capability-based** — every kernel resource gated by a typed `Cap<T>`,
  unforgeable, statically-routed, mechanically-verifiable.
- **AI-native** — first-class model bundles, capability-isolated
  inference, NPU drivers, foundation-model + VLA runtime.
- **Certifiable** — ISO 26262 ASIL-D path + ISO 21434 cybersecurity +
  AUTOSAR Adaptive subset. Open-source reference implementation.
- **Multi-platform** — RV64 + ARM Cortex-A/R + x86_64 from the start.
- **Modular** — every replaceable subsystem behind a trait, one impl
  per file, Cargo features select at compile time, runtime registry
  for future hot-swap.

## What we are NOT building

- Not a Linux competitor. Linux wins desktop / server / general-purpose.
- Not a ROS2 replacement. We are the *kernel underneath* a ROS2-bridged
  robotics stack.
- Not a research toy. Every architectural decision is taken with a real
  product roadmap in mind.
- Not "AGI for robots". Foundation models are tools we host, not the
  story we tell.

## Who this is for

- Robotics startups that need deterministic motor control + AI
  perception, with a defensible safety story for funding due diligence.
- Tier-2 automotive suppliers who want an open AUTOSAR Adaptive
  reference implementation instead of paying Apex.OS.
- Universities teaching modern OS design, who today have to choose
  between "1980s pedagogy on xv6" and "Linux internals". This OS is
  small enough to teach + advanced enough to matter.
- Open-source contributors who want to work on the systems-software
  frontier (caps + verification + AI) without a PhD as a prerequisite.

## How we know we have arrived

- 5 years from now, a graduate student writing a paper on "the Rust
  microkernel for robots" cites this OS as the reference baseline.
- A Tier-2 automotive supplier ships an ECU with this OS in production.
- A robotics startup files an FDA / TÜV submission citing this OS's
  ASIL-D path as their safety argument.
- The OS appears in the EU AI Act regulatory technical specifications
  as an example of a compliant runtime.
- A Linux Foundation or Eclipse working group references this OS's
  modular pattern as the recommended approach.

That is the destination. The plan that follows is how we get there.
