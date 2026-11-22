//! Pure text helpers for the extractor: code stripping, sentence splitting
//! and path-token detection. No I/O.

use std::sync::LazyLock;

use regex::Regex;

const MIN_SENTENCE: usize = 15;
const MAX_SENTENCE: usize = 280;
const MAX_INLINE_SPAN: usize = 40;
/// A sentence is dropped when more than this percentage of it is redaction markers.
const MAX_REDACTED_PCT: usize = 30;
const REDACTED_OPEN: &str = "[REDACTED:";
const ABBREVIATIONS: &[&str] = &["e.g.", "i.e.", "etc."];

/// File extensions treated as source/doc paths (anchors and scope).
const PATH_EXTS: &[&str] = &[
    "rs", "toml", "md", "py", "js", "ts", "tsx", "jsx", "go", "java", "c", "h", "cpp", "hpp",
    "json", "yaml", "yml", "sh", "sql", "html", "css", "rb", "swift", "kt", "lock",
];
/// A bare `a/b` is a path only when the first segment is one of these.
const COMMON_DIRS: &[&str] = &[
    "src", "lib", "tests", "test", "docs", "app", "pkg", "cmd", "bin", "crates",
];

static JSON_KEY_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^"[^"]+"\s*:"#).expect("valid regex"));
static BACKTICKED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`[^`]+`").expect("valid regex"));
static LINE_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":\d+(?::\d+)?$").expect("valid regex"));

/// Removes fenced code (triple-backtick and `~~~`), inline backtick spans longer than 40
/// chars, and lines that look like JSON/YAML dumps (start with `{`, `[` or
/// `"key":`). Lines starting with a `[REDACTED:` marker are kept. An unclosed
/// fence drops the rest of the text.
#[must_use]
pub fn strip_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(marker) = fence {
            if t.starts_with(marker) {
                fence = None;
            }
            continue;
        }
        if let Some(marker) = ["```", "~~~"].into_iter().find(|m| t.starts_with(m)) {
            // A fence that closes on its own line (```code```) drops only that line.
            if !t[marker.len()..].contains(marker) {
                fence = Some(marker);
            }
            continue;
        }
        let dump = t.starts_with('{')
            || (t.starts_with('[') && !t.starts_with(REDACTED_OPEN))
            || JSON_KEY_LINE.is_match(t);
        if dump {
            continue;
        }
        strip_long_spans(line, &mut out);
        out.push('\n');
    }
    out
}

fn strip_long_spans(line: &str, out: &mut String) {
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(len) = after.find('`') else { break };
        let span = &after[..len];
        out.push_str(&rest[..open]);
        if span.chars().count() <= MAX_INLINE_SPAN {
            out.push('`');
            out.push_str(span);
            out.push('`');
        }
        rest = &after[len + 1..];
    }
    out.push_str(rest);
}

/// Splits on `. ! ?` followed by whitespace or end, and on newlines. Does not
/// split after `e.g.`/`i.e.`/`etc.`, nor inside version numbers or file names
/// (their dots are not followed by whitespace). Keeps trimmed sentences of
/// 15..=280 chars; drops questions and sentences that are mostly
/// `[REDACTED:` markers.
#[must_use]
pub fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut start = 0;
        for (i, c) in line.char_indices() {
            if !matches!(c, '.' | '!' | '?') {
                continue;
            }
            let end = i + 1;
            let boundary = line[end..].chars().next().is_none_or(char::is_whitespace);
            if boundary && !(c == '.' && ends_with_abbreviation(&line[start..end])) {
                keep(&line[start..end], &mut out);
                start = end;
            }
        }
        keep(&line[start..], &mut out);
    }
    out
}

fn ends_with_abbreviation(segment: &str) -> bool {
    let word = segment.rsplit(char::is_whitespace).next().unwrap_or("");
    let word = word
        .trim_start_matches(['(', '"', '\''])
        .to_ascii_lowercase();
    ABBREVIATIONS.contains(&word.as_str())
}

