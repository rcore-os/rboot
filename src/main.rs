//! Simple ELF OS Loader on UEFI
//!
//! 1. Load config from "\EFI\Boot\rboot.conf"
//! 2. Load kernel ELF file
//! 3. Map ELF segments to virtual memory
//! 4. Map kernel stack and all physical memory
//! 5. Startup all processors
//! 6. Exit boot and jump to ELF entry

#![no_std]
#![no_main]

#[macro_use]
extern crate alloc;
#[macro_use]
extern crate log;

use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, ModeInfo};
use uefi::proto::media::file::*;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::pi::mp::MPServices;
use uefi::table::boot::*;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr3};
use x86_64::structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};
use xmas_elf::ElfFile;

mod config;
mod page_table;

const CONFIG_PATH: &str = "\\EFI\\Boot\\rboot.conf";

#[no_mangle]
pub extern "C" fn efi_main(image: uefi::Handle, st: SystemTable<Boot>) -> Status {
    // Initialize utilities (logging, memory allocation...)
    uefi_services::init(&st).expect_success("failed to initialize utilities");

    info!("bootloader is running");
    let bs = st.boot_services();
    let config = {
        let mut file = open_file(bs, CONFIG_PATH);
        let buf = load_file(bs, &mut file);
        config::Config::parse(buf)
    };

    let graphic_mode = init_graphic(bs, config.resolution);
    info!("config: {:#x?}", config);

    let elf = {
        let mut file = open_file(bs, config.kernel_path);
        let buf = load_file(bs, &mut file);
        ElfFile::new(buf).expect("failed to parse ELF")
    };
    unsafe {
        ENTRY = elf.header.pt2.entry_point() as usize;
    }

    let mut page_table = current_page_table();
    // root page table is readonly
    // disable write protect
    unsafe {
        Cr0::update(|f| f.remove(Cr0Flags::WRITE_PROTECT));
    }
    page_table::map_elf(&elf, &mut page_table, &mut UEFIFrameAllocator(bs))
        .expect("failed to map ELF");
    // we use UEFI default stack, no need to allocate
    //    page_table::map_stack(
    //        config.kernel_stack_address,
    //        config.kernel_stack_size,
    //        &mut page_table,
    //        &mut UEFIFrameAllocator(bs),
    //    )
    //    .expect("failed to map stack");
    page_table::map_physical_memory(
        config.physical_memory_offset,
        0x100000000,
        &mut page_table,
        &mut UEFIFrameAllocator(bs),
    );
    // recover write protect
    unsafe {
        Cr0::update(|f| f.insert(Cr0Flags::WRITE_PROTECT));
    }

    start_aps(bs);

    info!("exit boot services");
    let max_mmap_size = st.boot_services().memory_map_size();
    let mut mmap_storage = vec![0; max_mmap_size];

    let (_rt, _) = st
        .exit_boot_services(image, &mut mmap_storage)
        .expect_success("Failed to exit boot services");
    // NOTE: log can no longer be used

    let entry: KernelEntry = unsafe { core::mem::transmute(ENTRY) };
    entry();
}

/// Open file at `path`
fn open_file(bs: &BootServices, path: &str) -> RegularFile {
    info!("opening file: {}", path);
    // FIXME: use LoadedImageProtocol to get the FileSystem of this image
    let fs = bs
        .locate_protocol::<SimpleFileSystem>()
        .expect_success("failed to get FileSystem");
    let fs = unsafe { &mut *fs.get() };

    let mut root = fs.open_volume().expect_success("failed to open volume");
    let handle = root
        .open(path, FileMode::Read, FileAttribute::empty())
        .expect_success("failed to open file");

    match handle.into_type().expect_success("failed to into_type") {
        FileType::Regular(regular) => regular,
        _ => panic!("Invalid file type"),
    }
}

/// Load file to new allocated pages
fn load_file(bs: &BootServices, file: &mut RegularFile) -> &'static mut [u8] {
    info!("loading file to memory");
    let mut info_buf = [0u8; 0x100];
    let info = file
        .get_info::<FileInfo>(&mut info_buf)
        .expect_success("failed to get file info");
    let pages = info.file_size() as usize / 0x1000 + 1;
    let mem_start = bs
        .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
        .expect_success("failed to allocate pages");
    let buf = unsafe { core::slice::from_raw_parts_mut(mem_start as *mut u8, pages * 0x1000) };
    let len = file.read(buf).expect_success("failed to read file");
    &mut buf[..len]
}

/// If `resolution` is some, then set graphic mode matching the resolution.
/// Return information of the final graphic mode.
fn init_graphic(bs: &BootServices, resolution: Option<(usize, usize)>) -> ModeInfo {
    let gop = bs
        .locate_protocol::<GraphicsOutput>()
        .expect_success("failed to get GraphicsOutput");
    let gop = unsafe { &mut *gop.get() };

    if let Some(resolution) = resolution {
        let mode = gop
            .modes()
            .map(|mode| mode.expect("Warnings encountered while querying mode"))
            .find(|ref mode| {
                let info = mode.info();
                info.resolution() == resolution
            })
            .expect("graphic mode not found");
        info!("switching graphic mode");
        gop.set_mode(&mode)
            .expect_success("Failed to set graphics mode");
        *mode.info()
    } else {
        gop.current_mode_info()
    }
}

/// Get current page table from CR3
fn current_page_table() -> OffsetPageTable<'static> {
    let p4_table_addr = Cr3::read().0.start_address().as_u64();
    let p4_table = unsafe { &mut *(p4_table_addr as *mut PageTable) };
    unsafe { OffsetPageTable::new(p4_table, VirtAddr::new(0)) }
}

/// Use `BootServices::allocate_pages()` as frame allocator
struct UEFIFrameAllocator<'a>(&'a BootServices);

unsafe impl FrameAllocator<Size4KiB> for UEFIFrameAllocator<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let addr = self
            .0
            .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 1)
            .expect_success("failed to allocate frame");
        Some(PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

/// Startup all application processors
fn start_aps(bs: &BootServices) {
    info!("starting application processors");
    let mp = bs
        .locate_protocol::<MPServices>()
        .expect_success("failed to get MPServices");
    let mp = unsafe { &mut *mp.get() };

    // `ap_main` will never return, add timeout to be non-block
    let timeout = core::time::Duration::from_secs(1);
    mp.startup_all_aps(false, ap_main, core::ptr::null_mut(), Some(timeout))
        .expect_error("failed to startup all application processors");
}

/// Main function for application processors
extern "win64" fn ap_main(_arg: *mut core::ffi::c_void) {
    let entry: KernelEntry = unsafe { core::mem::transmute(ENTRY) };
    entry();
}

type KernelEntry = extern "C" fn() -> !;
/// The entry point of kernel, set by BSP.
static mut ENTRY: usize = 0;
