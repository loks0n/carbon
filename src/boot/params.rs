//! Linux boot_params (zero page) setup.
//!
//! The `boot_params` structure (historically called the "zero page" because it
//! was located at physical address 0) is a critical data structure that passes
//! information from the boot loader to the Linux kernel. It contains everything
//! the kernel needs to understand its environment.
//!
//! # Structure Overview
//!
//! The boot_params structure is 4096 bytes (one page) and contains:
//!
//! - **Screen info** (0x000-0x040): Video mode information (optional for headless)
//! - **APM BIOS info** (0x040-0x054): Advanced Power Management (legacy)
//! - **Drive info** (0x080-0x090): BIOS drive parameters
//! - **SYS_DESC_TABLE** (0x0a0-0x0b0): System descriptor table info
//! - **Setup header** (0x1f1-0x268): Boot protocol header (from bzImage)
//! - **E820 map** (0x2d0-0x...): Memory map entries
//!
//! # E820 Memory Map
//!
//! The E820 memory map is the standard way for firmware (BIOS/UEFI) to communicate
//! available RAM regions to the operating system. Each entry describes a region:
//!
//! - **Address**: Start of the memory region
//! - **Size**: Length in bytes
//! - **Type**: What the memory is used for
//!
//! Memory types:
//! - Type 1 (RAM): Available for general use
//! - Type 2 (Reserved): Reserved by firmware, do not use
//! - Type 3 (ACPI): ACPI tables, reclaimable after parsing
//! - Type 4 (NVS): ACPI Non-Volatile Storage, must be preserved
//! - Type 5 (Unusable): Defective or otherwise unusable memory
//!
//! For a simple VM, we provide:
//! 1. Low memory (0 - 640KB) as usable RAM
//! 2. EBDA/ROM area (640KB - 1MB) as reserved
//! 3. High memory (1MB - total_mem) as usable RAM
//!
//! # Setup Header Integration
//!
//! The setup header is extracted from the bzImage and copied directly into
//! boot_params at offset 0x1f1. We then override specific fields to configure
//! the boot environment correctly.
//!
//! Reference: <https://www.kernel.org/doc/html/latest/x86/boot.html>
//! Reference: <https://www.kernel.org/doc/html/latest/x86/zero-page.html>

use super::acpi::RSDP_ADDR;
use super::bzimage::LoadedKernel;
use super::layout;
use super::memory::GuestMemory;
use super::{BootConfig, BootError};

/// Size of the boot_params structure (one 4KB page).
const BOOT_PARAMS_SIZE: usize = 4096;

/// E820 memory region types.
///
/// These values are defined by the BIOS E820 specification and tell the
/// kernel what each memory region can be used for.
#[repr(u32)]
#[derive(Clone, Copy)]
pub enum E820Type {
    /// Usable RAM - available for kernel and user processes.
    Ram = 1,

    /// Reserved - used by firmware or hardware, do not touch.
    Reserved = 2,
}

/// Byte offsets within the boot_params structure.
///
/// These offsets are defined by the Linux boot protocol specification.
/// See: <https://www.kernel.org/doc/html/latest/x86/zero-page.html>
mod offsets {
    /// ACPI RSDP address (8 bytes) - for direct RSDP pointer (faster than scanning).
    /// Set by EFI stub or bootloader to skip BIOS ROM area scan.
    pub const ACPI_RSDP_ADDR: usize = 0x70;

    /// Number of E820 memory map entries (1 byte).
    pub const E820_ENTRIES: usize = 0x1e8;

    /// Start of setup header within boot_params.
    pub const SETUP_HEADER: usize = 0x1f1;

    /// type_of_loader field (1 byte) - offset 0x210 in bzImage, 0x210 in boot_params.
    /// But relative to setup_header start (0x1f1), it's at 0x210 - 0x1f1 = 0x1f.
    pub const TYPE_OF_LOADER: usize = 0x210;

    /// loadflags field (1 byte) - offset 0x211 in bzImage/boot_params.
    pub const LOADFLAGS: usize = 0x211;

    /// cmd_line_ptr field (4 bytes) - offset 0x228 in boot_params.
    pub const CMD_LINE_PTR: usize = 0x228;

    /// Start of E820 memory map array (128 entries × 20 bytes each).
    pub const E820_MAP: usize = 0x2d0;
}

