//! Page table and CPU register setup for 64-bit Linux boot.
//!
//! This module configures the x86_64 CPU state required to boot Linux in 64-bit
//! long mode. The Linux boot protocol requires the CPU to be in a specific state
//! when entering the kernel's 64-bit entry point.
//!
//! # x86_64 Long Mode Requirements
//!
//! To run in 64-bit long mode, the CPU must have:
//!
//! 1. **Paging enabled** (CR0.PG = 1)
//! 2. **Physical Address Extension** (CR4.PAE = 1)
//! 3. **Long Mode Enable** in EFER MSR (EFER.LME = 1)
//! 4. **Long Mode Active** in EFER MSR (EFER.LMA = 1, set automatically)
//! 5. **Page tables** set up with CR3 pointing to PML4
//!
//! # Page Table Structure
//!
//! x86_64 uses a 4-level page table hierarchy:
//!
//! ```text
//! CR3 → PML4 (Page Map Level 4) → PDPTE → PDE → PTE → Physical Page
//!       512 entries              512     512    512
//!       each covers 512GB        1GB     2MB    4KB
//! ```
//!
//! For simplicity, we use 2MB "huge pages" which eliminates the PTE level:
//!
//! ```text
//! CR3 → PML4 → PDPTE → PDE (with PS bit) → 2MB Physical Page
//! ```
//!
//! This gives us identity-mapped (virtual = physical) access to the first 1GB
//! of memory, which is sufficient for early kernel boot. The kernel sets up
//! its own page tables during initialization and can map all available memory.
//!
//! # Global Descriptor Table (GDT)
//!
//! Even though segmentation is mostly disabled in long mode, the GDT is still
//! required. The CPU needs:
//!
//! - **Null descriptor** (index 0): Required, never used
//! - **Code segment** (CS): Must have L bit set for 64-bit mode
//! - **Data segment** (DS/ES/FS/GS/SS): Standard data segment
//! - **TSS** (TR): Task State Segment descriptor
//!
//! Note: The TSS GDT entry points to base 0, which isn't a real TSS structure.
//! This works because KVM uses its own TSS set up via `set_tss_address()`, not ours.
//! The GDT TSS entry is just needed so the TR register can be loaded with a valid selector.
//!
//! # Interrupt Descriptor Table (IDT)
//!
//! We provide a minimal (empty) IDT. The kernel immediately sets up its own IDT
//! during early initialization, so ours is just a placeholder to satisfy CPU
//! requirements. The IDT we provide has limit 0 (no valid entries).
//!
//! # Register Setup for Linux Boot
//!
//! The Linux 64-bit boot protocol expects:
//!
//! - **RIP**: Kernel entry point (load_address + 0x200)
//! - **RSI**: Pointer to boot_params structure
//! - **RSP/RBP**: Valid stack pointer
//! - **RFLAGS**: Interrupts disabled, reserved bit 1 set
//! - **CS**: 64-bit code segment
//! - **DS/ES/FS/GS/SS**: Valid data segments
//!
//! Reference: <https://www.kernel.org/doc/html/latest/x86/boot.html#id1>

use super::layout;
use super::memory::GuestMemory;
use super::BootError;
use crate::kvm::VcpuFd;
use kvm_bindings::{kvm_fpu, kvm_regs, kvm_segment};

// ============================================================================
// Page Table Addresses
// ============================================================================

/// PML4 (Page Map Level 4) table address.
///
/// This is the top-level page table, pointed to by CR3.
/// Each entry covers 512GB of virtual address space.
const PML4_START: u64 = 0x9000;

/// PDPTE (Page Directory Pointer Table Entry) address.
///
/// Second level of the page table hierarchy.
/// Each entry covers 1GB of virtual address space.
const PDPTE_START: u64 = 0xa000;

/// PDE (Page Directory Entry) table address.
///
/// Third level of the page table hierarchy.
/// With 2MB pages (PS bit set), each entry maps directly to a 2MB physical page.
const PDE_START: u64 = 0xb000;

// ============================================================================
// Control Register Flags
// ============================================================================

/// CR0.PE - Protection Enable.
///
/// Enables protected mode. Must be set for long mode to work.
/// When PE=1, the CPU uses segment descriptors from the GDT/LDT.
const X86_CR0_PE: u64 = 0x1;

/// CR0.PG - Paging Enable.
///
/// Enables paging. Must be set for long mode.
/// When PG=1, virtual addresses are translated through page tables.
const X86_CR0_PG: u64 = 0x8000_0000;

/// CR4.PAE - Physical Address Extension.
///
/// Enables 64-bit page table entries, required for long mode.
/// With PAE, page tables use 64-bit entries (vs 32-bit without PAE).
const X86_CR4_PAE: u64 = 0x20;

/// EFER.LME - Long Mode Enable.
///
/// Setting this bit enables long mode (will become active when paging is enabled).
/// Located in the EFER (Extended Feature Enable Register) MSR.
const EFER_LME: u64 = 0x100;

