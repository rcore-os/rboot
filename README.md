# rBoot

The x86_64 UEFI bootloader for rCore / zCore OS.

## Prerequisites

- Rust nightly toolchain (see `rust-toolchain.toml`)
- QEMU with x86_64 support
- OVMF firmware (`OVMF.fd`)

## Build

```sh
cargo build --release --target x86_64-unknown-uefi
```

The output EFI binary is at `target/x86_64-unknown-uefi/release/rboot.efi`.

## Run in QEMU

1. Create the ESP (EFI System Partition) directory:

```sh
mkdir -p esp/EFI/Boot esp/EFI/rCore
cp target/x86_64-unknown-uefi/release/rboot.efi esp/EFI/Boot/BootX64.efi
cp rboot.conf esp/EFI/Boot/rboot.conf
cp /path/to/kernel.elf esp/EFI/rCore/kernel.elf
```

2. Launch QEMU:

```sh
qemu-system-x86_64 \
  -machine q35 \
  -cpu qemu64 \
  -m 512M \
  -drive format=raw,if=pflash,readonly=on,file=OVMF.fd \
  -drive format=raw,file=fat:rw:esp \
  -nographic \
  -serial mon:stdio \
  -no-reboot
```

## Configuration

Edit `rboot.conf` to configure the bootloader. See the file for available options:

- `kernel_path` - path to the kernel ELF binary
- `kernel_stack_address` - virtual address for the kernel stack
- `kernel_stack_size` - kernel stack size in 4KiB pages
- `physical_memory_offset` - virtual address offset for physical memory mapping
- `resolution` - graphic output resolution (e.g. `1024x768`)
- `initramfs` - path to the initial ramdisk image
- `cmdline` - kernel command line
