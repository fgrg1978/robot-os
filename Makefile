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
# E11.AQ3 — ring-3 GPIO driver (Rust no_std ELF, built standalone).
GPIO_DRV_DIR  := userspace/gpio_drv
GPIO_DRV_BUILT:= $(GPIO_DRV_DIR)/target/riscv64imac-unknown-none-elf/release/gpio_drv
GPIO_DRV_ELF  := build/gpio_drv.elf
# Three more standalone Rust ring-3 ELFs. They compiled and were never once
# executed: nothing built them (they are absent from the `userspace:` target
# below), so nothing copied them to a disk image and nothing could exec them.
# Same shape of hole SYSTEST.ELF sat in for months.
UHELLO_DIR    := userspace/uhello
UHELLO_BUILT  := $(UHELLO_DIR)/target/riscv64imac-unknown-none-elf/release/uhello
UHELLO_ELF    := build/uhello.elf
REFLEX_DIR    := userspace/reflex
REFLEX_BUILT  := $(REFLEX_DIR)/target/riscv64imac-unknown-none-elf/release/reflex
REFLEX_ELF    := build/reflex.elf
BRAINCLI_DIR  := userspace/brain_client
BRAINCLI_BUILT:= $(BRAINCLI_DIR)/target/riscv64imac-unknown-none-elf/release/brain_client
BRAINCLI_ELF  := build/brain_client.elf
# captest — ring-3 capability test (positive AND negative halves).
CAPTEST_DIR   := userspace/captest
CAPTEST_BUILT := $(CAPTEST_DIR)/target/riscv64imac-unknown-none-elf/release/captest
CAPTEST_ELF   := build/captest.elf
# latbench — ring-3 syscall latency microbenchmark.
LATBENCH_DIR  := userspace/latbench
LATBENCH_BUILT:= $(LATBENCH_DIR)/target/riscv64imac-unknown-none-elf/release/latbench
LATBENCH_ELF  := build/latbench.elf
# abitest — conformidad del ABI de syscalls desde ring 3.
ABITEST_DIR   := userspace/abitest
ABITEST_BUILT := $(ABITEST_DIR)/target/riscv64imac-unknown-none-elf/release/abitest
ABITEST_ELF   := build/abitest.elf
# ipctest — sonda de IPC desde ring 3: ida y vuelta del camino rapido,
# suplantacion de servidor, y las puertas de propiedad de shm/port/io_ring.
IPCTEST_DIR   := userspace/ipctest
IPCTEST_BUILT := $(IPCTEST_DIR)/target/riscv64imac-unknown-none-elf/release/ipctest
IPCTEST_ELF   := build/ipctest.elf

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

# Fleet profile (RFC-0026): gateway boards with ≥ 1 GiB RAM.
# Static tables + 256 MiB heap don't fit the default 8 MiB linker.
# linker-fleet.ld carves a 1022 MiB RAM region above OpenSBI.
FLEET_LINKER   := kernel/linker-fleet.ld
FLEET_RUSTFLAGS := -C link-arg=-T$(FLEET_LINKER)

.PHONY: all build build-rvv build-fleet clean qemu qemu-smp qemu-full-smp qemu-net-pair \
        qemu-rvv qemu-full-smp-rvv qemu-systest qemu-dhcp-smoke qemu-pi-smoke userspace syscall-test make-mlp make-gguf \
        vf2 flash-vf2 k1 flash-k1 k1-console ci

all: build

build:
	$(CARGO) build $(CARGO_FLAGS) --features qemu

# Build kernel with RVV 1.0 support (requires QEMU with -cpu rv64,v=true).
build-rvv:
	$(CARGO) build $(CARGO_FLAGS) --features rvv,qemu

# Build kernel with PROFILE_FLEET defconfig and the fleet linker script
# (1022 MiB RAM region for the 256 MiB heap + per-task tables that
# overflow the default 8 MiB linker).  This target is the kernel side of
# the RFC-0026 fleet defconfig and runs only on gateway boards with
# >= 1 GiB RAM.  Not part of the default `build` target — opt-in.
build-fleet:
	@$(MAKE) defconfig-fleet
	RUSTFLAGS="$(FLEET_RUSTFLAGS)" \
	$(CARGO) build $(CARGO_FLAGS) \
		$$(python3 tools/kconfig_to_cargo.py .config | tr -s ' ')
	@echo "[FLEET] kernel built — use a gateway board with >= 1 GiB RAM"