/// EFER.LMA - Long Mode Active.
///
/// This bit is set automatically by the CPU when LME=1 and paging is enabled.
/// We set it explicitly to match expected state.
const EFER_LMA: u64 = 0x400;

// ============================================================================
// GDT Configuration
// ============================================================================
//
// The 64-bit Linux boot protocol requires specific segment selectors:
//   - __BOOT_CS = 0x10 (code segment)
//   - __BOOT_DS = 0x18 (data segment)
//
// Reference: Documentation/arch/x86/boot.rst section "64-bit Boot Protocol"

/// GDT entry index for code segment (__BOOT_CS = 0x10).
const GDT_CODE: u16 = 2;

/// GDT entry index for data segment (__BOOT_DS = 0x18).
const GDT_DATA: u16 = 3;

/// GDT entry index for Task State Segment.
const GDT_TSS: u16 = 4;

/// Pre-computed GDT entries matching Linux 64-bit boot protocol.
///
/// Layout:
///   0x00: NULL descriptor (required)
///   0x08: Reserved (unused, for alignment)
///   0x10: CODE (__BOOT_CS) - 64-bit code segment
///   0x18: DATA (__BOOT_DS) - data segment
///   0x20: TSS - Task State Segment
const GDT_TABLE: [u64; 5] = [
    gdt_entry(0, 0, 0),            // 0x00: NULL descriptor (required)
    gdt_entry(0, 0, 0),            // 0x08: Reserved
    gdt_entry(0xa09b, 0, 0xfffff), // 0x10: CODE (__BOOT_CS) - 64-bit, execute/read
    gdt_entry(0xc093, 0, 0xfffff), // 0x18: DATA (__BOOT_DS) - read/write
    gdt_entry(0x808b, 0, 0xfffff), // 0x20: TSS - Task State Segment
];

/// Pre-computed PDE entries for identity mapping first 1GB.
///
/// Each entry maps a 2MB page with flags: Present + Read/Write + Page Size (2MB).
/// Entry i maps virtual [i*2MB, (i+1)*2MB) to physical [i*2MB, (i+1)*2MB).
const fn compute_pde_entries() -> [u64; 512] {
    let mut entries = [0u64; 512];
    let mut i = 0;
    while i < 512 {
        // Physical address = i * 2MB, flags = 0x83 (Present + R/W + PS)
        entries[i] = ((i as u64) << 21) | 0x83;
        i += 1;
    }
    entries
}

/// Pre-computed PDE table for identity mapping.
const PDE_ENTRIES: [u64; 512] = compute_pde_entries();

/// Set up identity-mapped page tables for the first 1GB of memory.
///
/// Creates a simple page table hierarchy using 2MB pages:
///
/// ```text
/// PML4[0] → PDPTE[0] → PDE[0..511] → 2MB pages at 0MB, 2MB, 4MB, ... 1022MB
/// ```
///
/// This maps virtual addresses 0x0 - 0x3FFFFFFF to the same physical addresses
/// (identity mapping), which is what the kernel expects during early boot.
pub fn setup_page_tables(memory: &GuestMemory) -> Result<(), BootError> {
    // PML4 entry 0: Points to PDPTE table
    // Flags 0x03 = Present + Read/Write
    memory.write_u64(PML4_START, PDPTE_START | 0x03)?;

    // PDPTE entry 0: Points to PDE table
    // Flags 0x03 = Present + Read/Write
    memory.write_u64(PDPTE_START, PDE_START | 0x03)?;

    // Write all 512 PDE entries at once
    // Each entry is 8 bytes, so we write 4096 bytes total
    let pde_bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(PDE_ENTRIES.as_ptr() as *const u8, 512 * 8) };
    memory.write(PDE_START, pde_bytes)?;

    Ok(())
}

/// Construct a GDT (Global Descriptor Table) entry.
///
/// GDT entries are 8 bytes with a complex layout for historical reasons.
/// This is a const fn so entries can be computed at compile time.
const fn gdt_entry(flags: u16, base: u32, limit: u32) -> u64 {
    ((base as u64 & 0xff00_0000) << 32)
        | ((base as u64 & 0x00ff_ffff) << 16)
        | (limit as u64 & 0x0000_ffff)
        | (((limit as u64 & 0x000f_0000) >> 16) << 48)
        | ((flags as u64) << 40)
}

/// Create a KVM segment descriptor from a GDT entry.
fn kvm_segment_from_gdt(entry: u64, table_index: u8) -> kvm_segment {
    kvm_segment {
        base: ((entry >> 16) & 0xff_ffff) | (((entry >> 56) & 0xff) << 24),
        limit: ((entry & 0xffff) | (((entry >> 48) & 0xf) << 16)) as u32,
        selector: u16::from(table_index) * 8,
        type_: ((entry >> 40) & 0xf) as u8,
        present: ((entry >> 47) & 0x1) as u8,
        dpl: ((entry >> 45) & 0x3) as u8,
        db: ((entry >> 54) & 0x1) as u8,
        s: ((entry >> 44) & 0x1) as u8,
        l: ((entry >> 53) & 0x1) as u8,
        g: ((entry >> 55) & 0x1) as u8,
        ..Default::default()
    }
}

