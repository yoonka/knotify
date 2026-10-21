//! Read-only reconciliation after schema/root enrollment. Only the downstream
//! content committer may advance dirs/files; observing disk never does so.
use crate::{
    native::{self, Stamp},
    wire::{self, Value},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    excluded_ids: Mutex<BTreeSet<(Vec<u8>, Vec<u8>)>>,
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
impl Stored {
    fn matches(&self, stamp: &Stamp) -> bool {
        self.kind == "file"
            && stamp.kind == "file"
            && self.size == stamp.size
            && self.mtime == stamp.mtime_ns
            && self.ctime == stamp.ctime_ns
            && self.ino.as_ref().is_none_or(|ino| ino == &stamp.id)
            && self.status <= 1
    }
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
        if version > 2 {
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
            if conn.query_row("SELECT count(*) FROM root_info", [], |r| r.get::<_, i64>(0))? != 0
                || conn.query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))? != 0
                || conn.query_row("SELECT count(*) FROM dirs", [], |r| r.get::<_, i64>(0))? != 1
            {
                return Err("incomplete root enrollment: catalog is not empty".into());
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
            .map(|p| exclusion_path(&p))
            .collect::<io::Result<Vec<_>>>()?;
        // Migrate even an oversized legacy list without relaxing IPC limits.
        // Copy and remove the old key atomically; an interrupted upgrade retries.
        if let Some(bytes) = conn
            .query_row(
                "SELECT value FROM root_info WHERE key='excluded_identities'",
                [],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?
        {
            let legacy = wire::decode_legacy_exclusions(&bytes)?;
            let transaction = conn.transaction()?;
            for value in legacy.list()? {
                transaction.execute(
                    "INSERT OR IGNORE INTO excluded_dirs VALUES(?1,?2)",
                    params![value.get("volume")?.bytes()?, value.get("ino")?.bytes()?],
                )?;
            }
            transaction.execute("DELETE FROM root_info WHERE key='excluded_identities'", [])?;
            transaction.commit()?;
        }
        let excluded_ids = conn
            .prepare("SELECT volume,ino FROM excluded_dirs")?
            .query_map([], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<rusqlite::Result<BTreeSet<_>>>()?;
        let catalog = Self {
            root,
            database: database.to_path_buf(),
            root_id,
            identity,
            marker,
            excluded,
            excluded_ids: Mutex::new(excluded_ids),
            _lock: lock,
        };
        catalog.check_root()?;
        catalog.refresh_exclusions()?;
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
    fn remember_exclusion(&self, stat: &Stamp) -> Result<()> {
        let mut ids = self.excluded_ids.lock().unwrap();
        let id = (stat.volume.clone(), stat.id.clone());
        if !ids.contains(&id) {
            let conn = Connection::open(&self.database)?;
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            conn.execute(
                "INSERT OR IGNORE INTO excluded_dirs VALUES(?1,?2)",
                params![id.0, id.1],
            )?;
            ids.insert(id);
        }
        Ok(())
    }
    fn refresh_exclusions(&self) -> Result<()> {
        for path in &self.excluded {
            match native::stamp(path) {
                Ok(stat) if stat.kind == "dir" => self.remember_exclusion(&stat)?,
                Ok(_) => {}
                Err(e) if native::absent(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    fn ignored(&self, path: &Path, stat: &Stamp) -> Result<bool> {
        let by_path = self.excluded.iter().any(|p| path.starts_with(p))
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
                .unwrap_or(true);
        if by_path && stat.kind == "dir" {
            // Root policy metadata only; indexed rows remain untouched.
            self.remember_exclusion(stat)?;
        }
        let ids = self.excluded_ids.lock().unwrap();
        let id = (stat.volume.clone(), stat.id.clone());
        Ok(stat.kind == "other"
            || stat.volume != self.identity.volume
            || by_path
            || (stat.kind == "dir" && ids.contains(&id)))
    }
    fn ignored_tree(&self, path: &Path, stat: &Stamp) -> Result<bool> {
        if self.ignored(path, stat)? {
            return Ok(true);
        }
        let mut parent = path.parent();
        while let Some(p) = parent.filter(|p| p.starts_with(&self.root) && *p != self.root) {
            if self.ignored(p, &native::stamp(p)?)? {
                return Ok(true);
            }
            parent = p.parent();
        }
        Ok(false)
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
    fn children(conn: &Connection) -> Result<rusqlite::Statement<'_>> {
        // SQLite merges the two indexed name orders. Retain only the current
        // catalog child, not another complete copy of a large directory.
        Ok(conn.prepare(
            "SELECT name,0 AS kind,size,mtime_ns,ctime_ns,ino,content_status
             FROM files WHERE dir_id=(SELECT dir_id FROM dirs WHERE path=?1)
             UNION ALL
             SELECT name,1,0,0,0,NULL,0
             FROM dirs WHERE parent_id=(SELECT dir_id FROM dirs WHERE path=?1)
             ORDER BY name",
        )?)
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
            Some(s) if s.kind == "dir" => {
                emit(directory(path, old.is_none_or(|entry| entry.kind != "dir")))
            }
            Some(s) if s.kind == "file" => {
                let same = old.is_some_and(|o| o.matches(s));
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
        if actual.kind != "dir" || self.ignored_tree(&full, &actual)? {
            return self.compare(
                path,
                if self.ignored_tree(&full, &actual)? {
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
        let listing = native::listing(&full)?;
        self.check_root()?;
        let mut statement = Self::children(conn)?;
        let mut old = statement.query_map([path], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                Stored {
                    kind: if row.get::<_, i64>(1)? == 0 {
                        "file"
                    } else {
                        "dir"
                    },
                    size: row.get(2)?,
                    mtime: row.get(3)?,
                    ctime: row.get(4)?,
                    ino: row.get(5)?,
                    status: row.get(6)?,
                },
            ))
        })?;
        let mut previous = old.next().transpose()?;
        for (name, stat) in listing {
            while previous.as_ref().is_some_and(|(key, _)| key < &name) {
                let (missing, stored) = previous.take().unwrap();
                self.compare(&native::join(path, &missing), None, Some(&stored), emit)?;
                previous = old.next().transpose()?;
            }
            let stored = if previous.as_ref().is_some_and(|(key, _)| key == &name) {
                let (_, stored) = previous.take().unwrap();
                previous = old.next().transpose()?;
                Some(stored)
            } else {
                None
            };
            let child = native::join(path, &name);
            let full = full.join(native::decode(&name)?);
            #[cfg(windows)]
            let stat = match stat {
                // NTFS directory entries can retain stale metadata for a hard
                // link edited through another name. Bulk metadata may queue
                // a hint, but must not alone justify skipping indexed work.
                Ok(s)
                    if stored.as_ref().is_some_and(|old| old.matches(&s))
                        && !self.ignored(&full, &s)? =>
                {
                    native::stamp(&full)
                }
                other => other,
            };
            match stat {
                Ok(s) => {
                    self.compare(
                        &child,
                        if self.ignored(&full, &s)? {
                            None
                        } else {
                            Some(&s)
                        },
                        stored.as_ref(),
                        emit,
                    )?;
                }
                Err(e) => emit(unknown(&child, &e))?,
            }
        }
        while let Some((name, stored)) = previous {
            self.compare(&native::join(path, &name), None, Some(&stored), emit)?;
            previous = old.next().transpose()?;
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
                    let parent = split(path).0;
                    emit(directory(parent, Self::lookup(conn, parent)?.is_none()))?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let old = Self::lookup(conn, path)?;
            match native::stamp(&full) {
                Ok(s) if self.ignored_tree(&full, &s)? => {
                    self.compare(path, None, old.as_ref(), emit)?
                }
                Ok(_s) if old.is_none() => {
                    // A listing supplies the authoritative spelling.
                    let parent = split(path).0;
                    emit(directory(parent, Self::lookup(conn, parent)?.is_none()))?;
                }
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
        // Windows resolves configured paths with the containing directory's
        // native case rules. Capture late-created directories before comparing
        // the listing; never case-fold stored names or catalog keys.
        #[cfg(windows)]
        self.refresh_exclusions()?;
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
fn exclusion_path(path: &Path) -> io::Result<PathBuf> {
    // Keep policies for state directories created after enrollment. Resolve the
    // existing ancestor so Windows verbatim prefixes and directory aliases match
    // the canonical root; never silently discard an absent exclusion.
    let mut ancestor = std::path::absolute(path)?;
    let mut suffix = vec![];
    loop {
        match fs::canonicalize(&ancestor) {
            Ok(mut resolved) => {
                for part in suffix.into_iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }
            Err(e) if native::absent(&e) => {
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| io::Error::other("invalid exclusion path"))?
                        .to_os_string(),
                );
                if !ancestor.pop() {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        }
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
fn directory(path: &[u8], discovered: bool) -> Value {
    Value::map([
        ("type", "directory".into()),
        ("path", path.to_vec().into()),
        ("discovered", Value::Bool(discovered)),
    ])
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
        assert_eq!(out[1].get("discovered").unwrap(), &Value::Bool(true));
    }

    #[test]
    fn ordered_catalog_merge_preserves_known_and_new_directory_semantics() {
        let (r, _d, c) = fixture();
        for name in ["a-missing", "c-changed", "z-unchanged"] {
            fs::write(r.path().join(name), b"original").unwrap();
            seed(&c, name.as_bytes(), name.as_bytes()[0] as i64);
        }
        fs::remove_file(r.path().join("a-missing")).unwrap();
        fs::write(r.path().join("c-changed"), b"different size").unwrap();
        fs::write(r.path().join("b-new"), b"new").unwrap();
        fs::create_dir(r.path().join("d-known")).unwrap();
        fs::create_dir(r.path().join("e-new")).unwrap();
        let db = Connection::open(&c.database).unwrap();
        db.execute(
            "INSERT INTO dirs VALUES(2,1,?1,?1)",
            [b"d-known".as_slice()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO dirs VALUES(3,1,?1,?1)",
            [b"f-missing".as_slice()],
        )
        .unwrap();
        let out = scan(&c);
        let paths: Vec<_> = out
            .iter()
            .map(|v| v.get("path").unwrap().bytes().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                b"a-missing".as_slice(),
                b"b-new",
                b"c-changed",
                b"d-known",
                b"e-new",
                b"f-missing"
            ]
        );
        assert_eq!(out[0].get("op").unwrap().bytes().unwrap(), b"delete");
        assert_eq!(out[1].get("op").unwrap().bytes().unwrap(), b"upsert");
        assert_eq!(out[2].get("op").unwrap().bytes().unwrap(), b"upsert");
        assert_eq!(out[3].get("discovered").unwrap(), &Value::Bool(false));
        assert_eq!(out[4].get("discovered").unwrap(), &Value::Bool(true));
        assert_eq!(
            out[5].get("op").unwrap().bytes().unwrap(),
            b"delete_subtree"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
    }
    #[test]
    fn a_second_owner_is_rejected() {
        let (r, d, _c) = fixture();
        assert!(Catalog::open(r.path(), &d.path().join("catalog.sqlite3"), vec![]).is_err());
    }
    #[test]
    fn absent_exclusions_remain_excluded_when_the_directory_appears() {
        let r = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let excluded = r.path().join("future-state").join("cache");
        let c = Catalog::open(
            r.path(),
            &d.path().join("catalog.sqlite3"),
            vec![excluded.clone()],
        )
        .unwrap();
        fs::create_dir_all(&excluded).unwrap();
        fs::write(excluded.join("internal"), b"x").unwrap();
        let mut out = vec![];
        c.request(
            &Value::map([
                ("op", "reconcile_dir".into()),
                ("path", b"future-state".to_vec().into()),
            ]),
            |v| {
                out.push(v);
                Ok(())
            },
        )
        .unwrap();
        assert!(out.is_empty());
    }
    #[test]
    fn renamed_exclusions_and_their_descendants_stay_excluded_after_restart() {
        let r = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let path = r.path().join("managed");
        fs::create_dir_all(path.join("nested")).unwrap();
        fs::write(path.join("nested/internal"), b"state").unwrap();
        let db = d.path().join("catalog.sqlite3");
        let c = Catalog::open(r.path(), &db, vec![path.clone()]).unwrap();
        assert!(scan(&c).is_empty());
        fs::rename(&path, r.path().join("renamed")).unwrap();
        assert!(scan(&c).is_empty());
        for op in ["reconcile_dir", "reconcile_paths"] {
            let request = if op == "reconcile_dir" {
                Value::map([
                    ("op", op.into()),
                    ("path", b"renamed/nested".to_vec().into()),
                ])
            } else {
                Value::map([
                    ("op", op.into()),
                    (
                        "paths",
                        Value::List(vec![b"renamed/nested/internal".to_vec().into()]),
                    ),
                ])
            };
            c.request(&request, |_| panic!("excluded descendant leaked"))
                .unwrap();
        }
        drop(c);
        let c = Catalog::open(r.path(), &db, vec![path]).unwrap();
        assert!(scan(&c).is_empty());
    }

    #[test]
    fn exclusions_created_after_enrollment_retain_identity_when_renamed() {
        let r = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let path = r.path().join("future-state");
        let c = Catalog::open(
            r.path(),
            &d.path().join("catalog.sqlite3"),
            vec![path.clone()],
        )
        .unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("internal"), b"state").unwrap();
        assert!(scan(&c).is_empty());
        fs::rename(&path, r.path().join("renamed-state")).unwrap();
        assert!(scan(&c).is_empty());
    }
    #[cfg(windows)]
    #[test]
    fn late_created_exclusions_use_native_case_resolution_and_keep_identity() {
        let r = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let policy = r.path().join("FUTURE-STATE");
        let database = d.path().join("catalog.sqlite3");
        let c = Catalog::open(r.path(), &database, vec![policy.clone()]).unwrap();
        let actual = r.path().join("future-state");
        fs::create_dir(&actual).unwrap();
        fs::write(actual.join("internal"), b"state").unwrap();
        assert!(
            policy.is_dir(),
            "fixture requires a case-insensitive directory"
        );
        // Direct event checks must work even before a full scan observes it.
        c.request(
            &Value::map([
                ("op", "reconcile_paths".into()),
                (
                    "paths",
                    Value::List(vec![b"future-state/internal".to_vec().into()]),
                ),
            ]),
            |_| panic!("excluded event leaked"),
        )
        .unwrap();
        assert!(scan(&c).is_empty());
        fs::rename(actual, r.path().join("renamed-state")).unwrap();
        drop(c);
        let c = Catalog::open(r.path(), &database, vec![policy]).unwrap();
        assert!(scan(&c).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn exclusions_do_not_fold_case_on_case_sensitive_filesystems() {
        let r = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let policy = r.path().join("STATE");
        let c = Catalog::open(
            r.path(),
            &d.path().join("catalog.sqlite3"),
            vec![policy.clone()],
        )
        .unwrap();
        fs::create_dir(policy).unwrap();
        fs::create_dir(r.path().join("state")).unwrap();
        let out = scan(&c);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].get("path").unwrap().bytes().unwrap(), b"state");
    }

    #[test]
    fn large_exclusion_sets_survive_scan_and_restart() {
        let (r, d, c) = fixture();
        for n in 0..1700 {
            fs::create_dir(r.path().join(format!(".aaryx-state-{n}"))).unwrap();
        }
        assert!(scan(&c).is_empty());
        drop(c);
        let c = Catalog::open(r.path(), &d.path().join("catalog.sqlite3"), vec![]).unwrap();
        assert!(scan(&c).is_empty());
        let count: i64 = Connection::open(&c.database)
            .unwrap()
            .query_row("SELECT count(*) FROM excluded_dirs", [], |r| r.get(0))
            .unwrap();
        assert!(count >= 1700);
    }

    #[test]
    fn oversized_legacy_exclusions_migrate_without_changing_indexed_rows() {
        let (r, d, c) = fixture();
        fs::write(r.path().join("indexed"), b"content").unwrap();
        seed(&c, b"indexed", 1);
        fs::create_dir(r.path().join("managed")).unwrap();
        let stat = native::stamp(&r.path().join("managed")).unwrap();
        let mut values = vec![Value::map([
            ("volume", stat.volume.into()),
            ("ino", stat.id.into()),
        ])];
        for n in 1..1700u64 {
            values.push(Value::map([
                ("volume", b"legacy-volume".to_vec().into()),
                ("ino", n.to_be_bytes().to_vec().into()),
            ]));
        }
        let legacy = wire::encode(&Value::List(values)).unwrap();
        assert!(wire::decode(&legacy).is_err());
        let db = Connection::open(&c.database).unwrap();
        db.execute(
            "INSERT INTO root_info VALUES('excluded_identities',?1)",
            [legacy],
        )
        .unwrap();
        db.execute_batch("DELETE FROM excluded_dirs; PRAGMA user_version=1")
            .unwrap();
        drop(c);
        fs::rename(r.path().join("managed"), r.path().join("renamed")).unwrap();
        let c = Catalog::open(r.path(), &d.path().join("catalog.sqlite3"), vec![]).unwrap();
        assert!(scan(&c).is_empty());
        assert_eq!(
            db.query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(
            db.query_row("SELECT count(*) FROM excluded_dirs", [], |r| r
                .get::<_, i64>(0))
                .unwrap()
                >= 1700
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM root_info WHERE key='excluded_identities'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        drop(c);
        // The migration is idempotent on the next open too.
        assert!(
            scan(&Catalog::open(r.path(), &d.path().join("catalog.sqlite3"), vec![]).unwrap())
                .is_empty()
        );
    }
    #[test]
    fn indexed_rows_without_enrollment_are_never_adopted() {
        let (r, d, c) = fixture();
        fs::write(r.path().join("a"), b"x").unwrap();
        seed(&c, b"a", 1);
        Connection::open(&c.database)
            .unwrap()
            .execute("DELETE FROM root_info", [])
            .unwrap();
        drop(c);
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
    fn same_size_edits_through_one_hard_link_requeue_both_paths() {
        let (r, _d, c) = fixture();
        fs::write(r.path().join("first"), b"before").unwrap();
        fs::hard_link(r.path().join("first"), r.path().join("second")).unwrap();
        seed(&c, b"first", 1);
        seed(&c, b"second", 2);
        assert!(scan(&c).is_empty());
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(r.path().join("first"), b"after!").unwrap();
        let changes = scan(&c);
        assert_eq!(changes.len(), 2);
        for change in changes {
            assert_eq!(change.get("op").unwrap().bytes().unwrap(), b"upsert");
        }
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
