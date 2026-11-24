//! Code anchors: ties memories to the symbols and files they talk about, so
//! later edits to that code can flag the memory as stale.

pub mod lang;
mod staleness;

pub use staleness::{CheckReport, check};

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::model::{Anchor, normalize};
use lang::{Def, Lang, MAX_SOURCE_BYTES, definitions, language_for};

/// blake3 of `text` with whitespace runs collapsed and trimmed, first 16
/// bytes as 32 hex chars. A reformat keeps the hash; a content edit changes it.
#[must_use]
pub fn normalized_hash(text: &str) -> String {
    let hash = blake3::hash(normalize(text).as_bytes()).to_hex();
    hash[..32].to_owned()
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).expect("static regex"));
    };
}
re!(BACKTICK, r"`([^`\n]+)`");
re!(CAMEL, r"\b[A-Z][a-z0-9]+(?:[A-Z][a-z0-9]*)+\b");
re!(LOWER_CAMEL, r"\b[a-z][a-z0-9]*(?:[A-Z][a-z0-9]*)+\b");
re!(SNAKE, r"\b[a-z][a-z0-9]*(?:_[a-z0-9]+)+\b");

/// Words that are real identifiers often enough to be dangerous as anchors.
const COMMON: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "from", "into", "not", "use", "are", "was", "can",
    "should", "must", "when", "then", "have", "has", "main", "new", "get", "set", "run", "test",
    "code", "file", "true", "false", "none", "some", "self", "type", "name", "data", "path", "err",
    "ok", "all", "any", "one", "let", "mut", "fn", "pub", "str", "string", "vec", "box", "option",
    "result", "default", "value", "error", "list", "init", "load", "read", "write", "build",
];

