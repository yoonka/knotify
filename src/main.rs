use clap::Parser;

mod config;
use config::{Args, Config};
mod watcher;
use watcher::Watchman;
mod catalog;
mod hints;
mod native;
mod wire;

#[derive(Parser)]
struct CatalogArgs {
    #[arg(long)]
    root: std::path::PathBuf,
    #[arg(long)]
    database: std::path::PathBuf,
    #[arg(long)]
    exclude: Vec<std::path::PathBuf>,
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut args = std::env::args_os();
    let program = args.next().unwrap();
    if args.next().as_deref() == Some(std::ffi::OsStr::new("catalog")) {
        let args = CatalogArgs::parse_from(std::iter::once(program).chain(args));
        match catalog::Catalog::open(&args.root, &args.database, args.exclude) {
            Ok(c) => catalog::run(c),
            Err(e) => {
                let _ = wire::write(
                    &mut std::io::stdout(),
                    &wire::message(
                        "error",
                        [
                            ("id", (-1i64).into()),
                            ("code", "root_identity".into()),
                            ("message", e.to_string().into()),
                        ],
                    ),
                );
                Err(e)
            }
        }
    } else {
        let args = Args::parse();
        if args.protocol == "etf" {
            if args.path.len() != 1 {
                return Err("ETF hint mode requires exactly one root".into());
            }
            hints::run(args.path[0].clone(), args.recursive, args.debounce)
        } else {
            // Wrappers opt in without changing standalone legacy CLI defaults.
            // Older binaries safely ignore this environment variable.
            if std::env::var("KNOTIFY_EXIT_ON_EOF").as_deref() == Ok("1") {
                std::thread::spawn(|| {
                    use std::io::Read;
                    let mut input = std::io::stdin().lock();
                    let mut buffer = [0; 64];
                    loop {
                        match input.read(&mut buffer) {
                            Ok(0) | Err(_) => std::process::exit(0),
                            Ok(_) => {}
                        }
                    }
                });
            }
            let config = Config::from_args(args);
            Watchman::new(config)?.run()?;
            Ok(())
        }
    }
}
