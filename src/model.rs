//! Typed memory model: the vocabulary shared by every module.
//!
//! Pure data and validation, no I/O. Persistence lives in [`crate::store`].

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Separator between project and path in a module scope key. A control
/// character, so it cannot clash with real project or path text.
const SCOPE_SEP: char = '\u{1f}';

/// A string did not name a known enum variant or scope.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid {what}: {value:?}")]
pub struct ParseError {
    what: &'static str,
    value: String,
}

macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// Stable lowercase name, as stored in the database.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $s),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseError;

            fn from_str(s: &str) -> Result<Self, ParseError> {
                match s {
                    $($s => Ok(Self::$variant),)+
                    _ => Err(ParseError { what: stringify!($name), value: s.to_owned() }),
                }
            }
        }
    };
}

str_enum! {
    /// What a memory records.
    Kind {
        Decision => "decision",
        Fact => "fact",
        Procedure => "procedure",
        Gotcha => "gotcha",
        Preference => "preference",
        Task => "task",
    }
}

str_enum! {
    /// Lifecycle state. Memories are never hard-deleted.
    Status {
        Active => "active",
        NeedsReverify => "needs_reverify",
        Superseded => "superseded",
        Archived => "archived",
    }
}

str_enum! {
    /// How a memory entered the store.
    Origin {
        Backfill => "backfill",
        Capture => "capture",
        Remember => "remember",
    }
}

/// Where a memory applies.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    Global,
    Project(String),
    Module { project: String, path: String },
}

impl Scope {
    /// Project and path columns, `None` where the scope doesn't have one.
    #[must_use]
    pub fn parts(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Global => (None, None),
            Self::Project(p) => (Some(p), None),
            Self::Module { project, path } => (Some(project), Some(path)),
        }
    }

    /// Inverse of [`Scope::parts`].
    #[must_use]
    pub fn from_parts(project: Option<String>, path: Option<String>) -> Self {
        match (project, path) {
            (Some(project), Some(path)) => Self::Module { project, path },
            (Some(project), None) => Self::Project(project),
            _ => Self::Global,
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        let bad = |s: &str| s.is_empty() || s.contains(SCOPE_SEP);
        match self.parts() {
            (Some(p), _) if bad(p) => Err("scope project must be non-empty"),
            (_, Some(p)) if bad(p) => Err("scope path must be non-empty"),
            _ => Ok(()),
        }
    }
}

/// Canonical key: used for hashing, equality filters and `FromStr`.
impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => f.write_str("global"),
            Self::Project(p) => write!(f, "project:{p}"),
            Self::Module { project, path } => write!(f, "module:{project}{SCOPE_SEP}{path}"),
        }
    }
}

impl FromStr for Scope {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let err = || ParseError {
            what: "Scope",
            value: s.to_owned(),
        };
        let scope = if s == "global" {
            Self::Global
        } else if let Some(p) = s.strip_prefix("project:") {
            Self::Project(p.to_owned())
        } else if let Some(rest) = s.strip_prefix("module:") {
            let (project, path) = rest.split_once(SCOPE_SEP).ok_or_else(err)?;
            Self::Module {
                project: project.to_owned(),
                path: path.to_owned(),
            }
        } else {
            return Err(err());
        };
        scope.validate().map_err(|_| err())?;
        Ok(scope)
    }
}

/// Where a memory came from. At least one per memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub origin: Origin,
    pub harness: Option<String>,
    pub session_id: Option<String>,
    pub message_uuid: Option<String>,
    pub source_path: Option<String>,
    pub line: Option<i64>,
    pub git_commit: Option<String>,
}

impl Provenance {
    /// Provenance with only an origin set (e.g. an explicit `remember`).
    #[must_use]
    pub const fn new(origin: Origin) -> Self {
        Self {
            origin,
            harness: None,
            session_id: None,
            message_uuid: None,
            source_path: None,
            line: None,
            git_commit: None,
        }
    }
}

/// A code location a memory is tied to. Staleness logic lives in `anchor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub path: String,
    pub symbol: Option<String>,
    pub lang: Option<String>,
    pub content_hash: Option<String>,
    pub git_commit: Option<String>,
}

impl Anchor {
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            symbol: None,
            lang: None,
            content_hash: None,
            git_commit: None,
        }
    }
}

/// A memory about to be inserted.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMemory {
    pub kind: Kind,
    pub text: String,
    pub scope: Scope,
    /// In `0.0..=1.0`.
    pub confidence: f32,
    /// In `0..=3`.
    pub importance: u8,
}

