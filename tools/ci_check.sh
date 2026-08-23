#!/usr/bin/env bash
# Robot OS — CI verification (D07)
#
# 1. Builds every feature combination for zero errors AND zero warnings.
# 2. Runs drone algorithm unit tests in crates/flight-sim on the host.
# 3. Boots the kernel in QEMU and asserts real runtime behaviour.
#
# Step 3 exists because steps 1-2 cannot see the failures that actually hurt:
# esp32c3 rotted for months while CI was green, and a network smoke silently
# stopped running because it raced the scheduler. A build-only gate dates a
# regression to "somewhere in the last N commits" instead of to one commit.
#
# Usage: ./tools/ci_check.sh
#        make ci
#        CI_SKIP_QEMU=1 ./tools/ci_check.sh   # explicit opt-out, see below

set -uo pipefail

CARGO="${CARGO:-cargo}"
QEMU="${QEMU:-qemu-system-riscv64}"
KERNEL="target/riscv64imac-unknown-none-elf/release/kernel"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

PASS=0
FAIL=0

ok()   { echo "ok";   PASS=$((PASS + 1)); }
bad()  { echo "FAIL"; FAIL=$((FAIL + 1)); }

# ── Builds ──────────────────────────────────────────────────────────────────
#
# Warnings count as failures: the header has always claimed "zero
# errors/warnings" while only ever grepping for errors.
build() {
    local label="$1"; shift
    printf "  %-26s" "${label}..."
    local out
    out="$("$CARGO" build "$@" 2>&1)"
    if printf '%s\n' "$out" | grep -qE "^error"; then
        bad; printf '%s\n' "$out" | grep -E "^error" | head -5
        return
    fi
    if printf '%s\n' "$out" | grep -qE "^warning: (unused|function|variable|field|constant)"; then
        bad; printf '%s\n' "$out" | grep -E "^warning:" | head -5
        return
    fi
    ok
}

# vf2/k1 must be built exactly as the Makefile ships them — with their linker
# script. Without it CI validates a binary nobody ever runs.
build_board() {
    local label="$1" feat="$2" ld="kernel/linker-$2.ld"
    printf "  %-26s" "${label}..."
    local out
    out="$(RUSTFLAGS="-C link-arg=-T$ld" "$CARGO" build --release --features "$feat" \
           --config "build.rustflags=['-C','link-arg=-T$ld']" 2>&1)"
    if printf '%s\n' "$out" | grep -qE "^error"; then
        bad; printf '%s\n' "$out" | grep -E "^error" | head -5
    else
        ok
    fi
}

# Run one host test crate.
#
# --release on purpose. `cargo test` defaults to the dev profile, which for
# this workspace is opt-level 1 and roughly 30x slower — enough that the
# wall-clock ceilings in regression-tests/src/host_microbench.rs (crc8 under
# 5 us, parse_packet under 5 us) fail on a machine that has no problem
# meeting them. The release profile keeps `overflow-checks = true`, so
# nothing is given up by running the tests optimised.
#
# The old body piped cargo into grep and tested GREP's status, which threw
# away cargo's exit code: a crate that died without printing a matching line
# was reported as a pass.
test_host() {
    local label="$1" crate="$2" out
    printf "  %-26s" "${label}..."
    if out=$( (cd "${crate}" && "$CARGO" test --release 2>&1) ) \
       && ! echo "$out" | grep -q "test result: FAILED"; then
        ok
    else
        bad
        echo "$out" | grep -m4 -E "^error|test result: FAILED|panicked at" \
            | sed 's/^/      /'
    fi
}

