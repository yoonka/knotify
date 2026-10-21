use crate::config::{Config, EventFilter};

use notify::{Event, EventKind, RecursiveMode, Result, Watcher};
use serde_json::json;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Cache key for deduplication: combines path and event kind
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct EventKey {
    path: PathBuf,
    kind: String,
}

/// Event classes for understanding event relationships
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventClass {
    Create,
    Modify,
    Remove,
    Rename,
}

impl EventClass {
    fn from_kind_str(kind_str: &str) -> Self {
        if kind_str.starts_with("CREATE") {
            EventClass::Create
        } else if kind_str.starts_with("MODIFY") {
            EventClass::Modify
        } else if kind_str.starts_with("REMOVE") {
            EventClass::Remove
        } else if kind_str.starts_with("RENAME") {
            EventClass::Rename
        } else {
            EventClass::Modify // Default to modify for unknown types
        }
    }
}

/// Tracks the last event for a path
#[derive(Debug, Clone)]
struct LastEvent {
    kind: String,
    class: EventClass,
    timestamp: Instant,
}

/// Event cache for time-based deduplication with event relationship awareness
struct EventCache {
    // Track exact event (path + kind) timestamps for basic deduplication
    cache: HashMap<EventKey, Instant>,
    // Track the last event class per path for smart suppression
    last_event_per_path: HashMap<PathBuf, LastEvent>,
    debounce_duration: Duration,
}

impl EventCache {
    fn new(debounce_ms: u64) -> Self {
        Self {
            cache: HashMap::new(),
            last_event_per_path: HashMap::new(),
            debounce_duration: Duration::from_millis(debounce_ms),
        }
    }

    /// Check if event should be processed based on smart deduplication rules
    /// Returns true if event should be processed, false if it's a duplicate or spurious
    fn should_process(&mut self, path: PathBuf, kind: String) -> bool {
        let now = Instant::now();
        let event_class = EventClass::from_kind_str(&kind);

        // Clean up expired entries
        self.cache
            .retain(|_, &mut timestamp| now.duration_since(timestamp) < self.debounce_duration);
        self.last_event_per_path.retain(|_, last_event| {
            now.duration_since(last_event.timestamp) < self.debounce_duration
        });

        // Check for spurious events based on the last event for this path
        if let Some(last_event) = self.last_event_per_path.get(&path) {
            let time_since_last = now.duration_since(last_event.timestamp);

            // Only apply smart suppression within the debounce window
            if time_since_last < self.debounce_duration {
                // Rule 1: After a REMOVE event, suppress any MODIFY/CREATE events
                // (file system may report stale events after deletion)
                if last_event.class == EventClass::Remove {
                    if event_class == EventClass::Modify || event_class == EventClass::Create {
                        return false; // Suppress spurious event after deletion
                    }
                }

                // Rule 2: Suppress duplicate events of the same exact kind
                if last_event.kind == kind {
                    return false; // Exact duplicate
                }

                // Rule 3: Multiple MODIFY events of different subtypes - keep only the first
                if last_event.class == EventClass::Modify && event_class == EventClass::Modify {
                    return false; // Suppress subsequent modify variants
                }
            }
        }

        // Update tracking for this path
        self.last_event_per_path.insert(
            path.clone(),
            LastEvent {
                kind: kind.clone(),
                class: event_class,
                timestamp: now,
            },
        );

        // Also update the exact event cache
        let event_key = EventKey { path, kind };
        self.cache.insert(event_key, now);

        true
    }
}

/// Some backends report generic create/remove kinds, including Windows.
/// Remember directory names while they exist so removal can still be typed.
#[derive(Default)]
struct DirectoryKinds {
    paths: HashSet<PathBuf>,
}

impl DirectoryKinds {
    fn record_tree(&mut self, root: &Path, recursive: bool) {
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            let Ok(metadata) = path.symlink_metadata() else {
                continue;
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            self.paths.insert(path.clone());
            if recursive {
                if let Ok(entries) = path.read_dir() {
                    pending.extend(entries.filter_map(|entry| entry.ok().map(|e| e.path())));
                }
            }
        }
    }

