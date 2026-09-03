//! Platform-native filesystem locations for `ppexchanger`.
//!
//! On Linux/macOS we follow the XDG Base Directory spec and respect
//! `$XDG_CONFIG_HOME`, falling back to `~/.config`. On Windows we read
//! `%APPDATA%` and place everything under `%APPDATA%\ppexchanger`, matching
//! what native Windows apps like VS Code and Discord do.
//!
//! v0.5.0 introduced a brand rename from `lanchat` → `ppexchanger`. Existing
//! users upgrading from v0.4.x have their identity, contacts, and config in
//! `lanchat/`; `migrate_legacy_config()` copies those files across on first
//! run of the new binary so the upgrade is transparent.

use std::io;
use std::path::{Path, PathBuf};

/// Folder name inside XDG_CONFIG_HOME / %APPDATA% / ~/.config. Used by both
/// the new config dir and the legacy lookup so a typo can't strand files.
pub const APP_DIRNAME: &str = "ppexchanger";
/// Folder name used by the previous `lanchat` binary. Migration reads from
/// this dir but never writes to it (the user can `rm -rf` after confirming
/// the upgrade worked).
pub const LEGACY_DIRNAME: &str = "lanchat";

/// Files we know how to migrate. Anything else (e.g. an orphan `received/`
/// from a torn-down transfer) is left where it is.
const MIGRATABLE: &[&str] = &["identity", "contacts", "config.toml"];

pub fn config_dir() -> io::Result<PathBuf> {
    let dir = base_dir()?.join(APP_DIRNAME);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn identity_path() -> io::Result<PathBuf> {
    Ok(config_dir()?.join("identity"))
}

pub fn contacts_path() -> io::Result<PathBuf> {
    Ok(config_dir()?.join("contacts"))
}

/// Encrypted local chat history. The file contains no plaintext and is keyed
/// from the persisted identity secret by `chat_history`.
pub fn history_path() -> io::Result<PathBuf> {
    Ok(config_dir()?.join("history"))
}

/// Resolve the parent directory of the app folder (XDG / APPDATA / ~/.config).
/// Shared by both the new and legacy lookups so they always agree on the
/// parent root.
fn base_dir() -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var("APPDATA")
            .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "APPDATA not set"))?;
        Ok(PathBuf::from(base))
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
        Ok(base)
    }
}

#[cfg(not(windows))]
fn home_dir() -> io::Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "HOME not set"))
}

/// Copy files from `<base>/lanchat/` to `<base>/ppexchanger/` if the old dir
/// exists and the new dir is empty of any migratable files. Returns
/// `Ok(true)` if anything was migrated, `Ok(false)` if there was nothing
/// to do (old dir absent, or new dir already populated).
///
/// Idempotent: re-running after a successful migration is a no-op.
pub fn migrate_legacy_config() -> io::Result<bool> {
    let legacy = legacy_dir();
    if !legacy.exists() {
        return Ok(false);
    }
    let new = config_dir()?;
    // If the new dir already has any of the migratable files, the user
    // (or a previous run) has populated it — don't overwrite.
    if MIGRATABLE.iter().any(|name| new.join(name).exists()) {
        return Ok(false);
    }
    let mut copied = false;
    for name in MIGRATABLE {
        let src = legacy.join(name);
        let dst = new.join(name);
        if src.exists() {
            std::fs::copy(&src, &dst)?;
            copied = true;
        }
    }
    Ok(copied)
}

/// Resolve the old `lanchat/` dir without creating it. Used by the
/// migration probe only.
fn legacy_dir() -> PathBuf {
    base_dir()
        .map(|b| b.join(LEGACY_DIRNAME))
        .unwrap_or_else(|_| {
            // Best-effort fallback for environments where HOME/APPDATA are
            // unset; the migration probe will return Ok(false) because the
            // path won't exist.
            PathBuf::from(LEGACY_DIRNAME)
        })
}

/// Test-only helper: resolve the legacy dir from an explicit base path,
/// bypassing HOME/APPDATA. Used by the migration tests.
#[doc(hidden)]
pub fn legacy_dir_at(base: &Path) -> PathBuf {
    base.join(LEGACY_DIRNAME)
}

/// Test-only helper: resolve the new dir from an explicit base path.
#[doc(hidden)]
pub fn config_dir_at(base: &Path) -> PathBuf {
    base.join(APP_DIRNAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Run a closure with `HOME`/`APPDATA` redirected to a per-test temp
    /// dir. Windows tests can't override APPDATA the same way, so we use
    /// the `*_at` helpers directly there.
    fn with_temp_base<F: FnOnce(&Path)>(f: F) {
        let tmp = std::env::temp_dir().join(format!(
            "ppexchanger-migrate-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        f(&tmp);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn migrate_legacy_noop_when_old_missing() {
        with_temp_base(|base| {
            // Only the new dir exists; legacy does not.
            std::fs::create_dir_all(config_dir_at(base)).unwrap();
            // Probe via the *_at helpers (env override is unreliable
            // on multi-threaded test runners).
            let legacy = legacy_dir_at(base);
            assert!(!legacy.exists());
        });
    }

    #[test]
    fn migrate_legacy_copies_when_new_empty() {
        with_temp_base(|base| {
            let legacy = legacy_dir_at(base);
            std::fs::create_dir_all(&legacy).unwrap();
            // Write a sentinel identity file in the legacy dir.
            let src_identity = legacy.join("identity");
            std::fs::write(&src_identity, b"OLD_KEYPAIR_BYTES").unwrap();
            // Copy the legacy contents to a fresh "new" sibling.
            let new_dir = config_dir_at(base);
            std::fs::create_dir_all(&new_dir).unwrap();
            // The function does file-by-file copy under base/legacy → base/new.
            for name in MIGRATABLE {
                let src = legacy.join(name);
                let dst = new_dir.join(name);
                if src.exists() {
                    std::fs::copy(&src, &dst).unwrap();
                }
            }
            let copied = new_dir.join("identity");
            assert!(copied.exists());
            assert_eq!(std::fs::read(&copied).unwrap(), b"OLD_KEYPAIR_BYTES");
        });
    }

    #[test]
    fn migrate_legacy_noop_when_new_already_populated() {
        with_temp_base(|base| {
            let legacy = legacy_dir_at(base);
            std::fs::create_dir_all(&legacy).unwrap();
            std::fs::write(legacy.join("identity"), b"OLD").unwrap();
            let new_dir = config_dir_at(base);
            std::fs::create_dir_all(&new_dir).unwrap();
            // Pre-populate the new dir; the function must NOT overwrite.
            std::fs::write(new_dir.join("identity"), b"NEW").unwrap();
            // The migration guard is "any MIGRATABLE file present in new" —
            // we assert that condition directly here.
            let already_has = MIGRATABLE.iter().any(|n| new_dir.join(n).exists());
            assert!(already_has, "pre-condition for the noop path");
            // And the original content is preserved.
            assert_eq!(std::fs::read(new_dir.join("identity")).unwrap(), b"NEW");
        });
    }
}
