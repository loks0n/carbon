# Contributing to Carbon

Carbon requires x86_64 Linux with KVM for runtime. Building works on any x86_64 Linux.

## Development on macOS

Use OrbStack to run an emulated x86_64 Linux VM.

### Setup

```bash
# Install OrbStack: https://orbstack.dev

# Create VM (from repo root)
orb create --arch amd64 debian carbon-dev -c dev/cloud-init.yml

# Install Rust
orb -m carbon-dev bash -c "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"
```

### Workflow

Your macOS filesystem is mounted at the same paths inside the VM:

```bash
orb -m carbon-dev       # Enter VM
cd /path/to/carbon      # Same path as macOS
make build              # Build
```

The emulated VM has no KVM, so `make run` won't work. Use GitHub Actions for integration testing.

## Development on x86_64 Linux

```bash
# Install dependencies (Debian/Ubuntu)
sudo apt-get install -y build-essential curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build and run
make build
make kernel   # Copy kernel from /boot
make run      # Requires /dev/kvm
```

## Makefile

| Target         | Description            |
| -------------- | ---------------------- |
| `make build`   | Debug build            |
| `make release` | Release build          |
| `make check`   | Type check             |
| `make lint`    | Clippy lints           |
| `make fmt`     | Format check           |
| `make test`    | Run tests              |
| `make kernel`  | Copy kernel from /boot |
| `make run`     | Build and run VMM      |
| `make clean`   | Clean artifacts        |

## IDE

The project includes `.zed/settings.json` for rust-analyzer to target x86_64 Linux, enabling IDE features on macOS.

For VS Code, add to `.vscode/settings.json`:

```json
{
  "rust-analyzer.cargo.target": "x86_64-unknown-linux-gnu"
}
```

## CI

GitHub Actions runs on x86_64 Linux with KVM. Push to trigger:

- Check, lint, format
- Release build
- Boot test with real kernel
