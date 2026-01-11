//! Boot orchestration for Linux kernel on x86_64.
//!
//! This module implements the Linux boot protocol for 64-bit kernels, allowing
//! a VMM to load and execute a Linux kernel directly without a traditional BIOS
//! or bootloader like GRUB.
//!
//! # Linux Boot Protocol Overview
//!
//! The Linux kernel supports being loaded directly by a boot loader or VMM using
//! a well-defined protocol documented at:
//! <https://www.kernel.org/doc/html/latest/x86/boot.html>
//!
//! The boot process requires:
//!
//! 1. **Kernel Loading**: The bzImage must be parsed to extract the protected-mode
//!    kernel code, which is loaded at the 1MB mark (0x100000).
//!
//! 2. **Boot Parameters**: A `boot_params` structure (also called the "zero page")
//!    must be populated with system information including:
//!    - Memory map (E820)
//!    - Command line location and contents
//!    - Hardware configuration
//!
//! 3. **CPU State**: The processor must be configured for 64-bit long mode with:
//!    - Page tables set up for identity mapping
//!    - GDT (Global Descriptor Table) with proper code/data segments
//!    - Control registers (CR0, CR3, CR4) configured for protected mode + paging
//!    - EFER MSR set for long mode
//!
//! 4. **Entry Point**: For 64-bit boot, execution begins at kernel_load_address + 0x200
//!    with RSI pointing to the boot_params structure.
//!
//! # Supported Kernel Versions
//!
//! This implementation requires Linux boot protocol version 2.06 or higher,
//! which was introduced in Linux 2.6.20 (February 2007). Any modern kernel
//! (including all actively maintained versions) is supported.
//!
//! # Memory Layout
//!
//! The guest physical memory is organized as follows:
//!
//! ```text
//! 0x0000_0000 - 0x0000_0500  Reserved (real-mode IVT, BDA)
//! 0x0000_0500 - 0x0000_0520  GDT (Global Descriptor Table)
//! 0x0000_0520 - 0x0000_0540  IDT (Interrupt Descriptor Table) - minimal, kernel sets its own
//! 0x0000_7000 - 0x0000_8000  boot_params (zero page)
//! 0x0000_8000 - 0x0000_9000  Stack space (grows downward from 0x8ff0)
//! 0x0000_9000 - 0x0000_a000  PML4 (Page Map Level 4)
//! 0x0000_a000 - 0x0000_b000  PDPTE (Page Directory Pointer Table Entry)
//! 0x0000_b000 - 0x0000_c000  PDE (Page Directory Entries for 2MB pages)
//! 0x0002_0000 - 0x0002_0800  Kernel command line
//! 0x0009_fc00 - 0x000a_0000  MP Table (EBDA region)
//! 0x0010_0000 - kernel_end   Kernel code (loaded from bzImage)
//! kernel_end  - mem_size     Available RAM for kernel use
//! ```
//!
//! # Memory Limits
//!
//! - **Minimum**: Guest memory must be > 1MB to load the kernel
//! - **Maximum for identity mapping**: 1GB (512 × 2MB pages)
//!   - The initial page tables identity-map only the first 1GB
//!   - The kernel sets up its own page tables during boot, so larger VMs work fine
//!
//! # Default Command Line Flags
//!
//! The default command line includes:
//! - `console=ttyS0` - Direct console output to first serial port (COM1)
//! - `noapic` - Disable APIC (we don't emulate it yet)
//! - `noacpi` - Disable ACPI (we don't emulate it yet)
//! - `nolapic` - Disable local APIC
//!
//! # Example Usage
//!
//! ```ignore
//! let vm = kvm::create_vm()?;
//! let memory = GuestMemory::new(512 * 1024 * 1024)?;
//! let config = BootConfig {
//!     kernel_path: "vmlinuz".to_string(),
//!     cmdline: "console=ttyS0".to_string(),
//!     mem_size: 512 * 1024 * 1024,
//! };
//! setup_boot(&vm, &memory, &config)?;
//! let vcpu = vm.create_vcpu(0)?;
//! vcpu.set_boot_msrs()?;
//! setup_vcpu_regs(&vcpu, &memory)?;
//! ```

