//! Guest physical memory management using vm-memory crate.
//!
//! This module wraps `vm_memory::GuestMemoryMmap` to provide guest memory
//! for the virtual machine. The vm-memory crate is the standard abstraction
//! used across the rust-vmm ecosystem.
//!
//! # Memory Model
//!
//! In hardware virtualization:
//!
//! - **Host virtual addresses**: Where the VMM (us) sees the memory
//! - **Guest physical addresses**: What the guest kernel sees as physical RAM
//! - **Host physical addresses**: Actual hardware RAM (managed by KVM/hardware)
//!
//! KVM translates guest physical addresses to host physical addresses using
//! EPT (Extended Page Tables) on Intel or NPT (Nested Page Tables) on AMD.
//!
//! ```text
//! Guest Virtual → Guest Physical → Host Virtual → Host Physical
//!     (kernel)       (GPA)         (vm-memory)      (hardware)
//! ```
//!
//! # Memory Layout
//!
//! The guest physical address space is laid out as:
//!
//! ```text
//! 0x00000000 ┌─────────────────┐
//!            │ Low Memory      │ ← IVT, BDA, boot structures
//!            │ (0-1MB)         │
//! 0x00100000 ├─────────────────┤
//!            │ Kernel Code     │ ← bzImage loaded here
//!            │                 │
//!            │                 │
//!            │ High Memory     │ ← Available RAM
//!            │                 │
//! mem_size   └─────────────────┘
//! ```
//!
//! # Memory Limits
//!
//! - **Minimum**: Must be > 1MB to load kernel at the 1MB mark
//! - **Maximum for identity mapping**: 1GB (512 × 2MB pages)
//!   - Larger VMs work fine; the kernel sets up its own page tables during boot
//!   - Only the first 1GB is identity-mapped for early boot
//!
//! # Usage
//!
//! ```ignore
//! use vm_memory::GuestAddress;
//!
//! // Allocate 512MB of guest memory
//! let memory = GuestMemory::new(512 * 1024 * 1024)?;
//!
//! // Write data at guest physical address 0x7000
//! memory.write_slice(&boot_params_data, GuestAddress(0x7000))?;
//!
//! // Write typed values (little-endian)
//! memory.write_obj(0xDEADBEEF_u32, GuestAddress(0x100000))?;
//!
//! // Get host pointer for KVM registration
//! let (host_addr, size) = memory.as_raw_parts();
//! ```

use super::BootError;
use vm_memory::{Bytes, GuestAddress, GuestMemory as GuestMemoryTrait, GuestMemoryMmap};

/// Guest physical memory region backed by vm-memory.
///
/// This is a thin wrapper around `GuestMemoryMmap` that provides a simpler
/// API for our use case (single contiguous region starting at address 0).
///
/// The underlying memory is allocated using mmap with:
/// - `MAP_PRIVATE`: Changes are not written to any file
/// - `MAP_ANONYMOUS`: Not backed by a file
/// - `MAP_NORESERVE`: Don't reserve swap space (allows overcommit)
pub struct GuestMemory {
    /// The underlying vm-memory guest memory.
    inner: GuestMemoryMmap,
    /// Size of the memory region in bytes.
    size: u64,
}

impl GuestMemory {
    /// Allocate a new guest memory region.
    ///
    /// Creates a contiguous memory region of the specified size starting at
    /// guest physical address 0. The memory is:
    /// - Readable and writable
    /// - Private (changes aren't visible to other processes)
    /// - Anonymous (not backed by a file)
    ///
    /// # Arguments
    ///
    /// * `size` - Size in bytes (must be > 1MB for kernel loading)
    ///
    /// # Errors
    ///
    /// Returns an error if memory allocation fails.
    pub fn new(size: u64) -> Result<Self, BootError> {
        // Create a single memory region starting at guest address 0
        let regions = vec![(GuestAddress(0), size as usize)];

        let inner = GuestMemoryMmap::from_ranges(&regions).map_err(|e| {
            BootError::MemoryAllocation(std::io::Error::other(format!(
                "Failed to create guest memory: {}",
                e
            )))
        })?;

        Ok(Self { inner, size })
    }

