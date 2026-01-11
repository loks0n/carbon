//! KVM (Kernel-based Virtual Machine) wrapper module.
//!
//! This module provides a safe Rust interface to Linux KVM for hardware-assisted
//! virtualization. KVM allows running virtual machines with near-native performance
//! by leveraging CPU virtualization extensions (Intel VT-x or AMD-V).
//!
//! # KVM Architecture
//!
//! KVM operates as a kernel module that turns Linux into a hypervisor. The VMM
//! (Virtual Machine Monitor, i.e., us) communicates with KVM through ioctls on:
//!
//! - `/dev/kvm` - System-level operations (check capabilities, create VMs)
//! - VM file descriptor - VM-level operations (create vCPUs, set memory)
//! - vCPU file descriptor - vCPU-level operations (run, get/set registers)
//!
//! ```text
//! User Space (VMM)                    Kernel Space (KVM)
//! ┌──────────────┐                   ┌──────────────────┐
//! │   Carbon     │                   │   KVM Module     │
//! │   (VMM)      │                   │                  │
//! │              │    ioctl()        │  ┌────────────┐  │
//! │  VmFd ───────┼──────────────────►│  │ VM State   │  │
//! │              │                   │  └────────────┘  │
//! │  VcpuFd ─────┼──────────────────►│  ┌────────────┐  │
//! │              │                   │  │ vCPU State │  │
//! └──────────────┘                   │  └────────────┘  │
//!                                    └────────┬─────────┘
//!                                             │
//!                                    ┌────────▼─────────┐
//!                                    │  CPU Hardware    │
//!                                    │  (VT-x / AMD-V)  │
//!                                    └──────────────────┘
//! ```
//!
//! # VM Execution Model
//!
//! The vCPU runs in a loop:
//!
//! 1. VMM calls `vcpu.run()` - control transfers to guest
//! 2. Guest executes until a "VM exit" occurs
//! 3. KVM returns control to VMM with exit reason
//! 4. VMM handles the exit (I/O, MMIO, etc.)
//! 5. VMM calls `vcpu.run()` again
//!
//! Common VM exit reasons:
//! - **I/O**: Guest accessed an I/O port (e.g., serial port)
//! - **MMIO**: Guest accessed unmapped memory (memory-mapped I/O)
//! - **HLT**: Guest executed HLT instruction
//! - **Shutdown**: Guest requested shutdown
//!
//! # Required VM Components
//!
//! For x86 virtualization, KVM requires:
//!
//! - **TSS Address**: Task State Segment location (Intel VT-x requirement)
//! - **IRQ Chip**: Interrupt controllers (PIC + IOAPIC) for handling interrupts
//! - **PIT**: Programmable Interval Timer for timing
//! - **CPUID**: CPU feature information exposed to guest
//! - **Memory Regions**: Guest physical memory mappings
//!
//! # Example Usage
//!
//! ```ignore
//! // Create a VM
//! let vm = kvm::create_vm()?;
//!
//! // Set up memory
//! vm.set_user_memory_region(0, 0, size, host_addr)?;
//!
//! // Create a vCPU
//! let mut vcpu = vm.create_vcpu(0)?;
//!
//! // Configure CPU state
//! vcpu.set_regs(&regs)?;
//! vcpu.set_sregs(&sregs)?;
//! vcpu.set_boot_msrs()?;
//!
//! // Run the VM
//! loop {
//!     match vcpu.run_with_io(&mut handler)? {
//!         VcpuExit::Io => { /* handled by handler */ }
//!         VcpuExit::Shutdown => break,
//!         _ => {}
//!     }
//! }
//! ```

mod vcpu;
mod vm;

pub use vcpu::{IoData, IoHandler, MmioHandler, VcpuExit, VcpuFd};
pub use vm::VmFd;

use kvm_bindings::KVM_MAX_CPUID_ENTRIES;
use kvm_ioctls::Kvm;
use thiserror::Error;

