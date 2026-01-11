//! Virtio device infrastructure.
//!
//! This module implements the virtio specification for virtual device I/O.
//! Virtio provides a standard interface for virtual devices (block, network,
//! etc.) to communicate efficiently between guest and host.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                         Guest                                   │
//! │   ┌─────────────────────────────────────────────────────────┐  │
//! │   │                  virtio Driver                          │  │
//! │   │   - Writes requests to descriptor ring                  │  │
//! │   │   - Updates available ring                              │  │
//! │   │   - Notifies device via MMIO write                      │  │
//! │   └─────────────────────────────────────────────────────────┘  │
//! └──────────────────────────┬──────────────────────────────────────┘
//!                            │ Shared Memory (virtqueue)
//! ┌──────────────────────────▼──────────────────────────────────────┐
//! │                         VMM                                     │
//! │   ┌─────────────────────────────────────────────────────────┐  │
//! │   │                 virtio Device                           │  │
//! │   │   - Reads requests from descriptor ring                 │  │
//! │   │   - Processes requests (disk I/O, etc.)                 │  │
//! │   │   - Updates used ring                                   │  │
//! │   └─────────────────────────────────────────────────────────┘  │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # MMIO Transport
//!
//! We use the virtio-mmio transport (as opposed to PCI). The device appears
//! at a fixed memory address and is discovered via kernel command line:
//!
//! ```text
//! virtio_mmio.device=4K@0xd0000000:5
//! ```
//!
//! This tells Linux: "There's a 4KB virtio device at address 0xd0000000, IRQ 5"
//!
//! Reference: <https://docs.oasis-open.org/virtio/virtio/v1.1/virtio-v1.1.html>

pub mod blk;

use crate::boot::GuestMemory;

// ============================================================================
// MMIO Register Offsets (virtio-mmio v2)
// ============================================================================

/// Magic value register - always reads as "virt" (0x74726976).
pub const MMIO_MAGIC_VALUE: u64 = 0x000;

/// Version register - we implement version 2.
pub const MMIO_VERSION: u64 = 0x004;

/// Device type ID register.
pub const MMIO_DEVICE_ID: u64 = 0x008;

/// Vendor ID register.
pub const MMIO_VENDOR_ID: u64 = 0x00c;

/// Device features register (read).
pub const MMIO_DEVICE_FEATURES: u64 = 0x010;

/// Device features selection register (write).
pub const MMIO_DEVICE_FEATURES_SEL: u64 = 0x014;

/// Driver features register (write).
pub const MMIO_DRIVER_FEATURES: u64 = 0x020;

/// Driver features selection register (write).
pub const MMIO_DRIVER_FEATURES_SEL: u64 = 0x024;

/// Queue selection register (write).
pub const MMIO_QUEUE_SEL: u64 = 0x030;

/// Maximum queue size register (read).
pub const MMIO_QUEUE_NUM_MAX: u64 = 0x034;

/// Queue size register (write).
pub const MMIO_QUEUE_NUM: u64 = 0x038;

/// Queue ready register (read/write).
pub const MMIO_QUEUE_READY: u64 = 0x044;

/// Queue notify register (write).
pub const MMIO_QUEUE_NOTIFY: u64 = 0x050;

/// Interrupt status register (read).
pub const MMIO_INTERRUPT_STATUS: u64 = 0x060;

/// Interrupt acknowledge register (write).
pub const MMIO_INTERRUPT_ACK: u64 = 0x064;

/// Device status register (read/write).
pub const MMIO_STATUS: u64 = 0x070;

/// Queue descriptor low address register (write).
pub const MMIO_QUEUE_DESC_LOW: u64 = 0x080;

/// Queue descriptor high address register (write).
pub const MMIO_QUEUE_DESC_HIGH: u64 = 0x084;

/// Queue driver (available) low address register (write).
pub const MMIO_QUEUE_DRIVER_LOW: u64 = 0x090;

/// Queue driver (available) high address register (write).
pub const MMIO_QUEUE_DRIVER_HIGH: u64 = 0x094;

/// Queue device (used) low address register (write).
pub const MMIO_QUEUE_DEVICE_LOW: u64 = 0x0a0;

/// Queue device (used) high address register (write).
pub const MMIO_QUEUE_DEVICE_HIGH: u64 = 0x0a4;

// ============================================================================
// Magic and Version
// ============================================================================

/// Magic value "virt" (little-endian).
pub const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;

/// MMIO version we support.
pub const VIRTIO_MMIO_VERSION: u32 = 2;

/// Our vendor ID (arbitrary, not registered).
pub const VIRTIO_VENDOR_ID: u32 = 0x0;

// ============================================================================
// Device Status Flags
// ============================================================================

/// Guest has acknowledged the device.
pub const STATUS_ACKNOWLEDGE: u32 = 1;

/// Guest has loaded a driver.
pub const STATUS_DRIVER: u32 = 2;

/// Driver is ready.
pub const STATUS_DRIVER_OK: u32 = 4;

/// Feature negotiation complete.
pub const STATUS_FEATURES_OK: u32 = 8;

// ============================================================================
// Virtqueue Structures
// ============================================================================

/// Maximum queue size we support.
pub const MAX_QUEUE_SIZE: u16 = 128;

/// Descriptor flag: buffer continues in next descriptor.
pub const VIRTQ_DESC_F_NEXT: u16 = 1;

/// Descriptor flag: buffer is device-writable (vs device-readable).
pub const VIRTQ_DESC_F_WRITE: u16 = 2;

