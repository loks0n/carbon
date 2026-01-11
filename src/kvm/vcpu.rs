//! Virtual CPU management and execution.
//!
//! This module provides the vCPU abstraction for running guest code. A vCPU
//! represents a virtual processor that executes guest instructions using
//! hardware-assisted virtualization.
//!
//! # vCPU Execution Model
//!
//! The vCPU operates in a run loop:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                      VMM (User Space)                   │
//! │  ┌─────────┐         ┌─────────────┐                   │
//! │  │  Loop   │◄────────│ Handle Exit │                   │
//! │  │  Start  │         │  (I/O, etc) │                   │
//! │  └────┬────┘         └──────▲──────┘                   │
//! │       │                     │                          │
//! │       │ vcpu.run()          │ VM Exit                  │
//! │       ▼                     │                          │
//! ├───────┼─────────────────────┼──────────────────────────┤
//! │       │      KVM (Kernel)   │                          │
//! │       │                     │                          │
//! │       ▼                     │                          │
//! │  ┌─────────┐          ┌─────┴─────┐                    │
//! │  │  VMXON  │─────────►│   VMEXIT  │                    │
//! │  │ /VMRUN  │  Guest   │           │                    │
//! │  └─────────┘  Runs    └───────────┘                    │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! # VM Exits
//!
//! When the guest performs certain operations, the CPU exits back to the VMM.
//! Common exit reasons include:
//!
//! - **I/O Port Access**: Guest used IN/OUT instructions
//! - **MMIO**: Guest accessed unmapped memory
//! - **HLT**: Guest executed HLT (halt until interrupt)
//! - **Shutdown**: Triple fault or explicit shutdown
//! - **External Interrupt**: Interrupt for the host
//!
//! # I/O Handling
//!
//! When the guest accesses I/O ports (e.g., serial port at 0x3F8), KVM exits
//! to the VMM with the port number, direction (in/out), and data. The VMM
//! must emulate the device and, for reads, provide the response data.
//!
//! The `IoHandler` trait provides a clean interface for device emulation.
//! It uses fixed-size arrays (max 4 bytes) to avoid heap allocation.
//!
//! # CPU State
//!
//! The vCPU state includes:
//!
//! - **General registers**: RAX, RBX, RCX, RDX, RSI, RDI, RSP, RBP, R8-R15
//! - **Special registers**: CR0, CR3, CR4, EFER, segment registers
//! - **FPU/SSE state**: x87 registers, XMM registers, MXCSR
//! - **MSRs**: Model-specific registers (EFER, STAR, LSTAR, etc.)

use super::KvmError;
use kvm_bindings::{kvm_fpu, kvm_msr_entry, kvm_regs, kvm_sregs, Msrs};
use kvm_ioctls::VcpuExit as KvmVcpuExit;

/// Model-Specific Register (MSR) indices.
///
/// MSRs are CPU registers that control various processor features and provide
/// system software with ways to configure CPU behavior. These particular MSRs
/// are required for Linux boot on x86_64.
mod msr {
    /// SYSENTER_CS - Code segment for SYSENTER instruction (32-bit syscalls).
    pub const IA32_SYSENTER_CS: u32 = 0x174;

    /// SYSENTER_ESP - Stack pointer for SYSENTER instruction.
    pub const IA32_SYSENTER_ESP: u32 = 0x175;

    /// SYSENTER_EIP - Instruction pointer for SYSENTER instruction.
    pub const IA32_SYSENTER_EIP: u32 = 0x176;

    /// STAR - Segment selectors for SYSCALL/SYSRET.
    pub const STAR: u32 = 0xc000_0081;

    /// LSTAR - Long mode SYSCALL target RIP.
    pub const LSTAR: u32 = 0xc000_0082;

    /// CSTAR - Compatibility mode SYSCALL target RIP.
    pub const CSTAR: u32 = 0xc000_0083;

    /// SYSCALL_MASK - RFLAGS mask for SYSCALL.
    pub const SYSCALL_MASK: u32 = 0xc000_0084;

    /// KERNEL_GS_BASE - Swap target for SWAPGS instruction.
    pub const KERNEL_GS_BASE: u32 = 0xc000_0102;

    /// TSC - Time Stamp Counter.
    pub const IA32_TSC: u32 = 0x10;

