use crate::config::{Config, EventFilter};

use notify::{Event, EventKind, RecursiveMode, Result, Watcher};
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
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
        self.cache.retain(|_, &mut timestamp| {
            now.duration_since(timestamp) < self.debounce_duration
        });
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
        self.last_event_per_path.insert(path.clone(), LastEvent {
            kind: kind.clone(),
            class: event_class,
            timestamp: now,
        });

        // Also update the exact event cache
        let event_key = EventKey { path, kind };
        self.cache.insert(event_key, now);

        true
    }
}

pub struct Watchman {
    config: Config,
    event_cache: RefCell<EventCache>,
}

impl Watchman {
    pub fn new(config: Config) -> Result<Self> {
        let event_cache = RefCell::new(EventCache::new(config.debounce_ms));
        Ok(Watchman { config, event_cache })
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
                use notify::event::{ModifyKind, DataChange, RenameMode};
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

    fn handle_event(&self, event: Event) -> Result<()> {
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
            if !self.event_cache.borrow_mut().should_process(path.clone(), kind_str.clone()) {
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
                use notify::event::{ModifyKind, DataChange, RenameMode};
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

        for res in rx {
            match res {
                Ok(event) => self.handle_event(event)?,
                Err(e) => eprintln!("watch error: {:?}", e),
            }
        }

        Ok(())
    }
}
