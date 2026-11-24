//! The login hand-off page. The browser opener gets the path of a private
//! HTML file that redirects to the one-time login URL, so the code never
//! appears in any process's argv. The file lives in `<data dir>/run/` (mode
//! 0700), has mode 0600 and is deleted as soon as the code is spent.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::login_url;
use super::security::{LoginCode, random_hex};

const PREFIX: &str = "login-";
const SUFFIX: &str = ".html";
const POLL: Duration = Duration::from_millis(100);

/// `<data dir>/run`.
#[must_use]
pub fn run_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("run")
}

/// Creates (if needed) the private run dir and verifies that nobody but the
/// owner can enter it.
///
/// # Errors
/// I/O errors, or `PermissionDenied` when the directory is group/world accessible.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    check_owner_only(dir)
}

/// Unix: refuses a directory that group or others can enter. Elsewhere the
/// per-user profile ACLs already keep the data dir private, so nothing to check.
#[cfg(unix)]
fn check_owner_only(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(dir)?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is accessible by other users (mode {:o}); fix with chmod 700",
                dir.display(),
                mode & 0o777
            ),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // same signature as the unix check
fn check_owner_only(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Removes leftover `login-*.html` files (e.g. after a killed run).
pub fn sweep_stale(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(PREFIX) && name.ends_with(SUFFIX) {
            let _ = fs::remove_file(e.path());
        }
    }
}

fn page_html(url: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"0; url={url}\">\
         <title>Recalla</title><p>Opening Recalla... <a href=\"{url}\">Continue</a></p>\n"
    )
}

/// Writes the redirect page (mode 0600) into `dir` and returns its path.
///
/// # Errors
/// I/O errors, including a bad `dir` (see [`ensure_private_dir`]).
pub fn write_page(dir: &Path, port: u16, code: &str) -> io::Result<PathBuf> {
    ensure_private_dir(dir)?;
    let path = dir.join(format!("{PREFIX}{}{SUFFIX}", &random_hex()?[..16]));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&path)?;
    f.write_all(page_html(&login_url(port, code)).as_bytes())?;
    Ok(path)
}

/// Deletes the page once the code is redeemed or expired (a watcher thread),
/// or when [`PageCleanup::finish`] is called (server shutdown).
pub struct PageCleanup {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
}

impl PageCleanup {
    #[must_use]
    pub fn spawn(path: PathBuf, code: Arc<LoginCode>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (path.clone(), Arc::clone(&stop));
        let watcher = thread::spawn(move || {
            while !s.load(Ordering::SeqCst) && !code.is_spent() {
                thread::sleep(POLL);
            }
            let _ = fs::remove_file(p);
        });
        Self {
            path,
            stop,
            watcher: Some(watcher),
        }
    }

    /// Stops the watcher and removes the file now.
    pub fn finish(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(w) = self.watcher.take() {
            let _ = w.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::security::LOGIN_TTL;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn wait_gone(p: &Path) -> bool {
        (0..60).any(|_| {
            thread::sleep(Duration::from_millis(50));
            !p.exists()
        })
    }

    #[test]
    fn page_is_private_and_redirects() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = run_dir(tmp.path());
        let path = write_page(&dir, 4242, "abc").unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("login-") && name.rsplit('.').next() == Some("html"));
        let html = fs::read_to_string(&path).unwrap();
        assert!(html.contains("http-equiv=\"refresh\""));
        assert!(html.contains("http://127.0.0.1:4242/?c=abc"));
    }

    #[test]
    fn open_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = run_dir(tmp.path());
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let e = write_page(&dir, 1, "x").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn removed_after_redemption() {
        let tmp = tempfile::tempdir().unwrap();
        let code = Arc::new(LoginCode::generate(LOGIN_TTL).unwrap());
        let path = write_page(&run_dir(tmp.path()), 1, code.as_str()).unwrap();
        let cleanup = PageCleanup::spawn(path.clone(), Arc::clone(&code));
        thread::sleep(Duration::from_millis(250));
        assert!(path.exists(), "kept while the code is live");
        assert!(code.redeem(code.as_str()));
        assert!(wait_gone(&path));
        cleanup.finish();
    }

    #[test]
    fn removed_after_expiry() {
        let tmp = tempfile::tempdir().unwrap();
        let code = Arc::new(LoginCode::generate(Duration::from_millis(300)).unwrap());
        let path = write_page(&run_dir(tmp.path()), 1, code.as_str()).unwrap();
        let cleanup = PageCleanup::spawn(path.clone(), code);
        assert!(wait_gone(&path));
        cleanup.finish();
    }

    #[test]
    fn removed_on_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let code = Arc::new(LoginCode::generate(LOGIN_TTL).unwrap());
        let path = write_page(&run_dir(tmp.path()), 1, code.as_str()).unwrap();
        PageCleanup::spawn(path.clone(), code).finish();
        assert!(!path.exists());
    }

    #[test]
    fn sweep_removes_only_stale_login_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = run_dir(tmp.path());
        ensure_private_dir(&dir).unwrap();
        for n in ["login-aa.html", "login-bb.html", "keep.txt"] {
            fs::write(dir.join(n), "x").unwrap();
        }
        sweep_stale(&dir);
        let left: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].file_name(), "keep.txt");
    }
}
