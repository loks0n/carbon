//! Virtual Machine creation and memory management.
//!
//! This module handles VM-level KVM operations including:
//! - Initializing required VM components (TSS, IRQ chip, PIT)
//! - Registering guest memory regions
//! - Creating vCPUs
//!
//! # VM Initialization
//!
//! Before a VM can run, several x86-specific components must be initialized:
//!
//! ## TSS (Task State Segment)
//!
//! The TSS is an x86 structure that holds information about a task, including
//! stack pointers for different privilege levels. Intel VT-x requires a valid
//! TSS address even though we don't use hardware task switching.
//!
//! ## IRQ Chip (Interrupt Controllers)
//!
//! x86 PCs use two types of interrupt controllers:
//!
//! - **PIC** (8259A): Legacy Programmable Interrupt Controller, handles IRQ 0-15
//! - **IOAPIC**: Modern interrupt controller for PCI devices, IRQ routing
//!
//! KVM can emulate these in-kernel for better performance. The `create_irq_chip`
//! call sets up both PIC and IOAPIC.
//!
//! ## PIT (Programmable Interval Timer)
//!
//! The 8254 PIT provides timing services. It's historically connected to IRQ 0
//! and generates periodic interrupts. Even though modern systems use other
//! timers (HPET, TSC), the kernel still expects a PIT during early boot.
//!
//! # Memory Regions
//!
//! Guest memory is managed through "memory slots". Each slot maps a range of
//! guest physical addresses to host virtual addresses:
//!
//! ```text
//! Guest Physical          Host Virtual
//! ┌──────────────┐       ┌──────────────┐
//! │ 0x00000000   │ ────► │ mmap'd region│
//! │              │       │              │
//! │ 0x1FFFFFFF   │       │              │
//! └──────────────┘       └──────────────┘
//!     512 MB                 512 MB
//! ```
//!
//! KVM uses EPT (Extended Page Tables) or NPT (Nested Page Tables) to translate
//! guest physical addresses to host physical addresses through the host's MMU.

use super::{KvmError, VcpuFd};
use kvm_bindings::{
    kvm_cpuid_entry2, kvm_pit_config, kvm_userspace_memory_region, CpuId, KVM_PIT_SPEAKER_DUMMY,
};

/// Wrapper around the KVM VM file descriptor.
///
/// This structure represents a virtual machine and provides methods for:
/// - Registering guest memory regions
/// - Creating virtual CPUs
///
/// The VM is automatically initialized with required x86 components
/// (TSS address, IRQ chip, PIT) when created via `VmFd::new()`.
pub struct VmFd {
    /// The underlying KVM VM file descriptor.
    vm: kvm_ioctls::VmFd,

    /// Supported CPUID entries to apply to new vCPUs.
    ///
    /// When a guest executes CPUID, KVM returns these entries.
    /// This tells the guest what CPU features are available.
    supported_cpuid: CpuId,
}

impl VmFd {
    /// Create a new VmFd wrapper, initializing required x86 VM components.
    ///
    /// This function sets up:
    ///
    /// 1. **TSS Address** (0xfffbd000): Required by Intel VT-x for virtualization.
    ///    The address is in an unused region of the physical address space.
    ///
    /// 2. **IRQ Chip**: Creates in-kernel PIC and IOAPIC emulation.
    ///    This allows interrupt handling without VM exits for common cases.
    ///
    /// 3. **PIT**: Creates the 8254 Programmable Interval Timer.
    ///    We use `KVM_PIT_SPEAKER_DUMMY` to disable PC speaker emulation.
    ///
    /// # Arguments
    ///
    /// * `vm` - Raw KVM VM file descriptor
    /// * `supported_cpuid` - CPUID entries to apply to vCPUs
    ///
    /// # Errors
    ///
    /// Returns an error if any component fails to initialize.
    pub fn new(vm: kvm_ioctls::VmFd, supported_cpuid: CpuId) -> Result<Self, KvmError> {
        // Set TSS address (required for Intel VT-x)
        //
        // The TSS address must be set before creating vCPUs. We use an address
        // in the "hole" between 3GB and 4GB that's typically unused. This
        // doesn't need to point to valid memory; KVM just needs a valid address.
        vm.set_tss_address(0xfffb_d000)
            .map_err(KvmError::SetTssAddress)?;

        // Create the in-kernel IRQ chip (PIC + IOAPIC)
        //
        // This enables efficient interrupt handling:
        // - PIC: 8259A emulation for legacy interrupts (IRQ 0-15)
        // - IOAPIC: Modern interrupt routing for PCI devices
        //
        // Without this, every interrupt would cause a VM exit.
        vm.create_irq_chip().map_err(KvmError::CreateIrqChip)?;

        // Create PIT (Programmable Interval Timer)
        //
        // The 8254 PIT provides system timing. Even though modern kernels
        // prefer other time sources (TSC, HPET), the PIT is still used
        // during early boot for calibration.
        //
        // KVM_PIT_SPEAKER_DUMMY disables the PC speaker output (port 0x61).
        let pit_config = kvm_pit_config {
            flags: KVM_PIT_SPEAKER_DUMMY,
            ..Default::default()
        };
        vm.create_pit2(pit_config).map_err(KvmError::CreatePit2)?;

        Ok(Self {
            vm,
            supported_cpuid,
        })
    }

