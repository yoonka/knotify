use crate::config::{Config, EventFilter};

use notify::{Event, EventKind, RecursiveMode, Result, Watcher};
use serde_json::json;
use std::io::{self, Write};
use std::sync::mpsc;

pub struct Watchman {
    config: Config,
}

impl Watchman {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Watchman { config })
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

    fn handle_event(&self, event: Event) -> Result<()> {
        // Check if this event should be processed based on filters
        if !self.should_process_event(&event.kind) {
            return Ok(());
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