mod acpi;
mod bzimage;
mod memory;
mod mptable;
mod paging;
mod params;

pub use acpi::{setup_acpi, VirtioDeviceConfig};
pub use memory::GuestMemory;
pub use mptable::setup_mptable;

use crate::kvm::{KvmError, VmFd};
use thiserror::Error;

/// Guest physical memory layout constants.
///
/// These addresses are chosen to match the expectations of the Linux boot protocol
/// and avoid conflicts with reserved memory regions. The layout must account for:
///
/// - Real-mode memory regions (IVT, BDA, EBDA) below 1MB
/// - The kernel's expectation of being loaded at the 1MB mark
/// - Page table structures that must be page-aligned (4KB boundaries)
/// - Stack space for early kernel initialization
pub mod layout {
    /// GDT (Global Descriptor Table) location.
    ///
    /// The GDT defines memory segments for code and data access. In long mode,
    /// segmentation is mostly disabled, but the GDT is still required for:
    /// - Code segment (CS) with L bit set for 64-bit mode
    /// - Data segments (DS, ES, FS, GS, SS)
    /// - Task State Segment (TSS) for interrupt handling
    pub const GDT_START: u64 = 0x500;

    /// IDT (Interrupt Descriptor Table) location.
    ///
    /// The IDT maps interrupt vectors to handler addresses. We provide a minimal
    /// (empty) IDT because the kernel sets up its own handlers during initialization.
    /// The IDT we provide is just a placeholder to satisfy CPU requirements.
    pub const IDT_START: u64 = 0x520;

    /// boot_params structure location (also known as the "zero page").
    ///
    /// This 4KB structure contains all the information the kernel needs to
    /// understand its environment: memory map, command line, video mode, etc.
    /// The kernel expects this at a fixed address and reads it during early init.
    pub const BOOT_PARAMS_START: u64 = 0x7000;

    /// Initial stack pointer for the boot CPU.
    ///
    /// The kernel needs a small stack for early initialization before it sets
    /// up its own. This points to the top of a small stack area. The stack
    /// grows downward, so RSP/RBP start here and decrease as the stack is used.
    pub const BOOT_STACK_POINTER: u64 = 0x8ff0;

    /// Kernel command line location.
    ///
    /// The command line is a null-terminated string passed to the kernel.
    /// It controls kernel behavior (e.g., "console=ttyS0 root=/dev/vda").
    /// The address is stored in boot_params and must be below 4GB for
    /// compatibility with the 32-bit command line pointer field.
    pub const CMDLINE_START: u64 = 0x2_0000;

    /// Maximum kernel command line size in bytes.
    ///
    /// Modern kernels support up to 2KB. Older boot protocols had smaller limits.
    pub const CMDLINE_MAX_SIZE: usize = 2048;

    /// High memory start address (1MB mark).
    ///
    /// The protected-mode kernel code is loaded here. The 1MB address is
    /// traditional for x86 kernels because:
    /// - The first 640KB (0x00000-0x9FFFF) is "conventional memory"
    /// - 640KB-1MB is reserved for BIOS, video memory, ROMs
    /// - Memory above 1MB is "extended memory" available for the kernel
    pub const HIMEM_START: u64 = 0x10_0000;

    /// Default guest memory size (512MB).
    pub const DEFAULT_MEM_SIZE: u64 = 512 * 1024 * 1024;
}

