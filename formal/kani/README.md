# Kani harnesses

Harnesses live **in-tree** under `cfg(kani)` (so they don't affect normal
builds) and are listed here for the CI gate. To run:

```bash
cargo install --locked kani-verifier
cargo kani setup
cargo kani --target-dir target/kani --harness <name> -p <crate>
```

| Harness                           | Crate            | Property                          |
|-----------------------------------|------------------|-----------------------------------|
| `cap_forge_impossible`            | `robot_os_ipc`   | RFC-0003: forged caps fail get()  |
| `cap_revoked_stale`               | `robot_os_ipc`   | RFC-0003: revoked caps stay stale |
| `cap_perms_required`              | `robot_os_ipc`   | RFC-0003: missing perms rejected  |

Each harness must terminate within ~60 s on default Kani settings; if it
balloons, bound the loop in the harness rather than turning Kani off.
