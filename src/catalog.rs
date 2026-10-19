//! Read-only reconciliation after schema/root enrollment. Only the downstream
//! content committer may advance dirs/files; observing disk never does so.
use crate::{
    native::{self, Stamp},
    wire::{self, Value},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
};
use uuid::Uuid;

pub struct Catalog {
    pub root: PathBuf,
    pub database: PathBuf,
    pub root_id: String,
    identity: Stamp,
    marker: Option<String>,
    excluded: Vec<PathBuf>,
    _lock: File,
}
#[derive(Debug)]
struct Stored {
    kind: &'static str,
    size: i64,
    mtime: i64,
    ctime: i64,
    ino: Option<Vec<u8>>,
    status: i64,
}
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
impl Catalog {
    pub fn open(root: &Path, database: &Path, excluded: Vec<PathBuf>) -> Result<Self> {
        if native::stamp(root)?.kind != "dir" {
            return Err("root is not a regular directory".into());
        }
        let root = fs::canonicalize(root)?;
        let identity = native::stamp(&root)?;
        if let Some(p) = database.parent() {
            fs::create_dir_all(p)?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(database.with_extension("lock"))?;
        lock.try_lock()?;
        let mut conn = Connection::open(database)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 1 {
            return Err("unsupported catalog schema version".into());
        }
        conn.execute_batch(include_str!("catalog.sql"))?;
        let get = |key: &str| -> rusqlite::Result<Option<Vec<u8>>> {
            conn.query_row("SELECT value FROM root_info WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()
        };
        let existing = get("root_id")?;
        let (root_id, marker) = if let Some(id) = existing {
            let id = String::from_utf8(id)?;
            let saved = wire::decode(&get("identity")?.ok_or("missing enrolled identity")?)?;
            if saved.get("volume")?.bytes()? != identity.volume
                || saved.get("ino")?.bytes()? != identity.id
            {
                return Err("root_identity: native identity changed".into());
            }
            let marker = match saved.get("marker")? {
                Value::Nil => None,
                v => Some(String::from_utf8(v.bytes()?.to_vec())?),
            };
            (id, marker)
        } else {
            if conn.query_row("SELECT count(*) FROM root_info", [], |r| r.get::<_, i64>(0))? != 0 {
                return Err("incomplete root enrollment".into());
            }
            let path = root.join(".aaryx-root");
            let marker = match read_marker(&path) {
                Ok(id) => Some(id),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    let id = Uuid::new_v4().to_string();
                    match OpenOptions::new().write(true).create_new(true).open(&path) {
                        Ok(mut f) => {
                            writeln!(f, "{id}")?;
                            f.sync_all()?;
                            Some(id)
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
                            ) =>
                        {
                            None
                        }
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                            Some(read_marker(&path)?)
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(e) => return Err(e.into()),
            };
            let id = marker.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
            let saved = Value::map([
                ("volume", identity.volume.clone().into()),
                ("ino", identity.id.clone().into()),
                (
                    "marker",
                    marker.clone().map(Value::from).unwrap_or(Value::Nil),
                ),
            ]);
            let transaction = conn.transaction()?;
            transaction.execute(
                "INSERT INTO root_info VALUES('root_id',?1)",
                [id.as_bytes()],
            )?;
            transaction.execute(
                "INSERT INTO root_info VALUES('identity',?1)",
                [wire::encode(&saved)?],
            )?;
            transaction.commit()?;
            (id, marker)
        };
        let mut excluded = excluded;
        if let Some(parent) = database.parent() {
            excluded.push(parent.to_path_buf());
        }
        let excluded = excluded
            .into_iter()
            .filter_map(|p| fs::canonicalize(p).ok())
            .collect();
        let catalog = Self {
            root,
            database: database.to_path_buf(),
            root_id,
            identity,
            marker,
            excluded,
            _lock: lock,
        };
        catalog.check_root()?;
        Ok(catalog)
    }
    pub fn check_root(&self) -> Result<()> {
        let current = native::stamp(&self.root).map_err(|e| format!("root_identity: {e}"))?;
        if current.kind != "dir"
            || current.id != self.identity.id
            || current.volume != self.identity.volume
        {
            return Err("root_identity: native identity changed".into());
        }
        if let Some(expected) = &self.marker
            && read_marker(&self.root.join(".aaryx-root"))
                .map_err(|e| format!("root_identity: {e}"))?
                != *expected
        {
            return Err("root_identity: marker changed".into());
        }
        Ok(())
    }
    fn connection(&self) -> Result<Connection> {
        let c = Connection::open_with_flags(
            &self.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(c)
    }
    fn ignored(&self, path: &Path, stat: &Stamp) -> bool {
        stat.kind == "other"
            || stat.volume != self.identity.volume
            || self.excluded.iter().any(|p| path.starts_with(p))
            || path
                .strip_prefix(&self.root)
                .map(|r| {
                    r.components().any(|c| {
                        let name = native::encode(c.as_os_str());
                        #[cfg(windows)]
                        let name = name.to_ascii_lowercase();
                        name.starts_with(b".aaryx-") || name == b".zfs"
                    })
                })
                .unwrap_or(true)
    }
    fn lookup(conn: &Connection, path: &[u8]) -> Result<Option<Stored>> {
        if let Some(v) = conn
            .query_row("SELECT 1 FROM dirs WHERE path=?1", [path], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
        {
            let _ = v;
            return Ok(Some(Stored {
                kind: "dir",
                size: 0,
                mtime: 0,
                ctime: 0,
                ino: None,
                status: 0,
            }));
        }
        let (parent, name) = split(path);
        Ok(conn.query_row("SELECT f.size,f.mtime_ns,f.ctime_ns,f.ino,f.content_status FROM files f JOIN dirs d ON d.dir_id=f.dir_id WHERE d.path=?1 AND f.name=?2",params![parent,name],|r|Ok(Stored { kind:"file",size:r.get(0)?,mtime:r.get(1)?,ctime:r.get(2)?,ino:r.get(3)?,status:r.get(4)? })).optional()?)
    }
    fn children(conn: &Connection, path: &[u8]) -> Result<BTreeMap<Vec<u8>, Stored>> {
        let mut result = BTreeMap::new();
        let mut s=conn.prepare("SELECT f.name,f.size,f.mtime_ns,f.ctime_ns,f.ino,f.content_status FROM files f JOIN dirs d ON d.dir_id=f.dir_id WHERE d.path=?1")?;
        for item in s.query_map([path], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                Stored {
                    kind: "file",
                    size: r.get(1)?,
                    mtime: r.get(2)?,
                    ctime: r.get(3)?,
                    ino: r.get(4)?,
                    status: r.get(5)?,
                },
            ))
        })? {
            let (k, v) = item?;
            result.insert(k, v);
        }
        let mut s = conn.prepare(
            "SELECT d.name FROM dirs d JOIN dirs p ON p.dir_id=d.parent_id WHERE p.path=?1",
        )?;
        for item in s.query_map([path], |r| r.get::<_, Vec<u8>>(0))? {
            result.insert(
                item?,
                Stored {
                    kind: "dir",
                    size: 0,
                    mtime: 0,
                    ctime: 0,
                    ino: None,
                    status: 0,
                },
            );
        }
        Ok(result)
    }
    fn compare(
        &self,
        path: &[u8],
        actual: Option<&Stamp>,
        old: Option<&Stored>,
        emit: &mut impl FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        let kind = actual.map(|s| s.kind);
        if let Some(old) = old
            && Some(old.kind) != kind
        {
            emit(change(
                if old.kind == "dir" {
                    "delete_subtree"
                } else {
                    "delete"
                },
                path,
                None,
            ))?;
        }
        match actual {
            Some(s) if s.kind == "dir" => emit(directory(path)),
            Some(s) if s.kind == "file" => {
                let same = old.is_some_and(|o| {
                    o.kind == "file"
                        && o.size == s.size
                        && o.mtime == s.mtime_ns
                        && o.ctime == s.ctime_ns
                        && o.ino.as_ref().is_none_or(|ino| ino == &s.id)
                        && o.status <= 1
                });
                if !same {
                    emit(change("upsert", path, Some(s)))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    fn reconcile_dir(
        &self,
        conn: &Connection,
        path: &[u8],
        emit: &mut impl FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        let full = native::resolve(&self.root, path)?;
        let actual = match native::stamp(&full) {
            Ok(s) => s,
            Err(e) if native::absent(&e) => {
                return self.compare(path, None, Self::lookup(conn, path)?.as_ref(), emit);
            }
            Err(e) => return Err(e.into()),
        };
        if actual.kind != "dir" || self.ignored(&full, &actual) {
            return self.compare(
                path,
                if self.ignored(&full, &actual) {
                    None
                } else {
                    Some(&actual)
                },
                Self::lookup(conn, path)?.as_ref(),
                emit,
            );
        }
        // Finish enumeration before inferring absence. An entry stat failure is
        // unknown, not missing; a read_dir failure rejects the entire listing.
        let mut listing = BTreeMap::new();
        for entry in fs::read_dir(&full)? {
            let entry = entry?;
            let name = native::encode(&entry.file_name());
            let p = entry.path();
            let s = native::stamp(&p);
            listing.insert(
                name,
                s.map(|s| if self.ignored(&p, &s) { None } else { Some(s) }),
            );
        }
        self.check_root()?;
        let mut old = Self::children(conn, path)?;
        for (name, stat) in listing {
            let child = native::join(path, &name);
            let previous = old.remove(&name);
            match stat {
                Ok(s) => self.compare(&child, s.as_ref(), previous.as_ref(), emit)?,
                Err(e) => emit(unknown(&child, &e))?,
            }
        }
        for (name, previous) in old {
            self.compare(&native::join(path, &name), None, Some(&previous), emit)?;
        }
        Ok(())
    }
    fn reconcile_paths(
        &self,
        conn: &Connection,
        paths: &[Value],
        emit: &mut impl FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        if paths.len() > 256 {
            return Err("too many paths".into());
        }
        for path in paths {
            let path = path.bytes()?;
            let full = match native::resolve(&self.root, path) {
                Ok(p) => p,
                Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                    emit(directory(split(path).0))?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let old = Self::lookup(conn, path)?;
            match native::stamp(&full) {
                Ok(s) if self.ignored(&full, &s) => self.compare(path, None, old.as_ref(), emit)?,
                Ok(_s) if old.is_none() => emit(directory(split(path).0))?, // listing supplies authoritative spelling
                Ok(s) => self.compare(path, Some(&s), old.as_ref(), emit)?,
                Err(e) if native::absent(&e) => self.compare(path, None, old.as_ref(), emit)?,
                Err(e) => emit(unknown(path, &e))?,
            }
        }
        Ok(())
    }
    pub fn request(
        &self,
        request: &Value,
        mut emit: impl FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        self.check_root()?;
        let conn = self.connection()?;
        conn.execute_batch("BEGIN")?;
        match request.get("op")?.bytes()? {
            b"check_root" => {}
            b"reconcile_dir" => {
                self.reconcile_dir(&conn, request.get("path")?.bytes()?, &mut emit)?
            }
            b"reconcile_paths" => {
                self.reconcile_paths(&conn, request.get("paths")?.list()?, &mut emit)?
            }
            b"status" => {
                let files: i64 = conn.query_row("SELECT count(*) FROM files", [], |r| r.get(0))?;
                emit(Value::map([
                    ("type", "status".into()),
                    ("files", files.into()),
                ]))?;
            }
            _ => return Err("unknown catalog operation".into()),
        }
        self.check_root()?;
        Ok(())
    }
}
fn read_marker(path: &Path) -> io::Result<String> {
    if native::alias(&fs::symlink_metadata(path)?) {
        return Err(io::Error::other("root marker is a path alias"));
    }
    let f = File::open(path)?;
    use std::io::Read;
    let mut text = String::new();
    f.take(128).read_to_string(&mut text)?;
    Uuid::parse_str(text.trim())
        .map(|id| id.to_string())
        .map_err(io::Error::other)
}
fn split(path: &[u8]) -> (&[u8], &[u8]) {
    match path.iter().rposition(|b| *b == b'/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (b"", path),
    }
}
fn directory(path: &[u8]) -> Value {
    Value::map([("type", "directory".into()), ("path", path.to_vec().into())])
}
fn change(op: &str, path: &[u8], s: Option<&Stamp>) -> Value {
    Value::map([
        ("type", "change".into()),
        ("op", op.into()),
        ("path", path.to_vec().into()),
        ("stat", s.map(Stamp::wire).unwrap_or(Value::Nil)),
    ])
}
fn unknown(path: &[u8], error: &io::Error) -> Value {
    Value::map([
        ("type", "unknown".into()),
        ("path", path.to_vec().into()),
        ("error", error.to_string().into()),
    ])
}

pub fn run(catalog: Catalog) -> Result<()> {
    let catalog = Arc::new(catalog);
    let output = Arc::new(Mutex::new(io::stdout()));
    wire::write(
        &mut *output.lock().unwrap(),
        &wire::message(
            "ready",
            [
                ("root_id", catalog.root_id.clone().into()),
                ("service", "catalog".into()),
            ],
        ),
    )?;
    let (tx, rx) = mpsc::sync_channel::<(Value, mpsc::Receiver<bool>)>(4);
    let rx = Arc::new(Mutex::new(rx));
    let permits = Arc::new(Mutex::new(BTreeMap::<i64, mpsc::SyncSender<bool>>::new()));
    for _ in 0..2 {
        let catalog = Arc::clone(&catalog);
        let output = Arc::clone(&output);
        let rx = Arc::clone(&rx);
        let permits = Arc::clone(&permits);
        thread::spawn(move || {
            loop {
                let (request, permit) = match rx.lock().unwrap().recv() {
                    Ok(r) => r,
                    Err(_) => return,
                };
                let id = request.get("id").and_then(Value::int).unwrap_or(-1);
                let send = |v: &Value| -> Result<()> {
                    wire::write(&mut *output.lock().unwrap(), v)?;
                    Ok(())
                };
                let mut batch = vec![];
                let mut size = 0;
                let flush = |batch: &mut Vec<Value>| -> Result<()> {
                    if !batch.is_empty() {
                        catalog.check_root()?;
                        send(&wire::message(
                            "batch",
                            [
                                ("id", id.into()),
                                ("items", Value::List(std::mem::take(batch))),
                            ],
                        ))?;
                        if permit.recv_timeout(std::time::Duration::from_secs(60)) != Ok(true) {
                            return Err(
                                "catalog batch cancelled or acknowledgement timed out".into()
                            );
                        }
                    }
                    Ok(())
                };
                let result = if matches!(
                    request.get("version").and_then(Value::int),
                    Ok(wire::VERSION)
                ) {
                    catalog
                        .request(&request, |item| {
                            let n = wire::encode(&item)?.len();
                            if size + n > 256 * 1024 || batch.len() >= 128 {
                                flush(&mut batch)?;
                                size = 0;
                            }
                            size += n;
                            batch.push(item);
                            Ok(())
                        })
                        .and_then(|_| flush(&mut batch))
                } else {
                    Err("incompatible protocol version".into())
                };
                let response = match result {
                    Ok(()) => wire::message("done", [("id", id.into())]),
                    Err(e) => {
                        let text = e.to_string();
                        wire::message(
                            "error",
                            [
                                ("id", id.into()),
                                (
                                    "code",
                                    if text.starts_with("root_identity") {
                                        "root_identity"
                                    } else {
                                        "reconcile_failed"
                                    }
                                    .into(),
                                ),
                                ("message", text.into()),
                            ],
                        )
                    }
                };
                if send(&response).is_err() {
                    std::process::exit(0);
                }
                permits.lock().unwrap().remove(&id);
            }
        });
    }
    let mut input = io::stdin().lock();
    loop {
        match wire::read(&mut input) {
            Ok(Some(v)) => {
                let id = v.get("id")?.int()?;
                match v.get("op")?.bytes()? {
                    b"ack" | b"cancel" => {
                        if let Some(sender) = permits.lock().unwrap().get(&id) {
                            let ack = v.get("op")?.bytes()? == b"ack";
                            let _ = sender.try_send(ack);
                        }
                    }
                    _ => {
                        let (permit, wait) = mpsc::sync_channel(1);
                        {
                            let mut pending = permits.lock().unwrap();
                            if pending.len() >= 4 || pending.contains_key(&id) {
                                return Err("catalog request limit exceeded".into());
                            }
                            pending.insert(id, permit);
                        }
                        if tx.try_send((v, wait)).is_err() {
                            return Err("catalog request limit exceeded".into());
                        }
                    }
                }
            }
            Ok(None) => std::process::exit(0),
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Catalog) {
        let r = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let c = Catalog::open(r.path(), &d.path().join("catalog.sqlite3"), vec![]).unwrap();
        (r, d, c)
    }
    fn scan(c: &Catalog) -> Vec<Value> {
        let mut out = vec![];
        c.request(
            &Value::map([("op", "reconcile_dir".into()), ("path", vec![].into())]),
            |v| {
                out.push(v);
                Ok(())
            },
        )
        .unwrap();
        out
    }
    fn seed(c: &Catalog, name: &[u8], id: i64) {
        let s = native::stamp(&c.root.join(native::decode(name).unwrap())).unwrap();
        let db = Connection::open(&c.database).unwrap();
        db.execute(
            "INSERT INTO files VALUES(1,?1,?2,?3,?4,?5,?6,0)",
            params![name, id, s.size, s.mtime_ns, s.ctime_ns, s.id],
        )
        .unwrap();
    }
    #[test]
    fn producer_does_not_advance_catalog_and_consumer_can_complete_work() {
        let (r, _d, c) = fixture();
        fs::write(r.path().join("a"), b"first").unwrap();
        let first = scan(&c);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].get("op").unwrap().bytes().unwrap(), b"upsert");
        assert_eq!(scan(&c), first);
        seed(&c, b"a", 1);
        assert!(scan(&c).is_empty());
        fs::write(r.path().join("a"), b"changed size").unwrap();
        assert_eq!(scan(&c).len(), 1);
        fs::remove_file(r.path().join("a")).unwrap();
        assert_eq!(scan(&c)[0].get("op").unwrap().bytes().unwrap(), b"delete");
    }
    #[test]
    fn identity_mismatch_cannot_emit_deletions() {
        let (r, _d, c) = fixture();
        fs::write(r.path().join("a"), b"a").unwrap();
        seed(&c, b"a", 1);
        fs::remove_file(r.path().join("a")).unwrap();
        fs::remove_file(r.path().join(".aaryx-root")).unwrap();
        let mut out = vec![];
        assert!(
            c.request(
                &Value::map([("op", "reconcile_dir".into()), ("path", vec![].into())]),
                |v| {
                    out.push(v);
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(out.is_empty());
    }
    #[test]
    fn directory_replacement_removes_old_file_and_discovers_directory() {
        let (r, _d, c) = fixture();
        fs::write(r.path().join("a"), b"a").unwrap();
        seed(&c, b"a", 1);
        fs::remove_file(r.path().join("a")).unwrap();
        fs::create_dir(r.path().join("a")).unwrap();
        let out = scan(&c);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].get("op").unwrap().bytes().unwrap(), b"delete");
        assert_eq!(out[1].get("type").unwrap().bytes().unwrap(), b"directory");
    }
    #[test]
    fn a_second_owner_is_rejected() {
        let (r, d, _c) = fixture();
        assert!(Catalog::open(r.path(), &d.path().join("catalog.sqlite3"), vec![]).is_err());
    }
    #[test]
    fn stale_rows_are_requeued_and_hard_links_are_path_scoped() {
        let (r, _d, c) = fixture();
        fs::write(r.path().join("a"), b"a").unwrap();
        fs::hard_link(r.path().join("a"), r.path().join("b")).unwrap();
        assert_eq!(scan(&c).len(), 2);
        seed(&c, b"a", 1);
        seed(&c, b"b", 2);
        assert!(scan(&c).is_empty());
        Connection::open(&c.database)
            .unwrap()
            .execute(
                "UPDATE files SET content_status=2 WHERE name=?1",
                [b"a".as_slice()],
            )
            .unwrap();
        let changes = scan(&c);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].get("path").unwrap().bytes().unwrap(), b"a");
    }
    #[test]
    fn missing_directories_and_directory_to_file_replacements_are_explicit() {
        let (r, _d, c) = fixture();
        let db = Connection::open(&c.database).unwrap();
        db.execute("INSERT INTO dirs VALUES(2,1,?1,?1)", [b"old".as_slice()])
            .unwrap();
        let missing = scan(&c);
        assert_eq!(missing.len(), 1);
        assert_eq!(
            missing[0].get("op").unwrap().bytes().unwrap(),
            b"delete_subtree"
        );
        fs::write(r.path().join("old"), b"now a file").unwrap();
        let replaced = scan(&c);
        assert_eq!(replaced.len(), 2);
        assert_eq!(replaced[1].get("op").unwrap().bytes().unwrap(), b"upsert");
    }
    #[test]
    fn internal_files_and_explicit_state_directories_are_excluded() {
        let (r, d, c) = fixture();
        fs::create_dir(r.path().join(".aaryx-upload-tmp")).unwrap();
        fs::create_dir(r.path().join(".zfs")).unwrap();
        assert!(scan(&c).is_empty());
        drop(c);
        let state = r.path().join("state");
        fs::create_dir(&state).unwrap();
        let c = Catalog::open(r.path(), &d.path().join("catalog.sqlite3"), vec![state]).unwrap();
        assert!(scan(&c).is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn permission_denied_is_unknown_and_symlinks_are_not_followed() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (r, _d, c) = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("outside"), b"x").unwrap();
        symlink(outside.path(), r.path().join("link")).unwrap();
        assert!(scan(&c).is_empty());
        let blocked = r.path().join("blocked");
        fs::create_dir(&blocked).unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0)).unwrap();
        let result = c.request(
            &Value::map([
                ("op", "reconcile_dir".into()),
                ("path", b"blocked".to_vec().into()),
            ]),
            |_| panic!("failed listing emitted a record"),
        );
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            assert!(result.is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn non_utf8_names_remain_distinct() {
        let (r, _d, c) = fixture();
        for n in [vec![0xff], vec![0xfe]] {
            fs::write(r.path().join(native::decode(&n).unwrap()), b"a").unwrap();
        }
        let out = scan(&c);
        assert_eq!(out.len(), 2);
        assert_ne!(out[0].get("path").unwrap(), out[1].get("path").unwrap());
    }
}
