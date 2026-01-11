# RFC: Carbon

## Overview

Carbon is a minimal microVM runtime for AI agent computing. Agents get real computers—persistent, checkpointable, resumable—not disposable sandboxes.

**Core insight:** Agents don't want sandboxes. They want computers that don't vanish, with storage that persists, and the ability to checkpoint/restore state instantly.

**Non-goals:** General-purpose VM hosting, legacy OS support, live migration, broad hardware compatibility.

---

## Design Principles

1. **Computers, not sandboxes** — Persistent by default. Sleep/wake, not spawn/destroy.
2. **Checkpoint-first** — Any running VM can snapshot instantly. Restore in <1s.
3. **Minimal surface** — Only devices agents need.
4. **Modern Linux** — Require 6.x. Use io_uring, userfaultfd without fallbacks.
5. **Separation of concerns** — VMM handles virtualization. Network policy handled externally.

---

## Architecture

```
┌─────────────────────────────────────────────────┐
│                 Guest Linux                     │
│                                                 │
│  Agent process                                  │
│    ├─► eth0 (virtio-net) ───────────────────────┼──► Internet (via TAP)
│    ├─► /dev/vda (virtio-blk) ───────────────────┼──► Disk image (CoW)
│    └─► vsock:3 (virtio-vsock) ──────────────────┼──► Host control plane
│                                                 │
├─────────────────────────────────────────────────┤
│  virtio-blk │ virtio-net │ virtio-vsock │ serial│
├─────────────────────────────────────────────────┤
│  KVM + userfaultfd CoW memory                   │
├─────────────────────────────────────────────────┤
│  VMM Process (Rust)                             │
│  └─ io_uring event loop                         │
└─────────────────────────────────────────────────┘
```

### Device Responsibilities

| Device       | Purpose              | Data Path                |
| ------------ | -------------------- | ------------------------ |
| virtio-blk   | Root filesystem + storage | Guest ↔ Disk image (qcow2) |
| virtio-net   | Agent network access | Guest ↔ TAP ↔ Internet   |
| virtio-vsock | Control plane        | Guest ↔ VMM              |
| serial       | Debug output         | Guest → stdout           |

---

## Milestones

### Milestone 1: Boot Linux

**Goal:** Kernel boots, prints to serial.

**Tasks:**

| Task           | Description                                  |
| -------------- | -------------------------------------------- |
| KVM setup      | Open /dev/kvm, create VM, create vCPU        |
| Guest memory   | mmap region, register with KVM               |
| 64-bit mode    | GDT, page tables, long mode                  |
| bzImage loader | Parse setup header, load protected-mode code |
| boot_params    | Construct boot_params struct at 0x7000       |
| E820 map       | Report available memory regions              |
| Serial (8250)  | Handle IoOut to 0x3f8                        |

**Memory Layout:**

```
0x0000_0000 - 0x0000_0FFF  Reserved (real mode IVT, BDA)
0x0000_5000 - 0x0000_5FFF  GDT
0x0000_7000 - 0x0000_7FFF  boot_params (zero page)
0x0000_9000 - 0x0000_FFFF  Page tables (PML4, PDPT, PD)
0x0002_0000 - 0x0002_0FFF  Kernel command line
0x0010_0000 - ...          Kernel (loaded at 1MB)
```

**Deliverable:**

```
$ carbon --kernel vmlinux
[    0.000000] Linux version 6.x ...
[    0.000000] Command line: console=ttyS0
...
[    0.100000] Kernel panic - not syncing: VFS: Unable to mount root fs
```

Panic expected — no rootfs yet.

---

### Milestone 2: virtio-blk

**Goal:** Boot from disk image. Persistent, writable filesystem with instant CoW clones.

**Why virtio-blk:**
- ~200 lines of code (vs ~2000+ for virtio-fs)
- Standard block device, kernel handles filesystem
- Well-understood, battle-tested

**Why raw files + btrfs reflinks (not qcow2):**

| Approach | CoW | Snapshots | Complexity | Rust support |
|----------|-----|-----------|------------|--------------|
| qcow2 | ✓ | ✓ | High (L1/L2 tables, refcounts) | Partial, no snapshot support |
| Raw + btrfs reflinks | ✓ | ✓ | None—kernel handles it | Just file ops |

qcow2 downsides:
- Complex format requiring custom parser
- Rust crates don't support snapshots
- Would end up shelling out to `qemu-img`
- Parsing overhead on every I/O

btrfs reflinks:
- `cp --reflink=always` is O(1), instant regardless of file size
- Snapshots are just files
- Sparse by default
- Zero VMM complexity

