//! Installing Recalla's hooks into a Claude Code `settings.json`
//! (`recalla hooks install|uninstall|status`).
//!
//! Policy (ADR D-018): refuse a file that is not a JSON object, back it up
//! before every write, write atomically, and touch nothing but our own
//! entries. Key order is kept (`serde_json` feature `preserve_order`).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use thiserror::Error;

/// Settings events and the `recalla hook <sub>` command each one runs.
pub const EVENTS: [(&str, &str); 3] = [
    ("SessionStart", "session-start"),
    ("Stop", "capture"),
    ("PreCompact", "capture"),
];
/// Seconds Claude Code waits for a hook.
const TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Error)]
pub enum CfgError {
    #[error("cannot read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("cannot write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("{} is not valid JSON ({msg}); fix it first, nothing was changed", path.display())]
    Malformed { path: PathBuf, msg: String },
    #[error("{}: {msg}; nothing was changed", path.display())]
    Shape { path: PathBuf, msg: String },
}

/// What an operation did (or, for a dry run, would do).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// One line per event, e.g. `SessionStart: added`.
    pub changes: Vec<String>,
    pub changed: bool,
    /// Backup written before the change (never for dry runs).
    pub backup: Option<PathBuf>,
}

/// Shell-safe form of `exe` + subcommand; quotes the path when needed.
#[must_use]
pub fn command_for(exe: &Path, sub: &str) -> String {
    let p = exe.to_string_lossy();
    let safe = |c: char| c.is_ascii_alphanumeric() || "_./-+:@%=,~".contains(c);
    let p = if !p.is_empty() && p.chars().all(safe) {
        p.into_owned()
    } else {
        let mut q = String::from("\"");
        for c in p.chars() {
            if matches!(c, '"' | '\\' | '$' | '`') {
                q.push('\\');
            }
            q.push(c);
        }
        q + "\""
    };
    format!("{p} hook {sub}")
}

/// True only for a command exactly as `command_for` writes it: one path token
/// (bare, or double-quoted with `\\` escapes) whose file name is `recalla`, then
/// ` hook <sub>`, nothing else. Customized entries (env prefixes, extra flags)
/// and unrelated commands that merely mention recalla are never touched.
fn is_ours(cmd: &str, sub: Option<&str>) -> bool {
    let subs: &[&str] = match sub {
        Some(s) => &[s][..],
        None => &["session-start", "capture"],
    };
    subs.iter().any(|s| {
        cmd.strip_suffix(&format!(" hook {s}"))
            .and_then(exe_token)
            .is_some_and(|exe| Path::new(&exe).file_name().is_some_and(|n| n == "recalla"))
    })
}

/// The exe path from a command prefix, if it is a single bare or quoted token.
fn exe_token(t: &str) -> Option<String> {
    if let Some(inner) = t.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => out.push(chars.next()?),
                '"' => return None,
                c => out.push(c),
            }
        }
        return Some(out);
    }
    (!t.is_empty()
        && !t.chars().any(char::is_whitespace)
        && !t.contains(['"', '\'', '$', '`', ';', '&', '|']))
    .then(|| t.to_owned())
}

/// Mutable command strings of our hook entries in one event's groups.
fn our_commands<'a>(
    groups: &'a mut [Value],
    sub: Option<&'a str>,
) -> impl Iterator<Item = &'a mut Value> {
    groups
        .iter_mut()
        .filter_map(|g| g.get_mut("hooks")?.as_array_mut())
        .flatten()
        .filter_map(|h| h.get_mut("command"))
        .filter(move |c| c.as_str().is_some_and(|s| is_ours(s, sub)))
}

fn shape(path: &Path, msg: &str) -> CfgError {
    CfgError::Shape {
        path: path.to_owned(),
        msg: msg.to_owned(),
    }
}

