use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use clap::Parser;

use terminalroom::app::App;
use terminalroom::cache::Cache;
use terminalroom::{db, session, tui};

#[derive(Debug, Parser)]
#[command(name = "terminalroom")]
#[command(about = "Cull and develop RAW images in the terminal")]
struct Cli {
    path: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let session = session::discover(&cli.path)?;
    let mut db = db::Db::open_global()?;
    let now = unix_now();
    let mut rows = Vec::with_capacity(session.files.len());
    for f in &session.files {
        rows.push(db.upsert_file(f, now)?);
    }
    let cache = Cache::new()?;
    cache.prune_orphans(&db).ok();
    let mut app = App::init(session, db, rows)?;
    tui::run(&mut app, cache)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
