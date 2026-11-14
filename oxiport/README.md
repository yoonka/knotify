# oxiport

Runs SPARQL queries and updates against an
[Oxigraph](https://github.com/oxigraph/oxigraph) RDF store, for use as an
Erlang or Elixir port.

## Building

```sh
cargo build --release
```

Oxigraph's RocksDB storage is compiled from source, and its Rust bindings
are generated with bindgen, so the build needs a C++ compiler and libclang.
If libclang is not found, point `LIBCLANG_PATH` at the directory that holds
it (on FreeBSD, `/usr/local/llvm<version>/lib` from devel/llvm). The
default `geosparql` feature adds the GeoSPARQL functions to the interactive
mode; `--no-default-features` leaves them out.

## Interactive mode

```sh
oxiport --location data/
```

Opens the store in the directory, or an in-memory store without
`--location`, and reads one command per line: `query <SPARQL>`,
`update <SPARQL>`, `clear` or `exit`.

- `--quiet` drops the prompt and messages.
- `--results-format` and `--results-file` choose the format and file of
  query results.
- `--query-base` and `--update-base` set the base IRI.
- `--union-default-graph` makes the default graph the union of all graphs.

## Protocol mode

```sh
oxiport --jsonl --location data/
```

The request protocol is described in the
[repository README](../README.md#the---jsonl-protocol). Operations:

| Operation | Payload | Data in the reply |
|---|---|---|
| `health` | none | `{"protocol":1,"operations":["query","update"],"durable":true}` |
| `query` | `{"sparql":"..."}` | SPARQL 1.1 JSON results of a SELECT or ASK query, at most 1000 solutions |
| `update` | `{"sparql":"..."}` | `{"committed":true}`, after the store is flushed to disk |

Other query forms (CONSTRUCT, DESCRIBE) fail with
`select_or_ask_required`, and more than 1000 solutions with
`result_limit`.

## License

BSD 3-Clause License, see [LICENSE](../LICENSE).
