//! Platform-native filesystem locations for `lanchat`.
//!
//! On Linux/macOS we follow the XDG Base Directory spec and respect
//! `$XDG_CONFIG_HOME`, falling back to `~/.config`. On Windows we read
//! `%APPDATA%` and place everything under `%APPDATA%\lanchat`, matching
//! what native Windows apps like VS Code and Discord do.

use std::io;
use std::path::PathBuf;

pub fn config_dir() -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var("APPDATA").map_err(|_| {
            io::Error::new(io::ErrorKind::NotFound, "APPDATA not set")
        })?;
        let dir = PathBuf::from(base).join("lanchat");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
    #[cfg(not(windows))]
    {
        let base = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                PathBuf::from(xdg)
            } else {
                home_dir()?.join(".config")
            }
        } else {
            home_dir()?.join(".config")
        };
        let dir = base.join("lanchat");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

pub fn identity_path() -> io::Result<PathBuf> {
    Ok(config_dir()?.join("identity"))
}

pub fn contacts_path() -> io::Result<PathBuf> {
    Ok(config_dir()?.join("contacts"))
}

#[cfg(not(windows))]
fn home_dir() -> io::Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "HOME not set"))
}