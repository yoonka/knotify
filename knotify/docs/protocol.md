# Lossless filesystem hints

The legacy watcher CLI and JSON defaults remain available. Opt into versioned ETF hints with:

```text
knotify --protocol etf --path ROOT --recursive
```

The protocol is independent of how callers store or process their files. It performs no database operations.

## Framing and paths

Packets have a four-byte big-endian length followed by an uncompressed ETF map with binary keys and "version" => 1. The maximum packet size is 1 MiB. Unsupported versions, compressed terms and malformed packets fail closed.

Paths are lossless root-relative bytes joined with /: raw Unix bytes and WTF-8 on Windows. Do not lowercase or normalize them.

## Messages and flow control

- ready: service=watcher. Emitted after registration.
- paths: batch_id and a list of relative paths.
- rescan: batch_id, reason, and optional paths for scoped errors. Unscoped errors, pathless Any/Other events, notify rescan flags and overflow request a whole-root rescan.
- error: code and message, including registration failures.

After subscribing, grant up to four credits with an ETF map containing version=1, type=credit, count=4. Return a credit only after accepting the corresponding batch. A bounded 4,096-event callback channel and a bounded coalescer retain a rescan requirement on overflow. Access-only noise is dropped; rename endpoints and meaningful metadata changes remain hints.

Closing stdin stops hint mode. Legacy wrappers may set KNOTIFY_EXIT_ON_EOF=1 for the same lifecycle without changing standalone defaults.