    /// Register a guest memory region with KVM.
    ///
    /// This maps a range of guest physical addresses to a region of host
    /// virtual memory. After registration, guest accesses to these physical
    /// addresses will transparently access the host memory.
    ///
    /// # Arguments
    ///
    /// * `slot` - Memory slot number (0 for the first/main region)
    /// * `guest_addr` - Starting guest physical address (usually 0)
    /// * `memory_size` - Size of the region in bytes
    /// * `userspace_addr` - Host virtual address of the memory (from mmap)
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// - The host memory region remains valid for the lifetime of the VM
    /// - The memory is not freed while the VM is running
    /// - The region doesn't overlap with other registered regions
    ///
    /// # Memory Slot Management
    ///
    /// Each memory region is identified by a slot number. Slot 0 is typically
    /// used for the main guest RAM. Additional slots can be used for:
    /// - Device memory (e.g., framebuffer)
    /// - ROM regions
    /// - Memory hotplug
    pub unsafe fn set_user_memory_region(
        &self,
        slot: u32,
        guest_addr: u64,
        memory_size: u64,
        userspace_addr: u64,
    ) -> Result<(), KvmError> {
        let region = kvm_userspace_memory_region {
            slot,
            guest_phys_addr: guest_addr,
            memory_size,
            userspace_addr,
            flags: 0, // No special flags (could use KVM_MEM_READONLY, etc.)
        };

        unsafe {
            self.vm
                .set_user_memory_region(region)
                .map_err(KvmError::SetMemoryRegion)
        }
    }

    /// Create a new virtual CPU.
    ///
    /// This creates a vCPU with the specified ID and automatically configures
    /// its CPUID entries from the VM's supported_cpuid list.
    ///
    /// # Arguments
    ///
    /// * `id` - vCPU ID (0 for the first/boot CPU)
    ///
    /// # CPUID Setup
    ///
    /// The CPUID entries are set immediately after vCPU creation. These entries
    /// determine what features the guest sees when it executes CPUID:
    ///
    /// - Processor identification (vendor, family, model)
    /// - Feature flags (SSE, AVX, etc.)
    /// - Cache information
    /// - Topology (cores, threads)
    ///
    /// # Multi-vCPU Support
    ///
    /// For SMP guests, create multiple vCPUs with sequential IDs.
    /// vCPU 0 is the BSP (Bootstrap Processor) that runs first.
    /// Other vCPUs are APs (Application Processors) started by the BSP.
    pub fn create_vcpu(&self, id: u64) -> Result<VcpuFd, KvmError> {
        // Create the vCPU
        let vcpu = self.vm.create_vcpu(id).map_err(KvmError::CreateVcpu)?;

        // Get TSC frequency from KVM for fast boot (avoids calibration)
        let tsc_khz = vcpu.get_tsc_khz().unwrap_or(0);

        // Build CPUID with TSC frequency if available
        let cpuid = if tsc_khz > 0 {
            self.build_cpuid_with_tsc(tsc_khz)?
        } else {
            self.supported_cpuid.clone()
        };

        // Configure CPUID entries
        //
        // This must be done before the first vcpu.run() call.
        // The entries tell the guest what CPU features are available.
        vcpu.set_cpuid2(&cpuid).map_err(KvmError::SetCpuid)?;

        if tsc_khz > 0 {
            eprintln!(
                "[KVM] Set {} CPUID entries on vCPU {} (TSC: {} kHz)",
                cpuid.as_slice().len(),
                id,
                tsc_khz
            );
        } else {
            eprintln!(
                "[KVM] Set {} CPUID entries on vCPU {}",
                cpuid.as_slice().len(),
                id
            );
        }

        Ok(VcpuFd::new(vcpu))
    }

