# Lossless hints and catalog service

The existing `knotify --path ROOT --recursive` JSON interface is unchanged.
Version 1.1 adds two opt-in modes in the same executable:

```text
knotify --protocol etf --path ROOT --recursive
knotify catalog --root ROOT --database STATE/catalog.sqlite3 --exclude STATE
knotify check-roots --root ROOT --root OTHER_ROOT
```

Run these as separate processes. Diagnostics go to stderr. Closing stdin exits
either service, including a catalog worker waiting for acknowledgement. Legacy
JSON wrappers can set `KNOTIFY_EXIT_ON_EOF=1` to request the same lifecycle;
standalone legacy behavior is unchanged.

`check-roots` is a short-lived enrollment preflight. It resolves ancestor
aliases and compares native root/ancestor identities, returning one framed
`roots_checked` packet. It contains boolean `overlap`, `overlaps` (pairs of
zero-based input indexes), and `unavailable` (maps with `index` and `message`).
Missing, inaccessible and non-directory roots get root-local errors; other
roots are still inspected for overlaps. It writes no markers or catalogs.
Root configuration is revalidated before starting producer subtrees and at
each manager refresh. Only affected roots are stopped or deferred. If the
validator itself fails, unchanged active roots keep scanning under their own
catalog identity checks; new enrollment waits for a successful preflight.

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
| `rescan` | `batch_id`, `reason`, optional `paths: [binary]` for scoped watch errors |
| `error` | `code`, `message` |

Send `{"version": 1, "type": "credit", "count": 4}` as ETF after subscribing.
Each `paths` or `rescan` packet spends one credit. Return one credit only after
accepting that batch into bounded intake. At most four may be outstanding.
Registration errors precede termination. A watch error with known paths requests
those subtrees. Unscoped errors, overflow and notify rescan flags require a
whole-root rescan, regardless of any path attached to the flagged event.
The callback has a 4,096-event nonblocking channel and the coalescer has a
4,096-path limit. Either overflow retains a rescan requirement. Access-only
events are omitted before the callback channel; a rescan flag survives this
filtering. Renames and
delete/create pairs are path hints, never inferred final actions.
An `Any` or `Other` event without paths also requires a whole-root rescan,
even when notify has not set its explicit rescan flag.

## Catalog

The service enrolls root identity, obtains an exclusive adjacent `.lock` file,
initializes schema version 2 and sends `ready` with `service: "catalog"` and
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

Reconciliation requests may include `policy: {rules: [{path: binary,
exclude: boolean | nil}], default_exclusions: [binary]}`. The nearest explicit
exclusion rule wins; nil inherits. Default name patterns support a trailing `*`.
An inclusion beneath an exclusion remains reachable, but no soft rule can
override hard exclusions. `policy_change: true` on `reconcile_dir` emits a
`delete_subtree` hint for a newly excluded prefix, including an empty root path.
`background: true` requests best-effort per-thread idle I/O on Linux or Windows
background mode. The priority guard restores normal settings before event work.

Responses are `batch` (`id`, `items`), `done` (`id`), or `error` (`id`, `code`,
`message`). Batches contain at most 128 items and approximately 256 KiB. Each
batch must be acknowledged before that request emits another; acknowledgement
expires after 60 seconds. Two workers service requests, with a hard four-request
native limit; Aaryx limits itself to two.

Items are `directory` (`path`, boolean `discovered`), `change` (`path`, `op`, `stat`) or `unknown`
(`path`, `error`). Change operations are `upsert`, `delete`, `delete_subtree`.
`discovered` distinguishes a directory absent from the catalog (or replacing
a file) from an existing catalog directory. New discoveries have scheduling
priority 0; existing directories inherit the reconciliation pass's priority.
Observed metadata is advisory. Unknown entries and failed enumeration never
prove absence. Native root identity is checked before and after reconciliation
and before emitting batches. Own state, `.aaryx-*`, `.zfs`, links/reparse points
and other filesystem volumes are excluded.
Configured/internal directory exclusions also retain their native volume/file
IDs in root policy metadata. Renaming an excluded directory does not expose its
contents, including through direct descendant path checks or after restart.
Exclusions created after enrollment acquire their identity when first observed.
These policy writes never advance indexed `dirs` or `files` rows.
On Windows, configured exclusion directories are resolved again before each
request using the filesystem's own case rules. A late-created directory can
therefore be excluded even when its configured spelling has different casing,
without changing opaque filenames or case-sensitive filesystem semantics.

Schema 2 stores exclusion identities as individual `(volume, ino)` rows in
`excluded_dirs`, rather than one ETF blob. Version-1 blobs are migrated in a
transaction, including lists larger than the IPC decoder's item budget. The
old key is removed only with the copied rows committed. Packet size, depth and
item limits remain unchanged; a separate byte-bounded decoder is used only for
this local legacy migration. Indexed directory/file rows remain untouched.

Event parent components are untrusted spellings too. `reconcile_paths` schedules
the immediate parent only when its exact binary key is a known catalog directory;
otherwise it schedules the enrolled root. That listing rediscovers the actual
parent names before child keys are constructed. No case folding, Unicode
normalization or guessed parent prefix is persisted. Duplicate parent requests
coalesce within the native batch. Scoped watcher errors use this path-check
boundary before scheduling directory work as well.

Directory reconciliation completes and sorts one on-disk listing, then merges
it with an ordered SQLite cursor over catalog files and subdirectories. It
retains only the current catalog child. Memory still scales with the complete
disk listing of each active directory; bounded output is not a constant-memory
directory walk. A failed listing produces no removals.
Unix enumerates through `readdir` and `fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW)`.
Disappearing entries (ENOENT/ENOTDIR) count as absent; other stat failures remain
unknown. Event batches use one temporary-table join, including their parents,
instead of two catalog queries for each path. An ENOTDIR ancestor cannot abort
unrelated paths in the batch. Temporary tables do not modify indexed rows.

Enrollment also records a Linux filesystem UUID from `/dev/disk/by-uuid` or
a Linux/FreeBSD ZFS dataset GUID when exposed to the ordinary user. An enrolled
supplemental identity must continue to match; losing access to it fails closed.
Unavailable identifiers fall back to native volume/directory identity and the
root marker. FreeBSD/ZFS runtime validation is still required before rollout.

Windows uses 64 KiB `FileIdExtdDirectoryInfo` batches on a single directory
handle. This is the 128-bit file-ID variant of the design's bulk
`FileIdBothDirectoryInfo` operation: it preserves compatibility with the
existing `FileIdInfo` identity keys while returning size, modification time,
and ChangeTime without opening each child. Unsupported or incomplete bulk
enumeration fails reconciliation rather than silently inferring removals.
NTFS can cache old directory-entry timestamps for a hard link changed through
another name. Consequently, an apparently unchanged indexed file receives a
fresh handle-based metadata check before it can be skipped. New, changed or
stale entries already require a downstream hint and need no such extra read.
Bulk enumeration reduces work, but eliminating all per-file checks would
weaken hard-link correctness; the same-size hard-link edit regression covers it.

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