fn keep<'a>(raw: &'a str, out: &mut Vec<&'a str>) {
    let s = raw.trim();
    let s = s
        .strip_prefix("- ")
        .or_else(|| s.strip_prefix("* "))
        .unwrap_or(s)
        .trim();
    let chars = s.chars().count();
    if !(MIN_SENTENCE..=MAX_SENTENCE).contains(&chars) {
        return;
    }
    if s.trim_end_matches(['"', '\'', ')']).ends_with('?') {
        return;
    }
    if redacted_chars(s) * 100 > chars * MAX_REDACTED_PCT {
        return;
    }
    out.push(s);
}

fn redacted_chars(s: &str) -> usize {
    let mut total = 0;
    let mut rest = s;
    while let Some(i) = rest.find(REDACTED_OPEN) {
        let tail = &rest[i..];
        let len = tail.find(']').map_or(tail.len(), |j| j + 1);
        total += tail[..len].chars().count();
        rest = &tail[len..];
    }
    total
}

/// True when the sentence starts with a quote/blockquote marker, or `at` (a
/// byte offset) sits inside an unclosed double-quoted span.
#[must_use]
pub fn is_quoted(sentence: &str, at: usize) -> bool {
    sentence.starts_with(['"', '>', '\u{201c}']) || sentence[..at].matches('"').count() % 2 == 1
}

const INTERROGATIVES: &[&str] = &[
    "what", "why", "how", "when", "where", "which", "who", "can", "could", "would", "should", "is",
    "are", "do", "does", "did", "will",
];

static QUESTION_PHRASE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bi need to understand\b|\btell me (?:why|what|how)\b").expect("valid regex")
});

/// True when `sentence` is a question without needing a `?`: it opens with an
/// interrogative word (`do not ...` stays an imperative), or asks to be told
/// or to have something explained.
#[must_use]
pub fn is_question(sentence: &str) -> bool {
    let mut words = sentence
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .split_whitespace();
    let first = words.next().map(str::to_ascii_lowercase);
    let opens = first.as_deref().is_some_and(|w| {
        INTERROGATIVES.contains(&w)
            && !(w == "do" && words.next().is_some_and(|n| n.eq_ignore_ascii_case("not")))
            // "When writing tests, always use fixtures." is a conditional
            // clause, not a question: when/where followed by a comma clause.
            && !(matches!(w, "when" | "where") && sentence.contains(", "))
    });
    opens || QUESTION_PHRASE.is_match(sentence)
}

/// True when the text contains a non-empty backticked span.
#[must_use]
pub fn has_backticked(text: &str) -> bool {
    BACKTICKED.is_match(text)
}

/// A whitespace token stripped of wrapping quotes/brackets/punctuation and a
/// trailing `:line[:col]`. Empty if nothing path-like remains.
fn clean_token(tok: &str) -> &str {
    let t = tok
        .trim_start_matches(['`', '"', '\'', '(', '[', '<'])
        .trim_end_matches(['`', '"', '\'', ')', ']', '>', ',', ';', ':', '!', '?', '.']);
    LINE_SUFFIX.find(t).map_or(t, |m| &t[..m.start()])
}

fn valid_path_chars(t: &str) -> bool {
    !t.is_empty()
        && t.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '~' | '@' | '+' | '-')
        })
}

fn has_path_ext(t: &str) -> bool {
    t.rsplit_once('.').is_some_and(|(head, ext)| {
        !head.is_empty()
            && !head.ends_with('/')
            && PATH_EXTS.contains(&ext.to_ascii_lowercase().as_str())
    })
}

/// Relative or absolute source paths named in `sentence`: tokens with a known
/// source/doc extension (`src/store/mod.rs`, `main.rs`). Deduplicated, in
/// order. URLs are skipped.
#[must_use]
pub fn path_tokens(sentence: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in sentence.split_whitespace() {
        let t = clean_token(tok);
        if !t.contains("://")
            && valid_path_chars(t)
            && has_path_ext(t)
            && !out.iter().any(|p| p == t)
        {
            out.push(t.to_owned());
        }
    }
    out
}