# ── QEMU ────────────────────────────────────────────────────────────────────
#
# There is no `timeout` binary on macOS, so every run is bounded here by a
# polling loop. Two rules learned the hard way:
#   * Build and launch are separate steps. Chaining them lets QEMU start on the
#     PREVIOUS binary when a build is slow or interrupted — that produces
#     confident diagnoses of bugs that do not exist.
#   * A panic and a timeout are both failures. Waiting only for the success
#     marker makes a crash indistinguishable from "still working".
# Disk images are PREREQUISITES, not part of the scenario under test: a
# failed image build must abort the whole gate loudly, never hand QEMU 32 MB
# of zeros. Learned 2026-08-23: `mkfs.fat` (dosfstools) lives in
# /opt/homebrew/sbin, a PATH without sbin made every regenerated image
# silently unformattable, and the result was NINE scenario FAILs that read
# exactly like kernel regressions in the exec path. The `>/dev/null 2>&1`
# on these make calls is fine for noise — swallowing the EXIT CODE was the
# bug this helper exists to prevent.
make_disk() {
    if ! make "$@" >/dev/null 2>&1; then
        echo ""
        echo "  FATAL: disk image build failed: make $*"
        echo "         mkfs.fat missing from PATH? dosfstools installs it in"
        echo "         /opt/homebrew/sbin — every userspace/secure-boot/link"
        echo "         scenario would fail on a zeroed image, so aborting the"
        echo "         gate here instead."
        exit 1
    fi
}

qemu_run() { # qemu_run <label> <success-marker> <timeout-s> <qemu-args...>
    local label="$1" marker="$2" limit="$3"; shift 3
    printf "  %-26s" "${label}..."
    local log; log="$(mktemp)"
    "$QEMU" -machine virt -nographic -bios default -kernel "$KERNEL" "$@" >"$log" 2>&1 &
    local pid=$!
    local i=0
    while [ "$i" -lt "$((limit * 2))" ]; do
        # -a: kernel logs carry stray NUL bytes, and without it grep reports
        # "binary file matches" instead of the lines we need to show.
        if grep -aqi "panic" "$log" 2>/dev/null; then
            kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
            bad; echo "      kernel panic:"; grep -ai -m3 "panic" "$log" | sed 's/^/      /'
            rm -f "$log"; return
        fi
        if grep -aq "$marker" "$log" 2>/dev/null; then
            kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
            ok; rm -f "$log"; return
        fi
        kill -0 "$pid" 2>/dev/null || break
        i=$((i + 1)); sleep 0.5
    done
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    bad; echo "      no '$marker' within ${limit}s — last lines:"
    tail -5 "$log" | sed 's/^/      /'
    rm -f "$log"
}

echo "=== Robot OS CI Check ==="
echo ""
echo "[1/4] Building all feature combinations..."

# Force the workspace crates to actually recompile. rustc only emits warnings
# when it compiles, so on a warm cache `cargo build` prints nothing and the
# warning gate below silently passes over real warnings — which is exactly what
# happened: four pre-existing unused-constant warnings sat unnoticed until an
# unrelated edit forced a rebuild. A gate that only works on a cold cache is
# not a gate.
find crates kernel -name '*.rs' -newermt '@0' -exec touch {} + 2>/dev/null || true

build       "default (QEMU)"      --release
build       "qemu"                --release --features qemu
build       "no-ml"               --release --features no-ml
build       "no-mmu"              --release --features no-mmu
build       "rvv"                 --release --features rvv
build       "net-smoke"           --release --features qemu,net-smoke
build       "tftp-smoke"          --release --features qemu,tftp-smoke
build       "dhcp-smoke"          --release --features qemu,dhcp-smoke
build       "pi-smoke"            --release --features qemu,pi-smoke
# La politica de secure boot se fija en tiempo de compilacion (RFC-0011), asi
# que si esta feature no se construye en CI puede romperse sin que nadie lo
# note — exactamente como se pudrio esp32c3.
build       "secure-boot-enforced" --release --features qemu,secure-boot-enforced
build       "link-auth-enforced"   --release --features qemu,link-auth-enforced
build       "link-encrypt-enforced" --release --features qemu,link-encrypt-enforced
build       "reflex-smoke"        --release --features qemu,reflex-smoke
# ipc-census is load-bearing diagnostics: the userspace-IPC scenario's
# comment prescribes it as the wedge-reproduction recipe (K-C25), so it must
# stay inside the cold-cache warnings gate or it rots silently.
build       "ipc-census"          --release --features qemu,ipc-census
build_board "vf2 (+linker)"       vf2
build_board "k1 (+linker)"        k1

