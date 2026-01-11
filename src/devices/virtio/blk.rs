//! Virtio block device implementation.
//!
//! This module implements a virtio block device (virtio-blk) that allows
//! the guest to access a raw disk image file.
//!
//! # virtio-blk Protocol
//!
//! The guest communicates with the device using descriptor chains:
//!
//! 1. **Request Header** (16 bytes, device-readable):
//!    - type (4 bytes): 0=IN(read), 1=OUT(write), 4=FLUSH
//!    - reserved (4 bytes)
//!    - sector (8 bytes): starting sector number
//!
//! 2. **Data Buffer** (device-readable for writes, device-writable for reads)
//!
//! 3. **Status** (1 byte, device-writable):
//!    - 0 = OK
//!    - 1 = IOERR
//!    - 2 = UNSUPP
//!
//! # Example Request Flow (Read)
//!
//! ```text
//! Guest                               Device (VMM)
//!   │                                     │
//!   │ Write descriptors to ring           │
//!   │ Update avail->idx                   │
//!   │ Write to QUEUE_NOTIFY ──────────────►
//!   │                                     │ Read descriptors
//!   │                                     │ pread(disk, sector, len)
//!   │                                     │ Write data to guest buffer
//!   │                                     │ Write status byte
//!   │                                     │ Update used->idx
//!   │◄──────────────────────────── (poll) │
//!   │                                     │
//! ```

use crate::boot::GuestMemory;
use crate::devices::mmio::MmioDevice;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;

use super::{
    VirtqDesc, Virtqueue, MAX_QUEUE_SIZE, MMIO_DEVICE_FEATURES, MMIO_DEVICE_FEATURES_SEL,
    MMIO_DEVICE_ID, MMIO_DRIVER_FEATURES, MMIO_DRIVER_FEATURES_SEL, MMIO_INTERRUPT_ACK,
    MMIO_INTERRUPT_STATUS, MMIO_MAGIC_VALUE, MMIO_QUEUE_DESC_HIGH, MMIO_QUEUE_DESC_LOW,
    MMIO_QUEUE_DEVICE_HIGH, MMIO_QUEUE_DEVICE_LOW, MMIO_QUEUE_DRIVER_HIGH, MMIO_QUEUE_DRIVER_LOW,
    MMIO_QUEUE_NOTIFY, MMIO_QUEUE_NUM, MMIO_QUEUE_NUM_MAX, MMIO_QUEUE_READY, MMIO_QUEUE_SEL,
    MMIO_STATUS, MMIO_VENDOR_ID, MMIO_VERSION, STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK,
    STATUS_FEATURES_OK, VIRTIO_MMIO_MAGIC, VIRTIO_MMIO_VERSION, VIRTIO_VENDOR_ID,
    VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE,
};

/// Virtio device ID for block devices.
const VIRTIO_BLK_DEVICE_ID: u32 = 2;

/// Sector size in bytes.
const SECTOR_SIZE: u64 = 512;

/// Block size (logical block size reported to guest).
const BLK_SIZE: u32 = 512;

// Feature bits (from virtio spec)
/// Maximum size of any single segment is in `size_max`.
const VIRTIO_BLK_F_SIZE_MAX: u32 = 1 << 1;
/// Maximum number of segments in a request is in `seg_max`.
const VIRTIO_BLK_F_SEG_MAX: u32 = 1 << 2;
/// Block size of disk is in `blk_size`.
const VIRTIO_BLK_F_BLK_SIZE: u32 = 1 << 6;
/// Cache flush command support.
const VIRTIO_BLK_F_FLUSH: u32 = 1 << 9;

/// VIRTIO_F_VERSION_1 - Required for virtio-mmio v2 devices.
/// This is bit 32, so it goes in the high features word.
const VIRTIO_F_VERSION_1: u32 = 1 << 0; // Bit 32 = bit 0 of high word

/// Maximum segment size we support (1MB).
const SIZE_MAX: u32 = 1024 * 1024;
/// Maximum segments per request.
const SEG_MAX: u32 = 128;

// Block request types
const VIRTIO_BLK_T_IN: u32 = 0; // Read
const VIRTIO_BLK_T_OUT: u32 = 1; // Write
const VIRTIO_BLK_T_FLUSH: u32 = 4; // Flush

// Block status codes
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

// Config space offsets (relative to MMIO_CONFIG = 0x100)
const CONFIG_CAPACITY: u64 = 0x100; // 8 bytes
const CONFIG_SIZE_MAX: u64 = 0x108; // 4 bytes
const CONFIG_SEG_MAX: u64 = 0x10c; // 4 bytes
const CONFIG_BLK_SIZE: u64 = 0x114; // 4 bytes (after geometry)

/// Virtio block device.
pub struct VirtioBlk {
    /// The disk image file.
    disk: File,
    /// Disk capacity in sectors.
    capacity: u64,