/// True when the sentence names a path-like token (a `/` path or a file with
/// a known extension) or a backticked identifier.
#[must_use]
pub fn mentions_project_specifics(sentence: &str) -> bool {
    has_backticked(sentence)
        || sentence.split_whitespace().any(|tok| {
            let t = clean_token(tok);
            if t.contains("://") || !valid_path_chars(t) {
                return false;
            }
            has_path_ext(t) || is_slash_path(t)
        })
}

/// `a/b/c`, `/etc`, `./x`, `src/store`; but not `and/or`.
fn is_slash_path(t: &str) -> bool {
    let slashes = t.matches('/').count();
    if slashes == 0 || t == "/" {
        return false;
    }
    let first = t.split('/').next().unwrap_or("");
    slashes >= 2 || t.starts_with(['/', '~', '.']) || COMMON_DIRS.contains(&first)
}

#[cfg(test)]
mod tests {

    #[test]
    fn conditional_clause_is_not_a_question() {
        assert!(!is_question("When writing tests, always use fixtures."));
        assert!(!is_question("Where possible, prefer pure functions."));
        assert!(is_question("when do we deploy this"));
    }

    use super::*;

    #[test]
    fn strips_fenced_blocks_both_kinds() {
        let t = "Keep this line here.\n```rust\nlet x = 1;\n```\nAnd this one too.\n~~~\nraw\n~~~\nEnd of text here.";
        assert_eq!(
            strip_code(t),
            "Keep this line here.\nAnd this one too.\nEnd of text here.\n"
        );
    }

    #[test]
    fn single_line_fence_and_unclosed_fence() {
        assert_eq!(
            strip_code("a ok line\n```x```\nb ok line"),
            "a ok line\nb ok line\n"
        );
        assert_eq!(
            strip_code("before it\n```\nnever closed\nmore"),
            "before it\n"
        );
    }

    #[test]
    fn strips_only_long_inline_spans() {
        let long = "x".repeat(41);
        let t = format!("Use `short` and `{long}` here.");
        assert_eq!(strip_code(&t), "Use `short` and  here.\n");
        let edge = "y".repeat(40);
        assert!(strip_code(&format!("see `{edge}`")).contains(&edge));
        // Unbalanced backtick is left alone.
        assert_eq!(strip_code("odd ` tick"), "odd ` tick\n");
    }

    #[test]
    fn strips_json_and_yaml_dump_lines() {
        let t = "{\"a\": 1}\n[1, 2]\n\"key\": \"v\"\n  { indented }\nreal sentence stays.";
        assert_eq!(strip_code(t), "real sentence stays.\n");
    }

    #[test]
    fn keeps_redacted_marker_lines() {
        assert_eq!(
            strip_code("[REDACTED:token] was removed"),
            "[REDACTED:token] was removed\n"
        );
    }

    #[test]
    fn splits_on_terminators_and_newlines() {
        let s = sentences("First sentence is here. Second one follows!\nThird line has no stop");
        assert_eq!(
            s,
            [
                "First sentence is here.",
                "Second one follows!",
                "Third line has no stop"
            ]
        );
    }

    #[test]
    fn does_not_split_abbreviations_versions_or_filenames() {
        let s =
            sentences("Use tools, e.g. cargo or make, to build. Bump to v1.2.3 in main.rs today.");
        assert_eq!(
            s,
            [
                "Use tools, e.g. cargo or make, to build.",
                "Bump to v1.2.3 in main.rs today."
            ]
        );
        assert_eq!(
            sentences("Pick one of: a, b, etc. and then proceed."),
            ["Pick one of: a, b, etc. and then proceed."]
        );
        assert_eq!(
            sentences("That is, i.e. the same thing here."),
            ["That is, i.e. the same thing here."]
        );
    }