**Requirement:** Host filesystem must be btrfs. Reasonable for modern Linux-only VMM.

**Tasks:**

| Task             | Description                              |
| ---------------- | ---------------------------------------- |
| MMIO transport   | Device discovery at 0xd000_0000          |
| Virtqueue impl   | Descriptor tables, available/used rings  |
| virtio-blk       | Single request queue, read/write sectors |
| Raw disk I/O     | pread/pwrite to raw disk image           |
| Reflink helper   | CoW copy with fallback for non-btrfs     |
| Base image       | Build minimal rootfs with dev tools      |

**MMIO Layout:**

```
0xd000_0000  virtio-blk
0xd000_1000  virtio-vsock
0xd000_2000  virtio-net
```

**Disk Architecture:**

```
/srv/carbon/
├── bases/
│   └── ubuntu-dev.raw        # Shared base image (read-only)
└── vms/
    └── vm-123/
        └── disk.raw          # Reflink copy, VM-private writes
```

New VM = instant reflink clone:
```bash
cp --reflink=always bases/ubuntu-dev.raw vms/vm-123/disk.raw
```

**Reflink Helper:**

```rust
pub fn reflink_copy(src: &Path, dst: &Path) -> Result<()> {
    let status = Command::new("cp")
        .args(["--reflink=always", src.as_ref(), dst.as_ref()])
        .status()?;
    
    if status.success() {
        return Ok(());
    }
    
    // Fallback for non-btrfs (CI on ext4, local dev)
    std::fs::copy(src, dst)?;
    Ok(())
}

pub fn is_reflink_supported(path: &Path) -> bool {
    // Check filesystem type
    let output = Command::new("stat")
        .args(["-f", "-c", "%T", path.as_ref()])
        .output()
        .ok();
    
    output
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|t| t.trim() == "btrfs")
        .unwrap_or(false)
}
```

**Base Image Contents:**

```
- Minimal Debian/Alpine
- Python 3.x, Node.js, Git
- Common build tools (gcc, make)
- ~2GB compressed, ~8GB uncompressed
```

**CI Testing:**

btrfs via loopback mount (see `.github/actions/setup-btrfs`):
```yaml
- uses: ./.github/actions/setup-btrfs
  with:
    size: 4G
    path: /mnt/carbon
- run: make test-boot
  env:
    CARBON_STORAGE_PATH: /mnt/carbon
```

**Deliverables:**

1. **VM boots from raw disk:**
```bash
$ carbon --kernel vmlinux --disk ./disk.raw
[    0.000000] Linux version 6.x ...
...
[    0.150000] EXT4-fs (vda): mounted filesystem
/ # ls /
bin  dev  etc  home  proc  root  sys  tmp  usr  var
```

2. **Writes persist across reboot:**
```bash
/ # echo "hello" > /root/test.txt
/ # sync
$ carbon --kernel vmlinux --disk ./disk.raw
/ # cat /root/test.txt
hello
```

3. **Reflink clone is instant:**
```bash
$ time cp --reflink=always base.raw vm1.raw
real    0m0.003s   # <10ms regardless of image size

$ time cp --reflink=always base.raw vm2.raw
real    0m0.002s
```

4. **Clones are isolated:**
```bash
# VM1
$ carbon --disk vm1.raw
/ # echo "vm1" > /root/id.txt

# VM2 (from same base, doesn't see VM1's write)
$ carbon --disk vm2.raw
/ # cat /root/id.txt
cat: /root/id.txt: No such file or directory
```

5. **CI integration test passes:**
```rust
#[test]
fn test_disk_isolation() {
    let storage = PathBuf::from(
        env::var("CARBON_STORAGE_PATH").unwrap_or("/tmp".into())
    );
    
    let base = storage.join("base.raw");
    create_test_disk(&base, 512 * 1024 * 1024)?; // 512MB sparse
    
    let vm1_disk = storage.join("vm1.raw");
    let vm2_disk = storage.join("vm2.raw");
    
    reflink_copy(&base, &vm1_disk)?;
    reflink_copy(&base, &vm2_disk)?;
    
    // Boot VM1, write file
    let mut vm1 = Carbon::new()
        .kernel(&kernel_path)
        .disk(&vm1_disk)
        .spawn()?;
    vm1.wait_for_boot()?;
    vm1.shell("echo 'vm1' > /root/id.txt")?;
    vm1.shutdown()?;
    
    // Boot VM2, verify isolation
    let mut vm2 = Carbon::new()
        .kernel(&kernel_path)
        .disk(&vm2_disk)
        .spawn()?;
    vm2.wait_for_boot()?;
    let result = vm2.shell("cat /root/id.txt 2>&1");
    assert!(result.is_err() || result.unwrap().contains("No such file"));
    vm2.shutdown()?;
}
```