    /// Device features (low 32 bits).
    device_features_lo: u32,
    /// Device features (high 32 bits).
    device_features_hi: u32,
    /// Driver-selected features (low 32 bits).
    driver_features_lo: u32,
    /// Driver-selected features (high 32 bits).
    driver_features_hi: u32,
    /// Feature selection register.
    features_sel: u32,

    /// Device status.
    status: u32,
    /// Interrupt status.
    interrupt_status: u32,

    /// Queue selection register.
    queue_sel: u32,
    /// The virtqueue.
    queue: Virtqueue,

    /// Reference to guest memory for virtqueue processing.
    /// This is set after device creation via set_memory().
    memory: Option<*const GuestMemory>,

    /// Count of processed requests (for debugging).
    request_count: u64,
}

// Safety: VirtioBlk can be sent between threads. The raw pointer to GuestMemory
// is only used during MMIO operations which happen on the same thread.
unsafe impl Send for VirtioBlk {}

impl VirtioBlk {
    /// Create a new virtio block device backed by the given disk image.
    ///
    /// # Arguments
    ///
    /// * `disk_path` - Path to the raw disk image file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub fn new(disk_path: &str) -> std::io::Result<Self> {
        let disk = OpenOptions::new().read(true).write(true).open(disk_path)?;

        let metadata = disk.metadata()?;
        let capacity = metadata.len() / SECTOR_SIZE;

        eprintln!(
            "[virtio-blk] Opened disk: {} ({} sectors, {} bytes)",
            disk_path,
            capacity,
            metadata.len()
        );

        // Advertise our supported features
        let device_features_lo = VIRTIO_BLK_F_SIZE_MAX
            | VIRTIO_BLK_F_SEG_MAX
            | VIRTIO_BLK_F_BLK_SIZE
            | VIRTIO_BLK_F_FLUSH;

        // High features word includes VIRTIO_F_VERSION_1 (required for mmio v2)
        let device_features_hi = VIRTIO_F_VERSION_1;