# Build the minimal hello.elf user-space test binary + GPIO ring-3 driver.
userspace: $(HELLO_ELF) $(SYSTEST_ELF) $(GPIO_DRV_ELF) \
           $(UHELLO_ELF) $(REFLEX_ELF) $(BRAINCLI_ELF) $(CAPTEST_ELF) \
           $(LATBENCH_ELF) $(ABITEST_ELF) $(IPCTEST_ELF)

# E11.AQ3 ring-3 driver — Rust no_std ELF.  Builds via the crate's own
# .cargo/config.toml which pins target=riscv64imac-unknown-none-elf and
# the user.ld linker script.  The output is copied (not stripped) into
# build/gpio_drv.elf for the disk image to pick up.
$(GPIO_DRV_ELF): $(GPIO_DRV_DIR)/src/main.rs $(GPIO_DRV_DIR)/Cargo.toml $(GPIO_DRV_DIR)/user.ld
	@mkdir -p build
	cd $(GPIO_DRV_DIR) && $(CARGO) +nightly build --release
	cp $(GPIO_DRV_BUILT) $@
	@echo "[USPACE] Built $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

# Same standalone pattern as gpio_drv: each crate carries its own
# .cargo/config.toml pinning the target, user.ld and build-std.
$(UHELLO_ELF): $(UHELLO_DIR)/src/main.rs $(UHELLO_DIR)/Cargo.toml $(UHELLO_DIR)/user.ld
	@mkdir -p build
	cd $(UHELLO_DIR) && $(CARGO) +nightly build --release
	cp $(UHELLO_BUILT) $@
	@echo "[USPACE] Built $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

$(REFLEX_ELF): $(REFLEX_DIR)/src/main.rs $(REFLEX_DIR)/Cargo.toml $(REFLEX_DIR)/user.ld
	@mkdir -p build
	cd $(REFLEX_DIR) && $(CARGO) +nightly build --release
	cp $(REFLEX_BUILT) $@
	@echo "[USPACE] Built $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

$(BRAINCLI_ELF): $(BRAINCLI_DIR)/src/main.rs $(BRAINCLI_DIR)/Cargo.toml $(BRAINCLI_DIR)/user.ld
	@mkdir -p build
	cd $(BRAINCLI_DIR) && $(CARGO) +nightly build --release
	cp $(BRAINCLI_BUILT) $@
	@echo "[USPACE] Built $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

$(CAPTEST_ELF): $(CAPTEST_DIR)/src/main.rs $(CAPTEST_DIR)/Cargo.toml $(CAPTEST_DIR)/user.ld
	@mkdir -p build
	cd $(CAPTEST_DIR) && $(CARGO) +nightly build --release
	cp $(CAPTEST_BUILT) $@
	@echo "[USPACE] Built $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

$(LATBENCH_ELF): $(LATBENCH_DIR)/src/main.rs $(LATBENCH_DIR)/Cargo.toml $(LATBENCH_DIR)/user.ld
	@mkdir -p build
	cd $(LATBENCH_DIR) && $(CARGO) +nightly build --release
	cp $(LATBENCH_BUILT) $@
	@echo "[USPACE] Built $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

$(ABITEST_ELF): $(ABITEST_DIR)/src/main.rs $(ABITEST_DIR)/Cargo.toml $(ABITEST_DIR)/user.ld
	@mkdir -p build
	cd $(ABITEST_DIR) && $(CARGO) +nightly build --release
	cp $(ABITEST_BUILT) $@
	@echo "[USPACE] Built $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

# NOTA: cargo NO rastrea los ficheros .ld. `user.ld` esta en los requisitos
# para que make relance cargo, pero cargo respondera "Fresh" y NO reenlazara.
# Tras tocar el linker script hay que forzar la recompilacion (touch al
# main.rs o `cargo clean -p ipctest`) y comprobar las cabeceras PT_LOAD del
# ELF resultante con `riscv64-unknown-elf-readelf -l build/ipctest.elf`:
# ningun PT_LOAD puede llevar W y X a la vez.
$(IPCTEST_ELF): $(IPCTEST_DIR)/src/main.rs $(IPCTEST_DIR)/Cargo.toml $(IPCTEST_DIR)/user.ld
	@mkdir -p build
	cd $(IPCTEST_DIR) && $(CARGO) +nightly build --release
	cp $(IPCTEST_BUILT) $@
	@echo "[USPACE] Built $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

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

