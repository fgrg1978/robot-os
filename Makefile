# Robot OS (Rust) - Makefile wrapper
# Wraps cargo build + QEMU invocation

QEMU  := qemu-system-riscv64
QEMU_FLAGS := -machine virt -nographic -bios default
CARGO := $(shell command -v cargo 2>/dev/null || echo $(HOME)/.cargo/bin/cargo)

# Cargo output paths
TARGET := riscv64imac-unknown-none-elf
PROFILE ?= release
ifeq ($(PROFILE),release)
    CARGO_FLAGS := --release
    TARGET_DIR := target/$(TARGET)/release
else
    CARGO_FLAGS :=
    TARGET_DIR := target/$(TARGET)/debug
endif

KERNEL_ELF := $(TARGET_DIR)/kernel

# User-space toolchain
RISCV_AS  := riscv64-unknown-elf-as
RISCV_LD  := riscv64-unknown-elf-ld
HELLO_DIR     := userspace/hello
HELLO_ELF     := build/hello.elf
SYSTEST_DIR   := userspace/syscall_test
SYSTEST_ELF   := build/syscall_test.elf

# VisionFive 2 configuration (Phase 10)
# Override these from the command line as needed:
#   make vf2 VF2_SERIAL=/dev/tty.usbserial-XXXX
#   make flash-vf2 VF2_SD=/dev/disk4
VF2_SERIAL  ?= /dev/ttyUSB0
VF2_SD      ?= /dev/sdb       # SD card device on Linux; use diskN on macOS
VF2_BAUD    ?= 115200

# VF2 kernel ELF (built with vf2 feature + vf2 linker script)
VF2_LINKER   := kernel/linker-vf2.ld
VF2_RUSTFLAGS := -C link-arg=-T$(VF2_LINKER)
VF2_ELF      := target/$(TARGET)/release/kernel-vf2
VF2_BIN      := build/kernel-vf2.bin

# SpacemiT K1 (BananaPi BPI-F3) configuration (Phase B)
# Override from command line as needed:
#   make k1 K1_SERIAL=/dev/tty.usbserial-XXXX
#   make flash-k1 K1_SD=/dev/disk5
K1_SERIAL  ?= /dev/ttyUSB0
K1_SD      ?= /dev/sdb       # SD card device on Linux; use diskN on macOS
K1_BAUD    ?= 115200

# K1 kernel ELF (built with k1 feature + k1 linker script)
# K1 has native RVV 1.0 (VLEN=256) — k1 feature enables RVV code paths.
K1_LINKER   := kernel/linker-k1.ld
K1_RUSTFLAGS := -C link-arg=-T$(K1_LINKER)
K1_BIN      := build/kernel-k1.bin

# RVV: QEMU CPU model with Vector 1.0 extension (VLEN=128).
# Used by qemu-rvv and qemu-full-smp-rvv targets.
# K1 uses VLEN=256 natively (not QEMU emulated).
QEMU_RVV_CPU := rv64,v=true,vlen=128,vext_spec=v1.0

# ESP32-C3 configuration (Phase G2)
ESP32C3_LINKER   := kernel/linker-esp32c3.ld
ESP32C3_RUSTFLAGS := -C link-arg=-T$(ESP32C3_LINKER)

.PHONY: all build build-rvv clean qemu qemu-smp qemu-full-smp \
        qemu-rvv qemu-full-smp-rvv userspace syscall-test make-mlp make-gguf \
        vf2 flash-vf2 k1 flash-k1 k1-console esp32c3 ci

all: build

build:
	$(CARGO) build $(CARGO_FLAGS) --features qemu

# Build kernel with RVV 1.0 support (requires QEMU with -cpu rv64,v=true).
build-rvv:
	$(CARGO) build $(CARGO_FLAGS) --features rvv,qemu

# Build the minimal hello.elf user-space test binary.
userspace: $(HELLO_ELF) $(SYSTEST_ELF)

$(HELLO_ELF): $(HELLO_DIR)/hello.S $(HELLO_DIR)/user.ld
	@mkdir -p build
	$(RISCV_AS) -march=rv64imac -mabi=lp64 -o build/hello.o $(HELLO_DIR)/hello.S
	$(RISCV_LD) -T $(HELLO_DIR)/user.ld -o $@ build/hello.o
	@echo "[USPACE] Built $@"

# Build the syscall test user-space binary (Phase G3).
syscall-test: $(SYSTEST_ELF)

