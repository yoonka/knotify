# knotify

A cross-platform file system watcher utility built with Rust that monitors file and directory changes in real-time.

## Overview

knotify is a command-line tool that watches specified files and directories for changes and reports events in a structured format. It leverages the `notify` crate to provide efficient, cross-platform file system monitoring with support for various backends (inotify on Linux, kqueue on macOS/BSD).

## Features

- **Multi-path watching**: Monitor multiple files and directories simultaneously
- **Recursive monitoring**: Watch entire directory trees recursively
- **JSON output**: Structured event output in JSON format
- **Event filtering**: Filter events by type (create, modify, remove, rename)
- **Smart event handling**: Automatic filtering of metadata and access events
- **Cross-platform**: Works on Linux, macOS, Windows, and BSD systems
- **Real-time monitoring**: Instant notification of file system changes

## Installation

### Prerequisites

- Rust 1.70 or later
- Cargo (comes with Rust)

### Building from source

```bash
# Clone the repository
git clone 

# Build the project
cargo build --release

# The binary will be available at ./target/release/knotify
```

### Install globally

```bash
cargo install --path .
```

## Usage

### Basic Syntax

```bash
knotify --path <PATH> [OPTIONS]
```

### Command-line Arguments

| Argument | Description | Required | Default |
|----------|-------------|----------|---------|
| `--path` | One or more paths to watch (can be specified multiple times) | Yes | - |
| `--recursive` | Watch directories recursively | No | `false` |
| `--backend` | Backend to use: `auto`, `inotify`, or `kqueue` | No | `auto` |
| `--json` | Output events in JSON format | No | `true` |
| `--debounce` | Debounce delay in milliseconds | No | `100` |
| `--log` | Log level: `error`, `warn`, `info`, `debug`, `trace` | No | `info` |
| `--once` | Exit after first event (useful for testing) | No | `false` |
| `--filter` | Filter events by type (comma-separated): `create`, `modify`, `remove`, `rename` | No | All events |

### Examples

#### Watch a single directory

```bash
knotify --path /path/to/directory
```

#### Watch multiple paths

```bash
knotify --path /path/to/dir1 --path /path/to/dir2 --path /path/to/file.txt
```

#### Watch recursively

```bash
knotify --path /path/to/directory --recursive
```

#### Watch with specific backend (Linux)

```bash
knotify --path /path/to/directory --backend inotify
```

#### Watch with specific backend (macOS/BSD)

```bash
knotify --path /path/to/directory --backend kqueue
```

#### Non-JSON output (debug mode)

```bash
knotify --path /path/to/directory --json false
```

#### Watch and exit after first event

```bash
knotify --path /path/to/directory --once
```

#### Filter for specific event types

Watch only for file creation events:
```bash
knotify --path /path/to/directory --filter create
```

Watch for create and modify events only:
```bash
knotify --path /path/to/directory --filter create,modify
```

Watch for file deletions only:
```bash
knotify --path /path/to/directory --filter remove
```

Watch for rename events only:
```bash
knotify --path /path/to/directory --filter rename
```

## Event Types

knotify reports the following event types in JSON format:

### Create Events
- `CREATE_FILE` - A file was created
- `CREATE_FOLDER` - A directory was created
- `CREATE` - Generic creation event

### Modify Events
- `MODIFY_CONTENT` - File content was modified
- `MODIFY_SIZE` - File size changed
- `MODIFY_DATA` - Generic data modification
- `MODIFY` - Generic modification event

### Rename Events
- `RENAME_FROM` - Source of a rename operation
- `RENAME_TO` - Destination of a rename operation
- `RENAME_BOTH` - Complete rename event
- `RENAME` - Generic rename event

### Remove Events
- `REMOVE_FILE` - A file was deleted
- `REMOVE_FOLDER` - A directory was deleted
- `REMOVE` - Generic removal event

### Output Format

Events are output as JSON objects with the following structure:

```json
{
  "kind": "CREATE_FILE",
  "paths": ["/path/to/file.txt"]
}
```

```json
{
  "kind": "MODIFY_CONTENT",
  "paths": ["/path/to/modified-file.txt"]
}
```

```json
{
  "kind": "REMOVE_FILE",
  "paths": ["/path/to/deleted-file.txt"]
}
```

## Project Structure

```
.
├── Cargo.toml          # Project manifest and dependencies
├── src/
│   ├── main.rs         # Application entry point
│   ├── config.rs       # CLI argument parsing and configuration
│   └── watcher.rs      # File system watching logic and event handling
```

### Module Overview

- **main.rs** (`src/main.rs:1`): Entry point that parses CLI arguments and starts the watcher
- **config.rs** (`src/config.rs:1`): Defines CLI arguments using `clap` and converts them to internal configuration
- **watcher.rs** (`src/watcher.rs:1`): Implements the `Watchman` struct that handles file system events and JSON output

## How It Works