/// Errors that can occur during boot setup.
#[derive(Error, Debug)]
pub enum BootError {
    #[error("Failed to allocate guest memory: {0}")]
    MemoryAllocation(#[source] std::io::Error),

    #[error("KVM error: {0}")]
    Kvm(#[from] KvmError),

    #[error("Failed to read kernel: {0}")]
    ReadKernel(#[source] std::io::Error),

    #[error("Invalid kernel image: {0}")]
    InvalidKernel(String),

    #[error("Command line too long: {len} bytes (max {max})")]
    CmdlineTooLong { len: usize, max: usize },
}

/// Configuration for booting a Linux kernel.
pub struct BootConfig {
    /// Path to the kernel bzImage file.
    ///
    /// The bzImage is the standard format for bootable Linux kernels on x86.
    /// It contains a setup header, real-mode code, and compressed protected-mode code.
    pub kernel_path: String,

    /// Kernel command line arguments.
    ///
    /// Common options include:
    /// - `console=ttyS0` - Direct console output to first serial port
    /// - `root=/dev/vda` - Specify root filesystem device
    /// - `init=/bin/sh` - Override init process
    /// - `panic=-1` - Reboot on kernel panic
    /// - `noapic noacpi nolapic` - Disable APIC/ACPI (needed if not emulated)
    pub cmdline: String,

    /// Total guest memory size in bytes.
    ///
    /// This determines the E820 memory map provided to the kernel.
    /// The kernel uses this to know how much RAM is available.
    /// Must be > 1MB for kernel loading.
    pub mem_size: u64,
}

impl Default for BootConfig {
    fn default() -> Self {
        Self {
            kernel_path: String::new(),
            cmdline: "console=ttyS0".to_string(),
            mem_size: layout::DEFAULT_MEM_SIZE,
        }
    }
}

/// Set up the guest for booting Linux in 64-bit mode.
///
/// This function performs all the setup required before the vCPU can begin
/// executing the kernel:
///
/// 1. Loads the kernel from the bzImage file into guest memory at 1MB
/// 2. Sets up the boot_params structure with memory map and configuration
/// 3. Creates identity-mapped page tables for the first 1GB of memory
/// 4. Registers the guest memory region with KVM
///
/// After this function returns, call `setup_vcpu_regs` to configure the
/// vCPU's registers, then the vCPU is ready to run.
pub fn setup_boot(vm: &VmFd, memory: &GuestMemory, config: &BootConfig) -> Result<(), BootError> {
    // Load the kernel from bzImage into guest memory
    let loaded_kernel = bzimage::load_kernel(memory, &config.kernel_path)?;

    // Populate the boot_params structure with memory map, cmdline, etc.
    params::setup_boot_params(memory, config, &loaded_kernel)?;

    // Create page tables for 64-bit mode (identity mapping first 1GB)
    paging::setup_page_tables(memory)?;

    // Register the guest memory region with KVM so the CPU can access it
    let (host_addr, size) = memory.as_raw_parts();
    unsafe {
        vm.set_user_memory_region(0, 0, size, host_addr)?;
    }

    Ok(())
}

/// Configure vCPU registers for 64-bit Linux boot.
///
/// Sets up all CPU state required by the Linux boot protocol:
///
/// - **Control registers**: CR0 (protected mode + paging), CR3 (page table base),
///   CR4 (PAE), EFER (long mode enable)
/// - **Segment registers**: CS, DS, ES, FS, GS, SS, TR loaded from GDT
/// - **GDT/IDT**: Descriptor table registers pointing to our tables in memory
/// - **General registers**: RIP (entry point), RSP/RBP (stack), RSI (boot_params)
/// - **FPU state**: x87 control word and MXCSR for SSE
///
/// The kernel entry point for 64-bit boot is at kernel_load_address + 0x200.
/// This offset accounts for the real-mode entry point at +0x000 (unused for
/// direct 64-bit boot) and the 64-bit entry at +0x200.
pub fn setup_vcpu_regs(vcpu: &crate::kvm::VcpuFd, memory: &GuestMemory) -> Result<(), BootError> {
    paging::setup_cpu_regs(vcpu, memory)?;
    Ok(())
}