# Boot with a disk whose CONFIG.INI autoruns SYSTEST.ELF instead of the GPIO
# driver, so the syscall test actually executes. It exercises the ring-3 path
# end to end: ELF load from FAT32, exec, getpid/write/brk/exit via ecall.
# Prints `[SYSCALL_TEST] ALL PASSED!` or `FAILED!`.
qemu-systest: build userspace build/disk-systest.img
	$(QEMU) $(QEMU_FLAGS) -kernel $(KERNEL_ELF) \
		-smp 4 \
		-global virtio-mmio.force-legacy=false \
		-drive file=build/disk-systest.img,if=none,format=raw,id=hd0 \
		-device virtio-blk-device,drive=hd0

# DHCP against QEMU's built-in user-mode server. Asserts we reach Bound and
# end up with an address from the 10.0.2.x pool, not just that the call
# returned. Prints `[DHCPSMOKE] PASS ...` or `FAIL <reason>`.
qemu-dhcp-smoke:
	$(CARGO) build --release --features qemu,dhcp-smoke
	$(QEMU) $(QEMU_FLAGS) -kernel $(KERNEL_ELF) \
		-netdev user,id=net0 -device virtio-net-device,netdev=net0

# K-A14 — PiMutex donation on a single hart: a low-priority holder and a
# higher-priority waiter pinned to the same CPU. The old spinning mutex
# deadlocked here; the waiter never released the hart, so the owner it had
# just boosted could not run. Prints `[PISMOKE] PASS ...` or `FAIL <reason>`.
qemu-pi-smoke:
	$(CARGO) build --release --features qemu,pi-smoke
	$(QEMU) $(QEMU_FLAGS) -kernel $(KERNEL_ELF)

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

# Two disk images from one recipe, differing only in which ELF `autorun` starts.
# disk.img keeps GPIODRV (the ring-3 driver demo); disk-systest.img runs the
# syscall test, which was built and copied onto the disk for months without
# anything ever invoking it.
build/disk.img:         AUTORUN_ELF := /fat/GPIODRV.ELF
build/disk-systest.img: AUTORUN_ELF := /fat/SYSTEST.ELF
build/disk-uhello.img:  AUTORUN_ELF := /fat/UHELLO.ELF
build/disk-reflex.img:  AUTORUN_ELF := /fat/REFLEX.ELF
build/disk-braincli.img: AUTORUN_ELF := /fat/BRAINCLI.ELF
build/disk-captest.img: AUTORUN_ELF := /fat/CAPTEST.ELF
build/disk-latbench.img: AUTORUN_ELF := /fat/LATBENCH.ELF
build/disk-abitest.img: AUTORUN_ELF := /fat/ABITEST.ELF
build/disk-ipctest.img: AUTORUN_ELF := /fat/IPCTEST.ELF