    /// MISC_ENABLE - Miscellaneous feature enables.
    pub const IA32_MISC_ENABLE: u32 = 0x1a0;

    /// MTRR default type - Memory Type Range Register default.
    pub const MTRR_DEF_TYPE: u32 = 0x2ff;

    /// Bit 0 of MISC_ENABLE: Fast string operations.
    pub const MISC_ENABLE_FAST_STRING: u64 = 1;
}

/// Maximum size for I/O operations (x86 supports 1, 2, or 4 byte I/O).
pub const MAX_IO_SIZE: usize = 4;

/// Fixed-size I/O data buffer to avoid heap allocation.
///
/// x86 IN/OUT instructions support 1, 2, or 4 byte operations.
/// This type holds the data without allocating.
#[derive(Debug, Clone, Copy)]
pub struct IoData {
    /// The data bytes (only first `len` bytes are valid).
    data: [u8; MAX_IO_SIZE],
    /// Number of valid bytes (1, 2, or 4).
    len: u8,
}

impl IoData {
    /// Create a new IoData with the specified length.
    #[inline]
    pub fn new(len: usize) -> Self {
        debug_assert!(len <= MAX_IO_SIZE);
        Self {
            data: [0; MAX_IO_SIZE],
            len: len as u8,
        }
    }

    /// Create IoData from a slice.
    #[inline]
    pub fn from_slice(slice: &[u8]) -> Self {
        let len = slice.len().min(MAX_IO_SIZE);
        let mut data = [0u8; MAX_IO_SIZE];
        data[..len].copy_from_slice(&slice[..len]);
        Self {
            data,
            len: len as u8,
        }
    }

    /// Get the data as a slice.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// Get the length.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Set a byte at index.
    #[inline]
    pub fn set(&mut self, index: usize, value: u8) {
        if index < self.len as usize {
            self.data[index] = value;
        }
    }
}

impl Default for IoData {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Wrapper around the KVM vCPU file descriptor.
///
/// Provides methods to:
/// - Get/set CPU registers (general, special, FPU)
/// - Configure MSRs for boot
/// - Run the vCPU and handle exits
pub struct VcpuFd {
    /// The underlying KVM vCPU file descriptor.
    vcpu: kvm_ioctls::VcpuFd,
}

/// Exit reasons from vCPU execution.
///
/// When `run()` returns, it indicates why the guest stopped executing.
/// The VMM must handle the exit appropriately and typically call `run()`
/// again to continue execution.
#[derive(Debug)]
pub enum VcpuExit {
    /// I/O port or MMIO operation was handled.
    ///
    /// The IoHandler/MmioHandler already processed this; just continue running.
    Io,

    /// Guest executed HLT instruction.
    ///
    /// The CPU is waiting for an interrupt. The VMM can either:
    /// - Inject an interrupt and continue
    /// - Wait for an external event
    /// - Terminate if no more work to do
    Hlt,

    /// Guest requested shutdown.
    ///
    /// This happens on triple fault or explicit shutdown request.
    Shutdown,

    /// KVM internal error occurred.
    InternalError,

    /// Failed to enter guest mode.
    ///
    /// Contains the hardware-specific failure reason code.
    FailEntry(u64),

    /// System event (e.g., S3 sleep, reset).
    ///
    /// Contains the event type code.
    SystemEvent(u32),

    /// Unknown or unhandled exit reason.
    ///
    /// Contains a static description of the exit type.
    Unknown(&'static str),
}

/// Trait for handling I/O port operations.
///
/// When the guest executes IN or OUT instructions, KVM exits to the VMM.
/// The IoHandler processes these operations, typically by emulating
/// a device like a serial port.
///
/// # Example
///
/// ```ignore
/// struct MyDevices {
///     serial: Serial8250,
/// }
///
/// impl IoHandler for MyDevices {
///     fn io_read(&mut self, port: u16, data: &mut IoData) {
///         if port == 0x3f8 {
///             data.set(0, self.serial.read());
///         } else {
///             // Return 0xff for unhandled ports
///             for i in 0..data.len() {
///                 data.set(i, 0xff);
///             }
///         }
///     }
///
///     fn io_write(&mut self, port: u16, data: &IoData) {
///         if port == 0x3f8 {
///             if let Some(byte) = data.get(0) {
///                 self.serial.write(byte);
///             }
///         }
///     }
/// }
/// ```
pub trait IoHandler {
    /// Handle an I/O port read (IN instruction).
    ///
    /// The guest is trying to read from `port`. Fill `data` with the
    /// response (data.len() bytes).
    ///
    /// # Arguments
    ///
    /// * `port` - I/O port number (0x0000-0xFFFF)
    /// * `data` - Buffer to fill with response (pre-sized to 1, 2, or 4 bytes)
    fn io_read(&mut self, port: u16, data: &mut IoData);

