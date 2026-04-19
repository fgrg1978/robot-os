#!/bin/zsh
# UE04 — Build a UEFI-bootable .efi PE image of robot-os.
#
# Usage: ./scripts/build_uefi.sh [--platform vf2|k1|qemu]
#
# Produces:
#   target/uefi/BOOTRISCV64.EFI   — PE/COFF EFI Application
#   target/uefi/esp.img           — 64 MiB FAT32 ESP with BOOTRISCV64.EFI at /EFI/BOOT/
#
# Boot in QEMU+EDK2:
#   qemu-system-riscv64 -M virt -bios edk2-riscv64.fd \
#     -drive file=target/uefi/esp.img,format=raw,if=virtio
set -e

CARGO="$HOME/.cargo/bin/cargo"
OBJCOPY="/opt/homebrew/opt/llvm/bin/llvm-objcopy"
MKFS_FAT="/opt/homebrew/opt/dosfstools/sbin/mkfs.fat"
MCOPY="/opt/homebrew/bin/mcopy"
DD="/bin/dd"
REPO="/Users/azor/Library/Mobile Documents/com~apple~CloudDocs/Development/ia/robot-os"

PLATFORM="${1:-vf2}"
case "$PLATFORM" in
    --platform) PLATFORM="$2" ;;
esac
[[ "$PLATFORM" == "--platform" ]] && PLATFORM="$2"

cd "$REPO"

echo "=== Build with uefi feature ($PLATFORM) ==="
if [[ "$PLATFORM" == "qemu" ]]; then
    "$CARGO" build --release --features "uefi" 2>&1 | /usr/bin/tail -3
else
    "$CARGO" build --release --features "uefi,$PLATFORM" 2>&1 | /usr/bin/tail -3
fi

ELF="target/riscv64imac-unknown-none-elf/release/robot_os_kernel"
OUTDIR="target/uefi"
EFI_OUT="$OUTDIR/BOOTRISCV64.EFI"
ESP_IMG="$OUTDIR/esp.img"

/bin/mkdir -p "$OUTDIR"

echo ""
echo "=== Convert ELF → PE/COFF .efi ==="
# llvm-objcopy writes a PE/COFF EFI app when given efi-app-riscv64 target.
"$OBJCOPY" --target=efi-app-riscv64 "$ELF" "$EFI_OUT"
/bin/ls -la "$EFI_OUT"

echo ""
echo "=== Build ESP image (64 MiB FAT32) ==="
"$DD" if=/dev/zero of="$ESP_IMG" bs=1m count=64 2>/dev/null
if [[ -x "$MKFS_FAT" ]]; then
    "$MKFS_FAT" -F 32 -n ROBOT_EFI "$ESP_IMG" >/dev/null
elif /usr/bin/which mkfs.fat >/dev/null 2>&1; then
    /usr/bin/env mkfs.fat -F 32 -n ROBOT_EFI "$ESP_IMG" >/dev/null
else
    echo "warning: mkfs.fat not found; install dosfstools (brew install dosfstools)"
    echo "         ESP image will not be formatted."
fi

if [[ -x "$MCOPY" ]] && /usr/bin/which mmd >/dev/null 2>&1; then
    /usr/bin/env mmd -i "$ESP_IMG" ::/EFI 2>/dev/null || true
    /usr/bin/env mmd -i "$ESP_IMG" ::/EFI/BOOT 2>/dev/null || true
    "$MCOPY" -i "$ESP_IMG" "$EFI_OUT" ::/EFI/BOOT/BOOTRISCV64.EFI
    echo "✓ ESP image populated with /EFI/BOOT/BOOTRISCV64.EFI"
else
    echo "warning: mtools (mcopy/mmd) not found; install with 'brew install mtools'"
    echo "         to auto-populate the ESP image."
    echo "         Manually: mount '$ESP_IMG' and copy '$EFI_OUT' to /EFI/BOOT/"
fi

echo ""
echo "=== Summary ==="
echo "  EFI:  $EFI_OUT"
echo "  ESP:  $ESP_IMG"
echo ""
echo "Test in QEMU+EDK2 (requires edk2-riscv64.fd):"
echo "  qemu-system-riscv64 -M virt -bios edk2-riscv64.fd \\"
echo "      -drive file=$ESP_IMG,format=raw,if=virtio"