build/disk.img build/disk-systest.img build/disk-uhello.img \
build/disk-reflex.img build/disk-braincli.img build/disk-captest.img \
build/disk-latbench.img build/disk-abitest.img build/disk-ipctest.img: \
		$(HELLO_ELF) $(SYSTEST_ELF) $(GPIO_DRV_ELF) $(UHELLO_ELF) \
		$(REFLEX_ELF) $(BRAINCLI_ELF) $(CAPTEST_ELF) $(LATBENCH_ELF) $(ABITEST_ELF) \
		$(IPCTEST_ELF) \
		build/mlp.rmlp build/policy.gguf
	@mkdir -p build
	dd if=/dev/zero of=$@ bs=1M count=32
	mkfs.fat -F 32 -n "ROBTOS" $@
	@printf "Hello from Robot OS FAT32!\n" > /tmp/_robtos_hello.txt
	@printf "Robot OS Phase 18 — Persistent configuration + dynamic model loading\n" > /tmp/_robtos_readme.txt
	@printf "# Robot OS Configuration\nml_enabled=1\nlog_level=1\nmotor_max_speed=100\nwatchdog_ms=500\nnet_ip=10.0.2.15\nnet_gateway=10.0.2.2\nnet_mask=255.255.255.0\nbehavior_server_ip=10.0.2.2\nbehavior_server_port=9000\nbehavior_l1_enabled=1\nbehavior_l2_enabled=1\nbehavior_l3_enabled=1\nautorun=$(AUTORUN_ELF)\n" \
		> /tmp/_robtos_config.ini
	@printf "active_slot=a\nboot_count=0\nlast_good=a\nfw_version_a=0\nfw_version_b=0\n" \
		> /tmp/_robtos_bootmeta
	mcopy -i $@ /tmp/_robtos_hello.txt ::HELLO.TXT
	mcopy -i $@ /tmp/_robtos_readme.txt ::README.TXT
	mcopy -i $@ $(HELLO_ELF) ::HELLO.ELF
	mcopy -i $@ $(SYSTEST_ELF) ::SYSTEST.ELF
	mcopy -i $@ $(GPIO_DRV_ELF) ::GPIODRV.ELF
	mcopy -i $@ $(UHELLO_ELF) ::UHELLO.ELF
	mcopy -i $@ $(REFLEX_ELF) ::REFLEX.ELF
	mcopy -i $@ $(BRAINCLI_ELF) ::BRAINCLI.ELF
	mcopy -i $@ $(CAPTEST_ELF) ::CAPTEST.ELF
	mcopy -i $@ $(LATBENCH_ELF) ::LATBENCH.ELF
	mcopy -i $@ $(ABITEST_ELF) ::ABITEST.ELF
	mcopy -i $@ $(IPCTEST_ELF) ::IPCTEST.ELF
	mcopy -i $@ build/mlp.rmlp ::MLP.RMLP
	mcopy -i $@ build/policy.gguf ::POLICY.GGF
	mcopy -i $@ /tmp/_robtos_config.ini ::CONFIG.INI
	mcopy -i $@ /tmp/_robtos_bootmeta ::BOOTMETA
	@rm -f /tmp/_robtos_hello.txt /tmp/_robtos_readme.txt /tmp/_robtos_config.ini /tmp/_robtos_bootmeta
	@echo "[DISK] FAT32 image: $@ (autorun=$(AUTORUN_ELF))"

# Same image, plus a /fat/LINK.KEY, used to prove the `link-auth-enforced`
# gate ACCEPTS a valid key. A gate exercised only by its negative test is
# indistinguishable from a gate that always refuses, so CI runs both halves.
build/disk-linkkey.img: build/disk.img
	cp build/disk.img $@
	@dd if=/dev/urandom of=/tmp/_robtos_linkkey bs=32 count=1 2>/dev/null
	mcopy -i $@ /tmp/_robtos_linkkey ::LINK.KEY
	@rm -f /tmp/_robtos_linkkey
	@echo "[DISK] FAT32 image with LINK.KEY: $@"

# ── Secure-boot fixtures (Ed25519 accept / reject) ───────────────────────────
#
# `build/disk.img` carries no KERN_A.BIN and no KERN_A.SIG, so the only
# secure-boot scenario it can support is "signature file absent" — which
# `secure_boot_verify_slot_detailed()` answers before touching any crypto.
# These two images add the missing halves: one with a VALID signature (the
# Ed25519 verifier must run and accept) and one whose signature is
# mathematically wrong (the verifier must run and reject). Same argument as
# `build/disk-linkkey.img` above: a gate only ever observed refusing is
# indistinguishable from a gate wired to always refuse.
#
# The signing key is a TEST pair generated on demand into tools/keys/ and
# never committed — see tools/gen_test_key.py for why generating beats
# shipping a fixed pair. `tools/keys/.gitignore` already excludes both halves.
TEST_PRIV_KEY := tools/keys/test_priv.bin
TEST_PUB_KEY  := tools/keys/test_pub.bin

# One recipe, two outputs. gen_test_key.py is idempotent (keeps an intact
# private key, always re-derives the public half), so make invoking it once per
# target is harmless.
$(TEST_PRIV_KEY) $(TEST_PUB_KEY): tools/gen_test_key.py
	python3 tools/gen_test_key.py