/// Set up the boot_params structure at BOOT_PARAMS_START.
///
/// This function populates all the fields in boot_params that the kernel
/// needs to boot successfully:
///
/// 1. **Setup header**: Copied from the bzImage
/// 2. **Command line**: Pointer to our command line string
/// 3. **Memory map**: E820 entries describing available RAM
///
/// # Arguments
///
/// * `memory` - Guest memory where boot_params will be written
/// * `config` - Boot configuration (cmdline, memory size)
/// * `loaded_kernel` - Result from bzimage loading with setup_header
pub fn setup_boot_params(
    memory: &GuestMemory,
    config: &BootConfig,
    loaded_kernel: &LoadedKernel,
) -> Result<(), BootError> {
    // Start with a zeroed boot_params buffer
    let mut params = [0u8; BOOT_PARAMS_SIZE];

    // Copy the setup header from the loaded kernel
    // The setup_header bytes start at offset 0x1f1 in the bzImage,
    // and should be placed at offset 0x1f1 in boot_params
    let header_len = loaded_kernel
        .setup_header
        .len()
        .min(BOOT_PARAMS_SIZE - offsets::SETUP_HEADER);
    params[offsets::SETUP_HEADER..offsets::SETUP_HEADER + header_len]
        .copy_from_slice(&loaded_kernel.setup_header[..header_len]);

    // Set required fields that may not be in the setup header or need overriding

    // type_of_loader = 0xFF means undefined loader, use extended fields
    params[offsets::TYPE_OF_LOADER] = 0xff;

    // Load flags:
    // Bit 0 (LOADED_HIGH): Kernel is at 0x100000, not 0x10000
    // Bit 7 (CAN_USE_HEAP): heap_end_ptr field is valid
    params[offsets::LOADFLAGS] |= 0x01 | 0x80;

    // ACPI RSDP address - allows kernel to skip scanning BIOS ROM area
    let rsdp_addr_bytes = RSDP_ADDR.to_le_bytes();
    params[offsets::ACPI_RSDP_ADDR..offsets::ACPI_RSDP_ADDR + 8].copy_from_slice(&rsdp_addr_bytes);

    // Command line pointer - must be below 4GB
    let cmd_line_ptr = (layout::CMDLINE_START as u32).to_le_bytes();
    params[offsets::CMD_LINE_PTR..offsets::CMD_LINE_PTR + 4].copy_from_slice(&cmd_line_ptr);

    // Write the boot_params structure to guest memory
    memory.write(layout::BOOT_PARAMS_START, &params)?;

    // Set up command line
    setup_cmdline(memory, &config.cmdline)?;

    // Set up E820 memory map (writes directly to guest memory)
    let e820_entries = setup_e820_map(memory, config.mem_size)?;
    memory.write_u8(
        layout::BOOT_PARAMS_START + offsets::E820_ENTRIES as u64,
        e820_entries,
    )?;

    eprintln!(
        "[Boot] boot_params at {:#x}, cmdline at {:#x}",
        layout::BOOT_PARAMS_START,
        layout::CMDLINE_START
    );

    Ok(())
}

/// Write the kernel command line to guest memory.
///
/// The command line is a null-terminated string that controls kernel behavior.
/// It's written to CMDLINE_START and its address is stored in boot_params.
fn setup_cmdline(memory: &GuestMemory, cmdline: &str) -> Result<(), BootError> {
    if cmdline.len() >= layout::CMDLINE_MAX_SIZE {
        return Err(BootError::CmdlineTooLong {
            len: cmdline.len(),
            max: layout::CMDLINE_MAX_SIZE - 1,
        });
    }

    // Write command line string followed by null terminator
    memory.write(layout::CMDLINE_START, cmdline.as_bytes())?;
    memory.write_u8(layout::CMDLINE_START + cmdline.len() as u64, 0)?;

    eprintln!("[Boot] Command line: {}", cmdline);
    Ok(())
}

/// Set up the E820 memory map in boot_params.
///
/// The E820 map tells the kernel what physical memory regions exist
/// and what they can be used for. For a simple VM, we create three entries:
///
/// 1. **Low memory** (0x0 - 0x9FC00): ~640KB of usable RAM
///    This is the traditional "conventional memory" area.
///
/// 2. **Reserved** (0x9FC00 - 0x100000): ~384KB reserved
///    This covers the EBDA (Extended BIOS Data Area), video memory,
///    ROM area, and other legacy PC reserved regions.
///
/// 3. **High memory** (0x100000 - mem_size): Main RAM
///    All memory from 1MB to the end of guest RAM is usable.
fn setup_e820_map(memory: &GuestMemory, mem_size: u64) -> Result<u8, BootError> {
    let e820_addr = layout::BOOT_PARAMS_START + offsets::E820_MAP as u64;
    let entry_size = 20u64; // Each E820 entry is 20 bytes (8 + 8 + 4)
    let mut entry_idx = 0u64;

    // Entry 0: Low memory (conventional memory)
    write_e820_entry(
        memory,
        e820_addr + entry_idx * entry_size,
        0,        // Start at address 0
        0x9_fc00, // 640KB - 1KB = 654336 bytes
        E820Type::Ram,
    )?;
    entry_idx += 1;

    // Entry 1: Reserved region (EBDA, video, ROMs)
    write_e820_entry(
        memory,
        e820_addr + entry_idx * entry_size,
        0x9_fc00, // Start after low memory
        0x6_0400, // 1MB - 640KB + 1KB = 394240 bytes
        E820Type::Reserved,
    )?;
    entry_idx += 1;

    // Entry 2: High memory (extended memory)
    write_e820_entry(
        memory,
        e820_addr + entry_idx * entry_size,
        0x10_0000,            // Start at 1MB
        mem_size - 0x10_0000, // Rest of memory
        E820Type::Ram,
    )?;
    entry_idx += 1;

    eprintln!(
        "[Boot] E820 map: {} entries, {} MB total",
        entry_idx,
        mem_size / (1024 * 1024)
    );

    Ok(entry_idx as u8)
}

/// Write a single E820 entry to memory.
///
/// Each entry is 20 bytes:
/// - Bytes 0-7: Base address (u64, little-endian)
/// - Bytes 8-15: Size (u64, little-endian)
/// - Bytes 16-19: Type (u32, little-endian)
fn write_e820_entry(
    memory: &GuestMemory,
    addr: u64,
    base: u64,
    size: u64,
    type_: E820Type,
) -> Result<(), BootError> {
    memory.write_u64(addr, base)?;
    memory.write_u64(addr + 8, size)?;
    memory.write_u32(addr + 16, type_ as u32)?;
    Ok(())
}
