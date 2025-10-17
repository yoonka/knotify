#!/bin/bash

# Cross-compilation build script for knotify
# Builds binaries for multiple platforms using cross

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}Building knotify for multiple platforms...${NC}\n"

# Check if cross is installed
if ! command -v cross &> /dev/null; then
    echo -e "${YELLOW}cross is not installed. Installing...${NC}"
    cargo install cross --git https://github.com/cross-rs/cross
fi

# Create output directory
OUTPUT_DIR="target/release-builds"
mkdir -p "$OUTPUT_DIR"

# Define targets (compatible with bash 3.2)
TARGETS=(
    "x86_64-unknown-linux-gnu:Linux (x86_64, glibc)"
    "x86_64-unknown-linux-musl:Linux (x86_64, musl)"
    "aarch64-unknown-linux-gnu:Linux (ARM64, glibc)"
    "aarch64-unknown-linux-musl:Linux (ARM64, musl)"
    "x86_64-pc-windows-gnu:Windows (x86_64)"
    "x86_64-unknown-freebsd:FreeBSD (x86_64)"
    "x86_64-apple-darwin:macOS (x86_64)"
    "aarch64-apple-darwin:macOS (ARM64)"
)

# Build for each target
for target_desc in "${TARGETS[@]}"; do
    target="${target_desc%%:*}"
    description="${target_desc#*:}"
    echo -e "${YELLOW}Building for ${description} ($target)...${NC}"

    # Use cross for non-macOS targets, cargo for macOS when on macOS
    if [[ "$target" == *"apple-darwin"* ]] && [[ "$OSTYPE" == "darwin"* ]]; then
        # Native macOS build
        cargo build --release --target "$target" || {
            echo -e "${RED}Failed to build for $target${NC}"
            continue
        }
    else
        # Cross-compilation
        cross build --release --target "$target" || {
            echo -e "${RED}Failed to build for $target${NC}"
            continue
        }
    fi

    # Copy binary to output directory
    BINARY_NAME="knotify"
    if [[ "$target" == *"windows"* ]]; then
        BINARY_NAME="knotify.exe"
    fi

    if [ -f "target/$target/release/$BINARY_NAME" ]; then
        OUTPUT_NAME="${BINARY_NAME%.*}-$target"
        if [[ "$target" == *"windows"* ]]; then
            OUTPUT_NAME="$OUTPUT_NAME.exe"
        fi
        cp "target/$target/release/$BINARY_NAME" "$OUTPUT_DIR/$OUTPUT_NAME"
        echo -e "${GREEN}✓ Built successfully: $OUTPUT_DIR/$OUTPUT_NAME${NC}\n"
    fi
done

echo -e "${GREEN}Build complete! Binaries are in $OUTPUT_DIR${NC}"
ls -lh "$OUTPUT_DIR"