1. **Initialization**: The application parses command-line arguments using `clap`
2. **Configuration**: Arguments are converted to an internal `Config` structure
3. **Watcher Setup**: A `notify` watcher is created with the recommended backend for the platform
4. **Event Loop**: The watcher monitors specified paths and sends events through a channel
5. **Event Processing**: Events are filtered, categorized, and output as JSON
6. **Cleanup**: Smart filtering ignores metadata and access events to reduce noise

### Event Handling Details

The watcher implements intelligent event handling:
- **Metadata events** are filtered out to reduce noise
- **Access events** are ignored
- **Rename events** are analyzed to detect deletions disguised as renames on macOS
- **File existence** is checked to distinguish between actual renames and deletions

## Dependencies

- **clap** (v4.5.48): Command-line argument parsing
- **notify** (v8.2.0): Cross-platform file system notification library
- **serde_json** (v1.0.145): JSON serialization and deserialization

## Platform Support

- **Linux**: Uses inotify backend
- **macOS**: Uses kqueue/FSEvents backend
- **Windows**: Uses ReadDirectoryChangesW backend
- **BSD**: Uses kqueue backend

## Development

### Running in development mode

```bash
cargo run -- --path /path/to/watch
```

### Running tests

```bash
cargo test
```

### Building for release

```bash
cargo build --release
```

### Cross-compilation

knotify supports cross-compilation for multiple platforms using the `cross` tool.

#### Prerequisites

Install the `cross` tool:

```bash
cargo install cross --git https://github.com/cross-rs/cross
```

Or use the provided Makefile:

```bash
make install-cross
```

#### Building for all platforms

Use the provided build script to build for all supported platforms:

```bash
./build-all.sh
```

Or use the Makefile:

```bash
make build-all
```

This will create binaries for:
- Linux (x86_64 and ARM64, both glibc and musl)
- Windows (x86_64)
- macOS (x86_64 and ARM64)
- FreeBSD (x86_64)

Binaries will be placed in `target/release-builds/` with platform-specific names.

#### Building for specific platforms

Use the Makefile targets:

```bash
make build-linux     # Linux targets only
make build-windows   # Windows targets only
make build-macos     # macOS targets only
make build-freebsd   # FreeBSD targets only
```

Or build individual targets using `cross`:

```bash
# Linux (glibc)
cross build --release --target x86_64-unknown-linux-gnu

# Linux (musl - statically linked)
cross build --release --target x86_64-unknown-linux-musl

# Linux ARM64
cross build --release --target aarch64-unknown-linux-gnu

# Windows
cross build --release --target x86_64-pc-windows-gnu

# FreeBSD
cross build --release --target x86_64-unknown-freebsd

# macOS (native build on macOS)
cargo build --release --target x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
```

#### Supported targets

| Platform | Target Triple | Notes |
|----------|--------------|-------|
| Linux x86_64 (glibc) | `x86_64-unknown-linux-gnu` | Standard Linux |
| Linux x86_64 (musl) | `x86_64-unknown-linux-musl` | Statically linked, portable |
| Linux ARM64 (glibc) | `aarch64-unknown-linux-gnu` | For ARM servers/devices |
| Linux ARM64 (musl) | `aarch64-unknown-linux-musl` | ARM, statically linked |
| Windows x86_64 | `x86_64-pc-windows-gnu` | Windows 64-bit |
| macOS x86_64 | `x86_64-apple-darwin` | Intel Macs |
| macOS ARM64 | `aarch64-apple-darwin` | Apple Silicon (M1/M2/M3) |
| FreeBSD x86_64 | `x86_64-unknown-freebsd` | FreeBSD systems |

## Use Cases

- **Development tools**: Watch source files and trigger builds/tests on modifications
- **Deployment automation**: Monitor directories for new files to process (filter: `--filter create`)
- **Log monitoring**: Watch log directories for new entries (filter: `--filter create,modify`)
- **Backup systems**: Trigger backups when files change (filter: `--filter create,modify`)
- **Security monitoring**: Track file deletions for security purposes (filter: `--filter remove`)
- **Content management**: Monitor media directories for new uploads (filter: `--filter create`)
- **File synchronization**: Detect renames and moves (filter: `--filter rename`)

## Troubleshooting

### Permission Issues

Ensure you have read permissions for all directories you want to watch:

```bash
# Check permissions
ls -la /path/to/watch

# Run with appropriate permissions if needed
sudo knotify --path /path/to/protected/directory
```

### Too Many Open Files

On Linux/macOS, you may hit file descriptor limits when watching many directories recursively:

```bash
# Increase the limit temporarily
ulimit -n 4096

# Or set it permanently in /etc/security/limits.conf
```

### High CPU Usage

If experiencing high CPU usage:
- Increase the `--debounce` value to reduce event frequency
- Consider using `--recursive false` if you don't need recursive watching
- Filter paths to watch only what's necessary

## License

[Add your license information here]

## Contributing

[Add contribution guidelines here]

## Authors

[Add author information here]
