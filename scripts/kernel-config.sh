#!/bin/bash
#
# Kernel configuration for Carbon microVM
# Sourced by build scripts - expects to be run from kernel source directory
#

set -euo pipefail

echo "Configuring kernel..."

# Start from x86_64 defconfig
make ARCH=x86_64 x86_64_defconfig

CFG="./scripts/config"

#
# === CRITICAL: Disable slow subsystems ===
#

# No modules - everything built-in (faster boot)
$CFG --disable MODULES

# No debug/tracing (saves ~12ms boot time)
$CFG --disable FTRACE
$CFG --disable FUNCTION_TRACER
$CFG --disable DEBUG_KERNEL
$CFG --disable DEBUG_INFO
$CFG --disable DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT
$CFG --disable DEBUG_FS
$CFG --disable KPROBES
$CFG --disable PROFILING
$CFG --disable PERF_EVENTS
$CFG --disable SCHED_DEBUG
$CFG --disable SCHEDSTATS

# Enable minimal ACPI (needed for fast boot path)
$CFG --enable ACPI
$CFG --disable ACPI_AC
$CFG --disable ACPI_BATTERY
$CFG --disable ACPI_BUTTON
$CFG --disable ACPI_FAN
$CFG --disable ACPI_DOCK
$CFG --disable ACPI_PROCESSOR
$CFG --disable ACPI_THERMAL
$CFG --disable ACPI_DEBUG
$CFG --disable ACPI_PCI_SLOT
$CFG --disable ACPI_CONTAINER
$CFG --disable ACPI_HOTPLUG_CPU
$CFG --disable ACPI_HOTPLUG_MEMORY
$CFG --disable ACPI_HOTPLUG_IOAPIC
$CFG --disable ACPI_CUSTOM_METHOD

# No PCI - pure virtio-mmio
$CFG --disable PCI

# Single CPU - skip SMP init entirely
$CFG --disable SMP
$CFG --disable HOTPLUG_CPU

# No NUMA
$CFG --disable NUMA

# No power management
$CFG --disable CPU_FREQ
$CFG --disable CPU_IDLE
$CFG --disable HIBERNATION
$CFG --disable SUSPEND
$CFG --disable PM

#
# === Disable unused drivers ===
#

$CFG --disable SOUND
$CFG --disable USB_SUPPORT
$CFG --disable WLAN
$CFG --disable WIRELESS
$CFG --disable DRM
$CFG --disable FB
$CFG --disable VGA_CONSOLE
$CFG --disable INPUT_KEYBOARD
$CFG --disable INPUT_MOUSE
$CFG --disable SERIO
$CFG --disable HID
$CFG --disable I2C
$CFG --disable SPI
$CFG --disable HWMON
$CFG --disable THERMAL
$CFG --disable WATCHDOG
$CFG --disable MD
$CFG --disable SCSI
$CFG --disable ATA
$CFG --disable NVME_CORE
$CFG --disable FUSION
$CFG --disable MACINTOSH_DRIVERS
$CFG --disable PARPORT
$CFG --disable CDROM
$CFG --disable ACCESSIBILITY
$CFG --disable AUXDISPLAY
$CFG --disable MEDIA_SUPPORT
$CFG --disable RC_CORE
$CFG --disable CXL_BUS
$CFG --disable PCCARD
$CFG --disable RAPIDIO
$CFG --disable GNSS
$CFG --disable MTD
$CFG --disable OF
$CFG --disable REGULATOR
$CFG --disable PWM
$CFG --disable POWER_SUPPLY
$CFG --disable IIO
$CFG --disable NTB
$CFG --disable VME_BUS
$CFG --disable COMEDI
$CFG --disable STAGING

# No legacy/compat
$CFG --disable IA32_EMULATION
$CFG --disable X86_X32_ABI
$CFG --disable COMPAT

# No security frameworks (container handles security)
$CFG --disable SECURITY
$CFG --disable SECURITY_SELINUX
$CFG --disable SECURITY_APPARMOR
$CFG --disable AUDIT
$CFG --disable INTEGRITY

#
# === Disable unused networking ===
#

$CFG --disable IPV6
$CFG --disable NETFILTER
$CFG --disable BRIDGE
$CFG --disable VLAN_8021Q
$CFG --disable BT
$CFG --disable CFG80211
$CFG --disable RFKILL
$CFG --disable NET_SCHED
$CFG --disable DCB
$CFG --disable DNS_RESOLVER
$CFG --disable BATMAN_ADV
$CFG --disable VSOCKETS
$CFG --disable NETLINK_DIAG
$CFG --disable CGROUP_NET_PRIO
$CFG --disable CGROUP_NET_CLASSID
$CFG --disable NET_SWITCHDEV

