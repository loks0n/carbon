//! CMOS RTC (Real Time Clock) device emulation.
//!
//! The CMOS RTC is accessed via I/O ports 0x70 (index) and 0x71 (data).
//! The guest writes a register index to port 0x70, then reads/writes
//! the register value from/to port 0x71.
//!
//! We implement minimal emulation to avoid the kernel's RTC timeout.
//! When the kernel reads Status Register A (0x0A), bit 7 indicates
//! "update in progress". Returning 0x00 tells the kernel the RTC is
//! ready, avoiding a 1+ second timeout.
//!
//! Reference: <https://wiki.osdev.org/CMOS>

/// CMOS I/O port for the index register.
pub const CMOS_PORT_INDEX: u16 = 0x70;

/// CMOS I/O port for the data register.
pub const CMOS_PORT_DATA: u16 = 0x71;

/// Status Register A - bit 7 is UIP (Update In Progress).
const REG_STATUS_A: u8 = 0x0A;

/// Status Register B - format and interrupt control.
const REG_STATUS_B: u8 = 0x0B;

/// Status Register C - interrupt flags (read clears).
const REG_STATUS_C: u8 = 0x0C;

/// Status Register D - bit 7 indicates valid RAM/time.
const REG_STATUS_D: u8 = 0x0D;

/// CMOS RTC device.
///
/// Provides minimal RTC emulation to satisfy kernel boot requirements.
/// Returns static time values and status registers that indicate
/// the RTC is ready (not updating).
pub struct Cmos {
    /// Currently selected register index.
    index: u8,
}

impl Cmos {
    /// Create a new CMOS device.
    pub fn new() -> Self {
        Self { index: 0 }
    }

    /// Write to CMOS (port 0x70 or 0x71).
    ///
    /// Port 0x70: Sets the register index (lower 7 bits, bit 7 is NMI mask).
    /// Port 0x71: Writes to the selected register (mostly ignored).
    pub fn write(&mut self, port: u16, value: u8) {
        match port {
            CMOS_PORT_INDEX => {
                // Lower 7 bits are the register index
                // Bit 7 is NMI disable (we ignore it)
                self.index = value & 0x7F;
            }
            CMOS_PORT_DATA => {
                // We ignore writes to CMOS registers
                // (time setting, alarm, etc. not needed for boot)
            }
            _ => {}
        }
    }

    /// Read from CMOS (port 0x71).
    ///
    /// Returns the value of the currently selected register.
    pub fn read(&self, port: u16) -> u8 {
        if port != CMOS_PORT_DATA {
            return 0xFF;
        }

        match self.index {
            // Time registers - return zeros (midnight Jan 1)
            0x00 => 0x00, // Seconds
            0x02 => 0x00, // Minutes
            0x04 => 0x00, // Hours
            0x06 => 0x01, // Day of week (1 = Sunday)
            0x07 => 0x01, // Day of month
            0x08 => 0x01, // Month
            0x09 => 0x00, // Year (2000)
            0x32 => 0x20, // Century (20xx)

            // Status Register A: UIP=0 (not updating), divider and rate bits
            REG_STATUS_A => 0x26, // Standard divider settings, UIP=0

            // Status Register B: 24h mode, BCD format, no interrupts
            REG_STATUS_B => 0x02, // 24-hour mode

            // Status Register C: No interrupts pending
            REG_STATUS_C => 0x00,

            // Status Register D: Valid RAM and time (bit 7 set)
            REG_STATUS_D => 0x80,

            // All other registers return 0
            _ => 0x00,
        }
    }
}

impl Default for Cmos {
    fn default() -> Self {
        Self::new()
    }
}
