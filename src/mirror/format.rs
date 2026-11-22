//! The mirror file format, version 1: a small YAML subset written and parsed
//! here (ADR D-021), then the memory text verbatim.
//!
//! ```text
//! ---
//! format: 1
//! uid: 0123...
//! kind: decision
//! scope: project
//! project: /abs/key
//! ...
//! provenance:
//!   - origin: backfill
//!     recorded_at: 2026-11-22T10:00:00.000Z
//! anchors: []
//! ---
//! <text, verbatim, plus one trailing newline>
//! ```
//!
//! Scalars are bare when they match `[A-Za-z0-9_./:@+-]+` and can't be read
//! as another type; otherwise they are JSON string literals. Numbers are
//! bare. The frontmatter ends at the first `---` line, so the text may
//! itself contain `---` lines.

use std::collections::HashMap;
use std::fmt::Write as _;

use thiserror::Error;

use crate::harness::{format_rfc3339_ms, parse_rfc3339_ms};
use crate::model::{Anchor, Kind, Memory, Origin, Provenance, Scope, Status};
use crate::store::FullMemory;

/// The only format version this build reads and writes.
pub const FORMAT_VERSION: u32 = 1;
const FENCE: &str = "---";
const PROV_INDENT: &str = "  - ";
const CONT_INDENT: &str = "    ";
/// Plain scalars that other YAML readers would not take as strings.
const RESERVED: [&str; 9] = ["null", "true", "false", "yes", "no", "on", "off", "y", "n"];

/// A file the parser could not accept.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("line {line}: {msg}")]
pub struct FormatError {
    /// 1-based line in the file.
    pub line: usize,
    pub msg: String,
}

fn err<T>(line: usize, msg: impl Into<String>) -> Result<T, FormatError> {
    Err(FormatError {
        line,
        msg: msg.into(),
    })
}

fn is_bare(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_alphanumeric())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:@+-".contains(&b))
        && !s.starts_with('@')
        && !s.ends_with(':')
        && !RESERVED.contains(&s.to_ascii_lowercase().as_str())
        && s.parse::<f64>().is_err()
}

fn scalar(s: &str) -> String {
    if is_bare(s) {
        s.to_owned()
    } else {
        serde_json::to_string(s).expect("serializing a string cannot fail")
    }
}

/// RFC 3339 UTC with milliseconds, or the bare integer for years outside
/// `0000..=9999` (so every `i64` round-trips).
fn timestamp(ms: i64) -> String {
    format_rfc3339_ms(ms).unwrap_or_else(|| ms.to_string())
}

fn push(out: &mut String, indent: &str, key: &str, value: &str) {
    let _ = writeln!(out, "{indent}{key}: {value}");
}

fn push_opt(out: &mut String, indent: &str, key: &str, value: Option<&str>) {
    if let Some(v) = value {
        push(out, indent, key, &scalar(v));
    }
}

