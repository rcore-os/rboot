#!/bin/bash
# End-to-end test for the aarch64 bootloader using QEMU's virt machine.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RBOOT_DIR="$(dirname "$SCRIPT_DIR")"
ESP_DIR="$SCRIPT_DIR/esp-aarch64"

if [ -n "$AARCH64_UEFI_FIRMWARE" ]; then
    FIRMWARE="$AARCH64_UEFI_FIRMWARE"
elif [ -f /usr/share/AAVMF/AAVMF_CODE.fd ]; then
    FIRMWARE=/usr/share/AAVMF/AAVMF_CODE.fd
elif [ -f /usr/share/qemu-efi-aarch64/QEMU_EFI.fd ]; then
    FIRMWARE=/usr/share/qemu-efi-aarch64/QEMU_EFI.fd
elif command -v brew >/dev/null 2>&1; then
    FIRMWARE="$(brew --prefix qemu)/share/qemu/edk2-aarch64-code.fd"
else
    echo "ERROR: aarch64 UEFI firmware not found"
    exit 1
fi

echo "=== Building aarch64 rboot ==="
cd "$RBOOT_DIR"
cargo build --release --target aarch64-unknown-uefi

echo "=== Building aarch64 example kernel ==="
cd "$SCRIPT_DIR"
cargo build --release --target aarch64-unknown-none-softfloat

echo "=== Preparing ESP ==="
rm -rf "$ESP_DIR"
mkdir -p "$ESP_DIR/EFI/Boot" "$ESP_DIR/EFI/zCore"
cp "$RBOOT_DIR/target/aarch64-unknown-uefi/release/rboot.efi" "$ESP_DIR/EFI/Boot/BootAA64.efi"
cp "$SCRIPT_DIR/rboot-aarch64.conf" "$ESP_DIR/EFI/Boot/rboot.conf"
cp "$SCRIPT_DIR/target/aarch64-unknown-none-softfloat/release/example-kernel" \
    "$ESP_DIR/EFI/zCore/example-kernel"

echo "=== Running aarch64 QEMU ==="
OUTPUT=$(timeout 15 qemu-system-aarch64 \
    -machine virt \
    -cpu cortex-a53 \
    -m 512M \
    -smp 1 \
    -nographic \
    -bios "$FIRMWARE" \
    -drive format=raw,file=fat:rw:"$ESP_DIR" \
    -nic none \
    -no-reboot \
    2>/dev/null || true)

echo "$OUTPUT"

if echo "$OUTPUT" | grep -q "rboot is working correctly"; then
    echo ""
    echo "=== TEST PASSED ==="
    exit 0
else
    echo ""
    echo "=== TEST FAILED ==="
    echo "Expected output containing 'rboot is working correctly'"
    exit 1
fi