$(SYSTEST_ELF): $(SYSTEST_DIR)/test.S $(HELLO_DIR)/user.ld
	@mkdir -p build
	$(RISCV_AS) -march=rv64imac -mabi=lp64 -o build/syscall_test.o $(SYSTEST_DIR)/test.S
	$(RISCV_LD) -T $(HELLO_DIR)/user.ld -o $@ build/syscall_test.o
	@echo "[USPACE] Built $@"

make-gguf build/policy.gguf: tools/make_gguf.py
	@mkdir -p build
	python3 tools/make_gguf.py

clean:
	$(CARGO) clean
	rm -f build/hello.o $(HELLO_ELF) build/syscall_test.o $(SYSTEST_ELF) \
		build/mlp.rmlp build/policy.gguf build/disk.img

# Single CPU
qemu: build
	$(QEMU) $(QEMU_FLAGS) -kernel $(KERNEL_ELF)

# 4 CPUs (SMP testing)
qemu-smp: build
	$(QEMU) $(QEMU_FLAGS) -smp 4 -kernel $(KERNEL_ELF)

# Full: SMP + disk + network
qemu-full-smp: build userspace build/disk.img
	$(QEMU) $(QEMU_FLAGS) -kernel $(KERNEL_ELF) \
		-smp 4 \
		-global virtio-mmio.force-legacy=false \
		-drive file=build/disk.img,if=none,format=raw,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev user,id=net0,hostfwd=udp::5555-:5555,hostfwd=tcp::8080-:8080 \
		-device virtio-net-device,netdev=net0

# RVV: single CPU with Vector extension
qemu-rvv: build-rvv
	$(QEMU) $(QEMU_FLAGS) -cpu $(QEMU_RVV_CPU) -kernel $(KERNEL_ELF)

# RVV: 4 CPUs + disk + network + Vector extension
qemu-full-smp-rvv: build-rvv userspace build/disk.img
	$(QEMU) $(QEMU_FLAGS) -cpu $(QEMU_RVV_CPU) \
		-smp 4 \
		-global virtio-mmio.force-legacy=false \
		-drive file=build/disk.img,if=none,format=raw,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev user,id=net0,hostfwd=udp::5555-:5555,hostfwd=tcp::8080-:8080 \
		-device virtio-net-device,netdev=net0 \
		-kernel $(KERNEL_ELF)

# GDB debug
qemu-gdb: build
	$(QEMU) $(QEMU_FLAGS) -kernel $(KERNEL_ELF) -s -S

# Generate MLP weight file from the Python script (Phase 15).
# Writes tools/make_mlp.py output to build/mlp.rmlp (292 bytes).
make-mlp build/mlp.rmlp: tools/make_mlp.py
	@mkdir -p build
	python3 tools/make_mlp.py
	@echo "[ML] Weight file: build/mlp.rmlp ($$(wc -c < build/mlp.rmlp | tr -d ' ') bytes)"

build/disk.img: $(HELLO_ELF) $(SYSTEST_ELF) build/mlp.rmlp build/policy.gguf
	@mkdir -p build
	dd if=/dev/zero of=$@ bs=1M count=32
	mkfs.fat -F 32 -n "ROBTOS" $@
	@printf "Hello from Robot OS FAT32!\n" > /tmp/_robtos_hello.txt
	@printf "Robot OS Phase 18 — Persistent configuration + dynamic model loading\n" > /tmp/_robtos_readme.txt
	@printf "# Robot OS Configuration\nml_enabled=1\nlog_level=1\nmotor_max_speed=100\nwatchdog_ms=500\n" \
		> /tmp/_robtos_config.ini
	@printf "active_slot=a\nboot_count=0\nlast_good=a\nfw_version_a=0\nfw_version_b=0\n" \
		> /tmp/_robtos_bootmeta
	mcopy -i $@ /tmp/_robtos_hello.txt ::HELLO.TXT
	mcopy -i $@ /tmp/_robtos_readme.txt ::README.TXT
	mcopy -i $@ $(HELLO_ELF) ::HELLO.ELF
	mcopy -i $@ $(SYSTEST_ELF) ::SYSTEST.ELF
	mcopy -i $@ build/mlp.rmlp ::MLP.RMLP
	mcopy -i $@ build/policy.gguf ::POLICY.GGF
	mcopy -i $@ /tmp/_robtos_config.ini ::CONFIG.INI
	mcopy -i $@ /tmp/_robtos_bootmeta ::BOOTMETA
	@rm -f /tmp/_robtos_hello.txt /tmp/_robtos_readme.txt /tmp/_robtos_config.ini /tmp/_robtos_bootmeta
	@echo "[DISK] FAT32 image: $@ (HELLO.TXT + README.TXT + HELLO.ELF + SYSTEST.ELF + MLP.RMLP + POLICY.GGF + CONFIG.INI + BOOTMETA)"

