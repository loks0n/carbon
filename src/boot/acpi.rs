//! ACPI table generation for x86_64 microVM.
//!
//! This module generates minimal ACPI tables to describe the virtual hardware
//! to the guest kernel. Using ACPI instead of MP Tables allows the kernel to
//! use optimized boot paths, resulting in faster boot times.
//!
//! # Tables Generated
//!
//! - **RSDP** (Root System Description Pointer): Entry point for ACPI tables
//! - **XSDT** (Extended System Description Table): Lists all other tables
//! - **FADT** (Fixed ACPI Description Table): Hardware feature description
//! - **DSDT** (Differentiated System Description Table): AML code for devices
//! - **MADT** (Multiple APIC Description Table): Describes APIC configuration
//!
//! # HW_REDUCED ACPI Mode
//!
//! We use HW_REDUCED_ACPI mode (FADT flag bit 20) which tells the kernel we
//! don't emulate legacy PM hardware. Virtio devices are defined in the DSDT
//! with ACPI interrupt resources, allowing proper GSI routing through the
//! IOAPIC without requiring legacy IRQ preallocaiton.
//!
//! # Memory Layout
//!
//! ACPI tables are placed in the BIOS read-only area (0xE0000-0xFFFFF):
//! ```text
//! 0x000e_0000  RSDP (36 bytes)
//! 0x000e_1000  XSDT (variable)
//! 0x000e_2000  FADT (276 bytes)
//! 0x000e_3000  DSDT (variable, includes virtio device definitions)
//! 0x000e_4000  MADT (variable)
//! ```

use super::memory::GuestMemory;
use super::BootError;

/// RSDP location in guest memory (BIOS ROM area).
pub const RSDP_ADDR: u64 = 0x000e_0000;

/// XSDT location in guest memory.
const XSDT_ADDR: u64 = 0x000e_1000;

/// FADT location in guest memory.
const FADT_ADDR: u64 = 0x000e_2000;

/// DSDT location in guest memory.
const DSDT_ADDR: u64 = 0x000e_3000;

/// MADT location in guest memory.
const MADT_ADDR: u64 = 0x000e_4000;

/// Local APIC base address.
const LOCAL_APIC_ADDR: u32 = 0xfee0_0000;

/// I/O APIC base address.
const IO_APIC_ADDR: u32 = 0xfec0_0000;

/// I/O APIC ID.
const IO_APIC_ID: u8 = 1;

/// OEM ID for ACPI tables.
const OEM_ID: &[u8; 6] = b"CARBON";

/// OEM Table ID.
const OEM_TABLE_ID: &[u8; 8] = b"MICROVM ";

/// HW_REDUCED_ACPI flag in FADT (bit 20).
/// Indicates no legacy PM hardware emulation.
const FADT_HW_REDUCED_ACPI: u32 = 1 << 20;

/// PWR_BUTTON flag in FADT (bit 4).
/// If set, indicates system does NOT have a power button.
const FADT_PWR_BUTTON: u32 = 1 << 4;

/// SLP_BUTTON flag in FADT (bit 5).
/// If set, indicates system does NOT have a sleep button.
const FADT_SLP_BUTTON: u32 = 1 << 5;

/// IAPC_BOOT_ARCH: VGA not present (bit 2).
const IAPC_VGA_NOT_PRESENT: u16 = 1 << 2;

/// Configuration for a virtio-mmio device to be defined in DSDT.
#[derive(Clone, Debug)]
pub struct VirtioDeviceConfig {
    /// Device ID (0, 1, 2, ...).
    pub id: u8,
    /// MMIO base address.
    pub mmio_base: u64,
    /// MMIO region size.
    pub mmio_size: u32,
    /// GSI (Global System Interrupt) number.
    pub gsi: u32,
}

/// ACPI standard table header (used by XSDT, MADT, etc.).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct AcpiHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: [u8; 4],
    creator_revision: u32,
}

impl AcpiHeader {
    fn new(signature: &[u8; 4], length: u32, revision: u8) -> Self {
        Self {
            signature: *signature,
            length,
            revision,
            checksum: 0, // Computed later
            oem_id: *OEM_ID,
            oem_table_id: *OEM_TABLE_ID,
            oem_revision: 1,
            creator_id: *b"CBNV", // Carbon VMM
            creator_revision: 1,
        }
    }
}

