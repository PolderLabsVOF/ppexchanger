//! XDG-style filesystem locations for `lanchat`.
//!
//! Everything lives under `~/.config/lanchat`. We respect `$XDG_CONFIG_HOME`
//! when set (the XDG Base Directory spec) and fall back to `~/.config`.

use std::io;
use std::path::PathBuf;

pub fn config_dir() -> io::Result<PathBuf> {
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

pub fn identity_path() -> io::Result<PathBuf> {
    Ok(config_dir()?.join("identity"))
}

pub fn contacts_path() -> io::Result<PathBuf> {
    Ok(config_dir()?.join("contacts"))
}

fn home_dir() -> io::Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "HOME not set"))
}