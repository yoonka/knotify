# Makefile for cross-compilation

.PHONY: all clean install-cross build-linux build-windows build-freebsd build-macos help

# Default target
all: build-all

help:
	@echo "Available targets:"
	@echo "  make build-all       - Build for all platforms"
	@echo "  make build-linux     - Build for Linux (x86_64 and ARM64)"
	@echo "  make build-windows   - Build for Windows (x86_64)"
	@echo "  make build-freebsd   - Build for FreeBSD (x86_64)"
	@echo "  make build-macos     - Build for macOS (x86_64 and ARM64)"
	@echo "  make install-cross   - Install the cross tool"
	@echo "  make clean           - Clean build artifacts"

install-cross:
	@echo "Installing cross..."
	cargo install cross --git https://github.com/cross-rs/cross

build-all:
	@./build-all.sh

build-linux:
	@echo "Building for Linux targets..."
	cross build --release --target x86_64-unknown-linux-gnu
	cross build --release --target x86_64-unknown-linux-musl
	cross build --release --target aarch64-unknown-linux-gnu
	cross build --release --target aarch64-unknown-linux-musl

build-windows:
	@echo "Building for Windows..."
	cross build --release --target x86_64-pc-windows-gnu

build-freebsd:
	@echo "Building for FreeBSD..."
	cross build --release --target x86_64-unknown-freebsd

build-macos:
	@echo "Building for macOS..."
	cargo build --release --target x86_64-apple-darwin
	cargo build --release --target aarch64-apple-darwin

clean:
	cargo clean
	rm -rf target/release-builds
