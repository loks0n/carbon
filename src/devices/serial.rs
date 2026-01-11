//! 8250 UART serial port emulation.
//!
//! Implements a minimal 8250 UART for console output.
//! Only supports output (TX) - input is not implemented for milestone 1.

use std::io::{self, Write};

/// 8250 UART register offsets
mod regs {
    /// Transmit Holding Register (write) / Receive Buffer Register (read)
    pub const THR_RBR: u16 = 0;
    /// Interrupt Enable Register
    pub const IER: u16 = 1;
    /// Interrupt Identification Register (read) / FIFO Control Register (write)
    pub const IIR_FCR: u16 = 2;
    /// Line Control Register
    pub const LCR: u16 = 3;
    /// Modem Control Register
    pub const MCR: u16 = 4;
    /// Line Status Register
    pub const LSR: u16 = 5;
    /// Modem Status Register
    pub const MSR: u16 = 6;
    /// Scratch Register
    pub const SCR: u16 = 7;
}

/// Line Status Register bits
mod lsr {
    /// Data Ready
    #[allow(dead_code)]
    pub const DR: u8 = 0x01;
    /// Transmitter Holding Register Empty
    pub const THRE: u8 = 0x20;
    /// Transmitter Empty
    pub const TEMT: u8 = 0x40;
}

/// Interrupt Identification Register bits
mod iir {
    /// No interrupt pending
    pub const NO_INT: u8 = 0x01;
}

/// 8250 UART serial port.
pub struct Serial {
    /// Interrupt Enable Register
    ier: u8,
    /// Line Control Register
    lcr: u8,
    /// Modem Control Register
    mcr: u8,
    /// Scratch Register
    scr: u8,
    /// FIFO Control Register
    fcr: u8,
    /// Divisor Latch (low byte)
    dll: u8,
    /// Divisor Latch (high byte)
    dlh: u8,
}

impl Serial {
    pub fn new() -> Self {
        Self {
            ier: 0,
            lcr: 0,
            mcr: 0,
            scr: 0,
            fcr: 0,
            dll: 0,
            dlh: 0,
        }
    }

    /// Handle a read from the serial port.
    /// `offset` is the register offset from the base port (0-7).
    pub fn read(&self, offset: u16) -> u8 {
        let dlab = self.lcr & 0x80 != 0;

        match offset {
            regs::THR_RBR if dlab => self.dll,
            regs::THR_RBR => {
                // No data available (we don't support input)
                0
            }
            regs::IER if dlab => self.dlh,
            regs::IER => self.ier,
            regs::IIR_FCR => {
                // No interrupt pending
                iir::NO_INT
            }
            regs::LCR => self.lcr,
            regs::MCR => self.mcr,
            regs::LSR => {
                // Always ready to transmit, no data to receive
                lsr::THRE | lsr::TEMT
            }
            regs::MSR => {
                // Carrier Detect, Clear To Send, Data Set Ready
                0xb0
            }
            regs::SCR => self.scr,
            _ => 0,
        }
    }

    /// Handle a write to the serial port.
    /// `offset` is the register offset from the base port (0-7).
    pub fn write(&mut self, offset: u16, value: u8) {
        let dlab = self.lcr & 0x80 != 0;

        match offset {
            regs::THR_RBR if dlab => self.dll = value,
            regs::THR_RBR => {
                // Write character to stdout
                let _ = io::stdout().write_all(&[value]);
                let _ = io::stdout().flush();
            }
            regs::IER if dlab => self.dlh = value,
            regs::IER => self.ier = value,
            regs::IIR_FCR => self.fcr = value,
            regs::LCR => self.lcr = value,
            regs::MCR => self.mcr = value,
            regs::SCR => self.scr = value,
            _ => {}
        }
    }
}

impl Default for Serial {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lsr_always_ready() {
        let serial = Serial::new();
        let lsr = serial.read(regs::LSR);
        assert_eq!(lsr & lsr::THRE, lsr::THRE, "THRE should be set");
        assert_eq!(lsr & lsr::TEMT, lsr::TEMT, "TEMT should be set");
    }

    #[test]
    fn test_scratch_register() {
        let mut serial = Serial::new();
        serial.write(regs::SCR, 0x42);
        assert_eq!(serial.read(regs::SCR), 0x42);
    }

    #[test]
    fn test_ier_register() {
        let mut serial = Serial::new();
        serial.write(regs::IER, 0x0f);
        assert_eq!(serial.read(regs::IER), 0x0f);
    }

    #[test]
    fn test_lcr_register() {
        let mut serial = Serial::new();
        serial.write(regs::LCR, 0x03);
        assert_eq!(serial.read(regs::LCR), 0x03);
    }

    #[test]
    fn test_dlab_mode() {
        let mut serial = Serial::new();

        // Enable DLAB
        serial.write(regs::LCR, 0x80);

        // Write divisor latch
        serial.write(regs::THR_RBR, 0x01); // DLL
        serial.write(regs::IER, 0x00); // DLH

        // Read back
        assert_eq!(serial.read(regs::THR_RBR), 0x01);
        assert_eq!(serial.read(regs::IER), 0x00);

        // Disable DLAB
        serial.write(regs::LCR, 0x00);

        // Now reads should return normal registers
        assert_eq!(serial.read(regs::IER), 0x00);
    }

    #[test]
    fn test_iir_no_interrupt() {
        let serial = Serial::new();
        assert_eq!(serial.read(regs::IIR_FCR), iir::NO_INT);
    }
}