echo ""
echo "[2/4] Running host test suites..."
# Every crate in the tree that carries #[test] functions. Until 2026-08-20
# this stage ran flight-sim and nothing else, so ~650 tests across 22 other
# crates -- including all 72 OTA tests and the 113 in regression-tests --
# compiled, passed, and were never once executed by the gate. That is the
# same shape of hole that let esp32c3 rot for months while CI stayed green:
# the work exists, everyone assumes it runs, nobody checked.
for c in flight-sim regression-tests ota-tests sched-policy-tests msc-tests \
         tftp-tests topology-tests config-tests dfu-tests crypto-tests \
         flight-math-tests abi-tests arch-api-tests gguf-tests efi-tests \
         multi-stream-tests drivers-api-tests encrypt-link-tests \
         cam-ring-tests dtb-tests aead-link-tests \
         cap-tests \
         ipc-fast-tests ipc-lease-tests ipc-chan-tests sched-wake-tests; do
    test_host "$c" "${REPO_ROOT}/crates/${c}"
done

# K-C5: encrypt-link-tests asserts the LINK_ENCRYPT_ENFORCED const in BOTH
# feature states, and the enforced arm only compiles under the feature —
# without this second run, the exact assertions written to prevent the
# secure-boot-style silently-absent-policy failure never execute. The plain
# run above covers the OFF state; this covers ON.
test_host_features() { # test_host_features <label> <crate-dir> <features>
    local label="$1" crate="$2" feats="$3" out
    printf "  %-26s" "${label}..."
    if out=$( (cd "${crate}" && "$CARGO" test --release --features "$feats" 2>&1) ) \
       && ! echo "$out" | grep -q "test result: FAILED"; then
        ok
    else
        bad
        echo "$out" | grep -m4 -E "^error|test result: FAILED|panicked at" \
            | sed 's/^/      /'
    fi
}
test_host_features "encrypt-link(enforced)" \
    "${REPO_ROOT}/crates/encrypt-link-tests" "enforced"

echo ""
echo "[3/4] Runtime verification in QEMU..."

if [ "${CI_SKIP_QEMU:-0}" = "1" ]; then
    echo "  skipped (CI_SKIP_QEMU=1)"
elif ! command -v "$QEMU" >/dev/null 2>&1; then
    # Not a silent skip: a missing emulator means the runtime gate did not run,
    # and pretending otherwise is how rot goes unnoticed for months.
    printf "  %-26sFAIL\n" "qemu availability..."
    echo "      '$QEMU' not found. Install it, set QEMU=<path>, or set"
    echo "      CI_SKIP_QEMU=1 to accept an unverified runtime."
    FAIL=$((FAIL + 1))
