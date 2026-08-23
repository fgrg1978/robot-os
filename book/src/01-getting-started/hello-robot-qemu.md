# Hello robot in QEMU

This chapter gets you from a freshly built kernel to a robot driving in
QEMU within ~5 minutes.

> Coming in Phase 1: the full step-by-step walkthrough with screenshots
> + brain orchestration. The skeleton below maps the final shape.

## Step 1 — Boot the kernel

```bash
scripts/qemu.sh
```

You should see the kernel banner, then the shell prompt.

## Step 2 — Connect the brain

In another terminal:

```bash
cd ../phanes-brain   # separate repo from Phase 1
python -m server
```

The brain auto-connects via TCP loopback. Status should be `ONLINE`.

## Step 3 — Drive

```bash
# In the brain's REST API:
curl -X POST http://localhost:8088/v1/skills/forward \
     -H 'Content-Type: application/json' \
     -d '{"distance_mm": 1000}'
```

The QEMU-simulated motors execute; the brain receives motion telemetry
in real time.

## Step 4 — Watch the safety layer

```bash
curl -X POST http://localhost:8088/v1/skills/forward \
     -d '{"distance_mm": 100000}'
```

Geofence activates; ESTOP halts within 50 ms. The kernel rejects the
overrun.

## Next

[Your first skill](./first-skill.md).