    fn forget_tree(&mut self, root: &Path) {
        self.paths.retain(|path| !path.starts_with(root));
    }

    fn observe(&mut self, event: &mut Event, recursive: bool) {
        use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};

        match event.kind {
            EventKind::Create(kind) => {
                if kind == CreateKind::Any {
                    if let Some(metadata) =
                        event.paths.first().and_then(|p| p.symlink_metadata().ok())
                    {
                        event.kind = EventKind::Create(if metadata.is_dir() {
                            CreateKind::Folder
                        } else {
                            CreateKind::File
                        });
                    }
                }
                for path in &event.paths {
                    self.record_tree(path, recursive);
                }
            }
            EventKind::Remove(kind) => {
                if kind == RemoveKind::Any
                    && event.paths.first().is_some_and(|p| self.paths.contains(p))
                {
                    event.kind = EventKind::Remove(RemoveKind::Folder);
                }
                for path in &event.paths {
                    self.forget_tree(path);
                }
            }
            EventKind::Modify(ModifyKind::Name(mode)) => {
                if matches!(mode, RenameMode::From | RenameMode::Any) {
                    if let Some(path) = event.paths.first() {
                        if !path.exists() && self.paths.contains(path) {
                            event.kind = EventKind::Remove(RemoveKind::Folder);
                        }
                        self.forget_tree(path);
                    }
                } else if mode == RenameMode::Both {
                    if let Some(path) = event.paths.first() {
                        self.forget_tree(path);
                    }
                }
                if mode != RenameMode::From {
                    for path in &event.paths {
                        self.record_tree(path, recursive);
                    }
                }
            }
            _ => {}
        }
    }
}

pub struct Watchman {
    config: Config,
    event_cache: RefCell<EventCache>,
    directory_kinds: RefCell<DirectoryKinds>,
}

impl Watchman {
    pub fn new(config: Config) -> Result<Self> {
        let event_cache = RefCell::new(EventCache::new(config.debounce_ms));
        Ok(Watchman {
            config,
            event_cache,
            directory_kinds: RefCell::default(),
        })
    }

    pub fn recursive_mode(&self) -> RecursiveMode {
        match &self.config.recursive {
            true => RecursiveMode::Recursive,
            false => RecursiveMode::NonRecursive,
        }
    }

    fn should_process_event(&self, event_kind: &EventKind) -> bool {
        // If no filters are set, process all events
        let Some(ref filters) = self.config.filters else {
            return true;
        };

        // Check if the event matches any of the enabled filters
        filters.iter().any(|filter| match (filter, event_kind) {
            (EventFilter::Create, EventKind::Create(_)) => true,
            (EventFilter::Modify, EventKind::Modify(_)) => true,
            (EventFilter::Remove, EventKind::Remove(_)) => true,
            (EventFilter::Rename, EventKind::Modify(notify::event::ModifyKind::Name(_))) => true,
            _ => false,
        })
    }