---

### Milestone 3: virtio-vsock

**Goal:** Bidirectional control channel between host and guest.

**Tasks:**

| Task             | Description                       |
| ---------------- | --------------------------------- |
| vsock device     | TX/RX/event queues                |
| Host listener    | AF_VSOCK socket on host           |
| Control protocol | Commands + file transfer          |
| Guest agent      | Simple daemon to handle commands  |

**Control Protocol:**

```rust
enum Command {
    Ping,
    Exec { cmd: String, timeout_ms: u64 },
    Signal(i32),
    
    // File transfer (workspace in/out)
    WriteFile { path: String, data: Vec<u8> },
    ReadFile { path: String },
    
    // Lifecycle
    Checkpoint { name: String },
    Shutdown,
}

enum Response {
    Pong,
    ExecResult { stdout: Vec<u8>, stderr: Vec<u8>, exit_code: i32 },
    FileData(Vec<u8>),
    Ack,
    Error(String),
}
```

**Workspace Pattern:**

No shared filesystem needed. Transfer files explicitly:

```rust
// Send input
vm.send(WriteFile { 
    path: "/workspace/input.json".into(), 
    data: input_bytes 
})?;

// Run task
vm.send(Exec { cmd: "python process.py".into(), ... })?;

// Get output
let result = vm.send(ReadFile { path: "/workspace/output.json".into() })?;
```

For large transfers (git repos, node_modules), stream a tarball over vsock.

**Deliverable:**

```rust
let vm = Carbon::new()
    .kernel("vmlinux")
    .disk("instance.qcow2")
    .spawn()?;

vm.send(Ping)?; // → Pong

vm.send(WriteFile { 
    path: "/workspace/code.py".into(),
    data: b"print('hello')".to_vec(),
})?;

let resp = vm.send(Exec { 
    cmd: "python /workspace/code.py".into(),
    timeout_ms: 5000,
})?;
assert_eq!(resp.stdout, b"hello\n");
```

---

### Milestone 4: virtio-net

**Goal:** Full network access. git clone, npm install, curl all work.

**Tasks:**

| Task              | Description                   |
| ----------------- | ----------------------------- |
| virtio-net device | TX/RX virtqueues              |
| TAP integration   | Read/write to tap device      |
| MAC address       | Assign per-VM                 |

**Network Architecture:**

```
Guest                    Host
  │                        │
  │ virtio-net             │
  │   │                    │
  └───┼────────────────────┼───┐
      │                    │   │
      │              ┌─────▼───▼─────┐
      │              │  TAP device   │
      │              └───────┬───────┘
      │                      │
      │              ┌───────▼───────┐
      │              │    Bridge     │
      │              └───────┬───────┘
      │                      │
      │              ┌───────▼───────┐
      │              │  NAT/filter   │
      │              └───────┬───────┘
      │                      │
      │                      ▼
      │                  Internet
```

VMM just shuffles packets. Network config is external.

**Deliverable:**

```rust
let vm = Carbon::new()
    .kernel("vmlinux")
    .disk("instance.qcow2")
    .network(NetworkConfig {
        tap_name: "tap-vm-1".into(),
        mac_address: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
    })
    .spawn()?;

// Network just works
vm.send(Exec { cmd: "curl -s https://httpbin.org/ip".into(), ... })?;

// Dev workflows just work
vm.send(Exec { cmd: "git clone https://github.com/user/repo".into(), ... })?;
vm.send(Exec { cmd: "cd repo && npm install".into(), ... })?;
```

---

### Milestone 5: Checkpoint + Restore

**Goal:** Instant checkpoint from running VM. Restore in <1s.

**Key difference from ephemeral sandboxes:** Checkpoints are user-triggered, named, and multiple per VM. Not just a pre-built golden image.

**Checkpoint Contents:**

```rust
struct Checkpoint {
    // CPU state
    regs: kvm_regs,
    sregs: kvm_sregs,
    
    // Memory (sparse file, reflink-copied)
    memory: PathBuf,
    memory_size: u64,
    
    // Disk state (reflink-copied)
    disk: PathBuf,
    
    // Device state
    vsock: VsockState,
    net: VirtioNetState,
    blk: VirtioBlkState,
}
```

**Checkpoint Flow:**