/// RSDP (Root System Description Pointer) - ACPI 2.0+ version.
#[repr(C, packed)]
struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32, // Not used in ACPI 2.0+, but must be valid
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

impl Rsdp {
    fn new(xsdt_addr: u64) -> Self {
        Self {
            signature: *b"RSD PTR ",
            checksum: 0,
            oem_id: *OEM_ID,
            revision: 2,     // ACPI 2.0+
            rsdt_address: 0, // We only use XSDT
            length: core::mem::size_of::<Rsdp>() as u32,
            xsdt_address: xsdt_addr,
            extended_checksum: 0,
            reserved: [0; 3],
        }
    }
}

/// MADT Processor Local APIC entry.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MadtLocalApic {
    entry_type: u8, // 0 = Processor Local APIC
    length: u8,     // 8
    processor_id: u8,
    apic_id: u8,
    flags: u32, // Bit 0 = enabled
}

impl MadtLocalApic {
    fn new(processor_id: u8, apic_id: u8) -> Self {
        Self {
            entry_type: 0,
            length: 8,
            processor_id,
            apic_id,
            flags: 1, // Enabled
        }
    }
}

/// MADT I/O APIC entry.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MadtIoApic {
    entry_type: u8, // 1 = I/O APIC
    length: u8,     // 12
    io_apic_id: u8,
    reserved: u8,
    io_apic_address: u32,
    global_system_interrupt_base: u32,
}

impl MadtIoApic {
    fn new(io_apic_id: u8, address: u32, gsi_base: u32) -> Self {
        Self {
            entry_type: 1,
            length: 12,
            io_apic_id,
            reserved: 0,
            io_apic_address: address,
            global_system_interrupt_base: gsi_base,
        }
    }
}

/// FADT (Fixed ACPI Description Table) - ACPI 6.0 version (276 bytes).
/// Most fields are zero for a minimal VM with no power management hardware.
#[repr(C, packed)]
struct Fadt {
    header: AcpiHeader,
    firmware_ctrl: u32,          // 32-bit FACS address (0 = use X_FIRMWARE_CTRL)
    dsdt: u32,                   // 32-bit DSDT address (0 = use X_DSDT)
    reserved1: u8,               // Was INT_MODEL in ACPI 1.0
    preferred_pm_profile: u8,    // 0 = Unspecified
    sci_int: u16,                // SCI interrupt (0 = none)
    smi_cmd: u32,                // SMI command port (0 = none)
    acpi_enable: u8,             // Value to write to SMI_CMD to enable ACPI
    acpi_disable: u8,            // Value to write to SMI_CMD to disable ACPI
    s4bios_req: u8,              // Value to write for S4BIOS
    pstate_cnt: u8,              // Value to write for P-state control
    pm1a_evt_blk: u32,           // PM1a event block address
    pm1b_evt_blk: u32,           // PM1b event block address
    pm1a_cnt_blk: u32,           // PM1a control block address
    pm1b_cnt_blk: u32,           // PM1b control block address
    pm2_cnt_blk: u32,            // PM2 control block address
    pm_tmr_blk: u32,             // PM timer block address
    gpe0_blk: u32,               // GPE0 block address
    gpe1_blk: u32,               // GPE1 block address
    pm1_evt_len: u8,             // PM1 event block length
    pm1_cnt_len: u8,             // PM1 control block length
    pm2_cnt_len: u8,             // PM2 control block length
    pm_tmr_len: u8,              // PM timer block length
    gpe0_blk_len: u8,            // GPE0 block length
    gpe1_blk_len: u8,            // GPE1 block length
    gpe1_base: u8,               // GPE1 base offset
    cst_cnt: u8,                 // C-state control
    p_lvl2_lat: u16,             // P_LVL2 latency
    p_lvl3_lat: u16,             // P_LVL3 latency
    flush_size: u16,             // Cache flush size
    flush_stride: u16,           // Cache flush stride
    duty_offset: u8,             // Duty cycle offset
    duty_width: u8,              // Duty cycle width
    day_alrm: u8,                // RTC day alarm index
    mon_alrm: u8,                // RTC month alarm index
    century: u8,                 // RTC century index
    iapc_boot_arch: u16,         // IA-PC boot flags
    reserved2: u8,               // Reserved
    flags: u32,                  // Fixed feature flags
    reset_reg: [u8; 12],         // Generic Address Structure for reset
    reset_value: u8,             // Value to write for reset
    arm_boot_arch: u16,          // ARM boot flags
    fadt_minor_version: u8,      // FADT minor version
    x_firmware_ctrl: u64,        // 64-bit FACS address
    x_dsdt: u64,                 // 64-bit DSDT address
    x_pm1a_evt_blk: [u8; 12],    // Extended PM1a event block
    x_pm1b_evt_blk: [u8; 12],    // Extended PM1b event block
    x_pm1a_cnt_blk: [u8; 12],    // Extended PM1a control block
    x_pm1b_cnt_blk: [u8; 12],    // Extended PM1b control block
    x_pm2_cnt_blk: [u8; 12],     // Extended PM2 control block
    x_pm_tmr_blk: [u8; 12],      // Extended PM timer block
    x_gpe0_blk: [u8; 12],        // Extended GPE0 block
    x_gpe1_blk: [u8; 12],        // Extended GPE1 block
    sleep_control_reg: [u8; 12], // Sleep control register
    sleep_status_reg: [u8; 12],  // Sleep status register
    hypervisor_vendor_id: u64,   // Hypervisor vendor ID
}