# The signed slot payload. Content is arbitrary as far as Ed25519 cares, so it
# is generated rather than pulled from a build artifact: no dependency on
# riscv64-unknown-elf-objcopy (not installed everywhere), and no risk of the
# fixture changing size under us. Deterministic seed so a rebuild produces
# byte-identical output and cargo/make stay quiet.
#
# 256 KiB is chosen, not arbitrary: it is comfortably under both
# SECURE_BOOT_MAX_IMAGE_SIZE and MAX_VERIFY_SIZE (2 MiB each — exceeding either
# yields ImageTooLargeToVerify or a bogus SignatureInvalid), while spanning 64
# SECURE_BOOT_READ_CHUNK_SIZE reads, so the chunked `read_slot_image()` loop is
# exercised rather than short-circuited by a single-chunk file.
build/KERN_A.BIN:
	@mkdir -p build
	python3 -c "import random; random.seed(0x52424F53); open('build/KERN_A.BIN','wb').write(random.randbytes(262144))"
	@echo "[SECBOOT] slot payload: $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

build/KERN_A.SIG: build/KERN_A.BIN $(TEST_PRIV_KEY)
	python3 tools/sign_ota.py build/KERN_A.BIN --priv $(TEST_PRIV_KEY) --out $@

# Bad signature: well-formed RSIG header, trusted public key, wrong scalar.
# Anything cruder (missing file, broken magic, foreign key) is rejected before
# sig_verify is ever called. See tools/corrupt_sig.py.
build/KERN_BAD.SIG: build/KERN_A.SIG tools/corrupt_sig.py
	python3 tools/corrupt_sig.py build/KERN_A.SIG $@

# ACCEPT fixture: image + matching signature at the FAT volume ROOT. Root, not
# a /fat subdirectory — `secure_boot.rs` reaches the FAT32 driver directly
# rather than through the VFS mount point, and U-Boot's `fatload` (tools/boot.cmd)
# can only produce the root layout anyway.
build/disk-signed.img: build/disk.img build/KERN_A.BIN build/KERN_A.SIG
	cp build/disk.img $@
	mcopy -o -i $@ build/KERN_A.BIN ::KERN_A.BIN
	mcopy -o -i $@ build/KERN_A.SIG ::KERN_A.SIG
	@echo "[DISK] FAT32 image with signed slot A: $@"

# REJECT fixture: identical, except KERN_A.SIG has one flipped bit in s.
build/disk-badsig.img: build/disk-signed.img build/KERN_BAD.SIG
	cp build/disk-signed.img $@
	mcopy -o -i $@ build/KERN_BAD.SIG ::KERN_A.SIG
	@echo "[DISK] FAT32 image with CORRUPTED slot A signature: $@"

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

# DEV01 — TFTP fast-iteration netboot for the freshly-built kernel.
# `tftp-serve`: builds + raw-binarifies + serves on udp/$(TFTP_PORT).
# Default port is 6969 (unprivileged); override with TFTP_PORT=69 if
# you run as root.
TFTP_PORT ?= 6969
tftp-serve: $(OTA_BIN)
	python3 scripts/tftp_serve.py $(OTA_BIN) --port $(TFTP_PORT)

# DEV01.4 — QEMU built-in TFTP smoke. QEMU's user-mode network
# gateway (10.0.2.2) serves files from build/tftp/; the kernel,
# built with `--features tftp-smoke`, calls tftp_fetch at boot
# and prints `[TFTP] fetched N bytes ... OK`. No external server
# needed — `-netdev user,tftp=...` does it all in-process.
TFTP_SMOKE_DIR := build/tftp
TFTP_SMOKE_FILE := $(TFTP_SMOKE_DIR)/TFTP.BIN
TFTP_SMOKE_BYTES := 256

$(TFTP_SMOKE_FILE):
	@mkdir -p $(TFTP_SMOKE_DIR)
	@head -c $(TFTP_SMOKE_BYTES) /dev/urandom > $@
	@echo "[TFTP] payload $@ ($$(wc -c < $@ | tr -d ' ') bytes)"

# ── Two-node network smoke (DEV01.5) ─────────────────────────────────────────
# Boots two kernel instances wired by QEMU's `socket` net backend and asserts a
# 256-byte TCP payload round-trips byte-for-byte. Unlike qemu-tftp-smoke (UDP,
# one direction, peer is QEMU's own TFTP server) both ends here are our kernel,
# so it covers TCP handshake + RX checksum validation in both directions.
# Fails the build on FAIL, panic, or timeout.
qemu-net-pair:
	$(CARGO) build --release --features qemu,net-smoke
	QEMU=$(QEMU) bash tools/net_pair_smoke.sh