/// A virtqueue descriptor.
///
/// Each descriptor points to a buffer in guest memory and optionally
/// chains to another descriptor.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqDesc {
    /// Guest physical address of the buffer.
    pub addr: u64,
    /// Length of the buffer in bytes.
    pub len: u32,
    /// Flags (NEXT, WRITE, INDIRECT).
    pub flags: u16,
    /// Index of next descriptor if NEXT flag is set.
    pub next: u16,
}

impl VirtqDesc {
    /// Size of descriptor in bytes.
    pub const SIZE: usize = 16;

    /// Read a descriptor from guest memory.
    pub fn read_from(memory: &GuestMemory, addr: u64) -> Option<Self> {
        let mut buf = [0u8; Self::SIZE];
        memory.read(addr, &mut buf).ok()?;
        Some(Self {
            addr: u64::from_le_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]),
            len: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            flags: u16::from_le_bytes([buf[12], buf[13]]),
            next: u16::from_le_bytes([buf[14], buf[15]]),
        })
    }
}

/// Virtqueue state.
///
/// A virtqueue is the communication channel between guest and device.
/// It consists of three parts:
/// - Descriptor table: array of buffer descriptors
/// - Available ring: guest tells device which descriptors are ready
/// - Used ring: device tells guest which descriptors are complete
#[derive(Debug, Default)]
pub struct Virtqueue {
    /// Queue size (number of descriptors).
    pub size: u16,
    /// Whether the queue is ready for use.
    pub ready: bool,
    /// Guest physical address of descriptor table.
    pub desc_table: u64,
    /// Guest physical address of available ring.
    pub avail_ring: u64,
    /// Guest physical address of used ring.
    pub used_ring: u64,
    /// Last available index we processed.
    pub last_avail_idx: u16,
}

impl Virtqueue {
    /// Create a new virtqueue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if there are pending descriptors to process.
    pub fn has_pending(&self, memory: &GuestMemory) -> bool {
        if !self.ready || self.size == 0 {
            return false;
        }

        // Read avail->idx from guest memory
        // Available ring layout: flags (2) + idx (2) + ring[size] (2*size) + used_event (2)
        let avail_idx_addr = self.avail_ring + 2;
        let mut idx_buf = [0u8; 2];
        if memory.read(avail_idx_addr, &mut idx_buf).is_err() {
            return false;
        }
        let avail_idx = u16::from_le_bytes(idx_buf);

        avail_idx != self.last_avail_idx
    }

    /// Pop the next descriptor chain head from the available ring.
    ///
    /// Returns the descriptor index, or None if no descriptors are available.
    pub fn pop_avail(&mut self, memory: &GuestMemory) -> Option<u16> {
        if !self.ready || self.size == 0 {
            return None;
        }

        // Read avail->idx
        let avail_idx_addr = self.avail_ring + 2;
        let mut idx_buf = [0u8; 2];
        memory.read(avail_idx_addr, &mut idx_buf).ok()?;
        let avail_idx = u16::from_le_bytes(idx_buf);

        if avail_idx == self.last_avail_idx {
            return None;
        }

        // Read the descriptor index from avail->ring[last_avail_idx % size]
        let ring_offset = 4 + (self.last_avail_idx % self.size) as u64 * 2;
        let ring_addr = self.avail_ring + ring_offset;
        let mut desc_idx_buf = [0u8; 2];
        memory.read(ring_addr, &mut desc_idx_buf).ok()?;
        let desc_idx = u16::from_le_bytes(desc_idx_buf);

        self.last_avail_idx = self.last_avail_idx.wrapping_add(1);
        Some(desc_idx)
    }

    /// Add a descriptor chain to the used ring.
    ///
    /// # Arguments
    ///
    /// * `memory` - Guest memory
    /// * `desc_idx` - Head descriptor index of the completed chain
    /// * `len` - Total bytes written to the guest buffers
    pub fn push_used(&self, memory: &GuestMemory, desc_idx: u16, len: u32) -> Result<(), ()> {
        // Read used->idx
        let used_idx_addr = self.used_ring + 2;
        let mut idx_buf = [0u8; 2];
        memory.read(used_idx_addr, &mut idx_buf).map_err(|_| ())?;
        let used_idx = u16::from_le_bytes(idx_buf);

        // Write used->ring[used_idx % size]
        // Used ring element: id (4 bytes) + len (4 bytes)
        let ring_offset = 4 + (used_idx % self.size) as u64 * 8;
        let elem_addr = self.used_ring + ring_offset;

        // Write id (descriptor index as u32)
        memory
            .write(elem_addr, &(desc_idx as u32).to_le_bytes())
            .map_err(|_| ())?;
        // Write len
        memory
            .write(elem_addr + 4, &len.to_le_bytes())
            .map_err(|_| ())?;

        // Increment used->idx
        let new_idx = used_idx.wrapping_add(1);
        memory
            .write(used_idx_addr, &new_idx.to_le_bytes())
            .map_err(|_| ())?;

        Ok(())
    }

    /// Read a descriptor from the descriptor table.
    pub fn read_desc(&self, memory: &GuestMemory, idx: u16) -> Option<VirtqDesc> {
        if idx >= self.size {
            return None;
        }
        let desc_addr = self.desc_table + idx as u64 * VirtqDesc::SIZE as u64;
        VirtqDesc::read_from(memory, desc_addr)
    }
}