    /// Extract event kind string from event (same logic as emit_json but returns Option)
    fn get_event_kind_str(&self, event: &Event) -> Option<String> {
        let kind_str = match &event.kind {
            EventKind::Create(create_kind) => {
                use notify::event::CreateKind;
                match create_kind {
                    CreateKind::File => "CREATE_FILE",
                    CreateKind::Folder => "CREATE_FOLDER",
                    _ => "CREATE",
                }
            }
            EventKind::Modify(modify_kind) => {
                use notify::event::{DataChange, ModifyKind, RenameMode};
                match modify_kind {
                    ModifyKind::Data(DataChange::Content) => "MODIFY_CONTENT",
                    ModifyKind::Data(DataChange::Size) => "MODIFY_SIZE",
                    ModifyKind::Data(_) => "MODIFY_DATA",
                    ModifyKind::Metadata(_) => return None, // Ignore metadata
                    ModifyKind::Name(rename_mode) => {
                        let file_exists = !event.paths.is_empty() && event.paths[0].exists();
                        match (rename_mode, file_exists) {
                            (RenameMode::From, false) | (RenameMode::Any, false) => "REMOVE_FILE",
                            (RenameMode::From, true) => "RENAME_FROM",
                            (RenameMode::To, _) => "RENAME_TO",
                            (RenameMode::Both, _) => "RENAME_BOTH",
                            (RenameMode::Any, true) => "RENAME",
                            _ => "RENAME",
                        }
                    }
                    _ => "MODIFY",
                }
            }
            EventKind::Remove(remove_kind) => {
                use notify::event::RemoveKind;
                match remove_kind {
                    RemoveKind::File => "REMOVE_FILE",
                    RemoveKind::Folder => "REMOVE_FOLDER",
                    _ => "REMOVE",
                }
            }
            EventKind::Access(_) => return None, // Ignore access
            EventKind::Any => "ANY",
            EventKind::Other => "OTHER",
        };

        Some(kind_str.to_string())
    }

    fn handle_event(&self, mut event: Event) -> Result<()> {
        // Track kinds before filtering: a create may be needed to type a later
        // removal even when the caller only subscribes to removals.
        self.directory_kinds
            .borrow_mut()
            .observe(&mut event, self.config.recursive);
        // Check if this event should be processed based on filters
        if !self.should_process_event(&event.kind) {
            return Ok(());
        }

        // Get event kind string - returns None for metadata/access events
        let Some(kind_str) = self.get_event_kind_str(&event) else {
            return Ok(());
        };

        // Check for duplicates using the smart event cache
        // For events with multiple paths, check each path separately
        for path in &event.paths {
            // Use the new API: pass path and kind directly
            if !self
                .event_cache
                .borrow_mut()
                .should_process(path.clone(), kind_str.clone())
            {
                // Skip this entire event if any path is a duplicate or spurious
                return Ok(());
            }
        }

        if self.config.json_output {
            self.emit_json(event)?;
        } else {
            eprintln!("Event: {:?}", event);
        }
        Ok(())
    }

