# rBoot

The x86_64 and aarch64 UEFI bootloader for rCore / zCore OS.

## Build

```sh
cargo build --release --target x86_64-unknown-uefi
cargo build --release --target aarch64-unknown-uefi
```

The EFI binaries are located under `target/<target>/release/rboot.efi`.

## Example

See [`example-kernel/`](example-kernel/) for a minimal bare-metal kernel that boots via rboot and prints to serial.

Run `example-kernel/test.sh` to build and test in QEMU.
Run `example-kernel/test-aarch64.sh` for the equivalent aarch64 QEMU test.

## Configuration

Edit `rboot.conf` to configure the bootloader. See [`example-kernel/rboot.conf`](example-kernel/rboot.conf) for a working example. Available options:

- `kernel_path` - path to the kernel ELF binary
- `kernel_stack_address` - virtual address for the kernel stack
- `kernel_stack_size` - kernel stack size in 4KiB pages
- `physical_memory_offset` - virtual address offset for physical memory mapping
- `resolution` - graphic output resolution (e.g. `1024x768`)
- `initramfs` - path to the initial ramdisk image
- `cmdline` - kernel command line
- `uart_base` - UART physical address passed to an aarch64 kernel
- `gic_base` - GIC distributor physical address passed to an aarch64 kernel
- `firmware_type` - platform identifier passed to an aarch64 kernel

On aarch64, `physical_memory_offset` must be aligned to 512 GiB. The temporary
boot page tables derive RAM and device memory attributes from the UEFI memory
map before transferring control to the kernel.