/// Errors that can occur during KVM operations.
#[derive(Error, Debug)]
pub enum KvmError {
    /// Failed to open /dev/kvm device.
    ///
    /// This usually means:
    /// - KVM is not available (not running on Linux, or KVM module not loaded)
    /// - Insufficient permissions (user not in kvm group)
    /// - Running in a VM without nested virtualization enabled
    #[error("Failed to open /dev/kvm: {0}")]
    OpenKvm(#[source] kvm_ioctls::Error),

    /// Failed to create a new VM.
    #[error("Failed to create VM: {0}")]
    CreateVm(#[source] kvm_ioctls::Error),

    /// Failed to create a vCPU.
    #[error("Failed to create vCPU: {0}")]
    CreateVcpu(#[source] kvm_ioctls::Error),

    /// Failed to register guest memory with KVM.
    #[error("Failed to set user memory region: {0}")]
    SetMemoryRegion(#[source] kvm_ioctls::Error),

    /// Failed to set CPU registers.
    #[error("Failed to set registers: {0}")]
    SetRegisters(#[source] kvm_ioctls::Error),

    /// Failed to get CPU registers.
    #[error("Failed to get registers: {0}")]
    GetRegisters(#[source] kvm_ioctls::Error),

    /// Failed to run vCPU.
    #[error("Failed to run vCPU: {0}")]
    Run(#[source] kvm_ioctls::Error),

    /// Failed to set TSS address (required for Intel VT-x).
    #[error("Failed to set TSS address: {0}")]
    SetTssAddress(#[source] kvm_ioctls::Error),

    /// Failed to create in-kernel IRQ chip.
    #[error("Failed to create IRQ chip: {0}")]
    CreateIrqChip(#[source] kvm_ioctls::Error),

    /// Failed to create PIT (Programmable Interval Timer).
    #[error("Failed to create PIT2: {0}")]
    CreatePit2(#[source] kvm_ioctls::Error),

    /// Failed to get supported CPUID entries from KVM.
    #[error("Failed to get supported CPUID: {0}")]
    GetSupportedCpuid(#[source] kvm_ioctls::Error),

    /// Failed to set CPUID entries on vCPU.
    #[error("Failed to set CPUID: {0}")]
    SetCpuid(#[source] kvm_ioctls::Error),

    /// Failed to set MSRs (Model Specific Registers).
    #[error("Failed to set MSRs: {0}")]
    SetMsrs(#[source] kvm_ioctls::Error),
}

/// Open the KVM device and create a new virtual machine.
///
/// This function:
/// 1. Opens `/dev/kvm` to access KVM functionality
/// 2. Queries supported CPUID entries (for passing to vCPUs)
/// 3. Creates a new VM
/// 4. Initializes required VM components (TSS, IRQ chip, PIT)
///
/// # CPUID
///
/// The CPUID instruction allows software to query CPU features. KVM provides
/// a filtered set of CPUID entries that reflect the host CPU's capabilities
/// while hiding features the guest shouldn't see. We query these entries
/// here and apply them to vCPUs when they're created.
///
/// # Returns
///
/// A `VmFd` that can be used to configure memory and create vCPUs.
///
/// # Errors
///
/// Returns an error if:
/// - KVM is not available or accessible
/// - VM creation fails
/// - Required VM components cannot be initialized
pub fn create_vm() -> Result<VmFd, KvmError> {
    // Open /dev/kvm
    let kvm = Kvm::new().map_err(KvmError::OpenKvm)?;

    // Query supported CPUID entries from KVM
    // These will be set on each vCPU so the guest sees appropriate CPU features
    let supported_cpuid = kvm
        .get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)
        .map_err(KvmError::GetSupportedCpuid)?;

    // Create the VM
    let vm = kvm.create_vm().map_err(KvmError::CreateVm)?;

    // Initialize VM components and return
    VmFd::new(vm, supported_cpuid)
}
