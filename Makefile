.PHONY: build release check lint run test test-boot disk clean kernel-carbon

# Default paths - use bundled minimal kernel
KERNEL ?= bin/vmlinuz
DISK ?= disk.raw
MEMORY ?= 128
CMDLINE ?= console=ttyS0

# Build debug
build:
	cargo build

# Build release
release:
	cargo build --release

# Type check
check:
	cargo check

# Clippy lint
lint:
	cargo clippy -- -D warnings

# Format check
fmt:
	cargo fmt --check

# Run the VMM
run: build
	cargo run -- --kernel $(KERNEL) --memory $(MEMORY) --cmdline "$(CMDLINE)"

# Run with disk
run-disk: build disk
	cargo run -- --kernel $(KERNEL) --memory $(MEMORY) --disk $(DISK) --cmdline "$(CMDLINE)"

# Run release build
run-release: release
	cargo run --release -- --kernel $(KERNEL) --memory $(MEMORY) --cmdline "$(CMDLINE)"

# Run tests
test:
	cargo test

# Boot test - verify kernel boots with serial output and virtio-blk
test-boot: build disk
	@echo "=== Boot Test ($(KERNEL)) ==="
	timeout 15s cargo run -- --kernel $(KERNEL) --memory $(MEMORY) --disk $(DISK) --cmdline "$(CMDLINE)" 2>&1 | tee /tmp/boot.log || true
	@echo ""
	@grep -q "Linux version" /tmp/boot.log && echo "PASS: kernel booted" || (echo "FAIL: no kernel output"; exit 1)
	@grep -q "virtio_blk\|virtio-blk" /tmp/boot.log && echo "PASS: virtio-blk detected" || (echo "FAIL: virtio-blk not detected"; exit 1)

# Build minimal Carbon kernel (uses Docker on macOS, cached)
kernel-carbon:
	./scripts/build-kernel.sh

# Rebuild kernel from scratch
kernel-carbon-clean:
	./scripts/build-kernel.sh --clean

# Create test disk image
disk:
	@if [ ! -f $(DISK) ]; then \
		echo "Creating test disk image..."; \
		dd if=/dev/zero of=$(DISK) bs=1M count=64 2>/dev/null; \
		echo "Created $(DISK) (64MB)"; \
	else \
		echo "Disk image $(DISK) already exists"; \
	fi

# Clean build artifacts
clean:
	cargo clean
	rm -f disk.raw
