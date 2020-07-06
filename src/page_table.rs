//! This file is modified from 'page_table.rs' in 'rust-osdev/bootloader'
#[cfg(target_arch = "aarch64")]
use aarch64::{
    align_up,
    paging::{mapper::*, memory_attribute::*, PageTableFlags as PTF, *},
    translation::{ttbr_el1_read, ttbr_el1_write},
    PhysAddr, VirtAddr,
};
#[cfg(target_arch = "x86_64")]
use x86_64::{
    align_up,
    registers::control::Cr3,
    structures::paging::{mapper::*, PageTableFlags as PTF, *},
    PhysAddr, VirtAddr,
};
use xmas_elf::{program, ElfFile};

#[cfg(target_arch = "x86_64")]
type MapToError_ = MapToError<Size4KiB>;
#[cfg(target_arch = "aarch64")]
type MapToError_ = MapToError;

/// Get current page table from CR3
#[cfg(target_arch = "x86_64")]
pub fn current_page_table() -> OffsetPageTable<'static> {
    let p4_table_addr = Cr3::read().0.start_address().as_u64();
    let p4_table = unsafe { &mut *(p4_table_addr as *mut PageTable) };
    unsafe { OffsetPageTable::new(p4_table, VirtAddr::new(0)) }
}

/// Get current page table
#[cfg(target_arch = "aarch64")]
pub fn current_page_table() -> MappedPageTable<'static, fn(PhysFrame) -> *mut PageTable> {
    fn frame_to_page_table(frame: PhysFrame) -> *mut PageTable {
        frame.start_address().as_u64() as _
    }
    let p4_table_addr = ttbr_el1_read(1).start_address().as_u64();
    let p4_table = unsafe { &mut *(p4_table_addr as *mut PageTable) };
    unsafe { MappedPageTable::new(p4_table, frame_to_page_table) }
}

#[cfg(target_arch = "aarch64")]
pub fn init_kernel_page_table(frame_allocator: &mut impl FrameAllocator<Size4KiB>) {
    let frame = frame_allocator.allocate_frame().unwrap();
    let p4_table_addr = frame.start_address().as_u64();
    let p4_table = unsafe { &mut *(p4_table_addr as *mut PageTable) };
    p4_table.zero();
    ttbr_el1_write(1, frame);
}