/// Set up the GDT and IDT in guest memory.
fn setup_gdt_idt(memory: &GuestMemory) -> Result<(), BootError> {
    // Write GDT entries to guest memory (5 entries × 8 bytes = 40 bytes)
    let gdt_bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(GDT_TABLE.as_ptr() as *const u8, GDT_TABLE.len() * 8) };
    memory.write(layout::GDT_START, gdt_bytes)?;

    // Write a minimal IDT (just zeros)
    // The kernel will set up its own IDT during initialization
    memory.write_u64(layout::IDT_START, 0)?;

    Ok(())
}

/// Set up FPU (Floating Point Unit) registers.
fn setup_fpu(vcpu: &VcpuFd) -> Result<(), BootError> {
    let fpu = kvm_fpu {
        fcw: 0x37f,    // x87 FPU control word: all exceptions masked, double precision
        mxcsr: 0x1f80, // SSE control: all exceptions masked, round to nearest
        ..Default::default()
    };
    vcpu.set_fpu(&fpu)?;
    Ok(())
}

/// Set up CPU registers for 64-bit Linux boot.
///
/// This function configures all CPU state required by the Linux boot protocol:
///
/// 1. **GDT/IDT**: Set up descriptor tables in memory
/// 2. **FPU**: Initialize floating point unit
/// 3. **Segment registers**: Load from GDT (CS, DS, ES, FS, GS, SS, TR)
/// 4. **Control registers**: Enable protected mode and paging
/// 5. **EFER MSR**: Enable long mode
/// 6. **General registers**: Set entry point, stack, boot_params pointer
pub fn setup_cpu_regs(vcpu: &VcpuFd, memory: &GuestMemory) -> Result<(), BootError> {
    // Set up GDT and IDT in guest memory
    setup_gdt_idt(memory)?;

    // Initialize FPU state
    setup_fpu(vcpu)?;

    // Get segment descriptors from GDT entries
    let code_seg = kvm_segment_from_gdt(GDT_TABLE[GDT_CODE as usize], GDT_CODE as u8);
    let data_seg = kvm_segment_from_gdt(GDT_TABLE[GDT_DATA as usize], GDT_DATA as u8);
    let tss_seg = kvm_segment_from_gdt(GDT_TABLE[GDT_TSS as usize], GDT_TSS as u8);

    // Get current special registers and modify them
    let mut sregs = vcpu.get_sregs()?;

    // Configure GDT register (GDTR)
    sregs.gdt.base = layout::GDT_START;
    sregs.gdt.limit = (std::mem::size_of_val(&GDT_TABLE) - 1) as u16;

    // Configure IDT register (IDTR)
    // Minimal IDT with limit 0 - kernel will set up its own
    sregs.idt.base = layout::IDT_START;
    sregs.idt.limit = 0;

    // Load segment registers from GDT
    sregs.cs = code_seg;
    sregs.ds = data_seg;
    sregs.es = data_seg;
    sregs.fs = data_seg;
    sregs.gs = data_seg;
    sregs.ss = data_seg;
    sregs.tr = tss_seg;

    // Enable protected mode
    sregs.cr0 |= X86_CR0_PE;

    // Enable long mode in EFER MSR
    sregs.efer |= EFER_LME | EFER_LMA;

    // Set up paging
    sregs.cr3 = PML4_START;
    sregs.cr4 |= X86_CR4_PAE;
    sregs.cr0 |= X86_CR0_PG;

    vcpu.set_sregs(&sregs)?;

    eprintln!("[Boot] CPU special registers:");
    eprintln!("  - CR0: {:#x}", sregs.cr0);
    eprintln!("  - CR3: {:#x}", sregs.cr3);
    eprintln!("  - CR4: {:#x}", sregs.cr4);
    eprintln!("  - EFER: {:#x}", sregs.efer);

    // Set up general-purpose registers for Linux 64-bit boot
    let regs = kvm_regs {
        rflags: 0x2,                      // Only reserved bit 1 set, interrupts disabled
        rip: layout::HIMEM_START + 0x200, // 64-bit entry point
        rsp: layout::BOOT_STACK_POINTER,
        rbp: layout::BOOT_STACK_POINTER,
        rsi: layout::BOOT_PARAMS_START, // boot_params pointer
        ..Default::default()
    };

    vcpu.set_regs(&regs)?;

    eprintln!("[Boot] CPU general registers:");
    eprintln!("  - RIP: {:#x}", regs.rip);
    eprintln!("  - RSP: {:#x}", regs.rsp);
    eprintln!("  - RSI: {:#x} (boot_params)", regs.rsi);

    Ok(())
}
