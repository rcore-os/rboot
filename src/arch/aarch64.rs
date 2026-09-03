use log::info;
use uefi::boot::{self, AllocateType, MemoryType};
use xmas_elf::{ElfFile, program};

pub fn load_elf(elf: &ElfFile, physical_memory_offset: u64) -> usize {
    info!("loading ELF segments to physical memory");
    for ph in elf.program_iter() {
        if ph.get_type() == Ok(program::Type::Load) {
            let virt_addr = ph.virtual_addr();
            let mem_size = ph.mem_size() as usize;
            let file_size = ph.file_size() as usize;
            let paddr = if virt_addr >= physical_memory_offset {
                (virt_addr - physical_memory_offset) as usize
            } else {
                virt_addr as usize & 0x0000_ffff_ffff_ffff
            };

            let page_count = (mem_size + 0xfff) / 0x1000;
            info!(
                "mapping segment: paddr=0x{:x}, vaddr=0x{:x}, mem_size=0x{:x}, file_size=0x{:x}",
                paddr, virt_addr, mem_size, file_size
            );

            if let Err(e) = boot::allocate_pages(
                AllocateType::Address(paddr as u64),
                MemoryType::LOADER_DATA,
                page_count,
            ) {
                log::warn!("allocate_pages at 0x{:x} ({:?}): may overwrite", paddr, e);
            }

            unsafe {
                let dst = core::slice::from_raw_parts_mut(paddr as *mut u8, mem_size);
                dst.fill(0);
                let src = &elf.input[ph.offset() as usize..ph.offset() as usize + file_size];
                dst[..file_size].copy_from_slice(src);
            }
        }
    }
    elf.header.pt2.entry_point() as usize
}

/// Set up 2-level translation tables covering 512 GiB of physical space.
///
/// L0 table: All entries point to L1 table (pt1_paddr | 0b11) so all 512GB blocks in
/// TTBR0 and TTBR1 are uniformly mapped.
///
/// L1 table:
/// - Entry 0 (0..1 GiB): Device memory (UART, GIC, VirtIO)
/// - Entries 1..512 (1..512 GiB): Normal memory (RAM and peripherals above 1GB)
pub fn setup_page_tables() -> usize {
    let pt_pages = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 2)
        .expect("failed to allocate boot page tables");

    let pt0_paddr = pt_pages.as_ptr() as usize;
    let pt1_paddr = pt0_paddr + 0x1000;

    unsafe {
        core::ptr::write_bytes(pt0_paddr as *mut u8, 0, 0x2000);
        let pt0 = core::slice::from_raw_parts_mut(pt0_paddr as *mut u64, 512);
        let pt1 = core::slice::from_raw_parts_mut(pt1_paddr as *mut u64, 512);

        // L0 table entries: table descriptors pointing to L1 table
        for entry in pt0.iter_mut() {
            *entry = (pt1_paddr as u64) | 0b11; // VALID (bit 0) | TABLE (bit 1)
        }

        // L1 table entries:
        // Entry 0: 0x0000_0000..0x4000_0000 (0..1 GiB) Device-nGnRE memory
        // VALID (1), Attr0=Device (0<<2), Inner Shareable (3<<8), AF (1<<10), PXN (1<<53), UXN (1<<54)
        const ATTR_DEVICE: u64 = 1 | (0b11 << 8) | (1 << 10) | (1 << 53) | (1 << 54);
        pt1[0] = 0x0000_0000 | ATTR_DEVICE;

        // Entries 1..512: 1 GiB..512 GiB Normal memory
        // VALID (1), Attr1=Normal (1<<2), Inner Shareable (3<<8), AF (1<<10)
        const ATTR_NORMAL: u64 = 1 | (1 << 2) | (0b11 << 8) | (1 << 10);
        for i in 1..512 {
            let paddr = (i as u64) << 30;
            pt1[i] = paddr | ATTR_NORMAL;
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