    /// Build CPUID entries with TSC frequency for fast boot.
    ///
    /// Adds KVM paravirt CPUID leaves:
    /// - 0x40000000: KVM signature ("KVMKVMKVM")
    /// - 0x40000001: KVM features (clocksource, async PF, etc.)
    /// - 0x40000010: TSC frequency in kHz
    fn build_cpuid_with_tsc(&self, tsc_khz: u32) -> Result<CpuId, KvmError> {
        let mut entries: Vec<kvm_cpuid_entry2> = self.supported_cpuid.as_slice().to_vec();

        // Set hypervisor bit (ECX bit 31) in CPUID leaf 1
        // This tells the guest it's running in a VM
        for entry in &mut entries {
            if entry.function == 1 {
                entry.ecx |= 1 << 31; // X86_FEATURE_HYPERVISOR
            }
        }

        // Remove any existing KVM leaves (we'll add our own)
        entries.retain(|e| e.function < 0x40000000 || e.function > 0x400000ff);

        // KVM signature leaf (0x40000000)
        // Signature "KVMKVMKVM\0\0\0" stored as little-endian u32s
        entries.push(kvm_cpuid_entry2 {
            function: 0x40000000,
            index: 0,
            flags: 0,
            eax: 0x40000010, // Max KVM leaf supported
            ebx: 0x4b4d564b, // "KVMK" as little-endian
            ecx: 0x564b4d56, // "VMKV" as little-endian
            edx: 0x0000004d, // "M\0\0\0" as little-endian
            ..Default::default()
        });

        // KVM features leaf (0x40000001)
        // Enable paravirt features for fast boot
        const KVM_FEATURE_CLOCKSOURCE: u32 = 1 << 0; // kvm-clock v1
        const KVM_FEATURE_NOP_IO_DELAY: u32 = 1 << 1; // Skip I/O port delays (outb_p -> outb)
        const KVM_FEATURE_CLOCKSOURCE2: u32 = 1 << 3; // kvm-clock v2
        const KVM_FEATURE_ASYNC_PF: u32 = 1 << 4; // Async page faults
        const KVM_FEATURE_PV_EOI: u32 = 1 << 6; // Paravirtual EOI (faster interrupts)
        const KVM_FEATURE_PV_UNHALT: u32 = 1 << 7; // Paravirtual unhalt
        const KVM_FEATURE_CLOCKSOURCE_STABLE_BIT: u32 = 1 << 24; // TSC is stable

        entries.push(kvm_cpuid_entry2 {
            function: 0x40000001,
            index: 0,
            flags: 0,
            eax: KVM_FEATURE_CLOCKSOURCE
                | KVM_FEATURE_NOP_IO_DELAY
                | KVM_FEATURE_CLOCKSOURCE2
                | KVM_FEATURE_ASYNC_PF
                | KVM_FEATURE_PV_EOI
                | KVM_FEATURE_PV_UNHALT
                | KVM_FEATURE_CLOCKSOURCE_STABLE_BIT,
            ebx: 0,
            ecx: 0,
            edx: 0,
            ..Default::default()
        });

        // TSC frequency leaf (0x40000010)
        // EAX = TSC frequency in kHz - avoids slow PIT calibration
        entries.push(kvm_cpuid_entry2 {
            function: 0x40000010,
            index: 0,
            flags: 0,
            eax: tsc_khz,
            ebx: 0, // LAPIC timer frequency (optional)
            ecx: 0,
            edx: 0,
            ..Default::default()
        });

        CpuId::from_entries(&entries).map_err(|_| KvmError::SetCpuid(kvm_ioctls::Error::new(22)))
    }
}
