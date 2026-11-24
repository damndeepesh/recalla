//! Scope resolution: project keys from a cwd, and the global-vs-project rule.
//!
//! Only the event's own `cwd` is ever probed on disk. Paths mentioned in
//! event text are never followed.

use std::fs;
use std::path::Path;

use super::text::mentions_project_specifics;
use crate::model::Scope;

/// Walks up from `cwd` looking for a `.git` entry (via `exists`) and returns
/// that toplevel, else `cwd` itself. Relative or empty paths are returned
/// as-is: probing them would depend on the process's own working directory.
pub fn project_key_from(cwd: &str, exists: impl Fn(&Path) -> bool) -> String {
    let path = Path::new(cwd);
    if path.is_absolute() {
        for dir in path.ancestors() {
            if exists(&dir.join(".git")) {
                return dir.to_string_lossy().into_owned();
            }
        }
    }
    cwd.to_owned()
}

/// [`project_key_from`] against the real filesystem. `symlink_metadata`, so a
/// symlinked `.git` is detected without being followed.
#[must_use]
pub fn project_key(cwd: &str) -> String {
    project_key_from(cwd, |p| fs::symlink_metadata(p).is_ok())
}

/// `Project(key)` when a key is known, else `Global`.
#[must_use]
pub fn project_scope(project: Option<&str>) -> Scope {
    project
        .filter(|p| !p.is_empty())
        .map_or(Scope::Global, |p| Scope::Project(p.to_owned()))
}

/// Preferences are `Global` unless the sentence mentions a path-like token
/// (contains `/` or a file extension) or a backticked identifier.
#[must_use]
pub fn preference_scope(sentence: &str, project: Option<&str>) -> Scope {
    // Project by default: a preference said in one repo ("Prefer SwiftUI")
    // must not leak into every other project's pack. Only explicit cues
    // ("in all projects", "everywhere", "globally") make it global. User-wide
    // rules already live in the agent's own global instructions file.
    if GLOBAL_CUE.is_match(sentence) && !mentions_project_specifics(sentence) {
        Scope::Global
    } else {
        project_scope(project)
    }
}

static GLOBAL_CUE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:in (?:all|every|any) (?:projects?|repos?|repositories)|across (?:all )?(?:projects|repos)|everywhere|globally|in general|for every project)\b",
    )
    .expect("valid built-in regex")
});

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive prefix so the fixture paths are absolute on Windows too.
    const D: &str = if cfg!(windows) { "C:" } else { "" };

    fn fake(git_dirs: &[&str]) -> impl Fn(&Path) -> bool + use<> {
        let dirs: Vec<String> = git_dirs.iter().map(|d| format!("{D}{d}")).collect();
        move |p| dirs.iter().any(|d| Path::new(d).join(".git") == p)
    }

    #[test]
    fn walks_up_to_the_git_toplevel() {
        let exists = fake(&["/work/app"]);
        let app = format!("{D}/work/app");
        assert_eq!(project_key_from(&format!("{app}/src/deep"), &exists), app);
        assert_eq!(project_key_from(&app, &exists), app);
    }

    #[test]
    fn nearest_git_wins_for_nested_repos() {
        let exists = fake(&["/work", "/work/app/vendor"]);
        let vendor = format!("{D}/work/app/vendor");
        assert_eq!(project_key_from(&format!("{vendor}/x"), &exists), vendor);
    }

    #[test]
    fn falls_back_to_cwd_without_git_or_when_relative() {
        let gone = format!("{D}/gone/dir");
        assert_eq!(project_key_from(&gone, fake(&[])), gone);
        assert_eq!(project_key_from("rel/dir", |_| true), "rel/dir");
        assert_eq!(project_key_from("", |_| true), "");
    }

    #[test]
    fn real_fs_wrapper_finds_dot_git_file_or_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let deep = root.join("a/b");
        fs::create_dir_all(&deep).unwrap();
        fs::write(root.join(".git"), "gitdir: elsewhere").unwrap();
        assert_eq!(project_key(deep.to_str().unwrap()), root.to_str().unwrap());
    }

    #[test]
    fn preference_scope_rule() {
        let p = Some("/work/app");
        assert_eq!(
            preference_scope("Always write small commits.", p),
            Scope::Project("/work/app".into())
        );
        assert_eq!(
            preference_scope("Always write small commits in all projects.", p),
            Scope::Global
        );
        assert_eq!(
            preference_scope("Never touch that org, everywhere.", p),
            Scope::Global
        );
        assert_eq!(
            preference_scope("Never edit src/gen.rs by hand.", p),
            Scope::Project("/work/app".into())
        );
        assert_eq!(
            preference_scope("Prefer `anyhow` over custom errors.", p),
            Scope::Project("/work/app".into())
        );
        // No project known: stays global rather than inventing a key.
        assert_eq!(
            preference_scope("Never edit src/gen.rs by hand.", None),
            Scope::Global
        );
        assert_eq!(project_scope(Some("")), Scope::Global);
    }
}