/// Compute ACPI checksum for a byte slice.
/// The sum of all bytes (including checksum) must equal 0.
fn compute_checksum(data: &[u8]) -> u8 {
    let sum: u8 = data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    (!sum).wrapping_add(1)
}

/// Set up ACPI tables in guest memory.
///
/// # Arguments
/// * `memory` - Guest memory to write tables to
/// * `num_cpus` - Number of vCPUs (currently must be 1)
/// * `virtio_devices` - List of virtio-mmio devices to define in DSDT
///
/// # Returns
/// The address of the RSDP, which should be reported to the guest via
/// boot parameters or EBDA.
///
/// # ACPI Device Discovery
///
/// Virtio-mmio devices are defined in the DSDT with proper ACPI resource
/// descriptors. The kernel discovers them via ACPI enumeration (not kernel
/// command line), which works correctly with HW_REDUCED_ACPI mode.
pub fn setup_acpi(
    memory: &GuestMemory,
    num_cpus: u8,
    virtio_devices: &[VirtioDeviceConfig],
) -> Result<u64, BootError> {
    // Build DSDT (must be built before FADT which references it)
    let dsdt_size = build_dsdt(memory, virtio_devices)?;

    // Build FADT (Fixed ACPI Description Table)
    let fadt_size = build_fadt(memory)?;

    // Build MADT (Multiple APIC Description Table)
    let madt_size = build_madt(memory, num_cpus)?;

    // Build XSDT - FADT must be first per ACPI spec
    build_xsdt(memory, &[FADT_ADDR, MADT_ADDR])?;

    // Build RSDP (Root System Description Pointer)
    build_rsdp(memory)?;

    eprintln!(
        "[Boot] ACPI: RSDP={:#x} XSDT={:#x} FADT={:#x}({}) DSDT={:#x}({}) MADT={:#x}({}) virtio={}",
        RSDP_ADDR,
        XSDT_ADDR,
        FADT_ADDR,
        fadt_size,
        DSDT_ADDR,
        dsdt_size,
        MADT_ADDR,
        madt_size,
        virtio_devices.len()
    );

    Ok(RSDP_ADDR)
}

/// Build RSDP and write to guest memory.
fn build_rsdp(memory: &GuestMemory) -> Result<(), BootError> {
    let mut rsdp = Rsdp::new(XSDT_ADDR);

    // Compute ACPI 1.0 checksum (first 20 bytes)
    let rsdp_bytes = unsafe { core::slice::from_raw_parts(&rsdp as *const _ as *const u8, 20) };
    rsdp.checksum = compute_checksum(rsdp_bytes);

    // Compute extended checksum (all 36 bytes)
    let rsdp_bytes = unsafe {
        core::slice::from_raw_parts(&rsdp as *const _ as *const u8, core::mem::size_of::<Rsdp>())
    };
    rsdp.extended_checksum = compute_checksum(rsdp_bytes);

    // Write to guest memory
    let rsdp_bytes = unsafe {
        core::slice::from_raw_parts(&rsdp as *const _ as *const u8, core::mem::size_of::<Rsdp>())
    };
    memory.write(RSDP_ADDR, rsdp_bytes)?;

    Ok(())
}

