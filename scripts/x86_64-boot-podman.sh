#!/bin/zsh
# Boot x86_64-hello on a Linux QEMU inside podman.
#
# Why: QEMU 10.1 on macOS silently drops the PVH boot path even
# when the kernel ships a correctly-formed XEN_ELFNOTE_PHYS32_ENTRY
# note (verified via llvm-readobj — note shows up in PT_NOTE,
# QEMU still falls back to SeaBIOS).
#
# UPDATE 2026-05-17 (post-run): Linux QEMU 8.2 inside this
# podman container *also* drops to SeaBIOS. That means the bug
# isn't macOS-specific — our binary's PVH note isn't what QEMU
# expects to trigger pvh_load_kernel. Possible causes worth
# investigating in a dedicated session:
#   - ELF type is ET_EXEC vs what QEMU wants (some versions
#     require a specific entry point / e_phnum layout)
#   - Note offset vs file structure: ours sits at file offset
#     0x1000 inside a PT_LOAD that covers the same bytes; QEMU
#     may want PT_NOTE outside the load segment
#   - Note name padding ("Xen\0" — 4 bytes, but the spec hints
#     at an aligned-to-4 namesz that might want "Xen\0\0\0\0\0"
#     padding to 8 bytes)
# Compare against a known-good PVH kernel (e.g. Xen's stubdom)
# byte-for-byte to find the divergence.
#
# Usage:
#   scripts/x86_64-boot-podman.sh             # default 8 s run
#   DURATION=20 scripts/x86_64-boot-podman.sh # longer
#
# Prerequisites:
#   - podman installed (`brew install podman`)
#   - podman machine running (`podman machine init && podman machine start`)
#   - x86_64-hello built (`make x86_64-hello` from repo root)

set -u
set -o pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:${PATH:-}"

REPO="/Users/azor/Library/Mobile Documents/com~apple~CloudDocs/Development/ia/robot-os"
KERNEL_REL="crates/x86_64-hello/target/x86_64-unknown-none/release/x86_64_hello"
KERNEL_ABS="${REPO}/${KERNEL_REL}"
DURATION="${DURATION:-8}"

# Image: any Linux + qemu-system-x86_64 will do. We use a small
# Alpine-based image with qemu preinstalled to avoid an apt round-
# trip on every run.
IMAGE="${IMAGE:-tianon/qemu:8.2}"

if [[ ! -f "$KERNEL_ABS" ]]; then
    echo "[BOOT] kernel not built — run \`make x86_64-hello\` first"
    echo "[BOOT] expected at: $KERNEL_ABS"
    exit 10
fi

if ! command -v podman >/dev/null 2>&1; then
    echo "[BOOT] podman not on PATH"
    exit 11
fi

echo "[BOOT] booting $KERNEL_REL via $IMAGE (timeout=${DURATION}s)"

# `--rm` cleans the container, `-i` keeps stdout flowing,
# `-v ...:Z` is SELinux relabel-safe. `--platform linux/amd64`
# forces the right arch even on Apple Silicon.
podman run --rm -i \
    --platform linux/amd64 \
    -v "$KERNEL_ABS:/kernel:Z" \
    "$IMAGE" \
    timeout "$DURATION" qemu-system-x86_64 \
        -M q35 -cpu max -nographic -no-reboot \
        -kernel /kernel 2>&1 \
    | tee "${REPO}/build/x86_64-boot.log"

RC=${pipestatus[1]}
echo "[BOOT] qemu exit: $RC (124 = timeout = booted & ran)"
exit "$RC"