/// Highest valid importance.
pub const MAX_IMPORTANCE: u8 = 3;

impl NewMemory {
    /// Checks the invariants the store relies on.
    ///
    /// # Errors
    /// Returns a message naming the violated rule.
    pub fn validate(&self) -> Result<(), &'static str> {
        if normalize(&self.text).is_empty() {
            return Err("text must not be empty");
        }
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err("confidence must be within 0..=1");
        }
        if self.importance > MAX_IMPORTANCE {
            return Err("importance must be within 0..=3");
        }
        self.scope.validate()
    }
}

/// A stored memory. Times are unix milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    pub uid: String,
    pub kind: Kind,
    pub text: String,
    pub scope: Scope,
    pub confidence: f32,
    pub importance: u8,
    pub status: Status,
    pub superseded_by: Option<String>,
    pub seen_count: u32,
    pub created_at: i64,
    pub updated_at: i64,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub provenance: Vec<Provenance>,
    pub anchors: Vec<Anchor>,
}

/// Trims and collapses whitespace runs to one space. Case is preserved
/// because identifiers matter.
#[must_use]
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Deterministic id: first 16 bytes of blake3 over
/// `kind|scope|normalized_text`, as 32 hex chars. Re-importing the same
/// memory yields the same uid, so inserts are idempotent.
#[must_use]
pub fn uid(kind: Kind, scope: &Scope, text: &str) -> String {
    let input = format!("{kind}|{scope}|{}", normalize(text));
    let mut hex = blake3::hash(input.as_bytes()).to_hex().to_string();
    hex.truncate(32);
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(p: &str) -> Scope {
        Scope::Project(p.into())
    }

    #[test]
    fn enums_round_trip() {
        for &k in Kind::ALL {
            assert_eq!(k.as_str().parse::<Kind>(), Ok(k));
        }
        for &s in Status::ALL {
            assert_eq!(s.as_str().parse::<Status>(), Ok(s));
        }
        for &o in Origin::ALL {
            assert_eq!(o.as_str().parse::<Origin>(), Ok(o));
        }
        assert!("nope".parse::<Kind>().is_err());
        assert!("Active".parse::<Status>().is_err());
    }

    #[test]
    fn scope_round_trips() {
        let scopes = [
            Scope::Global,
            project("git@github.com:a/b"),
            Scope::Module {
                project: "p".into(),
                path: "src/a:b".into(),
            },
        ];
        for s in scopes {
            assert_eq!(s.to_string().parse::<Scope>(), Ok(s.clone()));
            let (p, m) = s.parts();
            assert_eq!(
                Scope::from_parts(p.map(str::to_owned), m.map(str::to_owned)),
                s
            );
        }
        assert!("project:".parse::<Scope>().is_err());
        assert!("bogus".parse::<Scope>().is_err());
        assert!("module:no-separator".parse::<Scope>().is_err());
    }

    #[test]
    fn normalize_collapses_whitespace_and_keeps_case() {
        assert_eq!(normalize("  Use \t\n  SQLite  "), "Use SQLite");
    }

    #[test]
    fn uid_is_stable_and_32_hex() {
        let a = uid(Kind::Fact, &Scope::Global, "Use SQLite");
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, uid(Kind::Fact, &Scope::Global, "  Use   SQLite\n"));
    }

    #[test]
    fn uid_differs_by_kind_scope_and_case() {
        let base = uid(Kind::Fact, &Scope::Global, "Use SQLite");
        assert_ne!(base, uid(Kind::Gotcha, &Scope::Global, "Use SQLite"));
        assert_ne!(base, uid(Kind::Fact, &project("x"), "Use SQLite"));
        assert_ne!(base, uid(Kind::Fact, &Scope::Global, "use sqlite"));
    }

    #[test]
    fn new_memory_validation() {
        let ok = NewMemory {
            kind: Kind::Fact,
            text: "x".into(),
            scope: Scope::Global,
            confidence: 1.0,
            importance: 3,
        };
        assert!(ok.validate().is_ok());
        let bad = |f: fn(&mut NewMemory)| {
            let mut m = ok.clone();
            f(&mut m);
            m.validate().is_err()
        };
        assert!(bad(|m| m.text = "  \n".into()));
        assert!(bad(|m| m.confidence = 1.1));
        assert!(bad(|m| m.confidence = -0.1));
        assert!(bad(|m| m.confidence = f32::NAN));
        assert!(bad(|m| m.importance = 4));
        assert!(bad(|m| m.scope = Scope::Project(String::new())));
    }
}
