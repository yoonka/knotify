use clap::Parser;

mod config;
use config::{Args, Config};
mod watcher;
use watcher::Watchman;
fn main() -> notify::Result<()>{


    let args = Args::parse();
    let config = Config::from_args(args);

    let watchman = Watchman::new(config)?;

    watchman.run()

}