/// Renders a memory as a mirror file. Deterministic: the same memory always
/// gives the same bytes.
#[must_use]
pub fn render(doc: &FullMemory) -> String {
    let m = &doc.memory;
    let mut out = format!("{FENCE}\n");
    push(&mut out, "", "format", &FORMAT_VERSION.to_string());
    push(&mut out, "", "uid", &scalar(&m.uid));
    push(&mut out, "", "kind", m.kind.as_str());
    let (project, module) = m.scope.parts();
    let scope = match &m.scope {
        Scope::Global => "global",
        Scope::Project(_) => "project",
        Scope::Module { .. } => "module",
    };
    push(&mut out, "", "scope", scope);
    push_opt(&mut out, "", "project", project);
    push_opt(&mut out, "", "module", module);
    push(&mut out, "", "status", m.status.as_str());
    push(&mut out, "", "confidence", &m.confidence.to_string());
    push(&mut out, "", "importance", &m.importance.to_string());
    push(&mut out, "", "seen_count", &m.seen_count.to_string());
    push(&mut out, "", "created_at", &timestamp(m.created_at));
    push(&mut out, "", "updated_at", &timestamp(m.updated_at));
    push(&mut out, "", "valid_from", &timestamp(m.valid_from));
    if let Some(t) = m.valid_to {
        push(&mut out, "", "valid_to", &timestamp(t));
    }
    push_opt(&mut out, "", "superseded_by", m.superseded_by.as_deref());
    if m.provenance.is_empty() {
        out.push_str("provenance: []\n");
    } else {
        out.push_str("provenance:\n");
    }
    for (i, p) in m.provenance.iter().enumerate() {
        let at = doc.provenance_at.get(i).copied().unwrap_or(m.created_at);
        push(&mut out, PROV_INDENT, "origin", p.origin.as_str());
        push(&mut out, CONT_INDENT, "recorded_at", &timestamp(at));
        push_opt(&mut out, CONT_INDENT, "harness", p.harness.as_deref());
        push_opt(&mut out, CONT_INDENT, "session_id", p.session_id.as_deref());
        push_opt(
            &mut out,
            CONT_INDENT,
            "message_uuid",
            p.message_uuid.as_deref(),
        );
        push_opt(
            &mut out,
            CONT_INDENT,
            "source_path",
            p.source_path.as_deref(),
        );
        if let Some(line) = p.line {
            push(&mut out, CONT_INDENT, "line", &line.to_string());
        }
        push_opt(&mut out, CONT_INDENT, "git_commit", p.git_commit.as_deref());
    }
    if m.anchors.is_empty() {
        out.push_str("anchors: []\n");
    } else {
        out.push_str("anchors:\n");
    }
    for (i, a) in m.anchors.iter().enumerate() {
        push(&mut out, PROV_INDENT, "path", &scalar(&a.path));
        push_opt(&mut out, CONT_INDENT, "symbol", a.symbol.as_deref());
        push_opt(&mut out, CONT_INDENT, "lang", a.lang.as_deref());
        push_opt(
            &mut out,
            CONT_INDENT,
            "content_hash",
            a.content_hash.as_deref(),
        );
        push_opt(&mut out, CONT_INDENT, "git_commit", a.git_commit.as_deref());
        push_opt(
            &mut out,
            CONT_INDENT,
            "stale_reason",
            doc.stale.get(i).and_then(Option::as_deref),
        );
    }
    out.push_str(FENCE);
    out.push('\n');
    out.push_str(&m.text);
    out.push('\n');
    out
}

/// One `key: value` pair with the line it came from.
type Pair = (usize, String);

/// Keys of one block (the top level or a list item), each at most once.
struct Fields {
    line: usize,
    map: HashMap<String, Pair>,
}

impl Fields {
    fn new(line: usize) -> Self {
        Self {
            line,
            map: HashMap::new(),
        }
    }

    fn add(&mut self, line: usize, key: &str, raw: &str) -> Result<(), FormatError> {
        if self
            .map
            .insert(key.to_owned(), (line, raw.to_owned()))
            .is_some()
        {
            return err(line, format!("duplicate key `{key}`"));
        }
        Ok(())
    }

    fn take(&mut self, key: &str) -> Option<Pair> {
        self.map.remove(key)
    }

    fn req(&mut self, key: &str) -> Result<Pair, FormatError> {
        let line = self.line;
        self.take(key)
            .map_or_else(|| err(line, format!("missing required key `{key}`")), Ok)
    }

    fn string(&mut self, key: &str) -> Result<Option<String>, FormatError> {
        self.take(key)
            .map(|(line, raw)| decode_string(line, &raw))
            .transpose()
    }

    fn req_string(&mut self, key: &str) -> Result<String, FormatError> {
        let (line, raw) = self.req(key)?;
        decode_string(line, &raw)
    }

    fn number<T: std::str::FromStr>(&mut self, key: &str) -> Result<Option<T>, FormatError> {
        self.take(key)
            .map(|(line, raw)| match raw.parse() {
                Ok(n) if !raw.starts_with('"') => Ok(n),
                _ => err(line, format!("`{key}` must be a number, got {raw}")),
            })
            .transpose()
    }

