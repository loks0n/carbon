//! Device emulation for the VMM.

mod cmos;
mod mmio;
mod serial;
pub mod virtio;

pub use cmos::{Cmos, CMOS_PORT_DATA, CMOS_PORT_INDEX};
pub use mmio::{MmioBus, VIRTIO_BLK_IRQ, VIRTIO_MMIO_BASE, VIRTIO_MMIO_SIZE};
pub use serial::Serial;
pub use virtio::blk::VirtioBlk;

/// I/O port range for COM1 serial port.
pub const SERIAL_COM1_BASE: u16 = 0x3f8;
pub const SERIAL_COM1_END: u16 = 0x3ff;
