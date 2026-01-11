//! MMIO (Memory-Mapped I/O) bus for virtio devices.
//!
//! This module provides the infrastructure for routing MMIO accesses to
//! the appropriate virtio device based on the guest physical address.
//!
//! # Memory Layout
//!
//! ```text
//! 0xd000_0000 - 0xd000_0FFF  virtio-blk MMIO (4KB)
//! 0xd000_1000 - 0xd000_1FFF  virtio-vsock MMIO (reserved)
//! 0xd000_2000 - 0xd000_2FFF  virtio-net MMIO (reserved)
//! ```
//!
//! Each virtio device gets a 4KB MMIO region for its configuration registers
//! and virtqueue notification.

/// Base address for virtio MMIO devices.
pub const VIRTIO_MMIO_BASE: u64 = 0xd000_0000;

/// Size of each virtio MMIO region (4KB).
pub const VIRTIO_MMIO_SIZE: u64 = 0x1000;

/// IRQ for virtio-blk device.
///
/// We use legacy IRQ 5 which is routed through the IOAPIC.
/// This works with standard ACPI mode (not HW_REDUCED).
pub const VIRTIO_BLK_IRQ: u32 = 5;

/// Trait for devices that respond to MMIO access.
///
/// Implementors handle reads and writes to their MMIO register space.
/// The offset is relative to the device's base address.
pub trait MmioDevice {
    /// Handle an MMIO read at the given offset.
    ///
    /// # Arguments
    ///
    /// * `offset` - Offset within the device's MMIO region (0 to size-1)
    /// * `data` - Buffer to fill with the read result
    fn read(&mut self, offset: u64, data: &mut [u8]);

    /// Handle an MMIO write at the given offset.
    ///
    /// # Arguments
    ///
    /// * `offset` - Offset within the device's MMIO region (0 to size-1)
    /// * `data` - Data being written
    fn write(&mut self, offset: u64, data: &[u8]);
}

/// A registered device on the MMIO bus.
struct MmioDeviceEntry {
    /// Base guest physical address of this device.
    base: u64,
    /// Size of the MMIO region.
    size: u64,
    /// The device implementation.
    device: Box<dyn MmioDevice>,
}

/// MMIO bus that routes accesses to registered devices.
///
/// When the guest accesses an MMIO address, the bus finds the device
/// that owns that address range and forwards the access to it.
pub struct MmioBus {
    /// Registered devices sorted by base address.
    devices: Vec<MmioDeviceEntry>,
}

impl MmioBus {
    /// Create a new empty MMIO bus.
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
        }
    }

    /// Register a device on the bus.
    ///
    /// # Arguments
    ///
    /// * `base` - Base guest physical address for the device
    /// * `size` - Size of the MMIO region
    /// * `device` - The device implementation
    pub fn register(&mut self, base: u64, size: u64, device: Box<dyn MmioDevice>) {
        self.devices.push(MmioDeviceEntry { base, size, device });
        // Keep sorted by base address for binary search
        self.devices.sort_by_key(|e| e.base);
    }

    /// Find the device that handles the given address.
    fn find_device(&mut self, addr: u64) -> Option<(&mut dyn MmioDevice, u64)> {
        for entry in &mut self.devices {
            if addr >= entry.base && addr < entry.base + entry.size {
                let offset = addr - entry.base;
                return Some((entry.device.as_mut(), offset));
            }
        }
        None
    }

    /// Handle an MMIO read from the guest.
    pub fn read(&mut self, addr: u64, data: &mut [u8]) {
        if let Some((device, offset)) = self.find_device(addr) {
            device.read(offset, data);
        } else {
            // Return 0xff for unmapped regions
            for byte in data.iter_mut() {
                *byte = 0xff;
            }
        }
    }

    /// Handle an MMIO write from the guest.
    pub fn write(&mut self, addr: u64, data: &[u8]) {
        if let Some((device, offset)) = self.find_device(addr) {
            device.write(offset, data);
        }
        // Writes to unmapped regions are silently ignored
    }
}

impl Default for MmioBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockDevice {
        value: u32,
    }

    impl MmioDevice for MockDevice {
        fn read(&mut self, offset: u64, data: &mut [u8]) {
            if offset == 0 && data.len() >= 4 {
                data[..4].copy_from_slice(&self.value.to_le_bytes());
            }
        }

        fn write(&mut self, offset: u64, data: &[u8]) {
            if offset == 0 && data.len() >= 4 {
                self.value = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            }
        }
    }

    #[test]
    fn test_mmio_bus() {
        let mut bus = MmioBus::new();
        bus.register(0x1000, 0x100, Box::new(MockDevice { value: 0x12345678 }));

        // Read from device
        let mut data = [0u8; 4];
        bus.read(0x1000, &mut data);
        assert_eq!(u32::from_le_bytes(data), 0x12345678);

        // Write to device
        bus.write(0x1000, &0xDEADBEEFu32.to_le_bytes());
        bus.read(0x1000, &mut data);
        assert_eq!(u32::from_le_bytes(data), 0xDEADBEEF);

        // Read from unmapped region returns 0xff
        bus.read(0x2000, &mut data);
        assert_eq!(data, [0xff; 4]);
    }
}