else
    "$CARGO" build --release --features qemu >/dev/null 2>&1
    qemu_run "boot + SMP scheduling" "Completed 2000 iterations" 60 -smp 4

    "$CARGO" build --release --features qemu,tftp-smoke >/dev/null 2>&1
    mkdir -p build/tftp && [ -f build/tftp/TFTP.BIN ] || \
        dd if=/dev/zero of=build/tftp/TFTP.BIN bs=256 count=1 2>/dev/null
    qemu_run "network: TFTP fetch" "TFTP] fetched" 60 \
        -netdev user,id=net0,tftp=build/tftp -device virtio-net-device,netdev=net0

    # DHCP against QEMU's built-in server. We had just hardened the XID and
    # server-id checks with no way to exercise them at all.
    "$CARGO" build --release --features qemu,dhcp-smoke >/dev/null 2>&1
    qemu_run "network: DHCP lease" "DHCPSMOKE] PASS" 90 \
        -netdev user,id=net0 -device virtio-net-device,netdev=net0

    # Userspace: ELF load from FAT32, exec into ring 3, and the syscall ABI
    # (getpid/write/brk/exit). SYSTEST.ELF sat unused on the disk image for
    # months — built, copied, never invoked.
    "$CARGO" build --release --features qemu >/dev/null 2>&1
    # Regenerate the image every run. The guest WRITES to this FAT32 (trajectory
    # CSV flush, CONFIG.INI), so a reused image is not the image the previous
    # run started from — the scenario stops being hermetic and starts failing
    # for reasons that have nothing to do with the change under test. `make`
    # alone will not rebuild it: the file exists and its prerequisites are older.
    rm -f build/disk-systest.img
    make_disk build/disk-systest.img
    # 180s, not 90: this is the heaviest scenario (boots a disk, mounts FAT32,
    # loads and execs a userspace ELF) and these bounds are WALL CLOCK, so they
    # have to absorb however loaded the machine is. Seen failing at 90s on a
    # busy host while passing in seconds on an idle one.
    qemu_run "userspace: syscall ABI" "SYSCALL_TEST] ALL PASSED" 180 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk-systest.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0

    # PiMutex donation with holder and waiter on ONE hart — the case the old
    # spinning implementation deadlocked on. Asserts the boost actually landed
    # and the owner returned to base priority, not merely that nothing hung.
    "$CARGO" build --release --features qemu,pi-smoke >/dev/null 2>&1
    # ONE hart on purpose. The property under test is two contenders sharing a
    # hart; with -smp 4 the pair can land on different harts despite the CPU
    # pin, the holder finishes in parallel, and there is no contention left to
    # measure — which the probe correctly reports as "no-boost" and which looks
    # like a regression. Single hart also matches the deployment that motivated
    # the fix.
    qemu_run "PiMutex donation (K-A14)" "PISMOKE] PASS" 90

    # ── Secure boot: all three verdicts, against a pinned TEST key ──────
    #
    # Until 2026-08-21 this was one scenario asserting "SECURE-BOOT] FATAL"
    # against an image with no signature sidecar, and it was green for the
    # wrong reason. `crates/ota/build.rs` embeds tools/keys/prod_pub.bin when
    # that file exists and QUIETLY falls back to an all-zero key when it does
    # not. With a zero key, `secure_boot_verify_slot_detailed()` returns
    # NoTrustedKey on its very first line — before read_sig_file, before
    # sig_parse_header, before sig_verify. Enforced + Unverified still prints
    # "SECURE-BOOT] FATAL", so the assertion passed with the Ed25519 code
    # never once executing. Worse, it passed for a DIFFERENT reason depending
    # on whether the developer happened to have a production key on disk:
    # green that cannot be reproduced cannot be debugged.
    #
    # Two things close that hole:
    #
    #   * PROD_PUBKEY_PATH is pinned to a generated TEST key, so neither the
    #     presence nor the absence of a real prod key on this machine can
    #     change the outcome. The path must be ABSOLUTE: cargo runs build
    #     scripts with CWD = the package root (crates/ota), so a repo-relative
    #     path resolves to crates/ota/tools/keys/... , misses, and takes the
    #     silent zero-key fallback — reintroducing the exact bug being fixed,
    #     invisibly.
    #
    #   * The assertions name the exact BootTrustReason. "Rejected" must not
    #     be allowed to mean "rejected because there is no key".
    #
    # Three scenarios because a signature check has three distinct verdicts
    # and the crypto only runs in two of them:
    #
    #   absent  — no .SIG on the volume. Bails before any crypto (that is the
    #             point: it is the control case, and on its own it proves
    #             nothing about Ed25519).
    #   valid   — image + matching signature. The verifier must run and ACCEPT.
    #   corrupt — well-formed RSIG, trusted pubkey, one flipped bit in the
    #             scalar s. The verifier must run and REJECT. A missing file
    #             does not exercise the curve arithmetic; a bad signature does.
    #             See tools/corrupt_sig.py for why s[0] and not any other byte.
    #
    # Same reasoning as the link-auth pair below: a gate observed only
    # refusing is indistinguishable from a gate wired to always refuse, and a
    # gate observed only accepting is indistinguishable from no gate at all.
    #
    # All three images are regenerated from scratch first, before any of them
    # boots — the guest writes to the FAT volume (BOOTMETA boot_count), and
    # disk-signed.img/disk-badsig.img are copies of disk.img, so building them
    # lazily between runs would fork them off a volume a previous scenario had
    # already scribbled on.
    SECBOOT_TEST_KEY="${REPO_ROOT}/tools/keys/test_pub.bin"
    rm -f build/disk.img build/disk-signed.img build/disk-badsig.img
    make_disk build/disk.img build/disk-signed.img build/disk-badsig.img
    PROD_PUBKEY_PATH="$SECBOOT_TEST_KEY" \
        "$CARGO" build --release --features qemu,secure-boot-enforced >/dev/null 2>&1

    # Prove the key actually made it into the binary BEFORE booting anything.
    # Without this, a broken PROD_PUBKEY_PATH degrades into three confusing
    # QEMU failures (or, historically, one confident false pass) instead of
    # one precise message. The check is a byte-scan of the ELF for the 32-byte
    # public key: SECURE_BOOT_PUBKEY is a const that gets promoted to .rodata,
    # so the bytes are there contiguously if — and only if — build.rs read the
    # file instead of taking its zero fallback.
    printf "  %-26s" "secure boot key embedded..."
    if python3 -c '
