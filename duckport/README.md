# duckport

Runs SQL against a [DuckDB](https://duckdb.org) database file. It reads
requests on standard input and writes JSON on standard output, so that an
Erlang or Elixir application can run it as a port.

## Building

```sh
cargo build --release
```

DuckDB is compiled from source (the duckdb crate's `bundled` feature),
which needs a C++ compiler and takes a while.

## Line mode

```sh
duckport --database analytics.duckdb
```

Each line on standard input is one SQL statement, executed once. Each
answer is one line, `{"rows":[{"column":value,...},...]}` or
`{"error":"message"}`. The database file is created if it does not exist.

## Protocol mode

```sh
duckport --jsonl --database analytics.duckdb
```

The request protocol is described in the
[repository README](../README.md#the---jsonl-protocol). Operations:

| Operation | Payload | Data in the reply |
|---|---|---|
| `health` | none | `{"protocol":1,"operations":["query","batch"],"durable":true}` |
| `query` | `{"sql":"...","params":[...]}` | `{"rows":[...]}`, at most 1000 rows |
| `batch` | `{"statements":[{"sql":"...","params":[...]},...]}` | `{"committed":true}`, after all statements ran in one transaction |

- `params` is required (it may be empty) and binds the statement's `?`
  placeholders. Parameters are JSON null, booleans, strings and numbers.
- Result columns may be NULL, BOOLEAN, TINYINT, SMALLINT, INTEGER, BIGINT,
  FLOAT, DOUBLE or VARCHAR. BIGINT values come back as strings, so that
  64-bit values survive JSON. Any other column type fails the query with
  `unsupported_result_type`; cast it in SQL.
- More than 1000 rows fail the query with `result_limit`.

## License

BSD 3-Clause License, see [LICENSE](../LICENSE).