    fn req_number<T: std::str::FromStr>(&mut self, key: &str) -> Result<T, FormatError> {
        let line = self.line;
        self.number(key)?
            .map_or_else(|| err(line, format!("missing required key `{key}`")), Ok)
    }

    fn time(&mut self, key: &str) -> Result<Option<i64>, FormatError> {
        self.take(key)
            .map(|(line, raw)| {
                let ms = if raw.ends_with('Z') {
                    parse_rfc3339_ms(&raw)
                } else {
                    raw.parse().ok()
                };
                ms.map_or_else(
                    || err(line, format!("`{key}` is not a timestamp: {raw}")),
                    Ok,
                )
            })
            .transpose()
    }

    fn req_time(&mut self, key: &str) -> Result<i64, FormatError> {
        let line = self.line;
        self.time(key)?
            .map_or_else(|| err(line, format!("missing required key `{key}`")), Ok)
    }

    fn parsed<T: std::str::FromStr>(&mut self, key: &str) -> Result<T, FormatError> {
        let (line, raw) = self.req(key)?;
        decode_string(line, &raw)?
            .parse()
            .or_else(|_| err(line, format!("invalid `{key}`: {raw}")))
    }

    /// Fails on any key nobody took.
    fn finish(self) -> Result<(), FormatError> {
        match self.map.into_iter().min_by_key(|(_, (line, _))| *line) {
            Some((key, (line, _))) => err(line, format!("unknown key `{key}`")),
            None => Ok(()),
        }
    }
}

fn decode_string(line: usize, raw: &str) -> Result<String, FormatError> {
    if raw.starts_with('"') {
        return serde_json::from_str(raw)
            .or_else(|e| err(line, format!("bad string literal: {e}")));
    }
    let ok = !raw.is_empty()
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:@+-".contains(&b));
    if ok {
        Ok(raw.to_owned())
    } else {
        err(
            line,
            format!("expected a bare word or a quoted string, got `{raw}`"),
        )
    }
}

/// Splits `key: value`; the key must be lowercase letters and `_`.
fn split_pair(line: usize, text: &str) -> Result<(&str, &str), FormatError> {
    let Some((key, value)) = text.split_once(": ") else {
        return err(line, format!("expected `key: value`, got `{text}`"));
    };
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        return err(line, format!("invalid key `{key}`"));
    }
    Ok((key, value))
}

/// The frontmatter's blocks before typing: top-level fields and list items.
struct Raw {
    top: Fields,
    provenance: Vec<Fields>,
    anchors: Vec<Fields>,
}

fn next_line<'a>(rest: &mut &'a str) -> Option<&'a str> {
    if rest.is_empty() {
        return None;
    }
    let (line, tail) = rest.split_once('\n').unwrap_or((rest, ""));
    *rest = tail;
    Some(line)
}

