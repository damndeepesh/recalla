//! Where Recalla keeps its data, and private file/dir creation.
//!
//! Data dir: `$RECALLA_HOME`, else `$XDG_DATA_HOME/recalla`, else
//! `~/.local/share/recalla` (`~` is `$HOME`, or `%USERPROFILE%` on Windows). Dirs are created 0700 and files 0600 (unix).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// SQLite database file name inside the data dir.
pub const DB_FILE: &str = "recalla.db";
/// Archive directory name inside the data dir.
pub const ARCHIVE_DIR: &str = "archive";
/// Markdown mirror directory name inside the data dir.
pub const MIRROR_DIR: &str = "mirror";

/// Windows `canonicalize` returns `\\?\C:\x`; tools and shells want `C:\x`.
/// Drops the prefix (drive paths only; UNC `\\?\UNC\…` is left alone).
#[must_use]
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p,
    }
}

/// The running binary as an absolute path fit to write into hook and MCP
/// config: symlinks resolved, no `\\?\` prefix, and on Windows forward
/// slashes (hooks run under bash, which eats backslashes; Windows accepts `/`).
///
/// # Errors
/// When the executable path cannot be determined or resolved.
pub fn current_exe() -> io::Result<PathBuf> {
    let exe = strip_verbatim(std::env::current_exe().and_then(fs::canonicalize)?);
    Ok(if cfg!(windows) {
        PathBuf::from(exe.to_string_lossy().replace('\\', "/"))
    } else {
        exe
    })
}

/// Pure form of [`data_dir`]. Empty strings count as unset.
#[must_use]
pub fn data_dir_from(
    recalla_home: Option<&str>,
    xdg_data: Option<&str>,
    home: Option<&str>,
) -> Option<PathBuf> {
    let set = |v: Option<&str>| v.filter(|s| !s.is_empty()).map(PathBuf::from);
    set(recalla_home)
        .or_else(|| set(xdg_data).map(|x| x.join("recalla")))
        .or_else(|| set(home).map(|h| h.join(".local/share/recalla")))
}

/// The user's home directory: `$HOME`, else `%USERPROFILE%` (Windows has no
/// `HOME`). Empty values count as unset.
#[must_use]
pub fn home_dir() -> Option<std::ffi::OsString> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
}

/// Data dir from the environment. Non-UTF-8 values count as unset.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    let var = |k: &str| std::env::var(k).ok();
    let home = home_dir().and_then(|h| h.into_string().ok());
    data_dir_from(
        var("RECALLA_HOME").as_deref(),
        var("XDG_DATA_HOME").as_deref(),
        home.as_deref(),
    )
}

/// `<data>/recalla.db`.
#[must_use]
pub fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DB_FILE)
}

/// `<data>/archive`.
#[must_use]
pub fn archive_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(ARCHIVE_DIR)
}

/// `<data>/mirror`.
#[must_use]
pub fn mirror_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(MIRROR_DIR)
}

/// Creates `path` and missing parents. Dirs created here are 0700 on unix;
/// existing dirs keep their permissions.
///
/// # Errors
/// I/O errors from creating the directories.
pub fn ensure_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

/// Permission bits of `dir` if it is an existing directory that grants anything to
/// group or others (unix only; `None` otherwise, including when it can't be read).
#[must_use]
pub fn loose_dir_mode(dir: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(dir).ok().filter(fs::Metadata::is_dir)?;
        let mode = meta.permissions().mode() & 0o777;
        (mode & 0o077 != 0).then_some(mode)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// Restricts an existing file to its owner (0600 on unix; no-op elsewhere).
///
/// # Errors
/// I/O errors from changing permissions.
pub fn restrict_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Creates `path` empty with 0600 if missing, then restricts it to 0600.
/// SQLite creates its `-wal`/`-shm` files with the main file's mode, so
/// calling this before opening the DB keeps those private too.
///
/// # Errors
/// I/O errors from creating the file or changing permissions.
pub fn ensure_private_file(path: &Path) -> io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)?;
    restrict_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_order_and_empty_values() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(data_dir_from(Some("/r"), Some("/x"), Some("/h")), p("/r"));
        assert_eq!(
            data_dir_from(Some(""), Some("/x"), Some("/h")),
            p("/x/recalla")
        );
        assert_eq!(
            data_dir_from(None, Some(""), Some("/h")),
            p("/h/.local/share/recalla")
        );
        assert_eq!(data_dir_from(Some(""), Some(""), Some("")), None);
        assert_eq!(data_dir_from(None, None, None), None);
    }

    #[test]
    fn file_names() {
        let d = Path::new("/d");
        assert_eq!(db_path(d), PathBuf::from("/d/recalla.db"));
        assert_eq!(archive_dir(d), PathBuf::from("/d/archive"));
        assert_eq!(mirror_dir(d), PathBuf::from("/d/mirror"));
    }

    #[cfg(unix)]
    #[test]
    fn private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a/b");
        ensure_dir(&dir).unwrap();
        ensure_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&tmp.path().join("a")), 0o700);

        let file = dir.join("f");
        fs::write(&file, b"keep").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        ensure_private_file(&file).unwrap();
        assert_eq!(mode(&file), 0o600);
        assert_eq!(fs::read(&file).unwrap(), b"keep");

        let fresh = dir.join("g");
        ensure_private_file(&fresh).unwrap();
        assert_eq!(mode(&fresh), 0o600);
    }

    #[test]
    fn verbatim_prefix_is_dropped_for_drive_paths_only() {
        let v = |s: &str| strip_verbatim(PathBuf::from(s));
        assert_eq!(
            v(r"\\?\C:\Users\a\recalla.exe"),
            PathBuf::from(r"C:\Users\a\recalla.exe")
        );
        assert_eq!(v(r"C:\Users\a"), PathBuf::from(r"C:\Users\a"));
        assert_eq!(v(r"\\?\UNC\srv\share"), PathBuf::from(r"\\?\UNC\srv\share"));
        assert_eq!(v("/usr/bin/recalla"), PathBuf::from("/usr/bin/recalla"));
    }

    #[test]
    fn current_exe_is_absolute_and_never_verbatim() {
        let exe = current_exe().unwrap();
        assert!(exe.is_absolute());
        assert!(!exe.to_string_lossy().starts_with(r"\\?\"));
        if cfg!(windows) {
            assert!(!exe.to_string_lossy().contains('\\'));
        }
    }
}