    /// Handle an I/O port write (OUT instruction).
    ///
    /// The guest is writing `data` to `port`.
    ///
    /// # Arguments
    ///
    /// * `port` - I/O port number (0x0000-0xFFFF)
    /// * `data` - Data being written (1, 2, or 4 bytes)
    fn io_write(&mut self, port: u16, data: &IoData);
}

/// Trait for handling memory-mapped I/O (MMIO) operations.
///
/// When the guest accesses unmapped memory regions (e.g., device registers
/// at 0xd0000000), KVM exits to the VMM. The MmioHandler processes these
/// operations by emulating device register access.
///
/// MMIO is used by virtio-mmio devices for configuration and virtqueue
/// notification.
pub trait MmioHandler {
    /// Handle an MMIO read operation.
    ///
    /// The guest is reading from `addr`. Fill `data` with the response.
    ///
    /// # Arguments
    ///
    /// * `addr` - Guest physical address being read
    /// * `data` - Buffer to fill with response (typically 1, 2, 4, or 8 bytes)
    fn mmio_read(&mut self, addr: u64, data: &mut [u8]);

    /// Handle an MMIO write operation.
    ///
    /// The guest is writing `data` to `addr`.
    ///
    /// # Arguments
    ///
    /// * `addr` - Guest physical address being written
    /// * `data` - Data being written (typically 1, 2, 4, or 8 bytes)
    fn mmio_write(&mut self, addr: u64, data: &[u8]);
}

impl VcpuFd {
    /// Create a new VcpuFd wrapper.
    pub fn new(vcpu: kvm_ioctls::VcpuFd) -> Self {
        Self { vcpu }
    }

    /// Get the current general-purpose registers.
    pub fn get_regs(&self) -> Result<kvm_regs, KvmError> {
        self.vcpu.get_regs().map_err(KvmError::GetRegisters)
    }

    /// Set the general-purpose registers.
    pub fn set_regs(&self, regs: &kvm_regs) -> Result<(), KvmError> {
        self.vcpu.set_regs(regs).map_err(KvmError::SetRegisters)
    }

    /// Get the special registers.
    pub fn get_sregs(&self) -> Result<kvm_sregs, KvmError> {
        self.vcpu.get_sregs().map_err(KvmError::GetRegisters)
    }

    /// Set the special registers.
    pub fn set_sregs(&self, sregs: &kvm_sregs) -> Result<(), KvmError> {
        self.vcpu.set_sregs(sregs).map_err(KvmError::SetRegisters)
    }

    /// Set the FPU/SSE state.
    pub fn set_fpu(&self, fpu: &kvm_fpu) -> Result<(), KvmError> {
        self.vcpu.set_fpu(fpu).map_err(KvmError::SetRegisters)
    }