/// Build XSDT and write to guest memory.
fn build_xsdt(memory: &GuestMemory, table_addrs: &[u64]) -> Result<(), BootError> {
    let header_size = core::mem::size_of::<AcpiHeader>();
    let table_size = header_size + table_addrs.len() * 8;

    let mut buffer = vec![0u8; table_size];

    // Create header
    let header = AcpiHeader::new(b"XSDT", table_size as u32, 1);

    // Copy header to buffer
    let header_bytes =
        unsafe { core::slice::from_raw_parts(&header as *const _ as *const u8, header_size) };
    buffer[..header_size].copy_from_slice(header_bytes);

    // Add table addresses
    for (i, &addr) in table_addrs.iter().enumerate() {
        let offset = header_size + i * 8;
        buffer[offset..offset + 8].copy_from_slice(&addr.to_le_bytes());
    }

    // Compute checksum
    buffer[9] = compute_checksum(&buffer);

    // Write to guest memory
    memory.write(XSDT_ADDR, &buffer)?;

    Ok(())
}

/// Build FADT (Fixed ACPI Description Table) and write to guest memory.
fn build_fadt(memory: &GuestMemory) -> Result<usize, BootError> {
    let fadt_size = core::mem::size_of::<Fadt>();
    let mut buffer = vec![0u8; fadt_size];

    // Create header - FADT signature is "FACP"
    let header = AcpiHeader::new(b"FACP", fadt_size as u32, 6); // ACPI 6.0

    // Copy header
    let header_size = core::mem::size_of::<AcpiHeader>();
    let header_bytes =
        unsafe { core::slice::from_raw_parts(&header as *const _ as *const u8, header_size) };
    buffer[..header_size].copy_from_slice(header_bytes);

    // FADT field offsets (from ACPI 6.0 spec):
    // - dsdt (32-bit): offset 40
    // - flags: offset 112
    // - fadt_minor_version: offset 131
    // - x_dsdt (64-bit): offset 140

    // Set DSDT pointer (32-bit, for compatibility)
    let dsdt_offset = 40;
    buffer[dsdt_offset..dsdt_offset + 4].copy_from_slice(&(DSDT_ADDR as u32).to_le_bytes());

    // Set X_DSDT pointer (64-bit)
    let x_dsdt_offset = 140;
    buffer[x_dsdt_offset..x_dsdt_offset + 8].copy_from_slice(&DSDT_ADDR.to_le_bytes());

    // HW_REDUCED_ACPI mode - we don't emulate legacy PM hardware.
    //
    // This flag tells the kernel not to expect legacy ACPI PM registers.
    // Virtio devices are defined in DSDT with ACPI interrupt resources,
    // so GSI routing works through IOAPIC without legacy IRQ preallocaiton.
    //
    // Additional flags (same as Firecracker):
    // - PWR_BUTTON: indicates no power button hardware
    // - SLP_BUTTON: indicates no sleep button hardware
    let flags: u32 = FADT_HW_REDUCED_ACPI | FADT_PWR_BUTTON | FADT_SLP_BUTTON;
    buffer[112..116].copy_from_slice(&flags.to_le_bytes());

    // IAPC_BOOT_ARCH flags (offset 109-110):
    // - VGA_NOT_PRESENT: indicates no VGA hardware
    let iapc_boot_arch_offset = 109;
    buffer[iapc_boot_arch_offset..iapc_boot_arch_offset + 2]
        .copy_from_slice(&IAPC_VGA_NOT_PRESENT.to_le_bytes());

    // With HW_REDUCED_ACPI, the PM registers are not used.
    // We leave X_PM GAS structures as all zeros (default) which indicates
    // "not present". The kernel will skip PM hardware initialization.

    // Set FADT minor version (ACPI 6.5 like Firecracker)
    let minor_version_offset = 131;
    buffer[minor_version_offset] = 5;

    // Compute checksum
    buffer[9] = compute_checksum(&buffer);

    // Write to guest memory
    memory.write(FADT_ADDR, &buffer)?;

    Ok(fadt_size)
}

