ARCH ?= x86_64
MODE ?= release
TARGET := $(ARCH)-unknown-uefi
EFI := target/$(TARGET)/$(MODE)/rboot.efi
OVMF := ovmf/$(ARCH).fd
ESP := esp
QEMU_ARGS := -m 512 -net none -smp cores=4 -nographic
#	-debugcon file:debug.log -global isa-debugcon.iobase=0x402

ifeq (${ARCH}, x86_64)
	BOOTEFI := BOOTX64.efi
else ifeq (${ARCH}, aarch64)
	BOOTEFI := BOOTAA64.efi
	TARGET := ${TARGET}.json
	QEMU_ARGS += -M virt -cpu cortex-a57
endif

BUILD_ARGS := -Z build-std=core,alloc --target $(TARGET)

ifeq (${MODE}, release)
	BUILD_ARGS += --release
endif

.PHONY: build run header asm doc

build:
	cargo build $(BUILD_ARGS)

clippy:
	cargo clippy $(BUILD_ARGS)

doc:
	cargo doc $(BUILD_ARGS)

uefi-run: build
	uefi-run \
		-b ${OVMF} \
		-q $(shell which qemu-system-x86_64) \
		$(EFI) \
		-- $(QEMU_ARGS)

run: build
	mkdir -p $(ESP)/EFI/Boot
	cp $(EFI) $(ESP)/EFI/Boot/${BOOTEFI}
	cp rboot.conf $(ESP)/EFI/Boot
	qemu-system-${ARCH} \
		-bios ${OVMF} \
		-drive format=raw,file=fat:rw:${ESP} \
		$(QEMU_ARGS)

header:
	rust-objdump -h $(EFI) | less

asm:
	rust-objdump -d $(EFI) | less