        Ok(Self {
            disk,
            capacity,
            device_features_lo,
            device_features_hi,
            driver_features_lo: 0,
            driver_features_hi: 0,
            features_sel: 0,
            status: 0,
            interrupt_status: 0,
            queue_sel: 0,
            queue: Virtqueue::new(),
            memory: None,
            request_count: 0,
        })
    }

    /// Set the guest memory reference for virtqueue processing.
    ///
    /// # Safety
    ///
    /// The caller must ensure the GuestMemory reference remains valid
    /// for the lifetime of this device.
    pub fn set_memory(&mut self, memory: &GuestMemory) {
        self.memory = Some(memory as *const GuestMemory);
    }

    /// Process all pending requests in the virtqueue.
    fn process_queue(&mut self) {
        let memory = match self.memory {
            Some(ptr) => unsafe { &*ptr },
            None => return,
        };

        while self.queue.has_pending(memory) {
            if let Some(desc_idx) = self.queue.pop_avail(memory) {
                let len = self.process_request(memory, desc_idx);
                if self.queue.push_used(memory, desc_idx, len).is_err() {
                    eprintln!("[virtio-blk] Failed to push to used ring");
                }
                self.request_count += 1;
                self.interrupt_status |= 1; // Set USED_BUFFER interrupt
            }
        }
    }

    /// Process a single block request.
    ///
    /// Returns the number of bytes written to guest-writable buffers.
    fn process_request(&mut self, memory: &GuestMemory, head_idx: u16) -> u32 {
        // Read the descriptor chain
        let mut desc_idx = head_idx;
        let mut descs = Vec::new();

        loop {
            let desc = match self.queue.read_desc(memory, desc_idx) {
                Some(d) => d,
                None => {
                    eprintln!("[virtio-blk] Failed to read descriptor {}", desc_idx);
                    return 0;
                }
            };
            descs.push(desc);

            if desc.flags & VIRTQ_DESC_F_NEXT == 0 {
                break;
            }
            desc_idx = desc.next;
        }

        if descs.len() < 2 {
            eprintln!(
                "[virtio-blk] Request too short: {} descriptors",
                descs.len()
            );
            return 0;
        }

        // First descriptor: request header (16 bytes)
        let header_desc = &descs[0];
        let mut header_buf = [0u8; 16];
        if memory.read(header_desc.addr, &mut header_buf).is_err() {
            eprintln!("[virtio-blk] Failed to read request header");
            return 0;
        }

        let req_type =
            u32::from_le_bytes([header_buf[0], header_buf[1], header_buf[2], header_buf[3]]);
        let sector = u64::from_le_bytes([
            header_buf[8],
            header_buf[9],
            header_buf[10],
            header_buf[11],
            header_buf[12],
            header_buf[13],
            header_buf[14],
            header_buf[15],
        ]);

        // Last descriptor: status byte (1 byte, device-writable)
        let status_desc = &descs[descs.len() - 1];
        if status_desc.flags & VIRTQ_DESC_F_WRITE == 0 {
            eprintln!("[virtio-blk] Status descriptor not writable");
            return 0;
        }

        // Middle descriptors: data buffers
        let data_descs = &descs[1..descs.len() - 1];
        let mut total_written = 0u32;

        let status = match req_type {
            VIRTIO_BLK_T_IN => {
                // Read from disk to guest
                self.handle_read(memory, sector, data_descs, &mut total_written)
            }
            VIRTIO_BLK_T_OUT => {
                // Write from guest to disk
                self.handle_write(memory, sector, data_descs)
            }
            VIRTIO_BLK_T_FLUSH => {
                // Sync disk
                self.handle_flush()
            }
            _ => {
                eprintln!("[virtio-blk] Unsupported request type: {}", req_type);
                VIRTIO_BLK_S_UNSUPP
            }
        };

        // Write status byte
        if memory.write(status_desc.addr, &[status]).is_err() {
            eprintln!("[virtio-blk] Failed to write status");
        }
        total_written += 1; // Status byte

        if self.request_count < 10 {
            eprintln!(
                "[virtio-blk] Request #{}: type={} sector={} status={} written={}",
                self.request_count, req_type, sector, status, total_written
            );
        }

        total_written
    }

    /// Handle a read request.
    fn handle_read(
        &self,
        memory: &GuestMemory,
        mut sector: u64,
        data_descs: &[VirtqDesc],
        total_written: &mut u32,
    ) -> u8 {
        for desc in data_descs {
            if desc.flags & VIRTQ_DESC_F_WRITE == 0 {
                continue; // Skip non-writable descriptors
            }

            let offset = sector * SECTOR_SIZE;
            let len = desc.len as usize;

            // Read from disk
            let mut buf = vec![0u8; len];
            if let Err(e) = self.disk.read_at(&mut buf, offset) {
                eprintln!("[virtio-blk] Read error at offset {}: {}", offset, e);
                return VIRTIO_BLK_S_IOERR;
            }

            // Write to guest memory
            if memory.write(desc.addr, &buf).is_err() {
                eprintln!("[virtio-blk] Failed to write to guest memory");
                return VIRTIO_BLK_S_IOERR;
            }

            *total_written += len as u32;
            sector += (len as u64) / SECTOR_SIZE;
        }

        VIRTIO_BLK_S_OK
    }

    /// Handle a write request.
    fn handle_write(&self, memory: &GuestMemory, mut sector: u64, data_descs: &[VirtqDesc]) -> u8 {
        for desc in data_descs {
            if desc.flags & VIRTQ_DESC_F_WRITE != 0 {
                continue; // Skip writable descriptors (we read from non-writable ones)
            }

            let offset = sector * SECTOR_SIZE;
            let len = desc.len as usize;

            // Read from guest memory
            let mut buf = vec![0u8; len];
            if memory.read(desc.addr, &mut buf).is_err() {
                eprintln!("[virtio-blk] Failed to read from guest memory");
                return VIRTIO_BLK_S_IOERR;
            }

            // Write to disk
            if let Err(e) = self.disk.write_at(&buf, offset) {
                eprintln!("[virtio-blk] Write error at offset {}: {}", offset, e);
                return VIRTIO_BLK_S_IOERR;
            }

            sector += (len as u64) / SECTOR_SIZE;
        }

        VIRTIO_BLK_S_OK
    }

    /// Handle a flush request.
    fn handle_flush(&self) -> u8 {
        match self.disk.sync_all() {
            Ok(()) => VIRTIO_BLK_S_OK,
            Err(e) => {
                eprintln!("[virtio-blk] Flush error: {}", e);
                VIRTIO_BLK_S_IOERR
            }
        }
    }

    /// Read a 32-bit register value.
    fn read_register(&mut self, offset: u64) -> u32 {
        match offset {
            MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC,
            MMIO_VERSION => VIRTIO_MMIO_VERSION,
            MMIO_DEVICE_ID => VIRTIO_BLK_DEVICE_ID,
            MMIO_VENDOR_ID => VIRTIO_VENDOR_ID,
            MMIO_DEVICE_FEATURES => {
                if self.features_sel == 0 {
                    self.device_features_lo
                } else {
                    self.device_features_hi
                }
            }
            MMIO_QUEUE_NUM_MAX => MAX_QUEUE_SIZE as u32,
            MMIO_QUEUE_READY => {
                if self.queue.ready {
                    1
                } else {
                    0
                }
            }
            MMIO_INTERRUPT_STATUS => self.interrupt_status,
            MMIO_STATUS => self.status,

            // Config space (see virtio spec 5.2.4)
            CONFIG_CAPACITY => (self.capacity & 0xFFFF_FFFF) as u32,
            0x104 => (self.capacity >> 32) as u32,
            CONFIG_SIZE_MAX => SIZE_MAX,
            CONFIG_SEG_MAX => SEG_MAX,
            CONFIG_BLK_SIZE => BLK_SIZE,

            _ => {
                if self.request_count < 100 {
                    eprintln!("[virtio-blk] Unknown register read: {:#x}", offset);
                }
                0
            }
        }
    }

    /// Write a 32-bit register value.
    fn write_register(&mut self, offset: u64, value: u32) {
        match offset {
            MMIO_DEVICE_FEATURES_SEL => {
                self.features_sel = value;
            }
            MMIO_DRIVER_FEATURES => {
                if self.features_sel == 0 {
                    self.driver_features_lo = value;
                } else {
                    self.driver_features_hi = value;
                }
            }
            MMIO_DRIVER_FEATURES_SEL => {
                self.features_sel = value;
            }
            MMIO_QUEUE_SEL => {
                self.queue_sel = value;
            }
            MMIO_QUEUE_NUM => {
                if value <= MAX_QUEUE_SIZE as u32 {
                    self.queue.size = value as u16;
                }
            }
            MMIO_QUEUE_READY => {
                self.queue.ready = value != 0;
                if self.queue.ready {
                    eprintln!(
                        "[virtio-blk] Queue {} ready: desc={:#x} avail={:#x} used={:#x}",
                        self.queue_sel,
                        self.queue.desc_table,
                        self.queue.avail_ring,
                        self.queue.used_ring
                    );
                }
            }
            MMIO_QUEUE_NOTIFY => {
                // Guest is notifying us that there are descriptors to process
                self.process_queue();
            }
            MMIO_INTERRUPT_ACK => {
                self.interrupt_status &= !value;
            }
            MMIO_STATUS => {
                self.status = value;
                if value == 0 {
                    // Reset
                    self.queue = Virtqueue::new();
                    self.interrupt_status = 0;
                    eprintln!("[virtio-blk] Device reset");
                } else {
                    // Log status transitions
                    let mut flags = Vec::new();
                    if value & STATUS_ACKNOWLEDGE != 0 {
                        flags.push("ACK");
                    }
                    if value & STATUS_DRIVER != 0 {
                        flags.push("DRIVER");
                    }
                    if value & STATUS_FEATURES_OK != 0 {
                        flags.push("FEATURES_OK");
                    }
                    if value & STATUS_DRIVER_OK != 0 {
                        flags.push("DRIVER_OK");
                    }
                    eprintln!("[virtio-blk] Status: {} ({:#x})", flags.join("|"), value);
                }
            }
            MMIO_QUEUE_DESC_LOW => {
                self.queue.desc_table =
                    (self.queue.desc_table & 0xFFFF_FFFF_0000_0000) | value as u64;
            }
            MMIO_QUEUE_DESC_HIGH => {
                self.queue.desc_table =
                    (self.queue.desc_table & 0x0000_0000_FFFF_FFFF) | ((value as u64) << 32);
            }
            MMIO_QUEUE_DRIVER_LOW => {
                self.queue.avail_ring =
                    (self.queue.avail_ring & 0xFFFF_FFFF_0000_0000) | value as u64;
            }
            MMIO_QUEUE_DRIVER_HIGH => {
                self.queue.avail_ring =
                    (self.queue.avail_ring & 0x0000_0000_FFFF_FFFF) | ((value as u64) << 32);
            }
            MMIO_QUEUE_DEVICE_LOW => {
                self.queue.used_ring =
                    (self.queue.used_ring & 0xFFFF_FFFF_0000_0000) | value as u64;
            }
            MMIO_QUEUE_DEVICE_HIGH => {
                self.queue.used_ring =
                    (self.queue.used_ring & 0x0000_0000_FFFF_FFFF) | ((value as u64) << 32);
            }
            _ => {
                if self.request_count < 100 {
                    eprintln!(
                        "[virtio-blk] Unknown register write: {:#x} = {:#x}",
                        offset, value
                    );
                }
            }
        }
    }
}

impl MmioDevice for VirtioBlk {
    fn read(&mut self, offset: u64, data: &mut [u8]) {
        let value = self.read_register(offset & !0x3); // Align to 4 bytes
        let bytes = value.to_le_bytes();

        // Handle sub-word reads
        let start = (offset & 0x3) as usize;
        let len = data.len().min(4 - start);
        data[..len].copy_from_slice(&bytes[start..start + len]);
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        // Only handle 4-byte aligned writes
        if data.len() != 4 || offset & 0x3 != 0 {
            eprintln!(
                "[virtio-blk] Non-aligned write: offset={:#x} len={}",
                offset,
                data.len()
            );
            return;
        }

        let value = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        self.write_register(offset, value);
    }
}
