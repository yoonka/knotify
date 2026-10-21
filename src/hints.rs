//! Loss-tolerant path hints. The notify callback never blocks on stdout or BEAM.
use crate::{
    native,
    wire::{self, Value},
};
use notify::{
    Event, EventKind, RecursiveMode, Watcher,
    event::{MetadataKind, ModifyKind},
};
use std::{
    collections::BTreeSet,
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

const CAPACITY: usize = 4096;
const WINDOW: usize = 4;
enum Input {
    Credit(usize),
    Closed,
}
pub fn run(
    root: PathBuf,
    recursive: bool,
    debounce: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let root = std::fs::canonicalize(root)?;
    let (tx, rx) = mpsc::sync_channel(CAPACITY);
    let overflow = Arc::new(AtomicBool::new(false));
    let lost = Arc::clone(&overflow);
    let mut watcher = notify::RecommendedWatcher::new(
        move |event: notify::Result<Event>| {
            if tx.try_send(event).is_err() {
                lost.store(true, Ordering::Release);
            }
        },
        notify::Config::default().with_follow_symlinks(false),
    )?;
    let mut output = io::stdout().lock();
    if let Err(e) = watcher.watch(
        &root,
        if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        },
    ) {
        wire::write(
            &mut output,
            &wire::message(
                "error",
                [
                    ("code", "registration_failed".into()),
                    ("message", e.to_string().into()),
                ],
            ),
        )?;
        return Err(e.into());
    }
    wire::write(
        &mut output,
        &wire::message("ready", [("service", "watcher".into())]),
    )?;
    let (input_tx, input_rx) = mpsc::sync_channel(WINDOW);
    thread::spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            match wire::read(&mut input) {
                Ok(Some(v))
                    if matches!(v.get("version").and_then(Value::int), Ok(wire::VERSION))
                        && v.get("type")
                            .and_then(Value::bytes)
                            .is_ok_and(|t| t == b"credit") =>
                {
                    match v.get("count").and_then(Value::int) {
                        Ok(n) if n > 0 && n <= WINDOW as i64 => {
                            if input_tx.send(Input::Credit(n as usize)).is_err() {
                                return;
                            }
                        }
                        _ => {
                            let _ = input_tx.send(Input::Closed);
                            return;
                        }
                    }
                }
                _ => {
                    // EOF must stop even while stdout is blocked on a dead owner.
                    std::process::exit(0);
                }
            }
        }
    });
    let mut credits = 0;
    let mut paths = BTreeSet::new();
    let mut rescan = false;
    let mut error: Option<String> = None;
    let mut seq = 0i64;
    let mut last = Instant::now();
    loop {
        loop {
            match input_rx.try_recv() {
                Ok(Input::Credit(n)) => credits = (credits + n).min(WINDOW),
                Ok(Input::Closed) => return Err("invalid credit".into()),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if overflow.swap(false, Ordering::AcqRel) {
            rescan = true;
            paths.clear();
        }
        for _ in 0..512 {
            match rx.try_recv() {
                Ok(Ok(event)) => {
                    absorb(&root, event, &mut paths, &mut rescan);
                }
                Ok(Err(e)) => {
                    error = Some(e.to_string());
                    rescan = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err("watcher disconnected".into()),
            }
        }
        if credits > 0
            && (rescan
                || paths.len() >= 128
                || last.elapsed() >= Duration::from_millis(debounce.max(10)))
        {
            let value = if rescan {
                paths.clear();
                rescan = false;
                Some(wire::message(
                    "rescan",
                    [
                        ("batch_id", seq.into()),
                        (
                            "reason",
                            error
                                .take()
                                .unwrap_or_else(|| "events_may_be_lost".into())
                                .into(),
                        ),
                    ],
                ))
            } else if !paths.is_empty() {
                let mut batch = vec![];
                let mut size = 0;
                while let Some(path) = paths.pop_first() {
                    if size + path.len() > 256 * 1024 && !batch.is_empty() {
                        paths.insert(path);
                        break;
                    }
                    size += path.len();
                    batch.push(path.into());
                    if batch.len() >= 128 {
                        break;
                    }
                }
                Some(wire::message(
                    "paths",
                    [("batch_id", seq.into()), ("paths", Value::List(batch))],
                ))
            } else {
                None
            };
            if let Some(value) = value {
                wire::write(&mut output, &value)?;
                credits -= 1;
                seq += 1;
            }
            last = Instant::now();
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn absorb(root: &std::path::Path, event: Event, paths: &mut BTreeSet<Vec<u8>>, rescan: &mut bool) {
    if event.need_rescan()
        || (event.paths.is_empty() && matches!(event.kind, EventKind::Any | EventKind::Other))
    {
        *rescan = true;
    }
    if matches!(
        event.kind,
        EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime))
    ) {
        return;
    }
    for path in event.paths {
        match path
            .strip_prefix(root)
            .ok()
            .and_then(|p| native::relative(p).ok())
        {
            Some(path) => {
                paths.insert(path);
            }
            None => *rescan = true,
        }
    }
    if paths.len() > CAPACITY {
        paths.clear();
        *rescan = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, Flag, RemoveKind};
    #[test]
    fn unscoped_ambiguous_events_require_a_rescan() {
        let root = PathBuf::from("root");
        for kind in [EventKind::Any, EventKind::Other] {
            let mut paths = BTreeSet::new();
            let mut rescan = false;
            absorb(&root, Event::new(kind), &mut paths, &mut rescan);
            assert!(rescan);
            assert!(paths.is_empty());
        }
        let mut paths = BTreeSet::new();
        let mut rescan = false;
        absorb(
            &root,
            Event::new(EventKind::Any).add_path(root.join("known")),
            &mut paths,
            &mut rescan,
        );
        assert!(!rescan);
        assert!(paths.contains(b"known".as_slice()));
    }
    #[test]
    fn recreate_is_retained_and_access_rescan_is_never_filtered() {
        let root = PathBuf::from("root");
        let mut paths = BTreeSet::new();
        let mut rescan = false;
        absorb(
            &root,
            Event::new(EventKind::Remove(RemoveKind::File)).add_path(root.join("a")),
            &mut paths,
            &mut rescan,
        );
        absorb(
            &root,
            Event::new(EventKind::Create(CreateKind::File)).add_path(root.join("a")),
            &mut paths,
            &mut rescan,
        );
        assert!(paths.contains(b"a".as_slice()));
        absorb(
            &root,
            Event::new(EventKind::Access(AccessKind::Any)).set_flag(Flag::Rescan),
            &mut paths,
            &mut rescan,
        );
        assert!(rescan);
    }
    #[test]
    fn coalescing_overflow_requires_root_reconciliation() {
        let root = PathBuf::from("root");
        let mut paths = BTreeSet::new();
        let mut rescan = false;
        for i in 0..=CAPACITY {
            absorb(
                &root,
                Event::new(EventKind::Create(CreateKind::File)).add_path(root.join(i.to_string())),
                &mut paths,
                &mut rescan,
            );
        }
        assert!(rescan);
        assert!(paths.len() <= CAPACITY);
    }
}
