//! Simple ELF OS Loader on UEFI
//!
//! 1. Load config from "\EFI\Boot\rboot.conf"
//! 2. Load kernel ELF file
//! 3. Map ELF segments and physical memory
//! 4. Exit boot and jump to ELF entry

#![no_std]
#![no_main]

extern crate alloc;

use log::info;
use uefi::boot::{self, AllocateType, MemoryType};
use uefi::prelude::*;
use uefi::proto::console::gop::GraphicsOutput;
use uefi::proto::media::file::*;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::table::cfg::ConfigTableEntry;
use xmas_elf::ElfFile;

mod arch;
mod config;

const CONFIG_PATH: &str = "\\EFI\\Boot\\rboot.conf";

#[entry]
fn efi_main() -> Status {
    uefi::helpers::init().expect("failed to init uefi helpers");
    info!("rboot bootloader is running");

    let config = {
        if let Some(mut file) = try_open_file(CONFIG_PATH) {
            let buf = load_file(&mut file);
            config::Config::parse(buf)
        } else {
            info!("config file not found, using default config");
            config::Config::parse(b"")
        }
    };
    info!("config: {:#x?}", config);

    let graphic_info = init_graphic(config.resolution);

    let acpi2_addr = system::with_config_table(|entries| {
        entries
            .iter()
            .find(|entry| entry.guid == ConfigTableEntry::ACPI2_GUID)
            .map(|entry| entry.address)
    })
    .unwrap_or(core::ptr::null());
    info!("acpi2: {:?}", acpi2_addr);

    let smbios_addr = system::with_config_table(|entries| {
        entries
            .iter()
            .find(|entry| entry.guid == ConfigTableEntry::SMBIOS_GUID)
            .map(|entry| entry.address)
    })
    .unwrap_or(core::ptr::null());
    info!("smbios: {:?}", smbios_addr);

    let elf_file_buf = {
        let mut file = if let Some(file) = try_open_file(config.kernel_path) {
            file
        } else if let Some(file) = try_open_file("\\os") {
            file
        } else if let Some(file) = try_open_file("\\EFI\\Boot\\os") {
            file
        } else if let Some(file) = try_open_file("\\EFI\\zCore\\zcore.elf") {
            file
        } else {
            panic!("failed to open kernel ELF file: {}", config.kernel_path);
        };
        load_file(&mut file)
    };
    let elf = ElfFile::new(elf_file_buf).expect("failed to parse kernel ELF");

    let (initramfs_addr, initramfs_size) = if let Some(path) = config.initramfs {
        if let Some(mut file) = try_open_file(path) {
            let buf = load_file(&mut file);
            (buf.as_ptr() as u64, buf.len() as u64)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    #[cfg(target_arch = "x86_64")]
    {
        use alloc::vec::Vec;
        use rboot::BootInfo;
        use uefi::mem::memory_map::MemoryMap;
        use x86_64::registers::control::*;

        let entry = elf.header.pt2.entry_point() as usize;

        let mmap = boot::memory_map(MemoryType::LOADER_DATA).expect("failed to get memory map");
        let max_phys_addr = mmap
            .entries()
            .map(|m| m.phys_start + m.page_count * 0x1000)
            .max()
            .unwrap()
            .max(0x1_0000_0000);

        let mut page_table = arch::current_page_table();
        unsafe {
            Cr0::update(|f| f.remove(Cr0Flags::WRITE_PROTECT));
            Efer::update(|f| f.insert(EferFlags::NO_EXECUTE_ENABLE));
        }
        arch::map_elf(&elf, &mut page_table, &mut arch::UEFIFrameAllocator)
            .expect("failed to map ELF");
        arch::map_stack(
            config.kernel_stack_address,
            config.kernel_stack_size,
            &mut page_table,
            &mut arch::UEFIFrameAllocator,
        )
        .expect("failed to map stack");
        arch::map_physical_memory(
            config.physical_memory_offset,
            max_phys_addr,
            &mut page_table,
            &mut arch::UEFIFrameAllocator,
        );
        let gdt = arch::prepare_gdt(config.physical_memory_offset);
        unsafe {
            Cr0::update(|f| f.insert(Cr0Flags::WRITE_PROTECT));
        }

        let stacktop = config.kernel_stack_address + config.kernel_stack_size * 0x1000;
        let mut bootinfo = BootInfo {
            memory_map: Vec::with_capacity(128),
            physical_memory_offset: config.physical_memory_offset,
            graphic_info: graphic_info.expect("failed to init GOP"),
            acpi2_rsdp_addr: acpi2_addr as u64,
            smbios_addr: smbios_addr as u64,
            initramfs_addr,
            initramfs_size,
            cmdline: config.cmdline,
        };

        info!("exit boot services");
        let mmap = unsafe { boot::exit_boot_services(None) };
        for desc in mmap.entries() {
            bootinfo.memory_map.push(*desc);
        }
        unsafe {
            arch::load_gdt(&gdt);
            arch::jump_to_entry(entry, &bootinfo, stacktop);
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        let _ = (initramfs_addr, initramfs_size, graphic_info);
        let entry = arch::load_elf(&elf, config.physical_memory_offset);
        let memory_map = boot::memory_map(MemoryType::LOADER_DATA)
            .expect("failed to get memory map for page-table setup");
        let pt0_paddr = arch::setup_page_tables(&memory_map);

        let bootinfo = rboot::Aarch64BootInfo {
            cmdline: config.cmdline,
            firmware_type: config.firmware_type,
            uart_base: config.uart_base,
            gic_base: config.gic_base,
            offset: config.physical_memory_offset as usize,
        };

        let bootinfo_box = alloc::boxed::Box::new(bootinfo);
        let bootinfo_ptr = alloc::boxed::Box::into_raw(bootinfo_box) as usize;

        info!("kernel entry point: 0x{:x}", entry);
        info!("exit boot services");
        let _ = unsafe { boot::exit_boot_services(None) };

        unsafe {
            arch::jump_to_kernel(entry, bootinfo_ptr, pt0_paddr);
        }
    }
}

/// Try to open file at `path`
fn try_open_file(path: &str) -> Option<RegularFile> {
    info!("trying to open file: {}", path);
    let handle = boot::get_handle_for_protocol::<SimpleFileSystem>().ok()?;
    let mut fs = boot::open_protocol_exclusive::<SimpleFileSystem>(handle).ok()?;
    let mut buf = [0u16; 256];
    let ucs_path = uefi::CStr16::from_str_with_buf(path, &mut buf).ok()?;
    let mut root = fs.open_volume().ok()?;
    let handle = root
        .open(ucs_path, FileMode::Read, FileAttribute::empty())
        .ok()?;

    match handle.into_type().ok()? {
        FileType::Regular(regular) => Some(regular),
        _ => None,
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
fn init_graphic(resolution: Option<(usize, usize)>) -> Option<rboot::GraphicInfo> {
    let handle = boot::get_handle_for_protocol::<GraphicsOutput>().ok()?;
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(handle).ok()?;

    if let Some(resolution) = resolution
        && let Some(mode) = gop.modes().find(|m| m.info().resolution() == resolution)
    {
        info!("switching graphic mode");
        let _ = gop.set_mode(&mode);
    }
    Some(rboot::GraphicInfo {
        mode: gop.current_mode_info(),
        fb_addr: gop.frame_buffer().as_mut_ptr() as u64,
        fb_size: gop.frame_buffer().size() as u64,
    })
}
