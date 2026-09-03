use alloc::vec::Vec;
use log::info;
use uefi::boot::{self, AllocateType, MemoryType};
use uefi::mem::memory_map::MemoryMap;
use xmas_elf::{ElfFile, program};

const PAGE_SIZE: u64 = 0x1000;
const L2_BLOCK_SIZE: u64 = 0x20_0000;
const L1_BLOCK_SIZE: u64 = 0x4000_0000;
const PHYSICAL_MAP_SIZE: u64 = 0x80_0000_0000;

const TABLE_DESCRIPTOR: u64 = 0b11;
const ATTR_DEVICE: u64 = 1 | (0b11 << 8) | (1 << 10) | (1 << 53) | (1 << 54);
const ATTR_NORMAL: u64 = 1 | (1 << 2) | (0b11 << 8) | (1 << 10);

fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

fn align_up(value: u64, align: u64) -> u64 {
    value
        .checked_add(align - 1)
        .expect("physical address overflow")
        & !(align - 1)
}

fn segment_paddr(virt_addr: u64, physical_memory_offset: u64) -> u64 {
    if virt_addr >= physical_memory_offset {
        virt_addr - physical_memory_offset
    } else {
        virt_addr & 0x0000_ffff_ffff_ffff
    }
}

pub fn load_elf(elf: &ElfFile, physical_memory_offset: u64) -> usize {
    info!("loading ELF segments to physical memory");
    assert_eq!(
        physical_memory_offset & (PHYSICAL_MAP_SIZE - 1),
        0,
        "physical memory offset must be aligned to the 512 GiB boot mapping"
    );

    // Multiple ELF segments may share a page. Reserve their merged physical
    // ranges first so that later UEFI allocations cannot overwrite the kernel.
    let mut ranges = Vec::new();
    for ph in elf.program_iter() {
        if ph.get_type() != Ok(program::Type::Load) || ph.mem_size() == 0 {
            continue;
        }
        assert!(
            ph.file_size() <= ph.mem_size(),
            "ELF segment file size exceeds memory size"
        );
        let paddr = segment_paddr(ph.virtual_addr(), physical_memory_offset);
        let end = paddr
            .checked_add(ph.mem_size())
            .expect("ELF segment physical address overflow");
        assert!(
            end <= PHYSICAL_MAP_SIZE,
            "ELF segment is outside the boot page-table mapping"
        );
        ranges.push((align_down(paddr, PAGE_SIZE), align_up(end, PAGE_SIZE)));
    }
    ranges.sort_unstable_by_key(|range| range.0);

    let mut merged_ranges: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged_ranges.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            merged_ranges.push((start, end));
        }
    }

    for (start, end) in merged_ranges {
        let page_count = ((end - start) / PAGE_SIZE) as usize;
        boot::allocate_pages(
            AllocateType::Address(start),
            MemoryType::LOADER_DATA,
            page_count,
        )
        .unwrap_or_else(|error| {
            panic!("failed to reserve ELF memory at {start:#x}..{end:#x}: {error:?}")
        });
    }

    for ph in elf.program_iter() {
        if ph.get_type() != Ok(program::Type::Load) {
            continue;
        }
        let virt_addr = ph.virtual_addr();
        let mem_size = ph.mem_size() as usize;
        let file_size = ph.file_size() as usize;
        let paddr = segment_paddr(virt_addr, physical_memory_offset) as usize;
        let file_start = ph.offset() as usize;
        let file_end = file_start
            .checked_add(file_size)
            .expect("ELF segment file offset overflow");
        let src = elf
            .input
            .get(file_start..file_end)
            .expect("ELF segment extends past the end of the file");

        info!(
            "loading segment: paddr=0x{:x}, vaddr=0x{:x}, mem_size=0x{:x}, file_size=0x{:x}",
            paddr, virt_addr, mem_size, file_size
        );

        unsafe {
            let dst = core::slice::from_raw_parts_mut(paddr as *mut u8, mem_size);
            dst.fill(0);
            dst[..file_size].copy_from_slice(src);
        }
    }
    elf.header.pt2.entry_point() as usize
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RegionKind {
    Normal,
    Device,
    Mixed,
}