    /// Get raw parts for KVM memory region registration.
    ///
    /// Returns (host_virtual_address, size) for use with `set_user_memory_region`.
    ///
    /// # Safety
    ///
    /// The returned pointer is valid only while this GuestMemory exists.
    /// Do not free or reallocate the memory.
    pub fn as_raw_parts(&self) -> (u64, u64) {
        // Get the first (and only) region
        let region = self.inner.iter().next().expect("memory has no regions");
        let host_addr = region.as_ptr() as u64;
        (host_addr, self.size)
    }

    /// Write bytes at a guest physical address.
    ///
    /// # Arguments
    ///
    /// * `addr` - Guest physical address to write to
    /// * `data` - Bytes to write
    ///
    /// # Errors
    ///
    /// Returns an error if the write would exceed memory bounds.
    pub fn write(&self, addr: u64, data: &[u8]) -> Result<(), BootError> {
        self.inner
            .write_slice(data, GuestAddress(addr))
            .map_err(|e| {
                BootError::MemoryAllocation(std::io::Error::other(format!(
                    "Failed to write to guest memory at {:#x}: {}",
                    addr, e
                )))
            })
    }

    /// Write a single byte at a guest physical address.
    pub fn write_u8(&self, addr: u64, value: u8) -> Result<(), BootError> {
        self.write(addr, &[value])
    }

    /// Write a 32-bit value at a guest physical address (little-endian).
    pub fn write_u32(&self, addr: u64, value: u32) -> Result<(), BootError> {
        self.write(addr, &value.to_le_bytes())
    }

    /// Write a 64-bit value at a guest physical address (little-endian).
    pub fn write_u64(&self, addr: u64, value: u64) -> Result<(), BootError> {
        self.write(addr, &value.to_le_bytes())
    }

    /// Read bytes from a guest physical address into a buffer.
    ///
    /// # Arguments
    ///
    /// * `addr` - Guest physical address to read from
    /// * `data` - Buffer to read into
    ///
    /// # Errors
    ///
    /// Returns an error if the read would exceed memory bounds.
    pub fn read(&self, addr: u64, data: &mut [u8]) -> Result<(), BootError> {
        self.inner
            .read_slice(data, GuestAddress(addr))
            .map_err(|e| {
                BootError::MemoryAllocation(std::io::Error::other(format!(
                    "Failed to read from guest memory at {:#x}: {}",
                    addr, e
                )))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to read and return a Vec for test assertions.
    fn read_vec(mem: &GuestMemory, addr: u64, len: usize) -> Vec<u8> {
        let mut data = vec![0u8; len];
        mem.read(addr, &mut data).unwrap();
        data
    }

    #[test]
    fn test_allocate() {
        let mem = GuestMemory::new(4096).unwrap();
        let (_, size) = mem.as_raw_parts();
        assert_eq!(size, 4096);
    }

    #[test]
    fn test_write_read() {
        let mem = GuestMemory::new(4096).unwrap();
        mem.write(0, &[1, 2, 3, 4]).unwrap();
        assert_eq!(read_vec(&mem, 0, 4), vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_write_u8() {
        let mem = GuestMemory::new(4096).unwrap();
        mem.write_u8(100, 0x42).unwrap();
        assert_eq!(read_vec(&mem, 100, 1), vec![0x42]);
    }

    #[test]
    fn test_write_u32() {
        let mem = GuestMemory::new(4096).unwrap();
        mem.write_u32(100, 0x12345678).unwrap();
        assert_eq!(read_vec(&mem, 100, 4), vec![0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn test_write_u64() {
        let mem = GuestMemory::new(4096).unwrap();
        mem.write_u64(100, 0x123456789abcdef0).unwrap();
        assert_eq!(
            read_vec(&mem, 100, 8),
            vec![0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12]
        );
    }

    #[test]
    fn test_write_out_of_bounds() {
        let mem = GuestMemory::new(100).unwrap();
        assert!(mem.write(99, &[1, 2]).is_err());
    }

    #[test]
    fn test_read_out_of_bounds() {
        let mem = GuestMemory::new(100).unwrap();
        let mut buf = [0u8; 2];
        assert!(mem.read(99, &mut buf).is_err());
    }
}
