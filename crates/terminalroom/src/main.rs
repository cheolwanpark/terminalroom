use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "terminalroom")]
#[command(about = "Cull and develop RAW images in the terminal")]
struct Cli {
    path: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli
        .path
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", cli.path.display()))?;

    if !path.is_file() && !path.is_dir() {
        bail!("{} is not a file or directory", path.display());
    }

    println!("terminalroom scaffold initialized for {}", path.display());
    Ok(())
}
