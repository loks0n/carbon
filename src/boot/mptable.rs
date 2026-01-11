//! MP (Multi-Processor) table generation for x86_64 microVM.
//!
//! MP tables provide interrupt routing information to the Linux kernel.
//! This is the legacy mechanism (Intel MP Spec 1.4) that works alongside
//! ACPI HW_REDUCED mode to enable interrupt routing without requiring
//! full ACPI power management emulation.
//!
//! Both Firecracker and Cloud Hypervisor use this approach:
//! - ACPI with HW_REDUCED flag (no PM hardware emulation needed)
//! - MP tables for interrupt routing information
//!
//! # Memory Layout
//!
//! MP tables are placed in the EBDA (Extended BIOS Data Area):
//! ```text
//! 0x0009_fc00  MP Floating Pointer Structure (16 bytes)
//! 0x0009_fc10  MP Configuration Table Header
//! 0x0009_fc10+ MP Configuration Table Entries
//! ```

use super::memory::GuestMemory;
use super::BootError;

/// MP table location in guest memory (EBDA region).
pub const MPTABLE_START: u64 = 0x0009_fc00;

/// Local APIC base address.
const LOCAL_APIC_ADDR: u32 = 0xfee0_0000;

/// I/O APIC base address.
const IO_APIC_ADDR: u32 = 0xfec0_0000;

/// APIC version (matches modern Intel APICs).
const APIC_VERSION: u8 = 0x14;

/// Number of legacy ISA IRQs to map (0-15).
const NUM_LEGACY_IRQS: u8 = 16;

// MP Specification constants
const MP_SIGNATURE: [u8; 4] = *b"_MP_";
const MPC_SIGNATURE: [u8; 4] = *b"PCMP";
const MP_SPEC_REVISION: u8 = 4; // MP Spec 1.4

// Entry type constants
const MP_PROCESSOR: u8 = 0;
const MP_BUS: u8 = 1;
const MP_IOAPIC: u8 = 2;
const MP_INTSRC: u8 = 3;
const MP_LINTSRC: u8 = 4;

// CPU flags
const CPU_ENABLED: u8 = 0x01;
const CPU_BOOT: u8 = 0x02;

// CPU features
const CPU_STEPPING: u32 = 0x600;
const CPU_FEATURE_APIC: u32 = 0x200;
const CPU_FEATURE_FPU: u32 = 0x001;

// Interrupt types
const INT_TYPE_INT: u8 = 0; // Vectored interrupt
const INT_TYPE_EXTINT: u8 = 3; // ExtINT (8259 compatible)
const INT_TYPE_NMI: u8 = 1; // NMI

// Polarity/trigger defaults
const MP_IRQPOL_DEFAULT: u16 = 0;

/// MP Floating Pointer Structure (16 bytes).
/// This is the entry point that the kernel searches for.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MpFloatingPointer {
    signature: [u8; 4], // "_MP_"
    physptr: u32,       // Physical address of MP config table
    length: u8,         // Length in 16-byte units (1)
    spec_rev: u8,       // MP spec revision (4 = 1.4)
    checksum: u8,       // Checksum (all bytes sum to 0)
    feature1: u8,       // MP feature byte 1
    feature2: u8,       // MP feature byte 2
    feature3: u8,       // Reserved
    feature4: u8,       // Reserved
    feature5: u8,       // Reserved
}

/// MP Configuration Table Header.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MpConfigTable {
    signature: [u8; 4],     // "PCMP"
    length: u16,            // Length of table including header
    spec_rev: u8,           // MP spec revision
    checksum: u8,           // Checksum
    oem_id: [u8; 8],        // OEM ID string
    product_id: [u8; 12],   // Product ID string
    oem_table_ptr: u32,     // OEM table pointer (0 = none)
    oem_table_size: u16,    // OEM table size
    entry_count: u16,       // Number of entries
    lapic_addr: u32,        // Local APIC address
    ext_table_length: u16,  // Extended table length
    ext_table_checksum: u8, // Extended table checksum
    reserved: u8,           // Reserved
}

/// MP Processor Entry (20 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MpProcessorEntry {
    entry_type: u8,     // 0 = processor
    apic_id: u8,        // Local APIC ID
    apic_version: u8,   // APIC version
    cpu_flags: u8,      // CPU flags (enabled, BSP)
    cpu_signature: u32, // CPU stepping/model/family
    feature_flags: u32, // CPU feature flags
    reserved: [u32; 2], // Reserved
}

/// MP Bus Entry (8 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MpBusEntry {
    entry_type: u8,    // 1 = bus
    bus_id: u8,        // Bus ID
    bus_type: [u8; 6], // Bus type string ("ISA   ")
}

/// MP I/O APIC Entry (8 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MpIoApicEntry {
    entry_type: u8,   // 2 = I/O APIC
    apic_id: u8,      // I/O APIC ID
    apic_version: u8, // I/O APIC version
    flags: u8,        // Flags (bit 0 = enabled)
    apic_addr: u32,   // I/O APIC base address
}

