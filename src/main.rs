//! Simple ELF OS Loader on UEFI
//!
//! 1. Load config from "\EFI\Boot\rboot.conf"
//! 2. Load kernel ELF file
//! 3. Map ELF segments to virtual memory
//! 4. Map kernel stack and all physical memory
//! 5. Exit boot and jump to ELF entry

#![no_std]
#![no_main]
#![allow(warnings)]

#[macro_use]
extern crate alloc;
#[macro_use]
extern crate log;

use alloc::vec::Vec;
use core::arch::asm;
use core::ptr::NonNull;
use rboot::{BootInfo, GraphicInfo};
use uefi::boot::{self, AllocateType, MemoryDescriptor, MemoryType, ScopedProtocol};
use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned};
use uefi::prelude::*;
use uefi::proto::console::gop::GraphicsOutput;
use uefi::proto::media::file::*;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::table::cfg::ConfigTableEntry;
use x86_64::registers::control::*;
use x86_64::structures::paging::*;
use x86_64::{PhysAddr, VirtAddr};
use xmas_elf::ElfFile;

mod config;
mod page_table;

const CONFIG_PATH: &str = "\\EFI\\Boot\\rboot.conf";

#[entry]
fn efi_main() -> Status {
    info!("bootloader is running");

    let config = {
        let mut file = open_file(CONFIG_PATH);
        let buf = load_file(&mut file);
        config::Config::parse(buf)
    };

    let graphic_info = init_graphic(config.resolution);
    info!("config: {:#x?}", config);

    let acpi2_addr = system::with_config_table(|entries| {
        entries
            .iter()
            .find(|entry| entry.guid == ConfigTableEntry::ACPI2_GUID)
            .expect("failed to find ACPI 2 RSDP")
            .address
    });
    info!("acpi2: {:?}", acpi2_addr);

    let smbios_addr = system::with_config_table(|entries| {
        entries
            .iter()
            .find(|entry| entry.guid == ConfigTableEntry::SMBIOS_GUID)
            .expect("failed to find SMBIOS")
            .address
    });
    info!("smbios: {:?}", smbios_addr);

    let elf = {
        let mut file = open_file(config.kernel_path);
        let buf = load_file(&mut file);
        ElfFile::new(buf).expect("failed to parse ELF")
    };
    unsafe {
        ENTRY = elf.header.pt2.entry_point() as usize;
    }

    let (initramfs_addr, initramfs_size) = if let Some(path) = config.initramfs {
        let mut file = open_file(path);
        let buf = load_file(&mut file);
        (buf.as_ptr() as u64, buf.len() as u64)
    } else {
        (0, 0)
    };

    let mmap = boot::memory_map(MemoryType::LOADER_DATA).expect("failed to get memory map");
    let max_phys_addr = mmap
        .entries()
        .map(|m| m.phys_start + m.page_count * 0x1000)
        .max()
        .unwrap()
        .max(0x1_0000_0000); // include IOAPIC MMIO area

    let mut page_table = current_page_table();
    // root page table is readonly
    // disable write protect
    unsafe {
        Cr0::update(|f| f.remove(Cr0Flags::WRITE_PROTECT));
        Efer::update(|f| f.insert(EferFlags::NO_EXECUTE_ENABLE));
    }
    page_table::map_elf(&elf, &mut page_table, &mut UEFIFrameAllocator).expect("failed to map ELF");
    page_table::map_stack(
        config.kernel_stack_address,
        config.kernel_stack_size,
        &mut page_table,
        &mut UEFIFrameAllocator,
    )
    .expect("failed to map stack");
    page_table::map_physical_memory(
        config.physical_memory_offset,
        max_phys_addr,
        &mut page_table,
        &mut UEFIFrameAllocator,
    );
    // recover write protect
    unsafe {
        Cr0::update(|f| f.insert(Cr0Flags::WRITE_PROTECT));
    }

    info!("exit boot services");

    let mmap = unsafe { boot::exit_boot_services(None) };
    // NOTE: alloc & log can no longer be used

    let mut memory_map = Vec::with_capacity(128);
    for desc in mmap.entries() {
        memory_map.push(*desc);
    }

    // construct BootInfo
    let bootinfo = BootInfo {
        memory_map,
        physical_memory_offset: config.physical_memory_offset,
        graphic_info,
        acpi2_rsdp_addr: acpi2_addr as u64,
        smbios_addr: smbios_addr as u64,
        initramfs_addr,
        initramfs_size,
        cmdline: config.cmdline,
    };
    let stacktop = config.kernel_stack_address + config.kernel_stack_size * 0x1000;
    unsafe {
        jump_to_entry(&bootinfo, stacktop);
    }
}

