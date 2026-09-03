#![no_std]

extern crate alloc;

use alloc::vec::Vec;
pub use uefi::boot::{MemoryAttribute, MemoryDescriptor, MemoryType};
pub use uefi::proto::console::gop::ModeInfo;

/// This structure represents the information that the bootloader passes to the kernel.
#[repr(C)]
#[derive(Debug)]
pub struct BootInfo {
    pub memory_map: Vec<MemoryDescriptor>,
    /// The offset into the virtual address space where the physical memory is mapped.
    pub physical_memory_offset: u64,
    /// The graphic output information
    pub graphic_info: GraphicInfo,
    /// Physical address of ACPI2 RSDP
    pub acpi2_rsdp_addr: u64,
    /// Physical address of SMBIOS
    pub smbios_addr: u64,
    /// The start physical address of initramfs
    pub initramfs_addr: u64,
    /// The size of initramfs
    pub initramfs_size: u64,
    /// Kernel command line
    pub cmdline: &'static str,
}

/// Graphic output information
#[derive(Debug, Copy, Clone)]
#[repr(C)]
pub struct GraphicInfo {
    /// Graphic mode
    pub mode: ModeInfo,
    /// Framebuffer base physical address
    pub fb_addr: u64,
    /// Framebuffer size
    pub fb_size: u64,
}

/// AArch64 Boot Info passed to zCore/rCore on AArch64
#[repr(C)]
#[derive(Debug)]
pub struct Aarch64BootInfo {
    /// Kernel command line
    pub cmdline: &'static str,
    /// Firmware type (e.g. "QEMU", "Raspi4")
    pub firmware_type: &'static str,
    /// UART base physical address
    pub uart_base: usize,
    /// GIC base physical address
    pub gic_base: usize,
    /// Physical to virtual mapping offset
    pub offset: usize,
}
