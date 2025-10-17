use std::path::PathBuf;
use clap::Parser;

/// CLI argument parser
#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Args {
    /// One or more paths to watch
    #[arg(long, required = true)]
    pub path: Vec<PathBuf>,

    /// Watch directories recursively
    #[arg(long, default_value_t = false)]
    pub recursive: bool,

    /// Backend: auto, inotify, or kqueue
    #[arg(long, default_value = "auto")]
    pub backend: String,

    /// Output JSON (default true)
    #[arg(long, default_value_t = true)]
    pub json: bool,

    /// Debounce delay in milliseconds
    #[arg(long, default_value_t = 100)]
    pub debounce: u64,

    /// Log level (error, warn, info, debug, trace)
    #[arg(long, default_value = "info")]
    pub log: String,

    /// Exit after first event (testing convenience)
    #[arg(long, default_value_t = false)]
    pub once: bool,

    /// Filter events by type (comma-separated): create, modify, remove, rename
    /// Example: --filter create,modify
    #[arg(long, value_delimiter = ',')]
    pub filter: Option<Vec<String>>,
}

/// Event filter types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventFilter {
    Create,
    Modify,
    Remove,
    Rename,
}

impl EventFilter {
    pub fn from_string(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "create" => Some(EventFilter::Create),
            "modify" => Some(EventFilter::Modify),
            "remove" => Some(EventFilter::Remove),
            "rename" => Some(EventFilter::Rename),
            _ => None,
        }
    }
}

/// Internal configuration used by the watcher
#[derive(Debug, Clone)]
pub struct Config {
    pub paths: Vec<PathBuf>,
    pub recursive: bool,
    // pub backend: Backend,
    pub json_output: bool,
    // pub debounce_ms: u64,
    // pub log_level: LogLevel,
    // pub once: bool,
    pub filters: Option<Vec<EventFilter>>,
}

// /// Enum for watcher backend
// #[derive(Debug, Clone)]
// pub enum Backend {
//     Auto,
//     Inotify,
//     Kqueue,
// }

// /// Enum for logging level
// #[derive(Debug, Clone)]
// pub enum LogLevel {
//     Error,
//     Warn,
//     Info,
//     Debug,
//     Trace,
// }

impl Config {
    pub fn from_args(args: Args) -> Self {
        let filters = args.filter.map(|filter_strings| {
            filter_strings
                .iter()
                .filter_map(|s| EventFilter::from_string(s))
                .collect()
        });

        Self {
            paths: args.path,
            recursive: args.recursive,
            // backend: match args.backend.as_str() {
            //     "inotify" => Backend::Inotify,
            //     "kqueue" => Backend::Kqueue,
            //     _ => Backend::Auto,
            // },
            json_output: args.json,
            // debounce_ms: args.debounce,
            // log_level: match args.log.as_str() {
            //     "error" => LogLevel::Error,
            //     "warn" => LogLevel::Warn,
            //     "debug" => LogLevel::Debug,
            //     "trace" => LogLevel::Trace,
            //     _ => LogLevel::Info,
            // },
            // once: args.once,
            filters,
        }
    }
}