# ── VisionFive 2 targets ──────────────────────────────────────────────────────

# Build kernel for VisionFive 2 (JH7110) with vf2 feature + linker script.
vf2: userspace
	RUSTFLAGS="$(VF2_RUSTFLAGS)" \
	$(CARGO) build --release --features vf2 \
		--config "build.rustflags=['-C','link-arg=-T$(VF2_LINKER)']"
	@mkdir -p build
	riscv64-unknown-elf-objcopy -O binary \
		target/$(TARGET)/release/kernel $(VF2_BIN)
	@echo "[VF2] Built $(VF2_BIN)"
	@ls -lh $(VF2_BIN)

# Flash kernel.bin to SD card for VisionFive 2 boot.
# Boot flow: U-Boot SPL → OpenSBI → U-Boot proper → kernel.bin from SD fat32 /boot/
flash-vf2: vf2
	@echo "[VF2] Flashing $(VF2_BIN) to SD card $(VF2_SD)"
	@echo "  Make sure $(VF2_SD)1 is FAT32 and mounted at /mnt/vf2boot (or adjust)"
	@if [ -b "$(VF2_SD)1" ]; then \
		mkdir -p /mnt/vf2boot && \
		mount $(VF2_SD)1 /mnt/vf2boot && \
		cp $(VF2_BIN) /mnt/vf2boot/kernel.bin && \
		umount /mnt/vf2boot && \
		echo "[VF2] kernel.bin written to $(VF2_SD)1:/kernel.bin"; \
	else \
		echo "[VF2] $(VF2_SD)1 not found — copy $(VF2_BIN) manually to the SD FAT32 partition"; \
	fi

# Open serial console to VF2 (requires minicom or picocom).
vf2-console:
	@echo "[VF2] Opening $(VF2_SERIAL) at $(VF2_BAUD) baud (Ctrl+A X to exit)"
	picocom -b $(VF2_BAUD) $(VF2_SERIAL) || \
	minicom -b $(VF2_BAUD) -D $(VF2_SERIAL)

# ── SpacemiT K1 (BananaPi BPI-F3) targets ────────────────────────────────────

# Build kernel for K1 (RV64GCVB + native RVV 1.0, VLEN=256).
# The k1 feature automatically enables RVV code paths.
k1: userspace
	RUSTFLAGS="$(K1_RUSTFLAGS)" \
	$(CARGO) build --release --features k1 \
		--config "build.rustflags=['-C','link-arg=-T$(K1_LINKER)']"
	@mkdir -p build
	riscv64-unknown-elf-objcopy -O binary \
		target/$(TARGET)/release/kernel $(K1_BIN)
	@echo "[K1] Built $(K1_BIN)"
	@ls -lh $(K1_BIN)

# Flash kernel.bin to SD card for K1 boot.
# Boot flow: Boot ROM → U-Boot SPL → OpenSBI → U-Boot proper → kernel.bin
# K1 U-Boot expects the kernel at offset 0x200000 in the boot partition.
flash-k1: k1
	@echo "[K1] Flashing $(K1_BIN) to SD card $(K1_SD)"
	@echo "  Make sure $(K1_SD)1 is FAT32 and mounted at /mnt/k1boot (or adjust)"
	@if [ -b "$(K1_SD)1" ]; then \
		mkdir -p /mnt/k1boot && \
		mount $(K1_SD)1 /mnt/k1boot && \
		cp $(K1_BIN) /mnt/k1boot/kernel.bin && \
		umount /mnt/k1boot && \
		echo "[K1] kernel.bin written to $(K1_SD)1:/kernel.bin"; \
	else \
		echo "[K1] $(K1_SD)1 not found — copy $(K1_BIN) manually to the SD FAT32 partition"; \
	fi