/// Splits the file into raw blocks and the body.
fn scan(input: &str) -> Result<(Raw, &str), FormatError> {
    let mut rest = input;
    if next_line(&mut rest) != Some(FENCE) {
        return err(1, "a mirror file starts with a `---` line");
    }
    let mut raw = Raw {
        top: Fields::new(0),
        provenance: Vec::new(),
        anchors: Vec::new(),
    };
    // Which list the indented lines belong to: 0 none, 1 provenance, 2 anchors.
    let mut list = 0;
    let mut lines_seen = 0;
    let mut first_key = None;
    let mut lists_seen = [false; 2];
    let mut lineno = 1;
    loop {
        let Some(line) = next_line(&mut rest) else {
            return err(lineno, "frontmatter is not closed by a `---` line");
        };
        lineno += 1;
        if line == FENCE {
            raw.top.line = lineno;
            return Ok((raw, rest));
        }
        if let Some(item) = line.strip_prefix(PROV_INDENT) {
            let (key, value) = split_pair(lineno, item)?;
            let target = match list {
                1 => &mut raw.provenance,
                2 => &mut raw.anchors,
                _ => return err(lineno, "list item outside a list"),
            };
            let mut fields = Fields::new(lineno);
            fields.add(lineno, key, value)?;
            target.push(fields);
        } else if let Some(cont) = line.strip_prefix(CONT_INDENT) {
            let (key, value) = split_pair(lineno, cont)?;
            let target = match list {
                1 => raw.provenance.last_mut(),
                2 => raw.anchors.last_mut(),
                _ => None,
            };
            let Some(fields) = target else {
                return err(lineno, "continuation line outside a list item");
            };
            fields.add(lineno, key, value)?;
        } else if line.starts_with(' ') || line.is_empty() {
            return err(lineno, "bad indentation or blank line");
        } else {
            lines_seen += 1;
            let (key, value) = match line.strip_suffix(':') {
                Some(k) => (k, ""),
                None => split_pair(lineno, line)?,
            };
            first_key.get_or_insert(key);
            list = match key {
                "provenance" | "anchors" => {
                    let i = usize::from(key == "anchors");
                    if std::mem::replace(&mut lists_seen[i], true) {
                        return err(lineno, format!("duplicate key `{key}`"));
                    }
                    match value {
                        "" => i + 1,
                        "[]" => 0,
                        _ => return err(lineno, format!("`{key}` is a list; use `[]` or items")),
                    }
                }
                _ if value.is_empty() => return err(lineno, format!("`{key}` needs a value")),
                _ => {
                    raw.top.add(lineno, key, value)?;
                    0
                }
            };
            if lines_seen == 1 && first_key != Some("format") {
                return err(lineno, "`format` must be the first key");
            }
        }
    }
}