/// Removes balanced `<...>` generics and anything from the first `(`.
fn strip_decoration(span: &str) -> String {
    let mut depth = 0u32;
    let mut out = String::new();
    for c in span.chars() {
        match c {
            '(' if depth == 0 => break,
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Identifier candidates in a memory's text: backticked spans (full path
/// and last segment), `CamelCase` and `snake_case` words. Deduped, stable order.
#[must_use]
pub fn candidate_identifiers(memory_text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut push = |s: &str| {
        let ok = s.len() >= 2 && !COMMON.contains(&s.to_ascii_lowercase().as_str());
        if ok && seen.insert(s.to_owned()) {
            out.push(s.to_owned());
        }
    };
    for cap in BACKTICK.captures_iter(memory_text) {
        let span = strip_decoration(cap[1].trim());
        let span = span.trim_matches(|c: char| c == ':' || c == '.');
        if span.is_empty()
            || !span
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.'))
        {
            continue;
        }
        push(span);
        if let Some(last) = span.rsplit(['.', ':']).next() {
            push(last);
        }
    }
    for re in [&CAMEL, &LOWER_CAMEL] {
        re.find_iter(memory_text).for_each(|m| push(m.as_str()));
    }
    SNAKE
        .find_iter(memory_text)
        .filter(|m| m.as_str().len() >= 4)
        .for_each(|m| push(m.as_str()));
    out
}

/// The directory a project key names, if it is an absolute, existing dir.
/// Anchoring and checking only apply to such keys.
#[must_use]
pub fn project_dir(key: &str) -> Option<&Path> {
    let p = Path::new(key);
    // `/`, `/Users`, `/etc` or the home dir (or any ancestor of it) would make
    // every readable file "in the project".
    let normal = p
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count();
    let too_broad =
        normal < 2 || std::env::var_os("HOME").is_some_and(|h| Path::new(&h).starts_with(p));
    (p.is_absolute() && p.is_dir() && !too_broad).then_some(p)
}

/// Anchor paths and symbols are persisted and shown in packs. They come from
/// file names and file contents, so an anchor whose path or symbol would be
/// altered by redaction is dropped rather than stored (redact before persist;
/// a redacted symbol could never be matched again anyway).
fn persistable(s: &str) -> bool {
    crate::redact::redact_path(s).is_clean()
}

/// Canonical, existing, regular file under `root`, of any size.
fn contained_path(root: &Path, path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    let canon = full.canonicalize().ok()?;
    if !canon.starts_with(root) {
        return None;
    }
    fs::metadata(&canon).ok()?.is_file().then_some(canon)
}

/// [`contained_path`] for a file of at most 2 MB.
fn contained_file(root: &Path, path: &str) -> Option<PathBuf> {
    let canon = contained_path(root, path)?;
    let len = fs::metadata(&canon).ok()?.len();
    usize::try_from(len)
        .is_ok_and(|n| n <= MAX_SOURCE_BYTES)
        .then_some(canon)
}

/// Anchors for the files in `paths` (under `project_root` only). Each
/// symbol defined in a file and named by the memory gets its own anchor;
/// a file with no such symbol (or no known language) gets a path-only one.
#[must_use]
pub fn resolve(memory_text: &str, paths: &[String], project_root: &Path) -> Vec<Anchor> {
    resolve_with(memory_text, paths, &[], project_root)
}

/// As [`resolve`], plus `symbol_only_paths`: files that only get anchors for
/// symbols the memory names (never a path-only one), e.g. files merely
/// touched near the memory in its session. A path in both lists is a normal
/// path.
#[must_use]
pub fn resolve_with(
    memory_text: &str,
    paths: &[String],
    symbol_only_paths: &[String],
    project_root: &Path,
) -> Vec<Anchor> {
    let Ok(root) = project_root.canonicalize() else {
        return Vec::new();
    };
    let candidates: HashSet<String> = candidate_identifiers(memory_text).into_iter().collect();
    let commit = git_commit(&root);
    let mut seen = HashSet::new();
    let mut anchors = Vec::new();
    let all = paths
        .iter()
        .map(|p| (p, true))
        .chain(symbol_only_paths.iter().map(|p| (p, false)));
    for (path, path_fallback) in all {
        let Some(file) = contained_file(&root, path) else {
            continue;
        };
        let Ok(rel) = file.strip_prefix(&root) else {
            continue;
        };
        let rel = rel.to_string_lossy().into_owned();
        if !persistable(&rel) || !seen.insert(rel.clone()) {
            continue;
        }
        let Ok(bytes) = fs::read(&file) else {
            continue;
        };
        let src = String::from_utf8_lossy(&bytes);
        let lang = language_for(&file);
        let matched = lang.map(|l| matched_defs(l, &src, &candidates));
        let make = |symbol: Option<String>, hash: String| Anchor {
            path: rel.clone(),
            symbol,
            lang: lang.map(|l| l.name().to_owned()),
            content_hash: Some(hash),
            git_commit: commit.clone(),
        };
        match matched {
            Some(defs) if !defs.is_empty() => {
                let mut syms = HashSet::new();
                for d in defs {
                    if persistable(&d.qualified) && syms.insert(d.qualified.clone()) {
                        let hash = normalized_hash(&src[d.range]);
                        anchors.push(make(Some(d.qualified), hash));
                    }
                }
            }
            _ if path_fallback => anchors.push(make(None, normalized_hash(&src))),
            _ => {}
        }
    }
    anchors
}

fn matched_defs(lang: Lang, src: &str, candidates: &HashSet<String>) -> Vec<Def> {
    definitions(lang, src)
        .into_iter()
        .filter(|d| candidates.contains(&d.name) || candidates.contains(&d.qualified))
        .collect()
}

// ---- git HEAD, read directly (no git2) ----

const MAX_GIT_FILE: u64 = 16 * 1024 * 1024;

fn read_capped(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    (meta.is_file() && meta.len() <= MAX_GIT_FILE).then_some(())?;
    fs::read_to_string(path).ok()
}

fn is_hash(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A ref name that stays inside `refs/`: no `..`, absolute or empty parts.
fn safe_ref(r: &str) -> bool {
    r.starts_with("refs/")
        && r.split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.contains('\\'))
}

/// Current commit of the repo at `root`, if readable. `None` on any failure.
fn git_commit(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    let gitdir = if dot_git.is_file() {
        let line = read_capped(&dot_git)?;
        let target = Path::new(line.trim().strip_prefix("gitdir:")?.trim());
        let dir = if target.is_absolute() {
            target.to_path_buf()
        } else {
            root.join(target)
        };
        dir.is_dir().then_some(dir)?
    } else {
        dot_git.is_dir().then_some(dot_git)?
    };
    // Linked worktrees keep refs in the common dir.
    let common = read_capped(&gitdir.join("commondir"))
        .map(|c| gitdir.join(c.trim()))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| gitdir.clone());

    let head = read_capped(&gitdir.join("HEAD"))?;
    let head = head.trim();
    if is_hash(head) {
        return Some(head.to_owned());
    }
    let name = head.strip_prefix("ref:")?.trim();
    if !safe_ref(name) {
        return None;
    }
    for dir in [&gitdir, &common] {
        if let Some(h) = read_capped(&dir.join(name)).map(|s| s.trim().to_owned())
            && is_hash(&h)
        {
            return Some(h);
        }
    }
    let packed = read_capped(&common.join("packed-refs"))?;
    packed.lines().find_map(|l| {
        let (hash, r) = l.split_once(' ')?;
        (r == name && is_hash(hash)).then(|| hash.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_dir_rejects_shallow_and_home_ancestor_paths() {
        for k in ["/", "/etc", "/Users", "/opt", "/tmp"] {
            assert!(project_dir(k).is_none(), "{k}");
        }
        let d = tempfile::tempdir().unwrap();
        assert!(project_dir(d.path().to_str().unwrap()).is_some());
        if let Some(home) = std::env::var_os("HOME")
            && let Some(parent) = Path::new(&home).parent()
        {
            assert!(project_dir(&parent.to_string_lossy()).is_none());
        }
    }

    const HASH_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const HASH_B: &str = "fedcba9876543210fedcba9876543210fedcba98";

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    const STORE_RS: &str = "struct Store;\nimpl Store {\n    fn insert(&self) { 1 }\n    fn other(&self) {}\n}\nfn helper() {}\n";

    #[test]
    fn hash_ignores_formatting_but_not_content() {
        let a = normalized_hash("fn a() {\n    x + 1\n}");
        assert_eq!(a.len(), 32);
        assert_eq!(a, normalized_hash("  fn a()   { x   +\t1 }  "));
        assert_ne!(a, normalized_hash("fn a() {\n    x + 2\n}"));
    }

    #[test]
    fn candidates_from_backticks_and_words() {
        let c = candidate_identifiers(
            "`Store::insert()` must call `Vec<T>::push` and `redact`; see NewMemory, \
             pack_budget and fooBar. The code is fine. `new`",
        );
        for want in [
            "Store::insert",
            "insert",
            "Vec::push",
            "push",
            "redact",
            "NewMemory",
            "pack_budget",
            "fooBar",
        ] {
            assert!(c.contains(&want.to_owned()), "missing {want}: {c:?}");
        }
        for bad in ["The", "code", "new", "must"] {
            assert!(!c.contains(&bad.to_owned()), "unexpected {bad}: {c:?}");
        }
        let mut dedup = c.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(dedup.len(), c.len());
        assert!(candidate_identifiers("`a.b.c`").contains(&"a.b.c".to_owned()));
    }

    #[test]
    fn symbol_anchor_from_backticked_method() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/store.rs", STORE_RS);
        let got = resolve(
            "`Store::insert` must stay cheap",
            &["src/store.rs".to_owned()],
            dir.path(),
        );
        assert_eq!(got.len(), 1, "{got:?}");
        let a = &got[0];
        assert_eq!(a.path, "src/store.rs");
        assert_eq!(a.symbol.as_deref(), Some("Store::insert"));
        assert_eq!(a.lang.as_deref(), Some("rust"));
        assert_eq!(
            a.content_hash.as_deref(),
            Some(normalized_hash("fn insert(&self) { 1 }").as_str())
        );
        assert_eq!(a.git_commit, None);
    }

    #[test]
    fn body_edit_changes_hash_other_fn_edit_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let hash_of = |src: &str| {
            write(dir.path(), "a.rs", src);
            let a = resolve("`insert`", &["a.rs".to_owned()], dir.path());
            a[0].content_hash.clone().unwrap()
        };
        let base = hash_of(STORE_RS);
        assert_eq!(
            base,
            hash_of(&STORE_RS.replace("fn other(&self) {}", "fn other() { 9 }"))
        );
        assert_eq!(base, hash_of(&STORE_RS.replace("{ 1 }", "{\n 1\n }")));
        assert_ne!(base, hash_of(&STORE_RS.replace("{ 1 }", "{ 2 }")));
    }

    #[test]
    fn path_only_fallbacks() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", STORE_RS);
        write(dir.path(), "notes.md", "hello  world");
        let got = resolve(
            "nothing relevant here",
            &["a.rs".to_owned(), "notes.md".to_owned()],
            dir.path(),
        );
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].symbol, None);
        assert_eq!(got[0].lang.as_deref(), Some("rust"));
        assert_eq!(got[0].content_hash, Some(normalized_hash(STORE_RS)));
        assert_eq!(got[1].lang, None);
        assert_eq!(got[1].content_hash, Some(normalized_hash("hello world")));
    }

    #[test]
    fn symbol_only_paths_never_get_a_path_only_anchor() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", STORE_RS);
        write(dir.path(), "b.rs", "fn unrelated() {}");
        let both = ["a.rs".to_owned(), "b.rs".to_owned()];
        let got = resolve_with("`insert`", &[], &both, dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, "a.rs");
        assert_eq!(got[0].symbol.as_deref(), Some("Store::insert"));
        assert!(resolve_with("nothing", &[], &both, dir.path()).is_empty());
        // Also listed as a normal path: the fallback applies.
        let got = resolve_with("nothing", &["b.rs".to_owned()], &both, dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, "b.rs");
    }

    #[test]
    fn paths_outside_root_are_dropped() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("proj");
        write(&root, "in.rs", "fn alpha() {}");
        write(outer.path(), "outside.rs", "fn secret() {}");
        let abs_outside = outer
            .path()
            .join("outside.rs")
            .to_string_lossy()
            .into_owned();
        let sneaky = root
            .join("..")
            .join("outside.rs")
            .to_string_lossy()
            .into_owned();
        let paths = vec![
            "../outside.rs".to_owned(),
            abs_outside,
            sneaky,
            "missing.rs".to_owned(),
            ".".to_owned(),
            "in.rs".to_owned(),
        ];
        let got = resolve("`secret` `alpha`", &paths, &root);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, "in.rs");
        assert_eq!(got[0].symbol.as_deref(), Some("alpha"));
    }

    #[test]
    fn root_prefix_is_component_wise() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("proj");
        let sibling = outer.path().join("proj-evil");
        write(&root, "a.rs", "fn a() {}");
        write(&sibling, "b.rs", "fn b() {}");
        let p = sibling.join("b.rs").to_string_lossy().into_owned();
        assert!(resolve("`b`", &[p], &root).is_empty());
    }

    #[cfg(unix)]
    #[cfg(unix)]
    #[test]
    fn symlink_escaping_root_is_dropped() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("proj");
        write(&root, "ok.rs", "fn a() {}");
        write(outer.path(), "target.rs", "fn secret() {}");
        std::os::unix::fs::symlink(outer.path().join("target.rs"), root.join("link.rs")).unwrap();
        std::os::unix::fs::symlink(outer.path(), root.join("dirlink")).unwrap();
        let paths = vec!["link.rs".to_owned(), "dirlink/target.rs".to_owned()];
        assert!(resolve("`secret`", &paths, &root).is_empty());
    }

    #[test]
    fn oversized_file_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "big.py",
            &"x = 1\n".repeat(MAX_SOURCE_BYTES / 4),
        );
        assert!(resolve("x", &["big.py".to_owned()], dir.path()).is_empty());
    }

    #[test]
    fn missing_root_yields_nothing() {
        assert!(resolve("x", &["a.rs".to_owned()], Path::new("/no/such/root/xyz")).is_empty());
    }

    #[test]
    fn git_head_from_branch_ref_and_detached() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".git/HEAD", "ref: refs/heads/main\n");
        write(dir.path(), ".git/refs/heads/main", &format!("{HASH_A}\n"));
        assert_eq!(git_commit(dir.path()).as_deref(), Some(HASH_A));
        write(dir.path(), ".git/HEAD", &format!("{HASH_B}\n"));
        assert_eq!(git_commit(dir.path()).as_deref(), Some(HASH_B));
    }

    #[test]
    fn git_head_from_packed_refs_and_failures() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_commit(dir.path()), None);
        write(dir.path(), ".git/HEAD", "ref: refs/heads/dev\n");
        assert_eq!(git_commit(dir.path()), None);
        write(
            dir.path(),
            ".git/packed-refs",
            &format!("# pack-refs\n{HASH_A} refs/heads/other\n{HASH_B} refs/heads/dev\n"),
        );
        assert_eq!(git_commit(dir.path()).as_deref(), Some(HASH_B));
        write(dir.path(), ".git/HEAD", "ref: refs/../../etc/passwd\n");
        assert_eq!(git_commit(dir.path()), None);
        write(dir.path(), ".git/HEAD", "garbage");
        assert_eq!(git_commit(dir.path()), None);
    }

    #[test]
    fn git_head_follows_gitdir_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".git", "gitdir: real/gitdir\n");
        write(dir.path(), "real/gitdir/HEAD", "ref: refs/heads/x\n");
        write(dir.path(), "real/gitdir/refs/heads/x", HASH_A);
        assert_eq!(git_commit(dir.path()).as_deref(), Some(HASH_A));
    }

    #[test]
    fn resolve_records_git_commit() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".git/HEAD", &format!("{HASH_A}\n"));
        write(dir.path(), "a.rs", "fn a() {}");
        let got = resolve("`a`", &["a.rs".to_owned()], dir.path());
        assert_eq!(got[0].git_commit.as_deref(), Some(HASH_A));
    }
}
