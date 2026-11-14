# beamports

Programs that Erlang and Elixir applications run as
[ports](https://www.erlang.org/doc/system/ports.html), and Elixir clients
for them. Each program is a Rust project of its own, with its own
`Cargo.lock`, so each one builds, updates its dependencies and is packaged
independently of the others.

| Directory | What it is |
|---|---|
| [`knotify/`](knotify/) | File system watcher (Rust's notify crate) reporting changes as JSON or Erlang terms |
| [`knotify_ex/`](knotify_ex/) | Elixir client for knotify, the Hex package [knotify_ex](https://hex.pm/packages/knotify_ex) |
| [`duckport/`](duckport/) | SQL on a [DuckDB](https://duckdb.org) database |
| [`oxiport/`](oxiport/) | SPARQL on an [Oxigraph](https://github.com/oxigraph/oxigraph) RDF store |
| [`tantiport/`](tantiport/) | Full-text search with [Tantivy](https://github.com/quickwit-oss/tantivy) |

## Building

Build a program in its own directory, for example:

```sh
cd duckport
cargo build --release
```

The binary is then in that directory's `target/release/`. Each program's
README lists what its build needs besides Rust.

## The `--jsonl` protocol

duckport, oxiport and tantiport started with `--jsonl` share one request
protocol on standard input and output: one JSON object per line in each
direction, at most 16 MiB per line, answered one at a time in the order
received.

```json
{"version":1,"request_id":7,"operation":"query","payload":{"sql":"SELECT 42 AS answer","params":[]}}
```

```json
{"data":{"rows":[{"answer":42}]},"ok":true,"request_id":7,"version":1}
{"error":"missing_sql","ok":false,"request_id":7,"version":1}
```

- `request_id` is copied into the reply, so a client can match replies to
  requests.
- A request with a `version` other than 1 is answered with the error
  `incompatible_protocol`.
- Every program answers `health` with the protocol version and its list of
  operations.
- A reply reports success only once the change is durable: committed by
  DuckDB or Tantivy, or flushed by Oxigraph.
- A line that is not JSON ends the program.

Each program's README lists its operations. knotify has a protocol of its
own, described in [`knotify/docs/protocol.md`](knotify/docs/protocol.md).

## License

BSD 3-Clause License, see [LICENSE](LICENSE). It covers everything in this
repository.