/// Open file at `path`
fn open_file(path: &str) -> RegularFile {
    info!("opening file: {}", path);
    let handle =
        boot::get_handle_for_protocol::<SimpleFileSystem>().expect("failed to get FileSystem");
    let mut fs = boot::open_protocol_exclusive::<SimpleFileSystem>(handle)
        .expect("failed to open FileSystem");
    let mut buf = [0u16; 256];
    let path =
        uefi::CStr16::from_str_with_buf(path, &mut buf).expect("failed to convert path to ucs-2");
    let mut root = fs.open_volume().expect("failed to open volume");
    let handle = root
        .open(path, FileMode::Read, FileAttribute::empty())
        .expect("failed to open file");

    match handle.into_type().expect("failed to into_type") {
        FileType::Regular(regular) => regular,
        _ => panic!("Invalid file type"),
    }
}

/// Load file to new allocated pages
fn load_file(file: &mut RegularFile) -> &'static mut [u8] {
    info!("loading file to memory");
    let mut info_buf = [0u8; 0x100];
    let info = file
        .get_info::<FileInfo>(&mut info_buf)
        .expect("failed to get file info");
    let pages = info.file_size() as usize / 0x1000 + 1;
    let mem_start = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
        .expect("failed to allocate pages");
    let buf = unsafe { core::slice::from_raw_parts_mut(mem_start.as_ptr(), pages * 0x1000) };
    let len = file.read(buf).expect("failed to read file");
    &mut buf[..len]
}

/// If `resolution` is some, then set graphic mode matching the resolution.
/// Return information of the final graphic mode.
fn init_graphic(resolution: Option<(usize, usize)>) -> GraphicInfo {
    let handle =
        boot::get_handle_for_protocol::<GraphicsOutput>().expect("failed to get GraphicsOutput");
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(handle)
        .expect("failed to open GraphicsOutput");

    if let Some(resolution) = resolution {
        let mode = gop
            .modes()
            .find(|mode| {
                let info = mode.info();
                info.resolution() == resolution
            })
            .expect("graphic mode not found");
        info!("switching graphic mode");
        gop.set_mode(&mode).expect("Failed to set graphics mode");
    }
    GraphicInfo {
        mode: gop.current_mode_info(),
        fb_addr: gop.frame_buffer().as_mut_ptr() as u64,
        fb_size: gop.frame_buffer().size() as u64,
    }
}

/// Get current page table from CR3
fn current_page_table() -> OffsetPageTable<'static> {
    let p4_table_addr = Cr3::read().0.start_address().as_u64();
    let p4_table = unsafe { &mut *(p4_table_addr as *mut PageTable) };
    unsafe { OffsetPageTable::new(p4_table, VirtAddr::new(0)) }
}

/// Use `boot::allocate_pages()` as frame allocator
struct UEFIFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for UEFIFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let addr = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 1)
            .expect("failed to allocate frame");
        let frame = PhysFrame::containing_address(PhysAddr::new(addr.as_ptr() as u64));
        Some(frame)
    }
}

/// Jump to ELF entry according to global variable `ENTRY`
unsafe fn jump_to_entry(bootinfo: *const BootInfo, stacktop: u64) -> ! {
    asm!("mov rsp, {}; call {}", in(reg) stacktop, in(reg) ENTRY, in("rdi") bootinfo);
    loop {
        asm!("nop");
    }
}

/// The entry point of kernel, set by BSP.
static mut ENTRY: usize = 0;
