# Security Audit — 2026-08-20

Nine parallel trails over **both project repos**: the kernel (`robot-os`) and
the brain (`../robot-brain`). Read-only; fixes were applied after, across
disjoint file tracks.

## How to Read This

- **CONFIRMED** = traced the data from attacker entry to dangerous use, citing
  both ends. **SUSPECTED** = looks wrong but path is not fully closed.
- **A reachable panic is a security finding in this project.** With
  `panic = "abort"` and `overflow-checks = true`, an arithmetic overflow or
  out-of-range index is a **full board restart**. In a robot, a spontaneous
  restart is a physical security event.
- *Latent* means the code is there but has no callers today. Included because
  the day it wires up, it will already be wrong.

---

## The Worst: Kernel/User Isolation

| # | Finding | Where | Status |
|---|---|---|---|
| 1 | **User tables share kernel's L1.** `load_elf` calls `copy_kernel_entries_to_user` on a newly created and **empty** table, so each L2 entry takes the wholesale branch and copies the *pointer* to kernel's L1. The merge branch never executes. Userspace links at `0x10000` (VPN2=0) and kernel maps CLINT at `0x0200_0000` (VPN2=0), so mapping user **writes L0 tables inside kernel's L1**. Consequences: user pages visible in kernel table; each new address space inherits the prior one; process B's `load_elf` can overwrite live process A's `.text`. | `sched/process.rs:93-99`, `mm/vmm.rs:681` | CONFIRMED — hand-verified |
| 2 | `load_elf` writes file bytes **to MMIO registers** (CLINT, virtio, PLIC, UART) using `translate`, which doesn't check U bit. An ELF with `p_vaddr = 0x0200_4000` injects arbitrary IPIs and `mtimecmp` in S mode. | `sched/process.rs:169,199` | CONFIRMED |
| 3 | `SYS_DRV_DMA_FREE` frees **any physical page**. Kernel pages are marked allocated, so bitmap check *passes*. Next alloc hands it out and **zeroes it**. `SYS_DRV_DMA_ALLOC` filters physical addresses to ring 3 and acts as a telescope sight. | `syscall/dispatch.rs:498,484` | CONFIRMED |
| 4 | `SYS_MUNMAP` unmaps **kernel table entries** (per #1). `munmap(0x1000_0000, 4096)` erases UART PTE; next `kprintln!` is fatal S-mode fault. `length = u64::MAX` additionally iterates ~2^52 times. | `syscall/handlers.rs:870` | CONFIRMED |
| 5 | **Four more sites with raw user pointers** — the same class fixed this morning in `copy_*_user`: `SYS_IPC_CALL` (read AND write), `SYS_IPC_REPLY`, `SYS_DNS_RESOLVE` (write 4 bytes to any address). Since `sstatus.SUM` never activates, only work against kernel — pure escape primitives with built-in DoS. | `syscall/dispatch.rs:176,194,210,404,412` | CONFIRMED |
| 6 | `SYS_IPC_MAP` maps shared memory **from any task** without ownership check, and `SYS_IPC_UNSHARE` gives **write-after-free** without race: you free the pages and your `USER_RW` PTEs stay alive. | `syscall/dispatch.rs:240,233` | CONFIRMED |
| 7 | Event ports: `SYS_PORT_UNBIND/WAIT/BIND` without ownership check. `port_owner()` exists with **zero callers**. | `syscall/dispatch.rs:687-723` | CONFIRMED |

**The pattern, which is the real finding:** this morning's W2-C3 fix closed the
`HANDLES` table. But `shm.rs`, `port.rs` and `io_ring.rs` have the **identical
form** — global table, owner as field, index chosen by user — and no checks.
All three `owner_task` fields exist and **none controls anything**. Fixed one
instance, not the class.

---

## Boot, OTA and Physical Access

| # | Finding | Where | Status |
|---|---|---|---|
| 8 | **OTA installs and activates unsigned kernels.** Only validates CRC-32 — a checksum the sender calculates — promotes image and changes boot slot. Nothing in tree writes a `.SIG`. Normal build → unauthenticated remote execution. Build with `secure-boot-enforced` → **permanent brick, even with legitimate update**. | `shell/lib.rs:1105,1289,1302` | CONFIRMED |
| 9 | **Secure boot test passes without running cryptography.** `tools/keys/*_pub.bin` is in `.gitignore` and untracked → `build.rs` falls to `[0u8; 32]` silently → `secure_boot_verify_slot_detailed` returns before reading signature. Gate asserts on `FATAL`, which that path produces anyway. **And passes for different reasons per machine.** | `ota/secure_boot.rs:373`, `ota/build.rs:56` | CONFIRMED — *was my own test this morning* |
| 10 | Anti-rollback defeated by `hdr.fw_version`, an **unsigned** header field. Floor also lives in BOOTMETA, which the USB path rewrites. | `shell/lib.rs:1196` | CONFIRMED |
| 11 | `boot_count += 1` on file value whose parser saturates to `u32::MAX` → panic on **every** boot. Irrecoverable loop from a text file. Legacy path doesn't even check CRC. | `ota/pure.rs:245` | CONFIRMED |
| 12 | DFU: zero-length DNLOAD **truncates a kernel slot to 0 bytes** and reports success. And unconditional `finish_sync()` on each GETSTATUS puts state machine in `Error` on legal requests — recovery path is broken by design. | `dfu_recovery.rs:248,292` | CONFIRMED (latent) |
| 13 | MSC: CBW length and address are parsed and **ignored**; the thirteen mismatch cases of BBB §6.7 don't exist. A read error mid-transfer sends **zeros under `CSW_STATUS_OK`**. | `msc/dispatch.rs:66`, `msc_gadget.rs:190` | CONFIRMED (latent) |

---

## Filesystem and Device Tree

| # | Finding | Where | Status |
|---|---|---|---|
| 14 | Cluster chains **with no cycle detection or bound**. `FAT[2] = 2` and kernel spins forever — and with sector cached it is a tight spin with no I/O or yield. Reachable from ring 3 `open()`. | `fs/fat32.rs:1226` and 6 more places | CONFIRMED |
| 15 | `first_sector + s` overflows u32 → panic → restart. Concrete image exists that passes all BPB validation and lands exactly at `0xFFFFFFFF`. | `fs/fat32.rs:504,1573` | CONFIRMED |
| 16 | Sector cache uses `u32::MAX` as "empty line" sentinel **and compares requested sector against it**, so reading sector `0xFFFFFFFF` returns `Ok` with 512 zeros without reaching block driver, which would have rejected it. This makes #15 deterministic. | `fs/fat32.rs:318,373` | CONFIRMED |
| 17 | Journal replay (LBA 1, inside volume exported by USB) trusts cluster number: zeros 4 bytes at arbitrary offset in each FAT copy, automatically on mount. | `fs/fat32.rs:224` | CONFIRMED |
| 18 | DTB header offsets never validated against blob → read ~4 GiB out, at early boot. `strlen`/`streq` have no bound. | `dtb/lib.rs:466,109` | CONFIRMED |

---

## Network — Only Remote Trail Without Physical Access

| # | Finding | Where | Status |
|---|---|---|---|
| 19 | **Half-open TCP slots never collected.** Four abandoned connections leave listener deaf until restart; eight exhaust `TCP_MAX_CONNS` and robot cannot *call* its brain either. Code comment says they are collected "on RTO" — they are not. | `net/tcp.rs:1534` | CONFIRMED |
| 20 | `SynSent` accepts SYN-ACK **without validating ACK**, and RST with any sequence. `SynRcvd` completes handshake with unvalidated ACK (blind spoofing). ISN randomization protects nothing because ISN is never checked on return. Aggravating: source port is `0xC000 + fd` — **16 possible values**. | `net/tcp.rs:1050,1080` | CONFIRMED |
| 21 | DNS: accepts responses from anyone, transaction ID `0x1234` incremented (no entropy, unlike DHCP which **was** hardened), and never compares question. One packet poisons next resolution for 5 minutes. | `net/dns.rs:83,176,223` | CONFIRMED |
| 22 | ARP learns from **unsolicited responses** despite comment claiming otherwise — no pending request state exists in whole file. | `net/arp.rs:127` | CONFIRMED |
| 23 | `Established` doesn't validate sequence on ACKs without payload: forged ACK with window 0 **stops transmission forever** with connection apparently alive. | `net/tcp.rs:1092` | CONFIRMED |
| 24 | Socket table is **global and unowned**: any task reads, writes, or closes another's connection, including OTA channel and brain link. | `net/socket.rs:59` | CONFIRMED |
| 25 | NTP sets clock from any host; SNTP anti-spoofing check is impossible because transmit timestamp is sent as zero. | `net/ntp.rs:191` | CONFIRMED |

---

## Cryptography

| # | Finding | Where | Status |
|---|---|---|---|
| 26 | **No forward secrecy.** `derive_ephemeral_priv` mixes PSK + `rdtime` + `rdcycle` + a salt that is *another* `rdtime`. Under the compromised-PSK model — the only one where forward secrecy means anything — everything reduces to timing on monotonic counters. **No TRNG in tree.** Estimate: 2^15–2^40 instead of 128 bits. Two kernel comments claim forward secrecy; they are false. | `behavior/encrypt_link.rs:28` | CONFIRMED (mechanism) |
| 27 | **Coalesced AEAD frames silently discarded.** `decrypt` checks `len < expected` but never `!=`, so if TCP delivers two frames together, second is thrown without error or log. A `PKT_ESTOP` after another command **is lost**. Is bug K-C3/C4 reappearing one layer further out, on emergency-stop path. | `crypto/secure_channel.rs:191` | CONFIRMED |
| 28 | No direction separation: both ends derive a single key pair, so a frame the kernel *sent* is valid *incoming* to the kernel. Today unexploitable only because packet types are disjoint — a convention holding up a cryptographic flaw. | `crypto/secure_channel.rs:107` | CONFIRMED |
| 29 | Zero key material zeroization. PSK also lives in three permanent `.bss` copies, two of them trivially reversible (`K ⊕ 0x36…`). | whole trail | CONFIRMED |
| 30 | PSK read in plaintext from `/fat/LINK.KEY`, on same volume USB gadget exports with unauthenticated `READ_10`. Physical access extracts it. | `main.rs:744`, `msc_gadget.rs` | CONFIRMED |

---

## The Brain (`../robot-brain`)

| # | Finding | Where | Status |
|---|---|---|---|
| 31 | **Actuation link has no authentication by default and boot does not protest.** Without `ROBOT_BRAIN_LINK_KEY` the only frame control is a CRC-8. Anyone opening TCP/9000 is accepted *as the robot* and receives all `ACTUATOR_CMD`. The asymmetry is the grave part: **both** HTTP levels refuse to boot without explicit decision — the highest-consequence link is the only one failing open. | `server.py:1385` | CONFIRMED |
| 32 | Unauthenticated STATUS packet **permanently corrupts security profile**: `robot_type` taken from wire and not reset between connections. Next real robot gets LAND/RTL on a wheeled chassis. | `server.py:1166,191` | CONFIRMED |
| 33 | Idle TCP connection blocks real robot indefinitely: `read_packet` has no `wait_for` and socket has no timeout. | `server.py:386` | CONFIRMED |
| 34 | Repo pushes operator to insecure config: `start_split.py` does `setdefault("ROBOT_BRAIN_ALLOW_INSECURE", "1")` **twice**, control plane listens on `0.0.0.0` even in insecure mode, and enabling authentication **breaks dashboard and data plane** because neither sends `Authorization` header. | `tools/start_split.py:104,347` | CONFIRMED |

**Clean in the brain, and verified:** zero committed secrets (also reviewed history),
zero `pickle`/`eval`/`yaml.load`/`shell=True`, packet parsers are genuinely bounded,
`hmac.compare_digest` in place, `/fleet/command` deliberately excludes `ACTUATOR_CMD`,
and dashboard escapes everything from the robot.

---

## What Turned Out To Be Well

Worth noting, because finding something is well costs as much as finding it is wrong:

- Typed `Cap<T>` path (generation per slot, Kani proofs) is solid, and denial
  order in degraded mode keeps those proofs valid.
- DHCP hardening (runtime XID, mandatory and bound server-id) is real and correct
  — unlike DNS, which was left out.
- Virtio rings, after prior fixes, hold up: `id` bounded, `used_idx` in range,
  `Acquire` fence in place, and the double-free reported once **does not exist**
  (guard is there).
- `verify_strict` in Ed25519 rejects non-canonical signatures and small-order
  points. Secure boot gate **is not** bypassable at runtime: setter exists but
  has zero callers and gate doesn't consult it.
- **No remotely reachable panic found** in the network stack.
- Topology signature verification **is not a stub** — it is real Ed25519. What
  happens is nobody calls it, because `CAPS.TOML` is never read anywhere:
  capabilities come from compiled Rust. The "writable FAT" threat model doesn't
  exist yet in this build.

---

## What the Owner Decides, Not an Agent

1. **OTA and signing.** Image installing without signature is not fixed by adding
   a check: must decide where `.SIG` comes from — does it travel in the flow?
   is a manifest signed? — and today `secure-boot-enforced` and OTA are
   **mutually incompatible**.
2. **Entropy.** No TRNG. Software PRNG here would be worse than the problem,
   because it would look like a fix.
3. **K-C5**, replay after reboot: persist counter (flash wear + rollback policy)
   versus require encryption mode.
4. **Direction separation in the KDF** changes frame format and **breaks the
   brain** if not changed in both repos at once.