```rust
// VM is running, user triggers checkpoint
vm.send(Command::Checkpoint { name: "after-npm-install".into() })?;

// VMM:
// 1. Pause vCPU
// 2. Flush virtio-blk queue, fsync disk
// 3. Reflink copy disk.raw → checkpoints/after-npm-install/disk.raw (instant)
// 4. Dump memory to sparse file → checkpoints/after-npm-install/memory.raw
// 5. Reflink copy memory file (instant)
// 6. Save CPU registers + device state as JSON/bincode
// 7. Resume vCPU (or keep paused for sleep)
```

Checkpoint directory structure:
```
/srv/carbon/vms/vm-123/
├── disk.raw                      # Current disk state
├── memory.raw                    # Current memory (when paused)
└── checkpoints/
    ├── after-npm-install/
    │   ├── disk.raw              # Reflink copy
    │   ├── memory.raw            # Reflink copy
    │   └── state.bin             # CPU + device state
    └── clean-slate/
        ├── disk.raw
        ├── memory.raw
        └── state.bin
```

**Restore Flow (userfaultfd for speed):**

```rust
fn restore(checkpoint: &Checkpoint) -> Result<Vm> {
    // 1. Reflink copy disk from checkpoint (instant)
    reflink_copy(&checkpoint.disk, &vm_disk_path)?;
    
    // 2. Memory with lazy loading via userfaultfd
    let mem = mmap(PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_NORESERVE);
    let uffd = userfaultfd()?;
    uffd.register(mem, checkpoint.memory_size)?;
    
    // 3. Page fault handler loads from checkpoint memory file
    let mem_file = File::open(&checkpoint.memory)?;
    spawn(move || {
        for event in uffd.events() {
            let offset = event.address - mem_base;
            let mut page = [0u8; 4096];
            mem_file.read_at(&mut page, offset)?;
            uffd.copy(event.address, &page)?;
        }
    });
    
    // 4. Restore CPU state
    vcpu.set_regs(&checkpoint.regs)?;
    vcpu.set_sregs(&checkpoint.sregs)?;
    
    // 5. Restore device state
    // ...
    
    Ok(Vm { mem, vcpu, disk, ... })
}
```

**Sleep/Wake Lifecycle:**

```rust
impl Vm {
    fn sleep(&mut self) -> Result<CheckpointId> {
        // Auto-checkpoint + release resources
        let cp = self.checkpoint("auto-sleep")?;
        self.release_memory();
        self.release_vcpu();
        Ok(cp)
    }
    
    fn wake(&mut self) -> Result<()> {
        // Restore from sleep checkpoint
        self.restore(&self.last_checkpoint)?;
        Ok(())
    }
}
```

**Deliverable:**

```rust
// Create VM, do expensive setup once
let mut vm = Carbon::create("my-dev-env")?;
vm.send(Exec { cmd: "apt-get install -y ffmpeg".into(), ... })?;
vm.send(Exec { cmd: "npm install".into(), ... })?;

// Checkpoint the good state
vm.send(Checkpoint { name: "ready".into() })?;

// ... time passes, maybe VM sleeps ...

// Later: instant restore
let mut vm = Carbon::restore("my-dev-env", "ready")?;
// ffmpeg and node_modules are there, instantly

// Mess something up?
vm.send(Exec { cmd: "rm -rf node_modules".into(), ... })?;

// Restore to checkpoint
vm.restore_to("ready")?;
// Everything's back
```

---

## Lifecycle Model

```
                    ┌─────────────┐
        create ────►│   Running   │◄──── wake
                    └──────┬──────┘
                           │
            ┌──────────────┼──────────────┐
            │              │              │
            ▼              ▼              ▼
       checkpoint       sleep        destroy
            │              │              │
            ▼              ▼              ▼
    ┌───────────┐   ┌───────────┐   ┌─────────┐
    │Checkpoints│   │  Sleeping │   │  Gone   │
    │  (named)  │   │  (idle)   │   │         │
    └───────────┘   └───────────┘   └─────────┘
            │              │
            └──────┬───────┘
                   │
                   ▼
               restore
```

**States:**
- **Running:** vCPU active, memory resident, burning resources
- **Sleeping:** Checkpointed, resources released, wake on demand
- **Checkpoints:** Named snapshots, can restore to any

---

## Project Structure