/// Build DSDT (Differentiated System Description Table) with virtio device definitions.
///
/// The DSDT contains AML (ACPI Machine Language) code that describes the system's
/// hardware. We generate device definitions for virtio-mmio devices so the kernel
/// can discover them via ACPI enumeration.
///
/// # AML Structure
///
/// ```text
/// Scope(\_SB) {
///     Device(VRT0) {
///         Name(_HID, "LNRO0005")    // virtio-mmio ACPI ID
///         Name(_UID, 0)
///         Name(_CRS, ResourceTemplate() {
///             Memory32Fixed(ReadWrite, base, size)
///             Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive) { gsi }
///         })
///     }
///     // ... more devices
/// }
/// ```
fn build_dsdt(
    memory: &GuestMemory,
    virtio_devices: &[VirtioDeviceConfig],
) -> Result<usize, BootError> {
    let header_size = core::mem::size_of::<AcpiHeader>();

    // Build AML code for all devices
    let mut aml_code = Vec::new();

    // Generate device AML for each virtio device
    let mut device_aml = Vec::new();
    for dev in virtio_devices {
        let dev_aml = build_virtio_device_aml(dev);
        device_aml.extend_from_slice(&dev_aml);
    }

    // Build Scope(\_SB) { devices... }
    // ScopeOp = 0x10
    // PkgLength encoding varies based on total size
    // \_SB_ = root char (0x5C) + "_SB_"
    let scope_name: [u8; 5] = [0x5C, 0x5F, 0x53, 0x42, 0x5F]; // "\_SB_"

    aml_code.push(0x10); // ScopeOp
                         // PkgLength covers: NameString (5 bytes for \_SB_) + TermList (device contents)
    encode_pkg_length(&mut aml_code, scope_name.len() + device_aml.len());
    aml_code.extend_from_slice(&scope_name); // \_SB_
    aml_code.extend_from_slice(&device_aml);

    let dsdt_size = header_size + aml_code.len();
    let mut buffer = vec![0u8; dsdt_size];

    // Create header - DSDT signature is "DSDT"
    let header = AcpiHeader::new(b"DSDT", dsdt_size as u32, 2);

    // Copy header
    let header_bytes =
        unsafe { core::slice::from_raw_parts(&header as *const _ as *const u8, header_size) };
    buffer[..header_size].copy_from_slice(header_bytes);

    // Copy AML code
    buffer[header_size..].copy_from_slice(&aml_code);

    // Compute checksum
    buffer[9] = compute_checksum(&buffer);

    // Debug: dump AML bytes
    eprintln!(
        "[DSDT] AML bytes ({} total, {} AML):",
        dsdt_size,
        aml_code.len()
    );
    eprint!("[DSDT] ");
    for (i, byte) in aml_code.iter().enumerate() {
        eprint!("{:02x} ", byte);
        if (i + 1) % 16 == 0 {
            eprintln!();
            eprint!("[DSDT] ");
        }
    }
    eprintln!();

    // Write to guest memory
    memory.write(DSDT_ADDR, &buffer)?;

    Ok(dsdt_size)
}

/// Build AML bytecode for a single virtio-mmio device.
///
/// Generates:
/// ```text
/// Device(VRTn) {
///     Name(_HID, "LNRO0005")
///     Name(_UID, n)
///     Name(_CRS, ResourceTemplate() {
///         Memory32Fixed(ReadWrite, base, size)
///         Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive) { gsi }
///     })
/// }
/// ```
fn build_virtio_device_aml(dev: &VirtioDeviceConfig) -> Vec<u8> {
    let mut device_contents = Vec::new();

    // Device name: VRTn (where n is 0-9, A-F for id 0-15)
    let name_char = if dev.id < 10 {
        b'0' + dev.id
    } else {
        b'A' + (dev.id - 10)
    };
    let device_name: [u8; 4] = [b'V', b'R', b'T', name_char];

    // Name(_HID, "LNRO0005")
    // NameOp (0x08) + NamePath + StringPrefix (0x0D) + String + NullChar
    device_contents.push(0x08); // NameOp
    device_contents.extend_from_slice(b"_HID");
    device_contents.push(0x0D); // StringPrefix
    device_contents.extend_from_slice(b"LNRO0005");
    device_contents.push(0x00); // Null terminator

    // Name(_UID, id)
    // NameOp (0x08) + NamePath + Integer
    // Integer encoding: 0x00 = ZeroOp, 0x01 = OneOp, 0x0A + byte = BytePrefix
    device_contents.push(0x08); // NameOp
    device_contents.extend_from_slice(b"_UID");
    if dev.id == 0 {
        device_contents.push(0x00); // ZeroOp
    } else if dev.id == 1 {
        device_contents.push(0x01); // OneOp
    } else {
        device_contents.push(0x0A); // BytePrefix
        device_contents.push(dev.id);
    }

    // Name(_STA, 0x0F) - Device present, enabled, functioning, shown in UI
    // This explicitly marks the device as present. While optional per ACPI spec,
    // some implementations may require it.
    device_contents.push(0x08); // NameOp
    device_contents.extend_from_slice(b"_STA");
    device_contents.push(0x0A); // BytePrefix
    device_contents.push(0x0F); // Present + Enabled + Functioning + ShowInUI

    // Name(_CRS, ResourceTemplate() { ... })
    // NameOp (0x08) + NamePath + Buffer
    let resource_template = build_resource_template(dev.mmio_base as u32, dev.mmio_size, dev.gsi);
    device_contents.push(0x08); // NameOp
    device_contents.extend_from_slice(b"_CRS");
    device_contents.extend_from_slice(&resource_template);

    // Build Device structure: DeviceOp + PkgLength + NamePath + contents
    let mut device_aml = Vec::new();
    device_aml.push(0x5B); // ExtOpPrefix
    device_aml.push(0x82); // DeviceOp
    encode_pkg_length(&mut device_aml, 4 + device_contents.len()); // name + contents
    device_aml.extend_from_slice(&device_name);
    device_aml.extend_from_slice(&device_contents);

    device_aml
}