fn is_normal_memory(ty: MemoryType) -> bool {
    matches!(
        ty,
        MemoryType::LOADER_CODE
            | MemoryType::LOADER_DATA
            | MemoryType::BOOT_SERVICES_CODE
            | MemoryType::BOOT_SERVICES_DATA
            | MemoryType::RUNTIME_SERVICES_CODE
            | MemoryType::RUNTIME_SERVICES_DATA
            | MemoryType::CONVENTIONAL
            | MemoryType::ACPI_RECLAIM
            | MemoryType::ACPI_NON_VOLATILE
            | MemoryType::PERSISTENT_MEMORY
    )
}

fn normal_memory_ranges(memory_map: &impl MemoryMap) -> Vec<(u64, u64)> {
    let mut ranges: Vec<(u64, u64)> = memory_map
        .entries()
        .filter(|descriptor| is_normal_memory(descriptor.ty))
        .filter_map(|descriptor| {
            let end = descriptor
                .page_count
                .checked_mul(PAGE_SIZE)?
                .checked_add(descriptor.phys_start)?
                .min(PHYSICAL_MAP_SIZE);
            (descriptor.phys_start < end).then_some((descriptor.phys_start, end))
        })
        .collect();
    ranges.sort_unstable_by_key(|range| range.0);

    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

fn classify_region(start: u64, end: u64, normal_ranges: &[(u64, u64)]) -> RegionKind {
    let mut covered_until = start;
    let mut overlaps = false;

    for &(range_start, range_end) in normal_ranges {
        if range_end <= start {
            continue;
        }
        if range_start >= end {
            break;
        }
        overlaps = true;
        if range_start > covered_until {
            return RegionKind::Mixed;
        }
        covered_until = covered_until.max(range_end.min(end));
        if covered_until == end {
            return RegionKind::Normal;
        }
    }

    if overlaps {
        RegionKind::Mixed
    } else {
        RegionKind::Device
    }
}

fn allocate_table() -> usize {
    let page = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 1)
        .expect("failed to allocate boot page table");
    let paddr = page.as_ptr() as usize;
    unsafe { core::ptr::write_bytes(paddr as *mut u8, 0, PAGE_SIZE as usize) };
    paddr
}

fn block_descriptor(paddr: u64, kind: RegionKind) -> u64 {
    paddr
        | match kind {
            RegionKind::Normal => ATTR_NORMAL,
            RegionKind::Device | RegionKind::Mixed => ATTR_DEVICE,
        }
}

fn page_descriptor(paddr: u64, kind: RegionKind) -> u64 {
    block_descriptor(paddr, kind) | 0b10
}

