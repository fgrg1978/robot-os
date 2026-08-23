# Brain overview

The **brain** is the host-side Python orchestrator. It runs on a
laptop, a mini-PC, or a per-site edge gateway — *not* on the robot
itself.

The brain is responsible for:

- Connecting to one or more robots over TCP / UART / LoRa.
- Running VLM (vision-language) and LLM (planning) inference via
  LM Studio, vLLM, or OpenAI-compatible APIs.
- Translating high-level missions into skill plans.
- Aggregating fleet telemetry.
- Hosting the operator dashboard, REST API, and Telegram bot.

The brain is **not** part of the kernel safety case. Its compromise
would not let an attacker bypass kernel-enforced safety (geofence,
ESTOP, max speed) — those run on the robot itself, in-kernel.

See [RFC-0017 — Brain role](../appendix/rfcs.md) for the full scope
boundary.

## Three-tier separation

PHANES's brain is split into:

1. **Generic framework** (`phanes-brain` repo) — protocol,
   `secure_channel`, plugin loader, REST scaffold, dashboard, fleet,
   sim adapters. Apache 2.0, public.
2. **Project-specific code** (your private `myrobots-stack` repo) —
   custom skills, robot-specific wiring, deployment configs, private
   models. Whatever license you prefer.

See [RFC-0018](../appendix/rfcs.md) for the rationale.
