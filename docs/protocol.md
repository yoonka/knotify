# Lossless hints and catalog service

The existing `knotify --path ROOT --recursive` JSON interface is unchanged.
Version 1.1 adds two opt-in modes in the same executable:

```text
knotify --protocol etf --path ROOT --recursive
knotify catalog --root ROOT --database STATE/catalog.sqlite3 --exclude STATE
```

Run these as separate processes. Diagnostics go to stderr. Closing stdin exits
either service, including a catalog worker waiting for acknowledgement. Legacy
JSON wrappers can set `KNOTIFY_EXIT_ON_EOF=1` to request the same lifecycle;
standalone legacy behavior is unchanged.

## Framing and paths

Every packet is a four-byte big-endian length followed by an uncompressed ETF
map with binary keys and `"version" => 1`. Maximum packet size is 1 MiB. The
native decoder accepts only bounded maps/lists, binaries, signed 64-bit
integers, booleans and nil. Unsupported versions, compressed terms, arbitrary
atoms and malformed lengths fail closed.

Paths are exact root-relative components joined with `/`: raw filename bytes
on Unix, explicitly encoded WTF-8 on Windows. Do not lowercase, normalize,
decode as display text, or use Elixir string/filesystem helpers on these paths.
Native resolution rejects traversal, aliases and Windows alternate streams.
Windows creation time is not change time. Metadata uses size, modification and
change timestamps in Unix nanoseconds, plus the native file ID as a binary.
Metadata equality does not prove byte equality when a writer preserves it.

## Watcher

Only one root is allowed in ETF hint mode. The service emits:

| Type | Fields |
| --- | --- |
| `ready` | `service: "watcher"` |
| `paths` | `batch_id`, `paths: [binary]` |
| `rescan` | `batch_id`, `reason` |
| `error` | `code`, `message` |

Send `{"version": 1, "type": "credit", "count": 4}` as ETF after subscribing.
Each `paths` or `rescan` packet spends one credit. Return one credit only after
accepting that batch into bounded intake. At most four may be outstanding.
Registration errors precede termination. Errors and notify rescan flags require
a whole-root rescan, regardless of any path attached to the original event.
The callback has a 4,096-event nonblocking channel and the coalescer has a
4,096-path limit. Either overflow retains a rescan requirement. Access-only
events are omitted; a rescan flag is processed before filtering. Renames and
delete/create pairs are path hints, never inferred final actions.

## Catalog

The service enrolls root identity, obtains an exclusive adjacent `.lock` file,
initializes schema version 1 and sends `ready` with `service: "catalog"` and
`root_id`. Writable roots receive `.aaryx-root`; read-only roots use native
identity alone. Existing identity is never silently replaced.

Requests contain `version`, integer `id` and `op`:

| Operation | Additional fields |
| --- | --- |
| `reconcile_dir` | `path` (empty binary means root) |
| `reconcile_paths` | `paths` (at most 256) |
| `check_root` | none |
| `status` | none |
| `ack` / `cancel` | the active request ID |

Responses are `batch` (`id`, `items`), `done` (`id`), or `error` (`id`, `code`,
`message`). Batches contain at most 128 items and approximately 256 KiB. Each
batch must be acknowledged before that request emits another; acknowledgement
expires after 60 seconds. Two workers service requests, with a hard four-request
native limit; Aaryx limits itself to two.

Items are `directory` (`path`), `change` (`path`, `op`, `stat`) or `unknown`
(`path`, `error`). Change operations are `upsert`, `delete`, `delete_subtree`.
Observed metadata is advisory. Unknown entries and failed enumeration never
prove absence. Native root identity is checked before and after reconciliation
and before emitting batches. Own state, `.aaryx-*`, `.zfs`, links/reparse points
and other filesystem volumes are excluded.

The catalog contains `root_info`, `dirs` and `files`; `files` is keyed by binary
directory/name and has a unique root-local integer `file_id`, size/timestamps,
optional binary `ino`, and `content_status` (0 indexed, 1 permanently failed,
2 stale, 3 racy). Scans never advance `dirs` or `files`. A future content
committer must extend this owner with transactional writes after its content
stores are durable; do not add an independent competing catalog writer. All
downstream IDs and audits must also be scoped by `root_id`.

## Checks

`cargo test --locked` covers catalog equality, replacement/deletion, identity
loss, hard links, exclusions, byte paths, coalescer overflow, recreate hints,
acknowledged batches and EOF. Unix tests also cover links and permission errors.
CI includes Linux, Windows and a FreeBSD 15.0 ZFS temporary dataset.