# Disable NFS/RPC (takes time to initialize)
$CFG --disable NETWORK_FILESYSTEMS
$CFG --disable NFS_FS
$CFG --disable NFSD
$CFG --disable SUNRPC

# Disable 9p filesystem
$CFG --disable 9P_FS
$CFG --disable NET_9P

#
# === Disable unused filesystems ===
#

$CFG --disable BTRFS_FS
$CFG --disable XFS_FS
$CFG --disable F2FS_FS
$CFG --disable REISERFS_FS
$CFG --disable JFS_FS
$CFG --disable GFS2_FS
$CFG --disable OCFS2_FS
$CFG --disable NILFS2_FS
$CFG --disable NTFS_FS
$CFG --disable NTFS3_FS
$CFG --disable FUSE_FS
$CFG --disable CIFS
$CFG --disable CEPH_FS
$CFG --disable ORANGEFS_FS
$CFG --disable AFFS_FS
$CFG --disable HFS_FS
$CFG --disable HFSPLUS_FS
$CFG --disable BEFS_FS
$CFG --disable BFS_FS
$CFG --disable EFS_FS
$CFG --disable CRAMFS
$CFG --disable SQUASHFS
$CFG --disable VXFS_FS
$CFG --disable MINIX_FS
$CFG --disable OMFS_FS
$CFG --disable HPFS_FS
$CFG --disable QNX4FS_FS
$CFG --disable QNX6FS_FS
$CFG --disable ROMFS_FS
$CFG --disable PSTORE
$CFG --disable SYSV_FS
$CFG --disable UFS_FS
$CFG --disable EROFS_FS
$CFG --disable EFIVAR_FS
$CFG --disable QUOTA
$CFG --disable AUTOFS_FS
$CFG --disable ISO9660_FS
$CFG --disable UDF_FS

#
# === Enable what we need ===
#

# VirtIO - our transport
$CFG --enable VIRTIO
$CFG --enable VIRTIO_MMIO
$CFG --enable VIRTIO_MMIO_CMDLINE_DEVICES
$CFG --enable VIRTIO_BLK
$CFG --enable VIRTIO_NET
$CFG --disable VIRTIO_PCI
$CFG --disable VIRTIO_BALLOON
$CFG --disable VIRTIO_CONSOLE
$CFG --disable VIRTIO_INPUT

# KVM guest optimizations - critical for fast boot
$CFG --enable HYPERVISOR_GUEST
$CFG --enable KVM_GUEST
$CFG --enable PARAVIRT
$CFG --enable PARAVIRT_CLOCK
$CFG --enable PARAVIRT_SPINLOCKS

# Essential filesystems only
$CFG --enable EXT4_FS
$CFG --enable TMPFS
$CFG --enable DEVTMPFS
$CFG --enable DEVTMPFS_MOUNT
$CFG --enable PROC_FS
$CFG --enable SYSFS

# Minimal serial (1 UART only)
$CFG --set-val SERIAL_8250_NR_UARTS 1
$CFG --set-val SERIAL_8250_RUNTIME_UARTS 1
$CFG --disable SERIAL_8250_EXTENDED
$CFG --disable SERIAL_8250_PNP
$CFG --disable SERIAL_8250_DMA

#
# === Boot time optimizations ===
#

# Timer - use HZ=100 for lower overhead
$CFG --disable HZ_1000
$CFG --disable HZ_250
$CFG --enable HZ_100
$CFG --set-val HZ 100

# Optimize for size
$CFG --enable CC_OPTIMIZE_FOR_SIZE
$CFG --disable CC_OPTIMIZE_FOR_PERFORMANCE

# LZ4 compression (faster decompression than gzip)
$CFG --enable KERNEL_LZ4
$CFG --disable KERNEL_GZIP
$CFG --disable KERNEL_ZSTD

# No EFI
$CFG --disable EFI
$CFG --disable EFI_STUB

# No KASLR (deterministic addresses, faster boot)
$CFG --disable RANDOMIZE_BASE
$CFG --disable RELOCATABLE

# Simpler memory allocator
$CFG --enable SLUB_TINY

# Keep printk timestamps for boot time measurement
$CFG --enable PRINTK_TIME

# Disable RCU boot expediting
$CFG --disable RCU_EXPEDITE_BOOT

# Don't zero memory on alloc/free
$CFG --disable INIT_ON_ALLOC_DEFAULT_ON
$CFG --disable INIT_ON_FREE_DEFAULT_ON

# Disable jump label patching at boot
$CFG --disable JUMP_LABEL

# Finalize config (resolve dependencies)
make ARCH=x86_64 olddefconfig

# Show stats
ENABLED=$(grep -c "=y" .config || true)
DISABLED=$(grep -c "is not set" .config || true)
echo "Config: ${ENABLED} enabled, ${DISABLED} disabled"