    pub fn emit_json(&self, event: Event) -> Result<()> {
        let paths: Vec<String> = event
            .paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        let kind_str = match &event.kind {
            EventKind::Create(create_kind) => {
                use notify::event::CreateKind;
                match create_kind {
                    CreateKind::File => "CREATE_FILE",
                    CreateKind::Folder => "CREATE_FOLDER",
                    _ => "CREATE",
                }
            }
            EventKind::Modify(modify_kind) => {
                use notify::event::{DataChange, ModifyKind, RenameMode};
                match modify_kind {
                    ModifyKind::Data(DataChange::Content) => "MODIFY_CONTENT",
                    ModifyKind::Data(DataChange::Size) => "MODIFY_SIZE",
                    ModifyKind::Data(_) => "MODIFY_DATA",
                    // Ignore metadata events
                    ModifyKind::Metadata(_) => return Ok(()),
                    ModifyKind::Name(rename_mode) => {
                        // Check if this is actually a delete disguised as a rename
                        // When a file is deleted, macOS sometimes reports it as a rename where the file doesn't exist
                        let file_exists = !event.paths.is_empty() && event.paths[0].exists();

                        match (rename_mode, file_exists) {
                            (RenameMode::From, false) | (RenameMode::Any, false) => "REMOVE_FILE",
                            (RenameMode::From, true) => "RENAME_FROM",
                            (RenameMode::To, _) => "RENAME_TO",
                            (RenameMode::Both, _) => "RENAME_BOTH",
                            (RenameMode::Any, true) => "RENAME",
                            _ => "RENAME",
                        }
                    }
                    _ => "MODIFY",
                }
            }
            EventKind::Remove(remove_kind) => {
                use notify::event::RemoveKind;
                match remove_kind {
                    RemoveKind::File => "REMOVE_FILE",
                    RemoveKind::Folder => "REMOVE_FOLDER",
                    _ => "REMOVE",
                }
            }
            EventKind::Access(_) => return Ok(()), // Ignore access events
            EventKind::Any => "ANY",
            EventKind::Other => "OTHER",
        };

        let json = json!({
            "kind": kind_str,
            "paths": paths,
        });

        let json_str = serde_json::to_string(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        // Handle stdout write errors gracefully (e.g., when pipe is broken)
        let mut stdout = io::stdout();
        if let Err(e) = writeln!(stdout, "{}", json_str) {
            // If we can't write to stdout (broken pipe), exit gracefully
            if e.kind() == io::ErrorKind::BrokenPipe {
                std::process::exit(0);
            }
            return Err(std::io::Error::new(io::ErrorKind::Other, e.to_string()).into());
        }

        Ok(())
    }

    pub fn run(&self) -> Result<()> {
        let (tx, rx) = mpsc::channel::<Result<Event>>();

        let mut watcher = notify::recommended_watcher(tx)?;

        for path in &self.config.paths {
            watcher.watch(path, self.recursive_mode())?;
        }

        for path in &self.config.paths {
            self.directory_kinds
                .borrow_mut()
                .record_tree(path, self.config.recursive);
        }

        for res in rx {
            match res {
                Ok(event) => self.handle_event(event)?,
                Err(e) => eprintln!("watch error: {:?}", e),
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};

    #[test]
    fn generic_folder_events_keep_their_type_after_removal() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("folder");
        std::fs::create_dir(&path).unwrap();
        let mut kinds = DirectoryKinds::default();
        let mut created = Event::new(EventKind::Create(CreateKind::Any)).add_path(path.clone());
        kinds.observe(&mut created, true);
        assert_eq!(created.kind, EventKind::Create(CreateKind::Folder));

        std::fs::remove_dir(&path).unwrap();
        let mut removed = Event::new(EventKind::Remove(RemoveKind::Any)).add_path(path.clone());
        kinds.observe(&mut removed, true);
        assert_eq!(removed.kind, EventKind::Remove(RemoveKind::Folder));
        assert!(!kinds.paths.contains(&path));
    }

    #[test]
    fn startup_and_moved_in_trees_preserve_nested_directory_kinds() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        std::fs::create_dir_all(old.join("nested")).unwrap();
        let mut kinds = DirectoryKinds::default();
        kinds.record_tree(root.path(), true);

        let new = root.path().join("new");
        std::fs::rename(&old, &new).unwrap();
        let mut renamed = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(old.clone())
            .add_path(new.clone());
        kinds.observe(&mut renamed, true);
        assert!(!kinds.paths.contains(&old));
        assert!(kinds.paths.contains(&new.join("nested")));

        std::fs::remove_dir(new.join("nested")).unwrap();
        let mut removed =
            Event::new(EventKind::Remove(RemoveKind::Any)).add_path(new.join("nested"));
        kinds.observe(&mut removed, true);
        assert_eq!(removed.kind, EventKind::Remove(RemoveKind::Folder));
    }

    #[test]
    fn generic_file_events_are_not_classified_as_folders() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, "content").unwrap();
        let mut kinds = DirectoryKinds::default();
        kinds.record_tree(root.path(), true);
        let mut created = Event::new(EventKind::Create(CreateKind::Any)).add_path(path.clone());
        kinds.observe(&mut created, true);
        assert_eq!(created.kind, EventKind::Create(CreateKind::File));
        std::fs::remove_file(&path).unwrap();
        let mut removed = Event::new(EventKind::Remove(RemoveKind::Any)).add_path(path);
        kinds.observe(&mut removed, true);
        assert_eq!(removed.kind, EventKind::Remove(RemoveKind::Any));
    }
}