/// Adds or updates our entries in `settings`; returns one line per event.
fn install_into(path: &Path, settings: &mut Value, exe: &Path) -> Result<Vec<String>, CfgError> {
    let root = settings
        .as_object_mut()
        .ok_or_else(|| shape(path, "root is not an object"))?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| shape(path, "`hooks` is not an object"))?;
    let mut lines = Vec::new();
    for (event, sub) in EVENTS {
        let want = command_for(exe, sub);
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| shape(path, &format!("`hooks.{event}` is not an array")))?;
        let mut found = false;
        let mut updated = false;
        for cmd in our_commands(groups, Some(sub)) {
            found = true;
            if cmd.as_str() != Some(&want) {
                *cmd = Value::String(want.clone());
                updated = true;
            }
        }
        if !found {
            groups.push(
                json!({"hooks": [{"type": "command", "command": want, "timeout": TIMEOUT_SECS}]}),
            );
        }
        let what = if !found {
            "added"
        } else if updated {
            "updated"
        } else {
            "already installed"
        };
        lines.push(format!("{event}: {what}"));
    }
    Ok(lines)
}

/// Removes our entries from `settings`; returns one line per event touched.
fn uninstall_from(path: &Path, settings: &mut Value) -> Result<Vec<String>, CfgError> {
    let root = settings
        .as_object_mut()
        .ok_or_else(|| shape(path, "root is not an object"))?;
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(Vec::new());
    };
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| shape(path, "`hooks` is not an object"))?;
    let mut lines = Vec::new();
    for (event, _) in EVENTS {
        let Some(groups) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        let mut removed = 0;
        for g in groups.iter_mut() {
            let Some(list) = g.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = list.len();
            list.retain(|h| {
                !h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| is_ours(c, None))
            });
            removed += before - list.len();
            if list.is_empty() && before > 0 {
                *g = Value::Null; // emptied by us: dropped below
            }
        }
        groups.retain(|g| !g.is_null());
        if removed > 0 {
            lines.push(format!("{event}: removed"));
            if groups.is_empty() {
                hooks.shift_remove(event);
            }
        }
    }
    if hooks.is_empty() && !lines.is_empty() {
        root.shift_remove("hooks");
    }
    Ok(lines)
}

/// Which events have a Recalla entry.
///
/// # Errors
/// Unreadable or malformed settings.
pub fn installed(path: &Path) -> Result<Vec<(&'static str, bool)>, CfgError> {
    let mut settings = load(path)?.map_or_else(|| json!({}), |(v, _)| v);
    Ok(EVENTS
        .iter()
        .map(|&(event, sub)| {
            let found = settings
                .pointer_mut(&format!("/hooks/{event}"))
                .and_then(Value::as_array_mut)
                .is_some_and(|g| our_commands(g, Some(sub)).next().is_some());
            (event, found)
        })
        .collect())
}

/// Parsed settings plus their raw text, or `None` when the file is missing.
fn load(path: &Path) -> Result<Option<(Value, String)>, CfgError> {
    let raw = match fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CfgError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let value: Value = if raw.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&raw).map_err(|e| CfgError::Malformed {
            path: path.to_owned(),
            msg: e.to_string(),
        })?
    };
    if !value.is_object() {
        return Err(shape(path, "root is not an object"));
    }
    Ok(Some((value, raw)))
}

/// Adds Recalla's hooks to the settings file (idempotent).
///
/// # Errors
/// Malformed or oddly shaped settings (nothing is written), or I/O errors.
pub fn install(path: &Path, exe: &Path, dry_run: bool) -> Result<Outcome, CfgError> {
    edit(path, dry_run, true, |p, v| install_into(p, v, exe))
}

/// Removes only Recalla's hooks from the settings file.
///
/// # Errors
/// Malformed or oddly shaped settings (nothing is written), or I/O errors.
pub fn uninstall(path: &Path, dry_run: bool) -> Result<Outcome, CfgError> {
    edit(path, dry_run, false, uninstall_from)
}