import sys
key = open(sys.argv[2], "rb").read()
img = open(sys.argv[1], "rb").read()
sys.exit(0 if (len(key) == 32 and any(key) and key in img) else 1)
' "$KERNEL" "$SECBOOT_TEST_KEY"; then
        ok
    else
        bad
        echo "      the secure-boot-enforced kernel does NOT carry the test"
        echo "      public key ($SECBOOT_TEST_KEY)."
        echo "      crates/ota/build.rs fell back to its all-zero key, which"
        echo "      short-circuits verification at NoTrustedKey — the three"
        echo "      scenarios below would then prove nothing about Ed25519."
        echo "      Usual causes: the key was never generated (python3 lacks"
        echo "      the 'cryptography' package, so tools/gen_test_key.py died"
        echo "      during the fixture build), or PROD_PUBKEY_PATH was passed"
        echo "      relative instead of absolute."
    fi

    # CONTROL: no .SIG at all. Asserts the reason as well as the refusal, so
    # this can no longer pass as "no trusted key".
    qemu_run "secure boot rejects unsigned" \
        "FATAL: slot A rejected — signature file absent" 120 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0

    # ACCEPT: the Ed25519 verifier runs over a 256 KiB image and succeeds.
    # This is the only scenario in the whole gate in which sig_verify() is
    # reached at all.
    qemu_run "secure boot accepts signed" "Slot A signature: verified" 120 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk-signed.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0

    # REJECT: same image, one bit flipped in the signature scalar. Must fail
    # with SignatureInvalid specifically — SignatureMalformed or
    # PubkeyMismatch here would mean the fixture is broken and the curve
    # arithmetic was skipped again.
    qemu_run "secure boot rejects bad sig" \
        "FATAL: slot A rejected — signature invalid for image contents" 120 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk-badsig.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0

    # Ring-3 capabilities. Until 2026-08-20 nothing ever called handle_grant,
    # so the handle table was permanently empty and cap_check denied every
    # hardware syscall from userspace. captest asserts BOTH halves: a granted
    # sensor/motor is usable, AND an ungranted GPIO/motor-id is still refused.
    # The negative half is what stops this passing on a kernel where someone
    # deleted cap_check outright.
    "$CARGO" build --release --features qemu >/dev/null 2>&1
    rm -f build/disk-captest.img
    make_disk build/disk-captest.img
    qemu_run "userspace: capabilities" "CAPTEST] ALL PASSED" 180 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk-captest.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0

    # Syscall latency microbenchmark from ring 3. Asserts only that it runs
    # to completion — deliberately NO timing threshold. A wall-clock ceiling
    # in a gate measures the host's load and the compiler's flags, not the
    # kernel: the three host_microbench failures found earlier today were
    # exactly that, failing under the dev profile and passing under release.
    # The numbers are printed for a human to compare across runs; the gate
    # only guarantees the path still works end to end.
    "$CARGO" build --release --features qemu >/dev/null 2>&1
    rm -f build/disk-latbench.img
    make_disk build/disk-latbench.img
    # Conformidad del ABI de syscalls desde ring 3. libsys y los handlers del
    # kernel habian divergido en una docena de firmas —exec, drv_mmap,
    # disk_read, readdir, pipe, toda la familia de servicios— y nada lo cazaba
    # porque ningun programa de userspace llamaba a ninguna de ellas. abitest
    # las llama. No necesita disco ni NIC, pero se le da disco porque el
    # autorun carga el ELF desde FAT32.
    "$CARGO" build --release --features qemu >/dev/null 2>&1
    rm -f build/disk-abitest.img
    make_disk build/disk-abitest.img
    qemu_run "userspace: ABI conformance" "ABITEST] ALL PASSED" 180 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk-abitest.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0

    qemu_run "userspace: syscall latency" "LATBENCH] DONE" 180 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk-latbench.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0

    # Ring-3 IPC probe. Until this scenario existed NO userspace program had
    # ever executed SYS_IPC_FAST_CALL/_ACCEPT/_REPLY, the shm/port/io_ring
    # ownership gates, or the typed Cap<T> family: the tree passed green over
    # code nothing exercised. ipctest exercises it and ASSERTS both halves of
    # every case — the legitimate caller works AND the stranger is denied —
    # because a positive case alone is indistinguishable from a gate that
    # always accepts, and a negative one alone from a gate that always
    # rejects.
    #
    # -smp 4 is NOT decorative: phase A depends on client and server running
    # on different harts to open the window between wake_fast_ipc_server()
    # and task_block() in SYS_IPC_FAST_CALL. With -smp 1 the race does not
    # exist and the phase proves nothing. Ring 3 has no syscall to read the
    # hart count, so the requirement lives here.
    #
    # No time threshold, same as latbench: a clock ceiling in a gate measures
    # host load, not the kernel. The 180 s limit is a hang detector, not a
    # measurement.
    #
    # EXPECTED STATE TODAY: usually green, NOT guaranteed — one wedge class
    # closed 2026-08-24 (K-C25), a second remains open (K-C26, below). A red
    # here is that open class striking inside the 180 s window (~1 in a few
    # runs); treat it as the documented open bug, not a regression. The
    # chain, for the record: the original failures (K-C11 fork regs,
    # server-side wakes) were closed by K-C17/K-C19 (state+stamp word) and
    # K-C24 ('Blocked does not mean parked'). What remained presented as
    # "~1 exchange/s throughput, hidden by ipc-trace" and was NOT slowness:
    # the QEMU `-icount` hunt (deterministic virtual time) showed phase A
    # completing 1600/1600 in ~30 s, and the extended ipc-census caught the
    # real residue — a K-C24 stamp landing between do_schedule's switch-away
    # sweep and context_switch.S clearing `context_saving` parks the task as
    # Blocked+WAKE_STAMP+saved, a state with NO consumer for one-shot wakes
    # (the fast-IPC reply fires exactly once). Clients wedged serially on
    # that, which read as throughput. Fix: the K-C25 reaper
    # (`sched_word::reap_orphaned_stamp` + `reap_stamped_sleepers()` on the
    # timer tick, counted as `late_dispatch` in wake_counters); 4 new host
    # tests in sched-wake-tests (75 total) pin the protocol, and the
    # falsified-hypothesis log (TCP handshake yield-storm — fixed anyway;
    # host App Nap — no effect) stays in docs/IPC_AUDIT_2026-08-22.md.
    #
    # OPEN — K-C26 (2026-08-24): a second, distinct wedge class. Terminal
    # census signature: the phase-A server `READY-UNQUEUED` (Ready, in no
    # queue, current on no hart) with all slots Pending and every client
    # asleep — the exact state K-C24 exists to prevent, arising from a
    # not-yet-identified genesis. Reproduce with the stock scenario command
    # + `--features qemu,ipc-census` (~1 wedge per handful of runs; good
    # runs finish in ~2 s), or with
    #   -accel tcg,thread=single -icount shift=3
    # for higher frequency. Read the last [IPC-CENSUS] block: slot states,
    # per-sleeper word/saving, per-hart currents. Next-session plan lives in
    # docs/IPC_AUDIT_2026-08-22.md's K-C25/K-C26 closure section.
    "$CARGO" build --release --features qemu >/dev/null 2>&1
    rm -f build/disk-ipctest.img
    make_disk build/disk-ipctest.img
    qemu_run "userspace: IPC" "IPCTEST] ALL PASSED" 180 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk-ipctest.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0

    # The reflex daemon must REACT, not merely start. A kernel task moves the
    # simulated rangefinder to 100 mm and then back to 1500 mm; reflex has to
    # trip and then recover. Asserting on "Clear" rather than the trigger is
    # deliberate: `overriding` is only ever set by a trigger, so that one
    # string proves both the reaction and the exit from it — and a daemon
    # wedged in override means motors held in reverse forever.
    "$CARGO" build --release --features qemu,reflex-smoke >/dev/null 2>&1
    rm -f build/disk-reflex.img
    make_disk build/disk-reflex.img
    qemu_run "userspace: reflex reacts" "reflex] Clear" 180 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk-reflex.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0

    # Brain-link authentication gate, BOTH directions. build/disk.img carries
    # no LINK.KEY, so the enforced kernel must refuse; build/disk-linkkey.img
    # carries one, so the same kernel must boot past the gate. Testing only the
    # refusal would pass just as happily on a gate wired to always refuse.
    "$CARGO" build --release --features qemu,link-auth-enforced >/dev/null 2>&1
    qemu_run "link auth rejects missing key" "SECCHAN] FATAL" 120 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0
    rm -f build/disk-linkkey.img
    make_disk build/disk-linkkey.img
    qemu_run "link auth accepts valid key" "brain link authenticated" 120 \
        -smp 4 -global virtio-mmio.force-legacy=false \
        -drive file=build/disk-linkkey.img,if=none,format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0

    # Two guests on one link: both ends are our stack, so this is the only
    # scenario that covers TCP handshake and RX checksum validation in both
    # directions. Has its own harness because it drives two QEMU processes.
    printf "  %-26s" "network: two-node TCP..."
    "$CARGO" build --release --features qemu,net-smoke >/dev/null 2>&1
    if QEMU="$QEMU" WAIT_SECS=90 bash "${REPO_ROOT}/tools/net_pair_smoke.sh" >/dev/null 2>&1; then
        ok
    else
        bad; echo "      re-run 'make qemu-net-pair' to see the logs"
    fi

    "$CARGO" build --release --features qemu >/dev/null 2>&1
fi

echo ""
echo "[4/4] Results: ${PASS} passed, ${FAIL} failed"

if [ "$FAIL" -gt 0 ]; then
    echo "=== CI FAILED ==="
    exit 1
fi

echo "=== All checks passed ==="
