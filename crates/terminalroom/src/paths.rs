use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

/// `~/.terminalroom/`. Created on first call.
pub fn app_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
    let dir = home.join(".terminalroom");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create app directory {}", dir.display()))?;
    Ok(dir)
}

/// `~/.terminalroom/db.sqlite`.
pub fn db_path() -> Result<PathBuf> {
    Ok(app_dir()?.join("db.sqlite"))
}

/// `~/.terminalroom/cache/`. Created on first call.
pub fn cache_dir() -> Result<PathBuf> {
    let dir = app_dir()?.join("cache");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create cache directory {}", dir.display()))?;
    Ok(dir)
}

/// `~/.terminalroom/looks/`. Watch directory for XMP sidecars; the Looks
/// modal scans this on open and reconciles registered looks against its
/// contents. Created on first call.
pub fn looks_dir() -> Result<PathBuf> {
    let dir = app_dir()?.join("looks");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create looks directory {}", dir.display()))?;
    Ok(dir)
}
