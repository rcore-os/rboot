use core::str::FromStr;
use log::warn;

/// Config for the bootloader
#[derive(Debug)]
pub struct Config<'a> {
    /// The address at which the kernel stack is placed
    pub kernel_stack_address: u64,
    /// The size of the kernel stack, given in number of 4KiB pages
    pub kernel_stack_size: u64,
    /// The offset into the virtual address space where the physical memory is mapped
    pub physical_memory_offset: u64,
    /// The path of kernel ELF
    pub kernel_path: &'a str,
    /// The resolution of graphic output
    pub resolution: Option<(usize, usize)>,
    /// The path of initramfs
    pub initramfs: Option<&'a str>,
    /// Kernel command line
    pub cmdline: &'a str,
    /// UART base physical address (aarch64)
    pub uart_base: usize,
    /// GIC base physical address (aarch64)
    pub gic_base: usize,
    /// Firmware type (aarch64)
    pub firmware_type: &'a str,
}

#[cfg(target_arch = "aarch64")]
pub const DEFAULT_CONFIG: Config = Config {
    kernel_stack_address: 0xFFFF_0000_8000_0000,
    kernel_stack_size: 512,
    physical_memory_offset: 0xFFFF_0000_0000_0000,
    kernel_path: "\\os",
    resolution: None,
    initramfs: None,
    cmdline: "",
    uart_base: 0x0900_0000,
    gic_base: 0x0800_0000,
    firmware_type: "QEMU",
};

#[cfg(not(target_arch = "aarch64"))]
pub const DEFAULT_CONFIG: Config = Config {
    kernel_stack_address: 0xFFFF_FF01_0000_0000,
    kernel_stack_size: 512,
    physical_memory_offset: 0xFFFF_8000_0000_0000,
    kernel_path: "\\EFI\\rCore\\kernel.elf",
    resolution: None,
    initramfs: None,
    cmdline: "",
    uart_base: 0,
    gic_base: 0,
    firmware_type: "PC",
};

fn parse_num(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        u64::from_str(value).ok()
    }
}

impl<'a> Config<'a> {
    pub fn parse(content: &'a [u8]) -> Self {
        let content = core::str::from_utf8(content).expect("failed to parse config as utf8");
        let mut config = DEFAULT_CONFIG;
        for line in content.split('\n') {
            let line = line.trim();
            // skip empty and comment
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // parse 'key=value'
            let mut iter = line.splitn(2, '=');
            let key = match iter.next() {
                Some(k) => k.trim(),
                None => continue,
            };
            let value = match iter.next() {
                Some(v) => v.trim(),
                None => continue,
            };
            config.process(key, value);
        }
        config
    }

    fn process(&mut self, key: &str, value: &'a str) {
        match key {
            "kernel_stack_address" => match parse_num(value) {
                Some(value) => self.kernel_stack_address = value,
                None => warn!("invalid kernel_stack_address: {value}"),
            },
            "kernel_stack_size" => match parse_num(value) {
                Some(value) => self.kernel_stack_size = value,
                None => warn!("invalid kernel_stack_size: {value}"),
            },
            "physical_memory_offset" => {
                if let Some(value) = parse_num(value) {
                    self.physical_memory_offset = value;
                } else {
                    warn!("invalid physical_memory_offset: {value}");
                }
            }
            "kernel_path" => self.kernel_path = value,
            "resolution" => {
                let mut iter = value.split('x');
                if let (Some(x), Some(y)) = (iter.next(), iter.next())
                    && let (Ok(x), Ok(y)) = (x.parse::<usize>(), y.parse::<usize>())
                {
                    self.resolution = Some((x, y));
                }
            }
            "initramfs" => self.initramfs = Some(value),
            "cmdline" => self.cmdline = value,
            "uart_base" => match parse_num(value) {
                Some(value) => self.uart_base = value as usize,
                None => warn!("invalid uart_base: {value}"),
            },
            "gic_base" => match parse_num(value) {
                Some(value) => self.gic_base = value as usize,
                None => warn!("invalid gic_base: {value}"),
            },
            "firmware_type" => self.firmware_type = value,
            _ => warn!("undefined config key: {}", key),
        }
    }
}