/// Set up translation tables that identity-map the lower 512 GiB and repeat
/// that mapping through TTBR0 and TTBR1. UEFI-described RAM is mapped as normal
/// cacheable memory; MMIO, reserved regions, and address-space holes are mapped
/// as Device-nGnRE and execute-never.
pub fn setup_page_tables(memory_map: &impl MemoryMap) -> usize {
    let normal_ranges = normal_memory_ranges(memory_map);
    let pt0_paddr = allocate_table();
    let pt1_paddr = allocate_table();

    unsafe {
        let pt0 = core::slice::from_raw_parts_mut(pt0_paddr as *mut u64, 512);
        let pt1 = core::slice::from_raw_parts_mut(pt1_paddr as *mut u64, 512);

        // Reusing the L1 table gives both the low identity map and the common
        // high-half direct-map offsets used by rCore/zCore.
        for entry in pt0.iter_mut() {
            *entry = pt1_paddr as u64 | TABLE_DESCRIPTOR;
        }

        for (l1_index, l1_entry) in pt1.iter_mut().enumerate() {
            let l1_start = l1_index as u64 * L1_BLOCK_SIZE;
            let l1_end = l1_start + L1_BLOCK_SIZE;
            match classify_region(l1_start, l1_end, &normal_ranges) {
                RegionKind::Normal => {
                    *l1_entry = block_descriptor(l1_start, RegionKind::Normal);
                }
                RegionKind::Device => {
                    *l1_entry = block_descriptor(l1_start, RegionKind::Device);
                }
                RegionKind::Mixed => {
                    let pt2_paddr = allocate_table();
                    *l1_entry = pt2_paddr as u64 | TABLE_DESCRIPTOR;
                    let pt2 = core::slice::from_raw_parts_mut(pt2_paddr as *mut u64, 512);

                    for (l2_index, l2_entry) in pt2.iter_mut().enumerate() {
                        let l2_start = l1_start + l2_index as u64 * L2_BLOCK_SIZE;
                        let l2_end = l2_start + L2_BLOCK_SIZE;
                        match classify_region(l2_start, l2_end, &normal_ranges) {
                            RegionKind::Normal => {
                                *l2_entry = block_descriptor(l2_start, RegionKind::Normal);
                            }
                            RegionKind::Device => {
                                *l2_entry = block_descriptor(l2_start, RegionKind::Device);
                            }
                            RegionKind::Mixed => {
                                let pt3_paddr = allocate_table();
                                *l2_entry = pt3_paddr as u64 | TABLE_DESCRIPTOR;
                                let pt3 =
                                    core::slice::from_raw_parts_mut(pt3_paddr as *mut u64, 512);
                                for (l3_index, l3_entry) in pt3.iter_mut().enumerate() {
                                    let page_start = l2_start + l3_index as u64 * PAGE_SIZE;
                                    let page_end = page_start + PAGE_SIZE;
                                    *l3_entry = page_descriptor(
                                        page_start,
                                        classify_region(page_start, page_end, &normal_ranges),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pt0_paddr
}

/// Jump to kernel entry point with boot info
pub unsafe fn jump_to_kernel(entry: usize, boot_info_ptr: usize, pt0_paddr: usize) -> ! {
    unsafe {
        core::arch::asm!(
            // 1. Check current EL
            "mrs x9, CurrentEL",
            "lsr x9, x9, #2",
            "cmp x9, #2",
            "b.lt 1f",

            // EL2 -> EL1 switch:
            "mov x9, #(1 << 31)", // HCR_EL2.RW = 1 (64-bit EL1)
            "msr hcr_el2, x9",
            "mov x9, #3",
            "msr cnthctl_el2, x9",
            "msr cntvoff_el2, xzr",
            "mov x9, #0x3c5",     // EL1h, all interrupts masked
            "msr spsr_el2, x9",
            "adr x9, 1f",
            "msr elr_el2, x9",
            "eret",

            "1:",
            // 2. Configure MAIR_EL1: Attr0 = 0x04 (Device-nGnRE), Attr1 = 0xff (Normal WB/WA)
            "movz x9, #0xff04",
            "msr mair_el1, x9",

            // 3. Configure TCR_EL1: 48-bit VA, 40-bit PA, 4KB granule
            "ldr x9, =0x2b5103510",
            "msr tcr_el1, x9",
            "isb",

            // 4. Set TTBR0_EL1 and TTBR1_EL1
            "msr ttbr0_el1, {pt0}",
            "msr ttbr1_el1, {pt0}",
            "isb",

            // 5. Invalidate TLB
            "tlbi vmalle1",
            "dsb sy",
            "isb",

            // 6. Enable MMU and Caches in SCTLR_EL1 (M=1, C=1, I=1)
            "mrs x9, sctlr_el1",
            "orr x9, x9, #0x1",    // M = 1
            "orr x9, x9, #0x4",    // C = 1
            "orr x9, x9, #0x1000", // I = 1
            "msr sctlr_el1, x9",
            "isb",

            // 7. Branch to kernel entry with boot_info in x0
            "mov x0, {boot_info}",
            "br {entry}",

            pt0 = in(reg) pt0_paddr,
            boot_info = in(reg) boot_info_ptr,
            entry = in(reg) entry,
            options(noreturn),
        );
    }
}
