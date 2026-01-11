#!/bin/bash
#
# Carbon Kernel Builder
# Builds ultra-minimal x86_64 kernel for fast microVM boot
#
# Usage: ./scripts/build-kernel.sh [options]
#
# Options:
#   --version VERSION   Kernel version (default: 6.6.70)
#   --clean             Clean build (re-extract source)
#   --native            Force native build (skip Docker)
#
# On macOS, automatically uses Docker with OrbStack.
# Kernel source is cached in .cache/kernel/ for fast iteration.
#

set -euo pipefail

# Defaults
KERNEL_VERSION="6.6.70"
CLEAN_BUILD=false
FORCE_NATIVE=false

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --version|-v) KERNEL_VERSION="$2"; shift 2 ;;
        --clean|-c) CLEAN_BUILD=true; shift ;;
        --native|-n) FORCE_NATIVE=true; shift ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

KERNEL_MAJOR="${KERNEL_VERSION%%.*}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
CACHE_DIR="${PROJECT_DIR}/.cache/kernel"
OUTPUT_DIR="${PROJECT_DIR}/bin"

echo "=== Carbon Kernel Builder ==="
echo "Version: ${KERNEL_VERSION}"
echo "Cache: ${CACHE_DIR}"
echo ""

# Detect if we need Docker (macOS or explicit)
USE_DOCKER=false
if [[ "$(uname)" == "Darwin" ]] && [[ "$FORCE_NATIVE" == "false" ]]; then
    USE_DOCKER=true
fi

if [[ "$USE_DOCKER" == "true" ]]; then
    echo "Building in Docker (x86_64)..."

    # Create cache directory on host
    mkdir -p "${CACHE_DIR}"
    mkdir -p "${OUTPUT_DIR}"

    # Build Docker image
    docker build --platform linux/amd64 \
        -t carbon-kernel-builder \
        -f "${SCRIPT_DIR}/Dockerfile.kernel" \
        "${SCRIPT_DIR}"

    # Run build with cache mounted
    docker run --rm \
        --platform linux/amd64 \
        -v "${CACHE_DIR}:/cache" \
        -v "${OUTPUT_DIR}:/output" \
        -e KERNEL_VERSION="${KERNEL_VERSION}" \
        -e CLEAN_BUILD="${CLEAN_BUILD}" \
        carbon-kernel-builder

    echo ""
    ls -lh "${OUTPUT_DIR}/vmlinuz"
    exit 0
fi

# Native Linux build
echo "Building natively..."

mkdir -p "${CACHE_DIR}"
mkdir -p "${OUTPUT_DIR}"

KERNEL_TARBALL="${CACHE_DIR}/linux-${KERNEL_VERSION}.tar.xz"
KERNEL_DIR="${CACHE_DIR}/linux-${KERNEL_VERSION}"

# Download kernel if needed
if [[ ! -f "${KERNEL_TARBALL}" ]]; then
    echo "Downloading Linux ${KERNEL_VERSION}..."
    curl -L -o "${KERNEL_TARBALL}" \
        "https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_MAJOR}.x/linux-${KERNEL_VERSION}.tar.xz"
fi

# Extract if needed or clean requested
if [[ ! -d "${KERNEL_DIR}" ]] || [[ "$CLEAN_BUILD" == "true" ]]; then
    echo "Extracting kernel source..."
    rm -rf "${KERNEL_DIR}"
    tar -xf "${KERNEL_TARBALL}" -C "${CACHE_DIR}"
fi

cd "${KERNEL_DIR}"

# Configure and build
source "${SCRIPT_DIR}/kernel-config.sh"

echo ""
echo "Building bzImage..."
make ARCH=x86_64 -j"$(nproc)" bzImage

# Copy output
if [[ -f arch/x86/boot/bzImage ]]; then
    cp arch/x86/boot/bzImage "${OUTPUT_DIR}/vmlinuz"
    echo ""
    echo "=== Success ==="
    ls -lh "${OUTPUT_DIR}/vmlinuz"
else
    echo "Build failed"
    exit 1
fi
