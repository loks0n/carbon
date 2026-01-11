#!/bin/bash
#
# Docker entrypoint for kernel builds
# Uses /cache for kernel source (mounted from host)
# Outputs to /output (mounted from host)
#

set -euo pipefail

KERNEL_VERSION="${KERNEL_VERSION:-6.6.70}"
KERNEL_MAJOR="${KERNEL_VERSION%%.*}"
CLEAN_BUILD="${CLEAN_BUILD:-false}"
JOBS="$(nproc)"

echo "=== Carbon Kernel Builder (Docker) ==="
echo "Version: ${KERNEL_VERSION}"
echo "CPUs: ${JOBS}"
echo ""

KERNEL_TARBALL="/cache/linux-${KERNEL_VERSION}.tar.xz"
KERNEL_DIR="/cache/linux-${KERNEL_VERSION}"

# Download kernel if not cached
if [[ ! -f "${KERNEL_TARBALL}" ]]; then
    echo "Downloading Linux ${KERNEL_VERSION}..."
    curl -L -o "${KERNEL_TARBALL}" \
        "https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_MAJOR}.x/linux-${KERNEL_VERSION}.tar.xz"
else
    echo "Using cached kernel source"
fi

# Extract if needed or clean build requested
if [[ ! -d "${KERNEL_DIR}" ]] || [[ "$CLEAN_BUILD" == "true" ]]; then
    echo "Extracting kernel source..."
    rm -rf "${KERNEL_DIR}"
    tar -xf "${KERNEL_TARBALL}" -C /cache
fi

cd "${KERNEL_DIR}"

# Clean previous build artifacts (needed when config changes)
make mrproper 2>/dev/null || true

# Apply configuration
source /build/kernel-config.sh

# Build
echo ""
echo "Building bzImage..."
if ! make ARCH=x86_64 -j"${JOBS}" bzImage 2>&1; then
    echo ""
    echo "=== Build failed! ==="
    exit 1
fi

# Copy output
if [[ -f arch/x86/boot/bzImage ]]; then
    cp arch/x86/boot/bzImage /output/vmlinuz
    SIZE=$(ls -lh arch/x86/boot/bzImage | awk '{print $5}')
    echo ""
    echo "=== Success: ${SIZE} ==="
else
    echo "Build failed"
    exit 1
fi