pub fn map_elf(
    elf: &ElfFile,
    page_table: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError_> {
    info!("mapping ELF");
    let kernel_start = PhysAddr::new(elf.input.as_ptr() as u64);
    for segment in elf.program_iter() {
        map_segment(&segment, kernel_start, page_table, frame_allocator)?;
    }
    Ok(())
}

pub fn map_stack(
    addr: u64,
    pages: u64,
    page_table: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError_> {
    info!("mapping stack at {:#x}", addr);
    // create a stack
    let stack_start = Page::containing_address(VirtAddr::new(addr));
    let stack_end = stack_start + pages;

    for page in Page::range(stack_start, stack_end) {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        unsafe {
            map(page_table, page, frame, default_ptf(), frame_allocator)?.flush();
        }
    }

    Ok(())
}

fn map_segment(
    segment: &program::ProgramHeader,
    kernel_start: PhysAddr,
    page_table: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError_> {
    if segment.get_type().unwrap() != program::Type::Load {
        return Ok(());
    }
    debug!("mapping segment: {:#x?}", segment);
    let mem_size = segment.mem_size();
    let file_size = segment.file_size();
    let file_offset = segment.offset() & !0xfff;
    let phys_start_addr = kernel_start + file_offset;
    let virt_start_addr = VirtAddr::new(segment.virtual_addr());

    let start_page: Page = Page::containing_address(virt_start_addr);
    let start_frame = PhysFrame::containing_address(phys_start_addr);
    let end_frame = PhysFrame::containing_address(phys_start_addr + file_size - 1u64);

    let page_table_flags = trans_flags(segment.flags());

    for frame in PhysFrame::range_inclusive(start_frame, end_frame) {
        let offset = frame - start_frame;
        let page = start_page + offset;
        unsafe {
            map(page_table, page, frame, page_table_flags, frame_allocator)?.flush();
        };
    }

    if mem_size > file_size {
        // .bss section (or similar), which needs to be zeroed
        let zero_start = virt_start_addr + file_size;
        let zero_end = virt_start_addr + mem_size;
        if zero_start.as_u64() & 0xfff != 0 {
            // A part of the last mapped frame needs to be zeroed. This is
            // not possible since it could already contains parts of the next
            // segment. Thus, we need to copy it before zeroing.

            let new_frame = frame_allocator
                .allocate_frame()
                .ok_or(MapToError::FrameAllocationFailed)?;

            type PageArray = [u64; 0x1000 / 8];

            let last_page = Page::containing_address(virt_start_addr + file_size - 1u64);
            let last_page_ptr = end_frame.start_address().as_u64() as *mut PageArray;
            let temp_page_ptr = new_frame.start_address().as_u64() as *mut PageArray;

            unsafe {
                // copy contents
                temp_page_ptr.write(last_page_ptr.read());
            }

            // remap last page
            if let Err(e) = page_table.unmap(last_page.clone()) {
                return Err(match e {
                    UnmapError::ParentEntryHugePage => MapToError::ParentEntryHugePage,
                    UnmapError::PageNotMapped => unreachable!(),
                    UnmapError::InvalidFrameAddress(_) => unreachable!(),
                });
            }
            unsafe {
                map(
                    page_table,
                    last_page,
                    new_frame,
                    page_table_flags,
                    frame_allocator,
                )?
                .flush();
            }
        }

        // Map additional frames.
        let start_page: Page =
            Page::containing_address(VirtAddr::new(align_up(zero_start.as_u64(), 0x1000)));
        let end_page = Page::containing_address(zero_end);
        for page in Page::range_inclusive(start_page, end_page) {
            let frame = frame_allocator
                .allocate_frame()
                .ok_or(MapToError::FrameAllocationFailed)?;
            unsafe {
                map(page_table, page, frame, page_table_flags, frame_allocator)?.flush();
            }
        }

        // zero bss
        unsafe {
            core::ptr::write_bytes(
                zero_start.as_mut_ptr::<u8>(),
                0,
                (mem_size - file_size) as usize,
            );
        }
    }
    Ok(())
}

/// Map physical memory [0, max_addr)
/// to virtual space [offset, offset + max_addr)
pub fn map_physical_memory(
    offset: u64,
    max_addr: u64,
    page_table: &mut impl Mapper<Size2MiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    info!("mapping physical memory");
    let start_frame = PhysFrame::containing_address(PhysAddr::new(0));
    let end_frame = PhysFrame::containing_address(PhysAddr::new(max_addr));
    for frame in PhysFrame::range_inclusive(start_frame, end_frame) {
        let page = Page::containing_address(VirtAddr::new(frame.start_address().as_u64() + offset));
        unsafe {
            map(page_table, page, frame, default_ptf(), frame_allocator)
                .expect("failed to map physical memory")
                .flush();
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn default_ptf() -> PTF {
    PTF::PRESENT | PTF::WRITABLE
}

#[cfg(target_arch = "aarch64")]
fn default_ptf() -> PTF {
    PTF::VALID | PTF::PXN
}

#[cfg(target_arch = "x86_64")]
fn trans_flags(flags: program::Flags) -> PTF {
    let mut page_table_flags = PTF::PRESENT;
    if !flags.is_execute() {
        page_table_flags |= PTF::NO_EXECUTE
    };
    if flags.is_write() {
        page_table_flags |= PTF::WRITABLE
    };
    page_table_flags
}

#[cfg(target_arch = "aarch64")]
fn trans_flags(flags: program::Flags) -> PTF {
    let mut page_table_flags = PTF::VALID;
    if !flags.is_execute() {
        page_table_flags |= PTF::PXN
    };
    if !flags.is_write() {
        page_table_flags |= PTF::AP_RO
    };
    page_table_flags
}

#[cfg(target_arch = "x86_64")]
unsafe fn map<S: PageSize>(
    page_table: &mut impl Mapper<S>,
    page: Page<S>,
    frame: PhysFrame<S>,
    flags: PTF,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<MapperFlush<S>, MapToError<S>> {
    page_table.map_to(page, frame, flags, frame_allocator)
}

#[cfg(target_arch = "aarch64")]
unsafe fn map<S: PageSize>(
    page_table: &mut impl Mapper<S>,
    page: Page<S>,
    frame: PhysFrame<S>,
    flags: PTF,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<MapperFlush<S>, MapToError> {
    page_table.map_to(
        page,
        frame,
        flags,
        MairNormal::attr_value(),
        frame_allocator,
    )
}
