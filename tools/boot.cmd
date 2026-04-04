# Robot OS — U-Boot boot script for A/B OTA slot selection.
#
# Compile to boot.scr with:
#   mkimage -C none -A riscv -T script -d tools/boot.cmd build/boot.scr
#
# Copy boot.scr + KERN_A.BIN + BOOTMETA to the FAT32 boot partition.
# U-Boot will execute boot.scr automatically if configured.

echo "[ROTA] Reading boot metadata..."
if fatload mmc 0:1 ${loadaddr} BOOTMETA 2>/dev/null; then
    env import -t ${loadaddr} ${filesize}
else
    echo "[ROTA] No BOOTMETA found — defaulting to slot A"
    setenv active_slot a
fi

if test "${active_slot}" = "b"; then
    echo "[ROTA] Booting slot B (KERN_B.BIN)"
    fatload mmc 0:1 ${kernel_addr_r} KERN_B.BIN
else
    echo "[ROTA] Booting slot A (KERN_A.BIN)"
    fatload mmc 0:1 ${kernel_addr_r} KERN_A.BIN
fi

booti ${kernel_addr_r} - ${fdt_addr}