fn edit(
    path: &Path,
    dry_run: bool,
    create: bool,
    f: impl FnOnce(&Path, &mut Value) -> Result<Vec<String>, CfgError>,
) -> Result<Outcome, CfgError> {
    // Write through a symlinked settings file (dotfile managers) instead of
    // replacing the link.
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let loaded = load(&path)?;
    if loaded.is_none() && !create {
        return Ok(Outcome::default());
    }
    let (mut value, raw) = loaded.unwrap_or_else(|| (json!({}), String::new()));
    let before = value.clone();
    let changes = f(&path, &mut value)?;
    let dirty = value != before;
    let mut out = Outcome {
        changes,
        changed: dirty,
        backup: None,
    };
    if !dirty || dry_run {
        return Ok(out);
    }
    let write_err = |source| CfgError::Write {
        path: path.clone(),
        source,
    };
    if !raw.is_empty() {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        // Never overwrite an earlier backup: create_new with a counter.
        let mut n = 0u32;
        let bak = loop {
            let mut name = path.file_name().unwrap_or_default().to_owned();
            name.push(if n == 0 {
                format!(".recalla-bak-{ts}")
            } else {
                format!(".recalla-bak-{ts}-{n}")
            });
            let candidate = path.with_file_name(name);
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
            match opts.open(&candidate) {
                Ok(mut f) => {
                    io::Write::write_all(&mut f, raw.as_bytes()).map_err(write_err)?;
                    f.sync_all().map_err(write_err)?;
                    break candidate;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
                Err(e) => return Err(write_err(e)),
            }
        };
        out.backup = Some(bak);
    }
    write_atomic(&path, &render(&value, &raw)).map_err(write_err)?;
    Ok(out)
}

/// Pretty JSON using the original file's indent and trailing newline.
fn render(value: &Value, original: &str) -> String {
    let indent: Vec<u8> = original
        .lines()
        .find(|l| l.starts_with([' ', '\t']))
        .map_or_else(
            || b"  ".to_vec(),
            |l| {
                l.bytes()
                    .take_while(|b| *b == b' ' || *b == b'\t')
                    .collect()
            },
        );
    let pretty = serde_json::to_string_pretty(value).expect("a Value always serializes");
    let indent = String::from_utf8_lossy(&indent);
    // Pretty output indents only with leading spaces (string newlines are
    // escaped), so re-indenting line by line is safe.
    let mut text = pretty
        .lines()
        .map(|l| {
            let body = l.trim_start_matches(' ');
            format!("{}{body}", indent.repeat((l.len() - body.len()) / 2))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if original.is_empty() || original.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// tmp file in the same dir, fsync, rename; keeps the original mode.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mode = fs::metadata(path).ok().map(|m| m.permissions());
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".recalla-tmp-{}", std::process::id()));
    let tmp = path.with_file_name(name);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        if let Some(m) = mode {
            fs::set_permissions(&tmp, m)?;
        }
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// MCP registration hints; printed, never run.
#[must_use]
pub fn mcp_hints(exe: &Path) -> String {
    let e = exe.display();
    format!(
        "Optional MCP registration (not run):\n  claude mcp add --scope user recalla -- {e} mcp\n  Codex, in ~/.codex/config.toml:\n    [mcp_servers.recalla]\n    command = \"{e}\"\n    args = [\"mcp\"]\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(
            command_for(Path::new("/a/recalla"), "capture"),
            "/a/recalla hook capture"
        );
        assert_eq!(
            command_for(Path::new("/my dir/recalla"), "capture"),
            "\"/my dir/recalla\" hook capture"
        );
        assert_eq!(
            command_for(Path::new("/a\"b$/recalla"), "capture"),
            "\"/a\\\"b\\$/recalla\" hook capture"
        );
    }

    #[test]
    fn matching_needs_recalla_and_hook_subcommand() {
        assert!(is_ours("/x/recalla hook capture", None));
        assert!(is_ours(
            "\"/x y/recalla\" hook session-start",
            Some("session-start")
        ));
        assert!(!is_ours("/x/recalla hook capture", Some("session-start")));
        assert!(!is_ours("/x/other hook capture", None));
        assert!(!is_ours("/x/recalla pack", None));
    }
}
