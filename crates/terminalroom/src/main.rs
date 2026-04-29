use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use clap::Parser;

use terminalroom::{db, session};

#[derive(Debug, Parser)]
#[command(name = "terminalroom")]
#[command(about = "Cull and develop RAW images in the terminal")]
struct Cli {
    path: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let session = session::discover(&cli.path)?;
    let mut db = db::Db::open(&session.root)?;
    let now = unix_now();
    let records = db.sync_files(&session.files, now)?;

    println!("session root: {}", session.root.display());
    println!("files: {}", records.len());
    for record in &records {
        println!(
            "  {:<7}  {}",
            record.state.as_str(),
            record.canonical_path.display()
        );
    }
    Ok(())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