/// Parses a mirror file written by [`render`].
///
/// # Errors
/// [`FormatError`] with the offending line for anything outside the format:
/// unknown or duplicate keys, bad indentation, wrong types, missing keys, an
/// unsupported `format` version.
#[allow(clippy::too_many_lines)] // one linear pass over the format's keys
pub fn parse(input: &str) -> Result<FullMemory, FormatError> {
    let (mut raw, body) = scan(input)?;
    let top = &mut raw.top;
    let (vline, version) = top.req("format")?;
    if version != FORMAT_VERSION.to_string() {
        return err(vline, format!("unsupported format {version}"));
    }
    let uid = top.req_string("uid")?;
    let kind: Kind = top.parsed("kind")?;
    let (sline, scope_name) = top.req("scope")?;
    let project = top.string("project")?;
    let module = top.string("module")?;
    let scope = match (scope_name.as_str(), project, module) {
        ("global", None, None) => Scope::Global,
        ("project", Some(project), None) => Scope::Project(project),
        ("module", Some(project), Some(path)) => Scope::Module { project, path },
        _ => {
            return err(
                sline,
                "`scope` doesn't match its `project` and `module` keys",
            );
        }
    };
    if scope.to_string().parse::<Scope>().is_err() {
        return err(sline, "scope project and module must be non-empty");
    }
    let status: Status = top.parsed("status")?;
    let (cline, confidence) = top.req("confidence")?;
    let confidence: f32 = match confidence.parse() {
        Ok(c) if (0.0..=1.0).contains(&c) => c,
        _ => return err(cline, "`confidence` must be a number within 0..=1"),
    };
    let (iline, importance) = top.req("importance")?;
    let importance: u8 = match importance.parse() {
        Ok(i) if i <= crate::model::MAX_IMPORTANCE => i,
        Ok(_) => return err(iline, "`importance` must be within 0..=3"),
        Err(_) => return err(iline, "`importance` must be a number"),
    };
    let memory_scalars = (
        top.req_number::<u32>("seen_count")?,
        top.req_time("created_at")?,
        top.req_time("updated_at")?,
        top.req_time("valid_from")?,
        top.time("valid_to")?,
        top.string("superseded_by")?,
    );
    let top_line = top.line;
    let leftover = std::mem::replace(top, Fields::new(top_line));
    leftover.finish()?;

    let mut provenance = Vec::new();
    let mut provenance_at = Vec::new();
    for mut f in raw.provenance {
        let origin: Origin = f.parsed("origin")?;
        provenance_at.push(f.req_time("recorded_at")?);
        provenance.push(Provenance {
            origin,
            harness: f.string("harness")?,
            session_id: f.string("session_id")?,
            message_uuid: f.string("message_uuid")?,
            source_path: f.string("source_path")?,
            line: f.number("line")?,
            git_commit: f.string("git_commit")?,
        });
        f.finish()?;
    }
    let mut anchors = Vec::new();
    let mut stale = Vec::new();
    for mut f in raw.anchors {
        anchors.push(Anchor {
            path: f.req_string("path")?,
            symbol: f.string("symbol")?,
            lang: f.string("lang")?,
            content_hash: f.string("content_hash")?,
            git_commit: f.string("git_commit")?,
        });
        stale.push(f.string("stale_reason")?);
        f.finish()?;
    }
    let (seen_count, created_at, updated_at, valid_from, valid_to, superseded_by) = memory_scalars;
    let text = body.strip_suffix('\n').unwrap_or(body).to_owned();
    Ok(FullMemory {
        memory: Memory {
            uid,
            kind,
            text,
            scope,
            confidence,
            importance,
            status,
            superseded_by,
            seen_count,
            created_at,
            updated_at,
            valid_from,
            valid_to,
            provenance,
            anchors,
            stale_reason: None,
        },
        provenance_at,
        stale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;

    /// Small seeded generator (xorshift64*): no `rand` dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            usize::try_from(self.next() % n as u64).unwrap()
        }

        fn chance(&mut self) -> bool {
            self.below(2) == 0
        }

        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[self.below(items.len())]
        }
    }

    const FRAGMENTS: [&str; 26] = [
        "word",
        "Use SQLite",
        " ",
        "  ",
        "\n",
        "\r\n",
        "\"",
        "'",
        ":",
        ": ",
        "---",
        "\n---\n",
        "#",
        "# not a comment",
        "héllo",
        "日本語",
        "😀",
        "\u{2028}",
        "\\",
        "\t",
        "\u{0}",
        "key: v",
        "- item",
        "null",
        "123",
        "",
    ];

    fn random_text(rng: &mut Rng) -> String {
        (0..=rng.below(12)).map(|_| *rng.pick(&FRAGMENTS)).collect()
    }

    fn random_opt(rng: &mut Rng) -> Option<String> {
        rng.chance().then(|| random_text(rng))
    }

    fn random_time(rng: &mut Rng) -> i64 {
        match rng.below(8) {
            0 => i64::try_from(rng.next() >> 2).unwrap(),
            1 => -i64::try_from(rng.next() >> 3).unwrap(),
            _ => i64::try_from(rng.next() % 4_000_000_000_000).unwrap(),
        }
    }

    fn random_memory(rng: &mut Rng) -> FullMemory {
        let non_empty = |rng: &mut Rng| format!("p{}", random_text(rng).replace('\u{1f}', ""));
        let scope = match rng.below(3) {
            0 => Scope::Global,
            1 => Scope::Project(non_empty(rng)),
            _ => Scope::Module {
                project: non_empty(rng),
                path: non_empty(rng),
            },
        };
        let kind = *rng.pick(Kind::ALL);
        let text = random_text(rng);
        let provenance: Vec<Provenance> = (0..rng.below(4))
            .map(|_| Provenance {
                origin: *rng.pick(Origin::ALL),
                harness: random_opt(rng),
                session_id: random_opt(rng),
                message_uuid: random_opt(rng),
                source_path: random_opt(rng),
                line: rng
                    .chance()
                    .then(|| i64::try_from(rng.next() >> 1).unwrap() - 5),
                git_commit: random_opt(rng),
            })
            .collect();
        let anchors: Vec<Anchor> = (0..rng.below(4))
            .map(|_| Anchor {
                path: random_text(rng),
                symbol: random_opt(rng),
                lang: random_opt(rng),
                content_hash: random_opt(rng),
                git_commit: random_opt(rng),
            })
            .collect();
        FullMemory {
            provenance_at: provenance.iter().map(|_| random_time(rng)).collect(),
            stale: anchors.iter().map(|_| random_opt(rng)).collect(),
            memory: Memory {
                uid: model::uid(kind, &scope, &text),
                kind,
                text,
                scope,
                confidence: f32::from(u16::try_from(rng.below(1001)).unwrap()) / 1000.0,
                importance: u8::try_from(rng.below(4)).unwrap(),
                status: *rng.pick(Status::ALL),
                superseded_by: random_opt(rng),
                seen_count: u32::try_from(rng.next() >> 33).unwrap(),
                created_at: random_time(rng),
                updated_at: random_time(rng),
                valid_from: random_time(rng),
                valid_to: rng.chance().then(|| random_time(rng)),
                provenance,
                anchors,
                stale_reason: None,
            },
        }
    }

    #[test]
    fn round_trips_300_random_memories() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for i in 0..300 {
            let doc = random_memory(&mut rng);
            let text = render(&doc);
            let back = parse(&text).unwrap_or_else(|e| panic!("#{i}: {e}\n{text}"));
            assert_eq!(back, doc, "#{i}\n{text}");
            assert_eq!(render(&back), text, "#{i}: render is not stable");
        }
    }

    fn fixed() -> FullMemory {
        let scope = Scope::Module {
            project: "/work/recalla".into(),
            path: "src/store".into(),
        };
        let text = "Use WAL.\nKeep `---` lines intact:\n---\nok";
        FullMemory {
            memory: Memory {
                uid: model::uid(Kind::Decision, &scope, text),
                kind: Kind::Decision,
                text: text.into(),
                scope,
                confidence: 0.85,
                importance: 2,
                status: Status::NeedsReverify,
                superseded_by: None,
                seen_count: 3,
                created_at: 1_735_787_045_678,
                updated_at: 1_735_787_046_000,
                valid_from: 1_735_787_045_678,
                valid_to: None,
                provenance: vec![Provenance {
                    harness: Some("claude-code".into()),
                    session_id: Some("s 1".into()),
                    line: Some(42),
                    ..Provenance::new(Origin::Backfill)
                }],
                anchors: vec![Anchor {
                    symbol: Some("Store::open".into()),
                    lang: Some("rust".into()),
                    ..Anchor::new("src/store/mod.rs")
                }],
                stale_reason: None,
            },
            provenance_at: vec![1_735_787_045_678],
            stale: vec![Some("symbol changed: Store::open".into())],
        }
    }

    #[test]
    fn golden_render() {
        let uid = fixed().memory.uid;
        let expected = format!(
            "---
format: 1
uid: {uid}
kind: decision
scope: module
project: /work/recalla
module: src/store
status: needs_reverify
confidence: 0.85
importance: 2
seen_count: 3
created_at: 2025-01-02T03:04:05.678Z
updated_at: 2025-01-02T03:04:06.000Z
valid_from: 2025-01-02T03:04:05.678Z
provenance:
  - origin: backfill
    recorded_at: 2025-01-02T03:04:05.678Z
    harness: claude-code
    session_id: \"s 1\"
    line: 42
anchors:
  - path: src/store/mod.rs
    symbol: Store::open
    lang: rust
    stale_reason: \"symbol changed: Store::open\"
---
Use WAL.
Keep `---` lines intact:
---
ok
"
        );
        assert_eq!(render(&fixed()), expected);
        assert_eq!(parse(&expected).unwrap(), fixed());
    }

    #[test]
    fn bare_and_quoted_scalars() {
        for bare in [
            "abc",
            "src/a.rs",
            "git@github.com:a/b",
            "a-b_c+d",
            "0xff",
            "1.2.3",
        ] {
            assert_eq!(scalar(bare), bare);
        }
        for quoted in [
            "", "null", "True", "yes", "123", "-1.5", "1e5", "inf", "NaN", "a b", "-", "@x",
            "tab\t", "é", "a\"b", "p:",
        ] {
            assert!(scalar(quoted).starts_with('"'), "{quoted}");
        }
    }

    #[test]
    fn out_of_range_times_round_trip_as_integers() {
        let mut doc = fixed();
        doc.memory.created_at = i64::MAX;
        doc.memory.valid_from = i64::MIN;
        let text = render(&doc);
        assert!(text.contains(&format!("created_at: {}\n", i64::MAX)));
        assert_eq!(parse(&text).unwrap(), doc);
    }

    fn fail(input: &str) -> FormatError {
        parse(input).unwrap_err()
    }

    #[test]
    fn parse_errors_name_the_line() {
        let good = render(&fixed());
        let edit = |from: &str, to: &str| good.replacen(from, to, 1);
        let cases = [
            ("no fence\n", 1, "starts with"),
            (&edit("format: 1\n", "format: 2\n"), 2, "unsupported format"),
            (
                &edit("kind: decision\n", "kind: decision\nmood: odd\n"),
                5,
                "unknown key",
            ),
            (
                &edit("kind: decision\n", "kind: decision\nkind: fact\n"),
                5,
                "duplicate",
            ),
            (&edit("kind: decision", "kind: bogus"), 4, "invalid `kind`"),
            (&edit("importance: 2", "importance: \"2\""), 10, "number"),
            (&edit("importance: 2", "importance: 9"), 10, "within 0..=3"),
            (
                &edit("confidence: 0.85", "confidence: 1.5"),
                9,
                "within 0..=1",
            ),
            (
                &edit("seen_count: 3\n", ""),
                0,
                "missing required key `seen_count`",
            ),
            (
                &edit(
                    "created_at: 2025-01-02T03:04:05.678Z",
                    "created_at: yesterday",
                ),
                12,
                "timestamp",
            ),
            (
                &edit("    harness: claude-code\n", "   harness: x\n"),
                18,
                "bad indentation",
            ),
            (
                &edit("    line: 42\n", "    line: 42\n    wat: 1\n"),
                21,
                "unknown key",
            ),
            (
                &edit("scope: module\n", "scope: project\n"),
                5,
                "doesn't match",
            ),
            (&edit("\"s 1\"", "\"s 1"), 19, "string literal"),
            (&edit("anchors:\n", "anchors: nope\n"), 21, "is a list"),
            (
                &edit("module: src/store\n", "\nmodule: src/store\n"),
                7,
                "blank",
            ),
            (&edit("uid:", "uid:  "), 3, "bare word"),
        ];
        for (input, line, msg) in cases {
            let e = fail(input);
            assert!(e.msg.contains(msg), "{e} (wanted {msg:?}) in\n{input}");
            if line > 0 {
                assert_eq!(e.line, line, "{e}");
            }
        }
        // A missing close and a missing required item key.
        assert!(fail("---\nformat: 1\n").msg.contains("not closed"));
        let no_path = edit("  - path: src/store/mod.rs\n    symbol", "  - symbol");
        let e = fail(&no_path);
        assert!(e.msg.contains("missing required key `path`"), "{e}");
        assert_eq!(e.line, 22);
        let first = edit("format: 1\nuid", "uid");
        assert!(fail(&first).msg.contains("`format` must be the first key"));
    }

    #[test]
    fn body_ends_at_the_first_fence_and_keeps_everything_after() {
        let mut doc = fixed();
        doc.memory.text = "a\n---\nformat: 2\n---\n\n".into();
        doc.memory.uid = model::uid(doc.memory.kind, &doc.memory.scope, &doc.memory.text);
        let back = parse(&render(&doc)).unwrap();
        assert_eq!(back.memory.text, doc.memory.text);
    }
}