qemu-tftp-smoke: $(TFTP_SMOKE_FILE)
	$(CARGO) build --release --features qemu,tftp-smoke
	$(QEMU) $(QEMU_FLAGS) -kernel $(KERNEL_ELF) \
		-netdev user,id=net0,tftp=$(TFTP_SMOKE_DIR) \
		-device virtio-net-device,netdev=net0

# ── ISA aparcadas (2026-08-20) ───────────────────────────────────────────────
# aarch64 y x86_64 viven en newfeatures/ y NO están en el workspace ni en CI.
# Estos targets se conservan a propósito, apuntando a su nueva ruta: la línea
# de QEMU de abajo (EL2, gic-version=3, -smp 2 para B1.gic.smp) es conocimiento
# caro de reconstruir y no cuesta nada dejarlo escrito. No implican soporte.
# Ver newfeatures/REVISAR-arch.md.

# B1.boot — minimal aarch64 boot binary. Requires:
#   rustup target add aarch64-unknown-none-softfloat
#   (+ qemu-system-aarch64 in PATH)
AARCH64_HELLO_BIN := newfeatures/aarch64-hello/target/aarch64-unknown-none-softfloat/release/aarch64_hello

$(AARCH64_HELLO_BIN):
	cd newfeatures/aarch64-hello && $(CARGO) +nightly build --release \
	    -Z build-std=core,compiler_builtins \
	    -Z build-std-features=compiler-builtins-mem

qemu-aarch64-hello: $(AARCH64_HELLO_BIN)
	# Boots at EL2 (no virtualization=off) so HVC #0 → QEMU's
	# emulated PSCI. We drop to EL1 via _drop_to_el1 before any
	# GIC programming. `-smp 2` is required for B1.gic.smp.
	qemu-system-aarch64 -M virt,gic-version=3 \
	    -cpu cortex-a72 -smp 2 -nographic -kernel $(AARCH64_HELLO_BIN)

# B2.boot — minimal x86_64 boot binary (Multiboot1).
X86_64_HELLO_BIN := newfeatures/x86_64-hello/target/x86_64-unknown-none/release/x86_64_hello

$(X86_64_HELLO_BIN):
	cd newfeatures/x86_64-hello && $(CARGO) +nightly build --release \
	    -Z build-std=core,compiler_builtins \
	    -Z build-std-features=compiler-builtins-mem

qemu-x86_64-hello: $(X86_64_HELLO_BIN)
	qemu-system-x86_64 -M q35 -nographic -kernel $(X86_64_HELLO_BIN)

# ── x86_64-hello GRUB ISO boot path (task #152 workaround attempt) ──
#
# QEMU's `-kernel` loader rejects our 64-bit ELF binary, regardless of
# whether it carries PVH / multiboot1 / multiboot2 headers, on QEMU
# 10.1 macOS + Linux QEMU 8.2. The standard workaround is to wrap the
# kernel in a GRUB-bootable ISO so the multiboot1 magic gets handed to
# GRUB instead of QEMU's `-kernel` loader.
#
# Build path WORKS — xorriso reports a valid El-Torito + MBR + GPT
# bootable image. Boot path BLOCKED — SeaBIOS on macOS QEMU 10.1 fails
# with "Could not read from CDROM (code 0009)" on every ISO type
# tested (q35, pc, microvm, virtio-blk, ide, sata). Issue is in the
# QEMU/SeaBIOS combination shipped with brew qemu 10.1, not in our
# binary or ISO. Same ISO mounts + verifies fine via `xorriso -ls`.
#
# Prerequisites:  brew install xorriso x86_64-elf-grub
#
# Use this target to (a) verify the GRUB ISO build pipeline stays
# working for when a fixed QEMU lands, and (b) hand the ISO to a
# different VMM (UTM, VMware Fusion, virtualbox) that doesn't share
# the SeaBIOS CDROM bug.
PHANES_ISO := build/phanes_x86_64.iso