    #[test]
    fn length_bounds() {
        assert!(sentences("Too short.").is_empty());
        assert_eq!(sentences(&format!("{}.", "a".repeat(14))).len(), 1);
        assert!(sentences(&format!("{}.", "a".repeat(13))).is_empty());
        let long = format!("{}.", "a".repeat(280));
        assert!(sentences(&long).is_empty());
        let ok = format!("{}.", "a".repeat(279));
        assert_eq!(sentences(&ok).len(), 1);
    }

    #[test]
    fn drops_questions() {
        assert!(sentences("Should we use the other approach?").is_empty());
        assert_eq!(
            sentences("Is this right? Then keep going with it."),
            ["Then keep going with it."]
        );
        assert!(sentences("Is it really done (maybe)?").is_empty());
    }

    #[test]
    fn detects_questions_without_a_mark() {
        for q in [
            "What i need to understand is why we chose redis",
            "how do we deploy this",
            "Should we always run the formatter",
            "Can you also check the docs",
            "Is the API on Node 20 in production",
            "Do we really never mock the database",
            "\"Why did the build fail\" is what I asked",
            "Please tell me why the cache broke again",
            "Now I need to understand the retry policy",
        ] {
            assert!(is_question(q), "{q}");
        }
        for s in [
            "Do not mock the database in integration tests.",
            "Don't add comments that restate the code.",
            "Always run the formatter before committing.",
            "We decided to keep the store in one module.",
            "Whatever we pick, keep it simple and small.",
            "Island hopping is how we describe it.",
            "",
        ] {
            assert!(!is_question(s), "{s}");
        }
    }

    #[test]
    fn drops_mostly_redacted_sentences() {
        assert!(sentences("[REDACTED:key] [REDACTED:key] and x.").is_empty());
        let kept = "Put the value [REDACTED:key] in the environment file please.";
        assert_eq!(sentences(kept), [kept]);
    }

    #[test]
    fn strips_bullets_and_trims() {
        assert_eq!(
            sentences("  - Always run the tests first.  "),
            ["Always run the tests first."]
        );
        assert_eq!(
            sentences("* Never skip the lint step."),
            ["Never skip the lint step."]
        );
    }

    #[test]
    fn handles_multibyte_text() {
        let s = sentences("Always keep the café menu in sync. Naïve split must not panic here.");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn quoted_detection() {
        assert!(is_quoted("\"Always do it\" he said", 1));
        assert!(is_quoted("> Never do that again", 2));
        assert!(is_quoted("He said \"never do that\" twice", 10));
        assert!(!is_quoted("He said \"x\" and never did", 15));
    }

    #[test]
    fn path_tokens_find_source_paths() {
        assert_eq!(
            path_tokens("Edit `src/store/mod.rs`, then main.rs and /abs/dir/lib.py:42."),
            ["src/store/mod.rs", "main.rs", "/abs/dir/lib.py"]
        );
        assert!(path_tokens("See https://example.com/a/b.html and e.g. foo and v1.2.3").is_empty());
        assert!(path_tokens("visit github.com or use 1.5 percent").is_empty());
        assert_eq!(path_tokens("a.rs and a.rs again"), ["a.rs"]);
    }

    #[test]
    fn project_specifics_detection() {
        for yes in [
            "keep it in src/store",
            "edit main.rs",
            "call `foo_bar` first",
            "see /etc/hosts",
            "use ./scripts/run",
            "put it under a/b/c",
        ] {
            assert!(mentions_project_specifics(yes), "{yes}");
        }
        for no in [
            "use and/or freely",
            "keep it 24/7",
            "write small commits",
            "see https://example.com/page",
            "version 1.2.3 is fine",
            "e.g. this thing",
        ] {
            assert!(!mentions_project_specifics(no), "{no}");
        }
    }
}