/// Build AML ResourceTemplate buffer for virtio device _CRS.
///
/// Contains:
/// - Memory32Fixed descriptor (MMIO region)
/// - Extended Interrupt descriptor (GSI)
/// - End tag
fn build_resource_template(base: u32, size: u32, gsi: u32) -> Vec<u8> {
    // Memory32Fixed descriptor (Small Resource, Type 0x86)
    // Tag: 0x86 (Memory32Fixed, length in next 2 bytes)
    // Length: 9 (1 + 4 + 4 for RW flag + base + length)
    let mut resources = vec![
        0x86, // Memory32Fixed tag
        0x09, // Length low byte
        0x00, // Length high byte
        0x01, // Read/Write flag (1 = ReadWrite)
    ];
    resources.extend_from_slice(&base.to_le_bytes()); // Base address
    resources.extend_from_slice(&size.to_le_bytes()); // Range length

    // Extended Interrupt descriptor (Large Resource, Type 0x89)
    // Format: Tag (1) + Length (2) + Flags (1) + Count (1) + Interrupts (4*count)
    resources.push(0x89); // Extended Interrupt tag
    resources.push(0x06); // Length low byte (1 + 1 + 4 = 6)
    resources.push(0x00); // Length high byte
                          // Flags: bit 0 = consumer (1), bit 1 = edge(0)/level(1), bit 2 = active high(0)/low(1)
                          //        bit 3 = shared(0)/exclusive(1)
                          // We want: consumer, level-triggered, active-high, exclusive = 0b00001011 = 0x0B
    resources.push(0x0B); // Flags: ResourceConsumer, Level, ActiveHigh, Exclusive
    resources.push(0x01); // Interrupt count
    resources.extend_from_slice(&gsi.to_le_bytes()); // GSI number

    // End tag (Small Resource, Type 0x79)
    resources.push(0x79); // End tag
    resources.push(0x00); // Checksum (0 = not used)

    // Wrap in Buffer: BufferOp (0x11) + PkgLength + BufferSize + data
    let mut buffer = Vec::new();
    buffer.push(0x11); // BufferOp

    // BufferSize is a TermArg (integer) - must use proper AML encoding:
    // - 0x00 = ZeroOp (value 0)
    // - 0x01 = OneOp (value 1)
    // - 0x0A + byte = BytePrefix (values 2-255)
    // - 0x0B + word = WordPrefix (larger values)
    let buffer_size_encoding = if resources.len() <= 1 {
        1 // ZeroOp or OneOp
    } else if resources.len() <= 255 {
        2 // BytePrefix + byte
    } else {
        3 // WordPrefix + word
    };
    encode_pkg_length(&mut buffer, buffer_size_encoding + resources.len());

    // BufferSize (integer representing buffer length)
    if resources.is_empty() {
        buffer.push(0x00); // ZeroOp
    } else if resources.len() == 1 {
        buffer.push(0x01); // OneOp
    } else if resources.len() <= 255 {
        buffer.push(0x0A); // BytePrefix
        buffer.push(resources.len() as u8);
    } else {
        buffer.push(0x0B); // WordPrefix
        buffer.extend_from_slice(&(resources.len() as u16).to_le_bytes());
    }

    buffer.extend_from_slice(&resources);

    buffer
}