```
carbon/
├── src/
│   ├── main.rs                CLI + vCPU run loop
│   ├── vm.rs                  VM lifecycle (create/checkpoint/restore)
│   ├── kvm/
│   │   ├── mod.rs             KVM wrappers
│   │   └── vcpu.rs            vCPU management
│   ├── boot/
│   │   ├── mod.rs             Boot orchestration
│   │   ├── memory.rs          Guest memory
│   │   ├── bzimage.rs         Kernel loading
│   │   ├── params.rs          boot_params
│   │   └── paging.rs          Page tables
│   ├── devices/
│   │   ├── serial.rs          8250 UART
│   │   └── virtio/
│   │       ├── mod.rs         Virtqueue impl
│   │       ├── blk.rs         virtio-blk
│   │       ├── vsock.rs       virtio-vsock  
│   │       └── net.rs         virtio-net
│   ├── checkpoint/
│   │   ├── mod.rs             Checkpoint format
│   │   ├── create.rs          Snapshot running VM
│   │   └── restore.rs         userfaultfd restore
│   └── disk/
│       └── qcow2.rs           qcow2 read/write/snapshot
```

---

## Dependencies

```toml
[dependencies]
libc = "0.2"
thiserror = "2"
clap = { version = "4", features = ["derive"] }

[target.'cfg(target_os = "linux")'.dependencies]
kvm-ioctls = "0.19"
kvm-bindings = { version = "0.10", features = ["fam-wrappers"] }
vm-memory = { version = "0.16", features = ["backend-mmap"] }
nix = { version = "0.29", features = ["fs", "mman"] }

# Future milestones
io-uring = "0.6"        # M2+: async I/O for virtio
userfaultfd = "0.5"     # M5: lazy page loading
```

No qcow2 crate needed—btrfs reflinks handle CoW at filesystem level.

---

## Testing Strategy

| Milestone | Test                                              |
| --------- | ------------------------------------------------- |
| 1         | Kernel boots, serial output captured              |
| 2         | File persists across reboot                       |
| 3         | vsock ping/pong, file transfer round-trip         |
| 4         | `curl` and `git clone` succeed                    |
| 5         | Checkpoint/restore preserves full state           |

**End-to-End Test:**

```rust
#[test]
fn test_agent_workflow() {
    // Create fresh VM
    let mut vm = Carbon::create("test-agent")?;
    
    // Simulate agent setup (expensive, one-time)
    vm.send(Exec { cmd: "npm init -y && npm install express".into(), ... })?;
    vm.send(Checkpoint { name: "deps-installed".into() })?;
    
    // Simulate multiple agent tasks from same checkpoint
    for i in 0..10 {
        let mut worker = Carbon::restore("test-agent", "deps-installed")?;
        
        worker.send(WriteFile { 
            path: "/workspace/task.json".into(),
            data: format!(r#"{{"task": {}}}"#, i).into(),
        })?;
        
        worker.send(Exec { cmd: "node process-task.js".into(), ... })?;
        
        let result = worker.send(ReadFile { 
            path: "/workspace/result.json".into() 
        })?;
        
        // Each restore is independent
        assert!(result.contains(&format!("task_{}", i)));
        
        worker.destroy()?;
    }
}
```

---

## Open Questions

1. **Memory file format** — Raw dump or sparse? Compression worth the CPU cost?
2. **Checkpoint GC** — Max checkpoints per VM? LRU eviction? User-managed?
3. **Idle detection** — VMM-side heuristics or guest agent reports idle?
4. **Resource limits** — cgroups on VMM? Memory balloon for sleeping VMs?
5. **Base image updates** — Rebuild all VMs? Layered approach?

---

## Storage Requirements

**Host filesystem:** btrfs required for production (instant reflinks).

Falls back to regular copy on ext4/xfs for development and CI, but:
- Clone/checkpoint operations become O(data) instead of O(1)
- CI uses btrfs loopback mount for realistic testing

**Disk space accounting:**
- Base image: ~8GB (shared across all VMs)
- Per-VM overhead: Only modified blocks (typically <1GB for dev workloads)
- Per-checkpoint overhead: Only blocks modified since last checkpoint

Example: 100 VMs from same base, each with 500MB modifications = 8GB + 50GB, not 800GB.

---

## Security Considerations

| Layer               | Mitigation                             |
| ------------------- | -------------------------------------- |
| Guest → Host escape | KVM hardware isolation                 |
| Network abuse       | External iptables, rate limits         |
| Resource exhaustion | cgroups on VMM process                 |
| Disk space          | qcow2 max size, quota per VM           |
| Checkpoint storage  | Quota, encryption at rest              |

---

## References

- [Firecracker design](https://github.com/firecracker-microvm/firecracker/blob/main/docs/design.md)
- [Linux boot protocol](https://www.kernel.org/doc/html/latest/x86/boot.html)
- [virtio spec v1.2](https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html)
- [userfaultfd(2)](https://man7.org/linux/man-pages/man2/userfaultfd.2.html)
- [btrfs reflinks](https://btrfs.readthedocs.io/en/latest/Reflink.html)
- [Fly.io Sprites](https://fly.io/blog/code-and-let-live/)
