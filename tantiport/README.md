# tantiport

Full-text search with [Tantivy](https://github.com/quickwit-oss/tantivy),
for use as an Erlang or Elixir port.

## Building

```sh
cargo build --release
```

## Demo mode

```sh
tantiport --index index/
```

Each line on standard input is `{"query":"..."}`, and each answer is
`{"results":[...]}` with up to 10 documents. A new, empty index is seeded
with three demonstration documents; without `--index` a temporary index is
used.

## Protocol mode

```sh
tantiport --jsonl --index index/
```

The request protocol is described in the
[repository README](../README.md#the---jsonl-protocol). A document has an
`id`, a `hierarchy` (an exact-match key that groups documents, such as a
collection), a `title` and a `text`, which are searched, and `metadata`
(a JSON object), which is stored and returned. Operations:

| Operation | Payload | Data in the reply |
|---|---|---|
| `health` | none | `{"protocol":1,"operations":["upsert","delete","search","list","get"],"durable":true}` |
| `upsert` | `{"id":"...","hierarchy":"...","title":"...","text":"...","metadata":{...}}` | `{"committed":true}`; replaces any document with the same `id` |
| `delete` | `{"id":"..."}` | `{"committed":true}` |
| `search` | `{"query":"...","hierarchy":"...","limit":N,"offset":N}` | `{"results":[...]}` |
| `list` | `{"hierarchy":"...","limit":N,"offset":N}` | `{"results":[...]}` |
| `get` | `{"id":"...","hierarchy":"..."}` | `{"results":[...]}` |

- Every upsert and delete is committed before the reply.
- `search` looks for the query text as one phrase in the title and text.
  The text may be up to 1024 bytes.
- `hierarchy` is optional for `search`, `list` and `get`, and limits the
  results to that hierarchy. `limit` is 1 to 100 (default 100), `offset`
  defaults to 0.
- Each result is the document's `metadata` with its `id` added.

## License

BSD 3-Clause License, see [LICENSE](../LICENSE).