/// Encode a PkgLength value into the buffer.
///
/// PkgLength encoding (ACPI spec 20.2.4):
/// - If total <= 63: single byte, bits 5:0 = length
/// - If total <= 4095: 2 bytes
///   - byte0[7:6] = 01 (indicates 2-byte encoding)
///   - byte0[3:0] = length[3:0] (low nibble)
///   - byte1 = length[11:4]
/// - 3-byte and 4-byte encodings follow the same pattern with more bytes
///
/// The `content_len` parameter is the size of content AFTER the PkgLength encoding.
/// The encoded value includes the PkgLength bytes themselves.
fn encode_pkg_length(buffer: &mut Vec<u8>, content_len: usize) {
    // Try 1-byte encoding: total = content + 1
    if content_len < 0x3F {
        buffer.push((content_len + 1) as u8);
        return;
    }

    // Try 2-byte encoding: total = content + 2
    if content_len + 2 <= 0x0FFF {
        let total = content_len + 2;
        // byte0: bits [7:6] = 01, bits [3:0] = total[3:0]
        buffer.push((1u8 << 6) | ((total & 0x0F) as u8));
        // byte1: total[11:4]
        buffer.push((total >> 4) as u8);
        return;
    }

    // Try 3-byte encoding: total = content + 3
    if content_len + 3 <= 0x0F_FFFF {
        let total = content_len + 3;
        // byte0: bits [7:6] = 10, bits [3:0] = total[3:0]
        buffer.push((2u8 << 6) | ((total & 0x0F) as u8));
        // byte1: total[11:4]
        buffer.push(((total >> 4) & 0xFF) as u8);
        // byte2: total[19:12]
        buffer.push(((total >> 12) & 0xFF) as u8);
        return;
    }

    // 4-byte encoding: total = content + 4
    let total = content_len + 4;
    // byte0: bits [7:6] = 11, bits [3:0] = total[3:0]
    buffer.push((3u8 << 6) | ((total & 0x0F) as u8));
    // byte1: total[11:4]
    buffer.push(((total >> 4) & 0xFF) as u8);
    // byte2: total[19:12]
    buffer.push(((total >> 12) & 0xFF) as u8);
    // byte3: total[27:20]
    buffer.push(((total >> 20) & 0xFF) as u8);
}

/// MADT Interrupt Source Override entry.
/// Maps legacy ISA IRQs to GSIs.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MadtInterruptOverride {
    entry_type: u8, // 2 = Interrupt Source Override
    length: u8,     // 10
    bus: u8,        // 0 = ISA
    source: u8,     // IRQ source
    global_system_interrupt: u32,
    flags: u16, // Polarity and trigger mode
}

impl MadtInterruptOverride {
    fn new(source: u8, gsi: u32, flags: u16) -> Self {
        Self {
            entry_type: 2,
            length: 10,
            bus: 0, // ISA
            source,
            global_system_interrupt: gsi,
            flags,
        }
    }
}

