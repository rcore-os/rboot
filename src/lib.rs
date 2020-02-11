#![no_std]
#![deny(warnings)]

use core::fmt;
pub use uefi::proto::console::gop::ModeInfo;
pub use uefi::table::boot::{MemoryAttribute, MemoryDescriptor, MemoryMapIter, MemoryType};

/// This structure represents the information that the bootloader passes to the kernel.
#[repr(C)]
#[derive(Debug)]
pub struct BootInfo {
    pub memory_map: MemoryMap,
    /// The offset into the virtual address space where the physical memory is mapped.
    pub physical_memory_offset: u64,
    /// The graphic output information
    pub graphic_info: GraphicInfo,
    /// Physical address of ACPI2 RSDP
    pub acpi2_rsdp_addr: u64,
    /// The start physical address of initramfs
    pub initramfs_addr: u64,
    /// The size of initramfs
    pub initramfs_size: u64,
    /// Kernel command line
    pub cmdline: &'static str,
}

pub struct MemoryMap {
    pub iter: MemoryMapIter<'static>,
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

impl Clone for MemoryMap {
    fn clone(&self) -> Self {
        unsafe { core::ptr::read(self) }
    }
}

impl fmt::Debug for MemoryMap {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut f = f.debug_list();
        for mmap in self.clone().iter {
            f.entry(mmap);
        }
        f.finish()
    }
}