$(PHANES_ISO): $(X86_64_HELLO_BIN)
	@mkdir -p build/iso/boot/grub
	@cp $(X86_64_HELLO_BIN) build/iso/boot/phanes_x86_64.elf
	@printf 'set timeout=0\nset default=0\nmenuentry "PHANES x86_64" {\n    multiboot /boot/phanes_x86_64.elf\n    boot\n}\n' > build/iso/boot/grub/grub.cfg
	x86_64-elf-grub-mkrescue -o $(PHANES_ISO) build/iso

x86_64-hello-iso: $(PHANES_ISO)
	@echo "Built $(PHANES_ISO). To boot:"
	@echo "  qemu-system-x86_64 -M q35 -cdrom $(PHANES_ISO) -nographic"
	@echo "Note: on macOS QEMU 10.1 the boot itself fails at SeaBIOS"
	@echo "CDROM read — known environment bug (task #152)."

qemu-x86_64-iso: $(PHANES_ISO)
	qemu-system-x86_64 -M q35 -cpu max -nographic -no-reboot -cdrom $(PHANES_ISO)

# B2.target.spec — hard-float build of x86_64-hello. Uses the
# custom `targets/x86_64-phanes-kernel.json` spec which enables
# SSE+SSE2 and disables soft-float so impl Vector for X86_64
# emits real `xmm` instructions instead of scalar polyfill.
X86_64_HELLO_HARDFLOAT_BIN := newfeatures/x86_64-hello/target/x86_64-phanes-kernel/release/x86_64_hello

$(X86_64_HELLO_HARDFLOAT_BIN):
	cd newfeatures/x86_64-hello && \
	    RUSTFLAGS="-C link-arg=-T./x86_64-q35.ld -C relocation-model=static -C link-arg=--no-pie" \
	    $(CARGO) +nightly build --release \
	    --target ../../targets/x86_64-phanes-kernel.json \
	    -Z build-std=core,compiler_builtins \
	    -Z build-std-features=compiler-builtins-mem \
	    -Z unstable-options -Z json-target-spec

x86_64-hello-hardfloat: $(X86_64_HELLO_HARDFLOAT_BIN)
	@echo "Built $(X86_64_HELLO_HARDFLOAT_BIN) with SSE2 hard-float ABI."

# ── RFC-0026 Kconfig targets ─────────────────────────────────────────────────
# Phase C1 skeleton.  See docs/CONFIG.md (C7) for full documentation.
# Install kconfiglib first:
#   /opt/homebrew/bin/python3 -m pip install --user --break-system-packages kconfiglib

KCONFIG_CONFIG     ?= .config
KCONFIG_DEFCONFIG_DIR := defconfigs
PYTHON             ?= /opt/homebrew/bin/python3

.PHONY: menuconfig config nconfig oldconfig olddefconfig
menuconfig:
	$(PYTHON) -m menuconfig

config: menuconfig

nconfig:
	$(PYTHON) -m menuconfig --style=nconfig

oldconfig:
	$(PYTHON) -m oldconfig

olddefconfig:
	$(PYTHON) -m olddefconfig

defconfig-%:
	@cp $(KCONFIG_DEFCONFIG_DIR)/$*.config $(KCONFIG_CONFIG)
	@$(PYTHON) -m olddefconfig
	@echo "[CONFIG] active = $*"

.PHONY: savedefconfig
savedefconfig:
	$(PYTHON) -m savedefconfig --out $(KCONFIG_DEFCONFIG_DIR)/last_saved.config

$(KCONFIG_CONFIG):
	@echo "[CONFIG] no .config — falling back to edge defconfig"
	@$(MAKE) defconfig-edge

# ── CI: build all feature combinations (0 errors, 0 warnings). ───────────────
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
	@echo "Secure-boot fixtures (used by tools/ci_check.sh):"
	@echo "  build/disk-signed.img - FAT32 + KERN_A.BIN and a VALID KERN_A.SIG"
	@echo "  build/disk-badsig.img - same, with one bit flipped in the signature"
	@echo "  Boot either with --features qemu,secure-boot-enforced and"
	@echo "  PROD_PUBKEY_PATH=\$$PWD/tools/keys/test_pub.bin (absolute: cargo runs"
	@echo "  build scripts from crates/ota, so a relative path silently misses)."
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