# Open serial console to K1 (requires minicom or picocom).
k1-console:
	@echo "[K1] Opening $(K1_SERIAL) at $(K1_BAUD) baud (Ctrl+A X to exit)"
	picocom -b $(K1_BAUD) $(K1_SERIAL) || \
	minicom -b $(K1_BAUD) -D $(K1_SERIAL)

# ── ESP32-C3 targets (Phase G2 — skeleton only) ─────────────────────────────

# Build kernel for ESP32-C3 (RV32IMC, no MMU, no ML).
# NOTE: Skeleton only — requires ESP32-C3 target triple (riscv32imc-unknown-none-elf)
#       and custom boot.S / UART driver for actual hardware.
ESP32C3_TARGET := riscv32imac-unknown-none-elf

esp32c3:
	$(CARGO) build --release --features esp32c3 --target $(ESP32C3_TARGET)

# OTA: send firmware to robot over TCP.
# Usage: make ota-send ROBOT=10.0.2.15 [PORT=8080] [PLATFORM=qemu] [FW_VER=1]
ROBOT    ?= 10.0.2.15
PORT     ?= 8080
PLATFORM ?= qemu
FW_VER   ?= 1

# OTA needs raw binary, not ELF (ELF has debug info = too large).
OTA_BIN := build/kernel-ota.bin

$(OTA_BIN): build
	@mkdir -p build
	riscv64-unknown-elf-objcopy -O binary $(KERNEL_ELF) $@
	@echo "[OTA] Raw binary: $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

ota-send: $(OTA_BIN)
	python3 tools/ota_send.py $(OTA_BIN) $(ROBOT) \
		--port $(PORT) --platform $(PLATFORM) --version $(FW_VER)

# Generate U-Boot boot script for A/B OTA slot selection (VF2/K1).
boot-scr: tools/boot.cmd
	@mkdir -p build
	mkimage -C none -A riscv -T script -d tools/boot.cmd build/boot.scr
	@echo "[OTA] boot.scr generated"

# CI: build all feature combinations (0 errors, 0 warnings).
ci:
	@bash tools/ci_check.sh

# Full CI: robot-os builds + robot-brain tests + protocol sync.
ci-full:
	@bash tools/ci_full.sh

help:
	@echo "Robot OS (Rust) Build System"
	@echo "============================"
	@echo ""
	@echo "Targets:"
	@echo "  build         - Build kernel (default)"
	@echo "  userspace     - Build user-space ELF binaries (hello.elf)"
	@echo "  make-mlp      - Generate build/mlp.rmlp weight file (Phase 15)"
	@echo "  clean         - Clean build artifacts"
	@echo "  qemu              - Run in QEMU (1 CPU)"
	@echo "  qemu-smp          - Run in QEMU (4 CPUs)"
	@echo "  qemu-full-smp     - Run in QEMU (4 CPUs + disk + net + hello.elf)"
	@echo "  qemu-gdb          - Run in QEMU with GDB server"
	@echo "  qemu-rvv          - Run in QEMU with RVV 1.0 (1 CPU, --features rvv)"
	@echo "  qemu-full-smp-rvv - RVV 1.0 + 4 CPUs + disk + net"
	@echo ""
	@echo "Options:"
	@echo "  PROFILE=debug   - Build in debug mode (default: release)"
	@echo ""
	@echo "VisionFive 2 targets:"
	@echo "  vf2           - Build kernel for VisionFive 2 (JH7110)"
	@echo "  flash-vf2     - Flash kernel.bin to SD card"
	@echo "  vf2-console   - Open UART serial console"
	@echo ""
	@echo "VF2 options:"
	@echo "  VF2_SD=<dev>     SD card device  (default: /dev/sdb)"
	@echo "  VF2_SERIAL=<dev> UART device     (default: /dev/ttyUSB0)"
	@echo ""
	@echo "SpacemiT K1 targets (Phase B):"
	@echo "  k1             - Build kernel for K1 (RV64GCVB + RVV1.0 VLEN=256)"
	@echo "  flash-k1       - Flash kernel.bin to SD card"
	@echo "  k1-console     - Open UART serial console"
	@echo ""
	@echo "K1 options:"
	@echo "  K1_SD=<dev>     SD card device  (default: /dev/sdb)"
	@echo "  K1_SERIAL=<dev> UART device     (default: /dev/ttyUSB0)"
	@echo ""
	@echo "ESP32-C3 targets (Phase G2 — skeleton):"
	@echo "  esp32c3        - Build kernel for ESP32-C3 (RV32IMC, no MMU/ML)"