/// Build MADT and write to guest memory.
fn build_madt(memory: &GuestMemory, num_cpus: u8) -> Result<usize, BootError> {
    let header_size = core::mem::size_of::<AcpiHeader>();

    // MADT has a fixed part after the header: Local APIC Address (4) + Flags (4)
    let fixed_size = 8;

    // Calculate entry sizes
    let local_apic_size = core::mem::size_of::<MadtLocalApic>();
    let io_apic_size = core::mem::size_of::<MadtIoApic>();
    let override_size = core::mem::size_of::<MadtInterruptOverride>();

    // We'll add:
    // - One Local APIC entry per CPU
    // - One I/O APIC entry
    // - Interrupt source override for IRQ 0 (timer -> GSI 2)
    let entries_size = (num_cpus as usize * local_apic_size) + io_apic_size + override_size;

    let table_size = header_size + fixed_size + entries_size;
    let mut buffer = vec![0u8; table_size];

    // Create header
    let header = AcpiHeader::new(b"APIC", table_size as u32, 4); // MADT revision 4

    // Copy header
    let header_bytes =
        unsafe { core::slice::from_raw_parts(&header as *const _ as *const u8, header_size) };
    buffer[..header_size].copy_from_slice(header_bytes);

    // Fixed fields after header
    let mut offset = header_size;

    // Local APIC Address (4 bytes)
    buffer[offset..offset + 4].copy_from_slice(&LOCAL_APIC_ADDR.to_le_bytes());
    offset += 4;

    // Flags (4 bytes) - bit 0 = PCAT_COMPAT (dual 8259 present)
    // With HW_REDUCED_ACPI, we don't have legacy 8259 PICs, so set to 0.
    buffer[offset..offset + 4].copy_from_slice(&0u32.to_le_bytes());
    offset += 4;

    // Add Local APIC entries (one per CPU)
    for i in 0..num_cpus {
        let entry = MadtLocalApic::new(i, i);
        let entry_bytes = unsafe {
            core::slice::from_raw_parts(&entry as *const _ as *const u8, local_apic_size)
        };
        buffer[offset..offset + local_apic_size].copy_from_slice(entry_bytes);
        offset += local_apic_size;
    }

    // Add I/O APIC entry
    let io_apic = MadtIoApic::new(IO_APIC_ID, IO_APIC_ADDR, 0);
    let io_apic_bytes =
        unsafe { core::slice::from_raw_parts(&io_apic as *const _ as *const u8, io_apic_size) };
    buffer[offset..offset + io_apic_size].copy_from_slice(io_apic_bytes);
    offset += io_apic_size;

    // Interrupt Source Override for IRQ 0 (PIT timer -> GSI 2)
    let override0 = MadtInterruptOverride::new(0, 2, 0);
    let override_bytes =
        unsafe { core::slice::from_raw_parts(&override0 as *const _ as *const u8, override_size) };
    buffer[offset..offset + override_size].copy_from_slice(override_bytes);

    // Compute checksum
    buffer[9] = compute_checksum(&buffer);

    // Write to guest memory
    memory.write(MADT_ADDR, &buffer)?;

    Ok(table_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum() {
        // A valid ACPI table should have checksum that makes sum = 0
        let data = [0x01, 0x02, 0x03, 0x04];
        let checksum = compute_checksum(&data);
        let sum: u8 = data
            .iter()
            .chain(std::iter::once(&checksum))
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        assert_eq!(sum, 0);
    }

    #[test]
    fn test_rsdp_size() {
        assert_eq!(core::mem::size_of::<Rsdp>(), 36);
    }

    #[test]
    fn test_header_size() {
        assert_eq!(core::mem::size_of::<AcpiHeader>(), 36);
    }

    #[test]
    fn test_fadt_size() {
        // FADT for ACPI 6.0 should be 276 bytes
        assert_eq!(core::mem::size_of::<Fadt>(), 276);
    }

    #[test]
    fn test_pkg_length_encoding() {
        // Test 1-byte encoding (total <= 63)
        let mut buf = Vec::new();
        encode_pkg_length(&mut buf, 10); // total = 11
        assert_eq!(buf, vec![11]);

        // Test 1-byte boundary
        let mut buf = Vec::new();
        encode_pkg_length(&mut buf, 62); // total = 63 = max for 1-byte
        assert_eq!(buf, vec![63]);

        // Test 2-byte encoding (total = 64)
        let mut buf = Vec::new();
        encode_pkg_length(&mut buf, 62 + 1); // content = 63, total = 65
                                             // total = 65 = 0x41
                                             // byte0 = (1 << 6) | (0x41 & 0x0F) = 0x40 | 0x01 = 0x41
                                             // byte1 = 0x41 >> 4 = 0x04
        assert_eq!(buf, vec![0x41, 0x04]);

        // Test 2-byte encoding with larger value (total = 100 = 0x64)
        let mut buf = Vec::new();
        encode_pkg_length(&mut buf, 98); // total = 100
                                         // byte0 = (1 << 6) | (0x64 & 0x0F) = 0x40 | 0x04 = 0x44
                                         // byte1 = 0x64 >> 4 = 0x06
        assert_eq!(buf, vec![0x44, 0x06]);

        // Test 2-byte encoding (total = 256 = 0x100)
        let mut buf = Vec::new();
        encode_pkg_length(&mut buf, 254); // total = 256
                                          // byte0 = (1 << 6) | (0x100 & 0x0F) = 0x40 | 0x00 = 0x40
                                          // byte1 = 0x100 >> 4 = 0x10
        assert_eq!(buf, vec![0x40, 0x10]);
    }
}