/// MP Interrupt Source Entry (8 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MpIntSrcEntry {
    entry_type: u8,   // 3 = interrupt source
    int_type: u8,     // Interrupt type
    int_flag: u16,    // Polarity/trigger mode
    src_bus_id: u8,   // Source bus ID
    src_bus_irq: u8,  // Source bus IRQ
    dst_apic_id: u8,  // Destination I/O APIC ID
    dst_apic_irq: u8, // Destination I/O APIC INTIN#
}

/// MP Local Interrupt Source Entry (8 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MpLocalIntSrcEntry {
    entry_type: u8,    // 4 = local interrupt source
    int_type: u8,      // Interrupt type
    int_flag: u16,     // Polarity/trigger mode
    src_bus_id: u8,    // Source bus ID
    src_bus_irq: u8,   // Source bus IRQ
    dst_apic_id: u8,   // Destination Local APIC ID (0xFF = all)
    dst_apic_lint: u8, // Destination LINT# (0 or 1)
}

/// Compute checksum for MP structures.
/// The sum of all bytes must equal 0.
fn compute_checksum(data: &[u8]) -> u8 {
    let sum: u8 = data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    (!sum).wrapping_add(1)
}

/// Set up MP tables in guest memory.
///
/// Creates the MP Floating Pointer and MP Configuration Table
/// that describe the system's processor and interrupt routing configuration.
pub fn setup_mptable(memory: &GuestMemory, num_cpus: u8) -> Result<u64, BootError> {
    let ioapic_id = num_cpus; // I/O APIC ID comes after CPU APIC IDs

    // Calculate sizes
    let fp_size = core::mem::size_of::<MpFloatingPointer>();
    let header_size = core::mem::size_of::<MpConfigTable>();
    let proc_size = core::mem::size_of::<MpProcessorEntry>();
    let bus_size = core::mem::size_of::<MpBusEntry>();
    let ioapic_size = core::mem::size_of::<MpIoApicEntry>();
    let intsrc_size = core::mem::size_of::<MpIntSrcEntry>();
    let lintsrc_size = core::mem::size_of::<MpLocalIntSrcEntry>();

    // Calculate total table size:
    // - 1 header
    // - num_cpus processor entries
    // - 1 bus entry (ISA)
    // - 1 I/O APIC entry
    // - NUM_LEGACY_IRQS interrupt source entries
    // - 2 local interrupt source entries (ExtINT, NMI)
    let table_size = header_size
        + (num_cpus as usize * proc_size)
        + bus_size
        + ioapic_size
        + (NUM_LEGACY_IRQS as usize * intsrc_size)
        + (2 * lintsrc_size);

    let mut table_buffer = vec![0u8; table_size];
    let mut offset = header_size; // Start after header (we'll fill it last)
    let mut entry_count: u16 = 0;

    // Add processor entries
    for cpu_id in 0..num_cpus {
        let entry = MpProcessorEntry {
            entry_type: MP_PROCESSOR,
            apic_id: cpu_id,
            apic_version: APIC_VERSION,
            cpu_flags: CPU_ENABLED | if cpu_id == 0 { CPU_BOOT } else { 0 },
            cpu_signature: CPU_STEPPING,
            feature_flags: CPU_FEATURE_APIC | CPU_FEATURE_FPU,
            reserved: [0; 2],
        };
        let entry_bytes =
            unsafe { core::slice::from_raw_parts(&entry as *const _ as *const u8, proc_size) };
        table_buffer[offset..offset + proc_size].copy_from_slice(entry_bytes);
        offset += proc_size;
        entry_count += 1;
    }

    // Add ISA bus entry
    let bus_entry = MpBusEntry {
        entry_type: MP_BUS,
        bus_id: 0,
        bus_type: *b"ISA   ",
    };
    let bus_bytes =
        unsafe { core::slice::from_raw_parts(&bus_entry as *const _ as *const u8, bus_size) };
    table_buffer[offset..offset + bus_size].copy_from_slice(bus_bytes);
    offset += bus_size;
    entry_count += 1;

    // Add I/O APIC entry
    let ioapic_entry = MpIoApicEntry {
        entry_type: MP_IOAPIC,
        apic_id: ioapic_id,
        apic_version: APIC_VERSION,
        flags: 1, // Enabled
        apic_addr: IO_APIC_ADDR,
    };
    let ioapic_bytes =
        unsafe { core::slice::from_raw_parts(&ioapic_entry as *const _ as *const u8, ioapic_size) };
    table_buffer[offset..offset + ioapic_size].copy_from_slice(ioapic_bytes);
    offset += ioapic_size;
    entry_count += 1;

    // Add interrupt source entries for ISA IRQs 0-15
    for irq in 0..NUM_LEGACY_IRQS {
        let intsrc_entry = MpIntSrcEntry {
            entry_type: MP_INTSRC,
            int_type: INT_TYPE_INT,
            int_flag: MP_IRQPOL_DEFAULT,
            src_bus_id: 0, // ISA bus
            src_bus_irq: irq,
            dst_apic_id: ioapic_id,
            dst_apic_irq: irq, // 1:1 mapping
        };
        let intsrc_bytes = unsafe {
            core::slice::from_raw_parts(&intsrc_entry as *const _ as *const u8, intsrc_size)
        };
        table_buffer[offset..offset + intsrc_size].copy_from_slice(intsrc_bytes);
        offset += intsrc_size;
        entry_count += 1;
    }

    // Add local interrupt source entry for ExtINT (LINT0)
    let extint_entry = MpLocalIntSrcEntry {
        entry_type: MP_LINTSRC,
        int_type: INT_TYPE_EXTINT,
        int_flag: MP_IRQPOL_DEFAULT,
        src_bus_id: 0,
        src_bus_irq: 0,
        dst_apic_id: 0,   // BSP
        dst_apic_lint: 0, // LINT0
    };
    let extint_bytes = unsafe {
        core::slice::from_raw_parts(&extint_entry as *const _ as *const u8, lintsrc_size)
    };
    table_buffer[offset..offset + lintsrc_size].copy_from_slice(extint_bytes);
    offset += lintsrc_size;
    entry_count += 1;

    // Add local interrupt source entry for NMI (LINT1)
    let nmi_entry = MpLocalIntSrcEntry {
        entry_type: MP_LINTSRC,
        int_type: INT_TYPE_NMI,
        int_flag: MP_IRQPOL_DEFAULT,
        src_bus_id: 0,
        src_bus_irq: 0,
        dst_apic_id: 0xFF, // All processors
        dst_apic_lint: 1,  // LINT1
    };
    let nmi_bytes =
        unsafe { core::slice::from_raw_parts(&nmi_entry as *const _ as *const u8, lintsrc_size) };
    table_buffer[offset..offset + lintsrc_size].copy_from_slice(nmi_bytes);
    entry_count += 1;

    // Now fill in the header
    let header = MpConfigTable {
        signature: MPC_SIGNATURE,
        length: table_size as u16,
        spec_rev: MP_SPEC_REVISION,
        checksum: 0, // Computed below
        oem_id: *b"CARBON  ",
        product_id: *b"MICROVM     ",
        oem_table_ptr: 0,
        oem_table_size: 0,
        entry_count,
        lapic_addr: LOCAL_APIC_ADDR,
        ext_table_length: 0,
        ext_table_checksum: 0,
        reserved: 0,
    };
    let header_bytes =
        unsafe { core::slice::from_raw_parts(&header as *const _ as *const u8, header_size) };
    table_buffer[..header_size].copy_from_slice(header_bytes);

    // Compute table checksum
    table_buffer[7] = compute_checksum(&table_buffer);

    // Write MP Configuration Table to guest memory
    let table_addr = MPTABLE_START + fp_size as u64;
    memory.write(table_addr, &table_buffer)?;

    // Create and write MP Floating Pointer
    let mut fp = MpFloatingPointer {
        signature: MP_SIGNATURE,
        physptr: table_addr as u32,
        length: 1, // 16 bytes
        spec_rev: MP_SPEC_REVISION,
        checksum: 0,
        feature1: 0, // Using MP config table (not default config)
        feature2: 0,
        feature3: 0,
        feature4: 0,
        feature5: 0,
    };
    let fp_bytes = unsafe { core::slice::from_raw_parts(&fp as *const _ as *const u8, fp_size) };
    let mut fp_buffer = vec![0u8; fp_size];
    fp_buffer.copy_from_slice(fp_bytes);
    fp_buffer[10] = compute_checksum(&fp_buffer);

    // Update fp struct with checksum for the write
    fp.checksum = fp_buffer[10];
    let fp_bytes = unsafe { core::slice::from_raw_parts(&fp as *const _ as *const u8, fp_size) };
    memory.write(MPTABLE_START, fp_bytes)?;

    eprintln!(
        "[Boot] MPTable: addr={:#x} entries={} ({}CPUs, {}IRQs)",
        MPTABLE_START, entry_count, num_cpus, NUM_LEGACY_IRQS
    );

    Ok(MPTABLE_START)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_struct_sizes() {
        assert_eq!(core::mem::size_of::<MpFloatingPointer>(), 16);
        assert_eq!(core::mem::size_of::<MpProcessorEntry>(), 20);
        assert_eq!(core::mem::size_of::<MpBusEntry>(), 8);
        assert_eq!(core::mem::size_of::<MpIoApicEntry>(), 8);
        assert_eq!(core::mem::size_of::<MpIntSrcEntry>(), 8);
        assert_eq!(core::mem::size_of::<MpLocalIntSrcEntry>(), 8);
    }

    #[test]
    fn test_checksum() {
        let data = [0x01, 0x02, 0x03, 0x04];
        let checksum = compute_checksum(&data);
        let sum: u8 = data
            .iter()
            .chain(std::iter::once(&checksum))
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        assert_eq!(sum, 0);
    }
}