    /// Set up MSRs required for Linux boot.
    ///
    /// Configures Model-Specific Registers needed for 64-bit Linux:
    ///
    /// - **SYSENTER MSRs**: For 32-bit system calls (legacy, but expected)
    /// - **SYSCALL MSRs**: For 64-bit system calls (STAR, LSTAR, CSTAR, SYSCALL_MASK)
    /// - **KERNEL_GS_BASE**: For per-CPU data access via SWAPGS
    /// - **TSC**: Time Stamp Counter (initialized to 0)
    /// - **MISC_ENABLE**: Enable fast string operations
    /// - **MTRR_DEF_TYPE**: Set default memory type to write-back
    pub fn set_boot_msrs(&self) -> Result<(), KvmError> {
        let msr_entry = |index: u32, data: u64| kvm_msr_entry {
            index,
            data,
            ..Default::default()
        };

        let entries = vec![
            msr_entry(msr::IA32_SYSENTER_CS, 0),
            msr_entry(msr::IA32_SYSENTER_ESP, 0),
            msr_entry(msr::IA32_SYSENTER_EIP, 0),
            msr_entry(msr::STAR, 0),
            msr_entry(msr::CSTAR, 0),
            msr_entry(msr::KERNEL_GS_BASE, 0),
            msr_entry(msr::SYSCALL_MASK, 0),
            msr_entry(msr::LSTAR, 0),
            msr_entry(msr::IA32_TSC, 0),
            msr_entry(msr::IA32_MISC_ENABLE, msr::MISC_ENABLE_FAST_STRING),
            msr_entry(msr::MTRR_DEF_TYPE, (1 << 11) | 6),
        ];

        let msrs = Msrs::from_entries(&entries).expect("failed to create MSRs");
        self.vcpu.set_msrs(&msrs).map_err(KvmError::SetMsrs)?;

        eprintln!("[KVM] Set {} boot MSRs", entries.len());
        Ok(())
    }

    /// Run the vCPU until it exits, handling I/O and MMIO with the provided handler.
    ///
    /// This is the main execution loop entry point. It:
    /// 1. Enters guest mode (VMRESUME/VMRUN)
    /// 2. Executes guest code until a VM exit
    /// 3. Returns with the exit reason
    ///
    /// For I/O exits (IN/OUT instructions), the handler is called immediately
    /// and data is exchanged with KVM's buffers. For MMIO exits, the handler
    /// processes memory-mapped device access.
    pub fn run_with_io<H: IoHandler + MmioHandler>(
        &mut self,
        handler: &mut H,
    ) -> Result<VcpuExit, KvmError> {
        match self.vcpu.run().map_err(KvmError::Run)? {
            KvmVcpuExit::IoIn(port, data) => {
                let mut io_data = IoData::new(data.len());
                handler.io_read(port, &mut io_data);
                let copy_len = io_data.len().min(data.len());
                data[..copy_len].copy_from_slice(&io_data.as_slice()[..copy_len]);
                Ok(VcpuExit::Io)
            }

            KvmVcpuExit::IoOut(port, data) => {
                let io_data = IoData::from_slice(data);
                handler.io_write(port, &io_data);
                Ok(VcpuExit::Io)
            }

            KvmVcpuExit::MmioRead(addr, data) => {
                handler.mmio_read(addr, data);
                Ok(VcpuExit::Io) // Return Io since we handled it inline
            }

            KvmVcpuExit::MmioWrite(addr, data) => {
                handler.mmio_write(addr, data);
                Ok(VcpuExit::Io) // Return Io since we handled it inline
            }

            KvmVcpuExit::Hlt => Ok(VcpuExit::Hlt),
            KvmVcpuExit::Shutdown => Ok(VcpuExit::Shutdown),
            KvmVcpuExit::InternalError => Ok(VcpuExit::InternalError),
            KvmVcpuExit::SystemEvent(event, _) => Ok(VcpuExit::SystemEvent(event)),
            KvmVcpuExit::FailEntry(reason, _) => Ok(VcpuExit::FailEntry(reason)),

            // Map known exits to static strings
            KvmVcpuExit::Hypercall(_) => Ok(VcpuExit::Unknown("Hypercall")),
            KvmVcpuExit::Debug(_) => Ok(VcpuExit::Unknown("Debug")),
            KvmVcpuExit::Exception => Ok(VcpuExit::Unknown("Exception")),
            KvmVcpuExit::IrqWindowOpen => Ok(VcpuExit::Unknown("IrqWindowOpen")),
            KvmVcpuExit::S390Sieic => Ok(VcpuExit::Unknown("S390Sieic")),
            KvmVcpuExit::S390Reset => Ok(VcpuExit::Unknown("S390Reset")),
            KvmVcpuExit::Dcr => Ok(VcpuExit::Unknown("Dcr")),
            KvmVcpuExit::Nmi => Ok(VcpuExit::Unknown("Nmi")),
            KvmVcpuExit::Watchdog => Ok(VcpuExit::Unknown("Watchdog")),
            KvmVcpuExit::Epr => Ok(VcpuExit::Unknown("Epr")),
            _ => Ok(VcpuExit::Unknown("Other")),
        }
    }
}
