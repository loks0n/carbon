//! Carbon - A minimal microVM runtime for AI agent sandboxing.
//!
//! Milestone 2: Boot Linux with virtio-blk disk support.
//!
//! This VMM requires Linux with KVM support. It will not run on other platforms.

#[cfg(target_os = "linux")]
mod boot;
#[cfg(target_os = "linux")]
mod devices;
#[cfg(target_os = "linux")]
mod kvm;

use clap::Parser;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(name = "carbon")]
#[command(about = "A minimal microVM runtime for AI agent sandboxing")]
struct Args {
    /// Path to the Linux kernel bzImage
    #[arg(short, long)]
    kernel: String,

    /// Kernel command line (fast-boot options added automatically)
    #[arg(short, long, default_value = "console=ttyS0")]
    cmdline: String,

    /// Memory size in megabytes
    #[arg(short, long, default_value = "512")]
    memory: u64,

    /// Path to raw disk image (enables virtio-blk device)
    #[arg(short, long)]
    disk: Option<String>,
}

fn main() -> ExitCode {
    let args = Args::parse();

    if let Err(e) = run(args) {
        eprintln!("Error: {e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

#[cfg(target_os = "linux")]
fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    use boot::{BootConfig, GuestMemory, VirtioDeviceConfig};
    use devices::{
        Cmos, MmioBus, Serial, VirtioBlk, CMOS_PORT_DATA, CMOS_PORT_INDEX, SERIAL_COM1_BASE,
        SERIAL_COM1_END, VIRTIO_BLK_IRQ, VIRTIO_MMIO_BASE, VIRTIO_MMIO_SIZE,
    };
    use kvm::{IoData, IoHandler, MmioHandler, VcpuExit};

    eprintln!("[VMM] Carbon starting...");
    eprintln!("[VMM] Kernel: {}", args.kernel);
    eprintln!("[VMM] Memory: {} MB", args.memory);
    if let Some(ref disk) = args.disk {
        eprintln!("[VMM] Disk: {}", disk);
    }

    // Create VM
    let vm = kvm::create_vm()?;

    // Allocate guest memory
    let mem_size = args.memory * 1024 * 1024;
    let memory = GuestMemory::new(mem_size)?;

    // Set up MMIO bus and virtio-blk device if disk provided
    let mut mmio_bus = MmioBus::new();

    // Build kernel command line
    // Note: virtio devices are discovered via ACPI, not kernel command line
    let mut cmdline_parts = vec![args.cmdline.clone()];
    cmdline_parts.push("reboot=t".into());
    cmdline_parts.push("panic=-1".into());
    cmdline_parts.push("noapictimer".into());
    let cmdline = cmdline_parts.join(" ");
    eprintln!("[VMM] Cmdline: {}", cmdline);

    // Build virtio device configuration for ACPI DSDT
    let mut virtio_devices = Vec::new();
    if args.disk.is_some() {
        virtio_devices.push(VirtioDeviceConfig {
            id: 0,
            mmio_base: VIRTIO_MMIO_BASE,
            mmio_size: VIRTIO_MMIO_SIZE as u32,
            gsi: VIRTIO_BLK_IRQ,
        });
    }

    // Set up ACPI tables with HW_REDUCED flag and virtio device definitions
    boot::setup_acpi(&memory, 1, &virtio_devices)?;

    // Set up MP tables for interrupt routing (used with HW_REDUCED ACPI)
    boot::setup_mptable(&memory, 1)?;

    // Set up boot using Linux 64-bit boot protocol
    let config = BootConfig {
        kernel_path: args.kernel.clone(),
        cmdline,
        mem_size,
    };
    boot::setup_boot(&vm, &memory, &config)?;

    // Create virtio-blk device after memory is set up
    if let Some(ref disk_path) = args.disk {
        let mut blk = VirtioBlk::new(disk_path)?;
        blk.set_memory(&memory);
        mmio_bus.register(VIRTIO_MMIO_BASE, VIRTIO_MMIO_SIZE, Box::new(blk));
        eprintln!("[VMM] virtio-blk registered at {:#x}", VIRTIO_MMIO_BASE);
    }

    // Create vCPU (also sets CPUID)
    let mut vcpu = vm.create_vcpu(0)?;

    // Set up CPU registers for 64-bit long mode boot
    vcpu.set_boot_msrs()?;
    boot::setup_vcpu_regs(&vcpu, &memory)?;

    // Create I/O and MMIO handler with devices
    struct DeviceHandler {
        serial: Serial,
        cmos: Cmos,
        mmio_bus: MmioBus,
        io_count: u64,
    }

    impl IoHandler for DeviceHandler {
        fn io_read(&mut self, port: u16, data: &mut IoData) {
            self.io_count += 1;
            if (SERIAL_COM1_BASE..=SERIAL_COM1_END).contains(&port) {
                let offset = port - SERIAL_COM1_BASE;
                let value = self.serial.read(offset);
                for i in 0..data.len() {
                    data.set(i, value);
                }
                if self.io_count <= 10 {
                    eprintln!(
                        "[I/O] IN  port={:#x} (serial+{}) -> {:#x}",
                        port, offset, value
                    );
                }
            } else if port == CMOS_PORT_INDEX || port == CMOS_PORT_DATA {
                let value = self.cmos.read(port);
                for i in 0..data.len() {
                    data.set(i, value);
                }
            } else {
                // Return 0xff for unhandled ports
                for i in 0..data.len() {
                    data.set(i, 0xff);
                }
                if self.io_count <= 10 {
                    eprintln!(
                        "[I/O] IN  port={:#x} size={} -> 0xff (unhandled)",
                        port,
                        data.len()
                    );
                }
            }
        }

        fn io_write(&mut self, port: u16, data: &IoData) {
            self.io_count += 1;
            if (SERIAL_COM1_BASE..=SERIAL_COM1_END).contains(&port) {
                let offset = port - SERIAL_COM1_BASE;
                if self.io_count <= 10 {
                    eprintln!(
                        "[I/O] OUT port={:#x} (serial+{}) <- {:?}",
                        port,
                        offset,
                        data.as_slice()
                    );
                }
                for &byte in data.as_slice() {
                    self.serial.write(offset, byte);
                }
            } else if port == CMOS_PORT_INDEX || port == CMOS_PORT_DATA {
                for &byte in data.as_slice() {
                    self.cmos.write(port, byte);
                }
            } else if self.io_count <= 10 {
                eprintln!(
                    "[I/O] OUT port={:#x} <- {:?} (unhandled)",
                    port,
                    data.as_slice()
                );
            }
        }
    }

    impl MmioHandler for DeviceHandler {
        fn mmio_read(&mut self, addr: u64, data: &mut [u8]) {
            self.io_count += 1;
            self.mmio_bus.read(addr, data);
        }

        fn mmio_write(&mut self, addr: u64, data: &[u8]) {
            self.io_count += 1;
            self.mmio_bus.write(addr, data);
        }
    }

    let mut handler = DeviceHandler {
        serial: Serial::new(),
        cmos: Cmos::new(),
        mmio_bus,
        io_count: 0,
    };

    eprintln!("[VMM] Starting vCPU...");
    use std::io::Write;
    std::io::stderr().flush().ok();

    // Run the VM
    let mut iteration = 0u64;
    loop {
        iteration += 1;
        if iteration == 1 {
            eprintln!("[VMM] Entering KVM (first run)...");
            std::io::stderr().flush().ok();
        }
        let exit = vcpu.run_with_io(&mut handler)?;
        if iteration == 1 {
            eprintln!("[VMM] First vCPU exit received!");
        }

        // Log first 10 exits and every 100000 after
        if iteration <= 10 || iteration.is_multiple_of(100000) {
            eprintln!(
                "[VMM] iteration {}: {:?}, {} I/O ops",
                iteration, exit, handler.io_count
            );
        }
        match exit {
            VcpuExit::Io => {
                // I/O handled by the handler
            }
            VcpuExit::Hlt => {
                eprintln!(
                    "\n[VMM] Guest halted after {} iterations, {} I/O ops",
                    iteration, handler.io_count
                );
                break;
            }
            VcpuExit::Shutdown => {
                eprintln!(
                    "\n[VMM] Guest shutdown after {} iterations, {} I/O ops",
                    iteration, handler.io_count
                );
                if let Ok(regs) = vcpu.get_regs() {
                    eprintln!("[VMM] Final RIP: {:#x}", regs.rip);
                }
                break;
            }
            VcpuExit::InternalError => {
                eprintln!("[VMM] KVM internal error");
                break;
            }
            VcpuExit::FailEntry(reason) => {
                eprintln!("[VMM] Failed to enter guest: reason={}", reason);
                break;
            }
            VcpuExit::SystemEvent(event) => {
                eprintln!("[VMM] System event: {}", event);
                break;
            }
            VcpuExit::Unknown(reason) => {
                eprintln!("[VMM] Unknown exit: {}", reason);
                break;
            }
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn run(_args: Args) -> Result<(), Box<dyn std::error::Error>> {
    Err("Carbon requires Linux with KVM support. This platform is not supported.".into())
}
