//! Linux bzImage loader.
//!
//! This module parses and loads Linux kernel images in the bzImage format,
//! which is the standard bootable kernel format for x86/x86_64 systems.
//!
//! # bzImage Format
//!
//! A bzImage (big zImage) consists of three main parts:
//!
//! 1. **Boot Sector** (512 bytes): Legacy boot code, mostly unused for direct boot.
//!
//! 2. **Setup Code** (setup_sects × 512 bytes): Real-mode setup code and the
//!    setup header containing boot protocol information.
//!
//! 3. **Protected-Mode Kernel**: The actual kernel code (usually compressed),
//!    which is loaded at the 1MB mark.
//!
//! ```text
//! +------------------+ 0x0000
//! |   Boot Sector    | 512 bytes
//! +------------------+ 0x0200
//! |   Setup Header   | Contains magic, version, load addresses
//! |   & Setup Code   | (setup_sects × 512 bytes)
//! +------------------+
//! | Protected-Mode   |
//! |     Kernel       | Loaded at 0x100000 (1MB)
//! +------------------+
//! ```
//!
//! # Setup Header
//!
//! The setup header starts at offset 0x1f1 in the bzImage and contains critical
//! information about the kernel:
//!
//! - **Magic number** (0x202): "HdrS" (0x53726448) identifies a valid Linux kernel
//! - **Version** (0x206): Boot protocol version (we require 2.06+)
//! - **setup_sects** (0x1f1): Number of 512-byte setup sectors
//! - **loadflags** (0x211): Kernel loading behavior flags
//!
//! # Supported Kernel Versions
//!
//! This loader requires boot protocol version 2.06 or higher, which was
//! introduced in Linux 2.6.20 (February 2007). This version added:
//! - 64-bit boot support
//! - Extended command line size
//! - Relocatable kernel support
//!
//! Reference: <https://www.kernel.org/doc/html/latest/x86/boot.html>

use super::layout;
use super::memory::GuestMemory;
use super::BootError;
use std::fs::File;
use std::io::Read;

/// Linux boot protocol magic number "HdrS" (ASCII: 0x48, 0x64, 0x72, 0x53).
const BOOT_MAGIC: u32 = 0x5372_6448;

/// Minimum supported boot protocol version (2.06 for 64-bit boot).
const MIN_BOOT_VERSION: u16 = 0x0206;

/// Offset of the setup header within the bzImage.
const SETUP_HEADER_OFFSET: usize = 0x1f1;

/// Result of loading a bzImage kernel.
pub struct LoadedKernel {
    /// Raw setup header bytes to copy to boot_params.
    pub setup_header: Vec<u8>,
}

/// Load a Linux bzImage kernel into guest memory.
///
/// This function:
/// 1. Reads the bzImage file from disk
/// 2. Parses and validates the setup header
/// 3. Loads the protected-mode kernel at the 1MB mark (0x100000)
/// 4. Extracts the setup header for boot_params configuration
///
/// # Arguments
///
/// * `memory` - Guest memory to load the kernel into
/// * `kernel_path` - Path to the bzImage file
///
/// # Returns
///
/// A `LoadedKernel` containing load addresses and setup header.
///
/// # Entry Point
///
/// For 64-bit boot, the entry point is `kernel_load + 0x200`. The first
/// 512 bytes (0x000-0x1FF) contain the 16-bit entry point; the 64-bit
/// entry point is at offset 0x200.
pub fn load_kernel(memory: &GuestMemory, kernel_path: &str) -> Result<LoadedKernel, BootError> {
    let mut file = File::open(kernel_path).map_err(BootError::ReadKernel)?;
    let mut kernel_data = Vec::new();
    file.read_to_end(&mut kernel_data)
        .map_err(BootError::ReadKernel)?;

    eprintln!("[Boot] Kernel image size: {} bytes", kernel_data.len());

    // Validate minimum size for setup header
    if kernel_data.len() < 0x250 {
        return Err(BootError::InvalidKernel(
            "Image too small to contain setup header".into(),
        ));
    }

    // Verify magic number "HdrS" at offset 0x202
    let magic = u32::from_le_bytes([
        kernel_data[0x202],
        kernel_data[0x203],
        kernel_data[0x204],
        kernel_data[0x205],
    ]);
    if magic != BOOT_MAGIC {
        return Err(BootError::InvalidKernel(format!(
            "Invalid boot magic: expected {:#x}, got {:#x}",
            BOOT_MAGIC, magic
        )));
    }

    // Check boot protocol version at offset 0x206
    let version = u16::from_le_bytes([kernel_data[0x206], kernel_data[0x207]]);
    if version < MIN_BOOT_VERSION {
        return Err(BootError::InvalidKernel(format!(
            "Unsupported boot protocol version: {:#x} (minimum {:#x} for 64-bit boot)",
            version, MIN_BOOT_VERSION
        )));
    }

    // Get setup_sects at 0x1f1 (default to 4 if 0 for old kernels)
    let setup_sects = kernel_data[0x1f1];
    let setup_sects = if setup_sects == 0 { 4 } else { setup_sects };

    eprintln!("[Boot] Setup header:");
    eprintln!("  - Boot protocol version: {:#x}", version);
    eprintln!("  - Setup sectors: {}", setup_sects);
    eprintln!("  - Loadflags: {:#x}", kernel_data[0x211]);

    // Calculate offset to protected-mode kernel
    let setup_size = (setup_sects as usize + 1) * 512;
    if setup_size >= kernel_data.len() {
        return Err(BootError::InvalidKernel(
            "Setup size exceeds kernel image size".into(),
        ));
    }

    // Extract protected-mode kernel and load at 1MB
    let kernel_code = &kernel_data[setup_size..];
    memory.write(layout::HIMEM_START, kernel_code)?;

    eprintln!(
        "[Boot] Loaded {} bytes of kernel code at {:#x}",
        kernel_code.len(),
        layout::HIMEM_START
    );

    // Extract setup header (0x1f1 to ~0x270) for boot_params
    let header_end = (SETUP_HEADER_OFFSET + 0x80).min(kernel_data.len());
    let setup_header = kernel_data[SETUP_HEADER_OFFSET..header_end].to_vec();

    eprintln!(
        "[Boot] Entry point at {:#x} (HIMEM_START + 0x200)",
        layout::HIMEM_START + 0x200
    );

    Ok(LoadedKernel { setup_header })
}
