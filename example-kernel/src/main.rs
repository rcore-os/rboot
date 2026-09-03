#![no_std]
#![no_main]

use core::arch::asm;

#[cfg(target_arch = "x86_64")]
const PHYSICAL_MEMORY_OFFSET: u64 = 0xffff_8000_0000_0000;

#[cfg(target_arch = "x86_64")]
#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// Write a byte to COM1 serial port (0x3F8).
#[cfg(target_arch = "x86_64")]
fn serial_putchar(c: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") 0x3F8u16,
            in("al") c,
        );
    }
}

/// Write a byte to the QEMU virt PL011 UART through the physical-memory map.
#[cfg(target_arch = "aarch64")]
fn serial_putchar(c: u8) {
    const UART: *mut u8 = 0xffff_0000_0900_0000 as *mut u8;
    unsafe { UART.write_volatile(c) };
}

/// Write a string to serial port.
fn serial_print(s: &str) {
    for b in s.bytes() {
        serial_putchar(b);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    serial_print("\n[test-kernel] Hello from rboot test kernel!\n");

    #[cfg(target_arch = "x86_64")]
    {
        let mut gdtr = DescriptorTablePointer { limit: 0, base: 0 };
        unsafe {
            asm!("sgdt [{}]", in(reg) &mut gdtr, options(nostack, preserves_flags));
        }
        let gdt_base = unsafe { core::ptr::addr_of!(gdtr.base).read_unaligned() };
        if gdt_base < PHYSICAL_MEMORY_OFFSET {
            serial_print("[test-kernel] GDTR still points to low memory!\n");
            exit_qemu(1);
        }
    }

    serial_print("[test-kernel] rboot is working correctly.\n");

    #[cfg(target_arch = "x86_64")]
    exit_qemu(0);

    loop {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            asm!("hlt")
        };
        #[cfg(target_arch = "aarch64")]
        unsafe {
            asm!("wfe")
        };
    }
}

#[cfg(target_arch = "x86_64")]
fn exit_qemu(code: u8) {
    unsafe {
        // Shutdown QEMU via ISA debug exit device (port 0x501).
        asm!("out dx, al", in("dx") 0x501u16, in("al") code);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    serial_print("[test-kernel] PANIC!\n");
    loop {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            asm!("hlt")
        };
        #[cfg(target_arch = "aarch64")]
        unsafe {
            asm!("wfe")
        };
    }
}
