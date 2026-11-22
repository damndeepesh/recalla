//! Secret scrubbing for all text before it is persisted.
//!
//! Three layers, all offline and linear-time (`regex` has no backtracking):
//! 1. known credential formats ([`rules`]),
//! 2. values of keyword assignments such as `password=...` ([`rules`]),
//! 3. a Shannon-entropy catch-all for unknown token formats ([`entropy`]).
//!
//! Layers never defer to each other: each emits its own candidates and
//! [`merge`] resolves overlaps. Header-shaped keys get the same header
//! parsing in layers 1 and 2. A final residual pass redacts any value whose
//! key or header was recognised as secret-bearing but that no finding
//! covers (see `rules::residual`).
//!
//! Input is normally unescaped: harness parsers JSON-decode strings before
//! calling [`redact`]. Tolerance for still-escaped text (`\"`, `\n`, `\/`
//! in tool output that contains JSON) is defense in depth only; there is
//! deliberately no JSON unescaper here.
//!
//! Each finding is replaced by `[REDACTED:<rule>]`. Markers are never
//! re-matched, and the output is rescanned to a fixpoint, so `redact` is
//! idempotent. Best-effort, not a guarantee: novel
//! low-entropy secrets without a telling key name can slip through.

mod entropy;
mod rules;

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

/// One redacted span. Offsets are byte offsets into the ORIGINAL text and
/// always fall on UTF-8 char boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Short kebab-case rule id, e.g. `github-token`.
    pub rule: &'static str,
    pub start: usize,
    pub end: usize,
}

/// Result of [`redact`]: the scrubbed text and what was removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted<'a> {
    /// Borrowed when nothing was found, so clean text costs no allocation.
    pub text: Cow<'a, str>,
    /// Sorted by `start`, non-overlapping.
    pub findings: Vec<Finding>,
}

impl Redacted<'_> {
    /// True when no secret was found (the text is returned unchanged).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// A raw detection before merging. `layer` breaks ties (1 beats 2 beats 3).
#[derive(Debug, Clone, Copy)]
struct Candidate {
    rule: &'static str,
    start: usize,
    end: usize,
    layer: u8,
}

/// A value region a layer recognised as secret-bearing (a secret key or a
/// credential header), for the residual pass. `min` is the key's minimum
/// value length.
#[derive(Debug, Clone, Copy)]
struct Recognised {
    start: usize,
    end: usize,
    min: usize,
}

/// Matches a marker we emitted, so no layer ever re-redacts it.
static MARKER: LazyLock<Regex> = LazyLock::new(|| re(r"\[REDACTED:[a-z0-9-]+\]"));

/// Compiles a built-in pattern. Patterns are constants covered by tests, so a
/// failure is a programming error.
fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|e| panic!("invalid built-in redact pattern: {e}"))
}

/// Extra detection passes over our own output before giving up on a
/// fixpoint. Real text converges in one; the cap bounds pathological input.
const MAX_PASSES: usize = 4;

/// Scrubs secrets from `text`, replacing each with `[REDACTED:<rule>]`.
///
/// Idempotent by construction: the output is scanned again, and anything
/// found there (a marker can join two runs into one, or swallow a quote
/// that bounded a value) is mapped back to the original text and merged,
/// until a pass finds nothing (at most [`MAX_PASSES`] extra passes).
#[must_use]
pub fn redact(text: &str) -> Redacted<'_> {
    redact_layers(text, true)
}

/// As [`redact`], for file paths and branch names: layers 1 and 2 plus the
/// residual pass (known formats, keyword assignments, URL credentials), but
/// not the entropy layer. Random path components (tempdirs, hashes) are
/// common and rarely secrets; redacting them would corrupt project keys.
#[must_use]
pub fn redact_path(text: &str) -> Redacted<'_> {
    redact_layers(text, false)
}

fn redact_layers(text: &str, entropy: bool) -> Redacted<'_> {
    let mut findings = detect(text, entropy);
    if findings.is_empty() {
        return Redacted {
            text: Cow::Borrowed(text),
            findings,
        };
    }
    let mut out = render(text, &findings);
    for _ in 0..MAX_PASSES {
        let again = detect(&out, entropy);
        if again.is_empty() {
            break;
        }
        let mut all: Vec<Candidate> = findings
            .iter()
            .map(|f| Candidate {
                rule: f.rule,
                start: f.start,
                end: f.end,
                layer: 0,
            })
            .collect();
        all.extend(map_back(&findings, &again));
        findings = merge(all);
        out = render(text, &findings);
    }
    Redacted {
        text: Cow::Owned(out),
        findings,
    }
}

/// All layers (entropy only when asked) plus the residual pass over `text`, merged.
fn detect(text: &str, entropy: bool) -> Vec<Finding> {
    let (mut found, mut seen) = (Vec::new(), Vec::new());
    rules::known(text, &mut found);
    rules::assignments(text, &mut found, &mut seen);
    if entropy {
        entropy::scan(text, &mut found);
    }

    let markers: Vec<(usize, usize)> = MARKER
        .find_iter(text)
        .map(|m| (m.start(), m.end()))
        .collect();
    let clean = |found: &mut Vec<Candidate>| {
        found.retain(|c| c.start < c.end && !inside_marker(&markers, c));
        for c in found.iter_mut() {
            trim_escapes(text, c);
        }
    };
    clean(&mut found);

    let findings = merge(found.clone());
    let mut extra = Vec::new();
    rules::residual(text, &seen, &findings, &mut extra);
    clean(&mut extra);
    if extra.is_empty() {
        return findings;
    }
    found.extend(extra);
    merge(found)
}

/// `text` with each finding replaced by its marker.
fn render(text: &str, findings: &[Finding]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for f in findings {
        out.push_str(&text[last..f.start]);
        out.push_str("[REDACTED:");
        out.push_str(f.rule);
        out.push(']');
        last = f.end;
    }
    out.push_str(&text[last..]);
    out
}

/// Maps findings in `render(text, findings)` back to `text`. A position
/// inside a marker widens to the whole span that marker replaced; others
/// shift by the length change of the markers before them. Text outside
/// markers is copied verbatim, so char boundaries carry over.
fn map_back(findings: &[Finding], again: &[Finding]) -> Vec<Candidate> {
    // (output start, output end, original start, original end) per marker.
    let mut spans = Vec::with_capacity(findings.len());
    let mut shift = 0isize;
    for f in findings {
        let out_start = f.start.saturating_add_signed(shift);
        let len = "[REDACTED:]".len() + f.rule.len();
        spans.push((out_start, out_start + len, f.start, f.end));
        shift += isize::try_from(len).unwrap_or(isize::MAX)
            - isize::try_from(f.end - f.start).unwrap_or(isize::MAX);
    }
    let to_orig = |p: usize, is_end: bool| {
        // The last marker starting at or before `p` (strictly before, for
        // an exclusive end).
        let i = spans.partition_point(|s| if is_end { s.0 < p } else { s.0 <= p });
        match i.checked_sub(1).map(|j| spans[j]) {
            None => p,
            Some((_, oe, s, e)) if p < oe || (is_end && p == oe) => {
                if is_end {
                    e
                } else {
                    s
                }
            }
            Some((_, oe, _, e)) => e + (p - oe),
        }
    };
    again
        .iter()
        .map(|f| Candidate {
            rule: f.rule,
            start: to_orig(f.start, false),
            end: to_orig(f.end, true),
            layer: 0,
        })
        .collect()
}

/// True when `c` lies entirely within one marker. `markers` is sorted and
/// non-overlapping, so a binary search keeps this O(log n) per candidate.
fn inside_marker(markers: &[(usize, usize)], c: &Candidate) -> bool {
    let i = markers.partition_point(|&(s, _)| s <= c.start);
    i > 0 && c.end <= markers[i - 1].1
}

/// Keeps JSON/shell escapes intact when text is still escaped (raw JSONL):
/// a span must not swallow the letter of a leading `\n` or end on a lone `\`,
/// or the surrounding string literal breaks. Edge whitespace is trimmed
/// too: a marker that swallowed a space would join two runs into one on
/// the next pass, breaking idempotence.
fn trim_escapes(text: &str, c: &mut Candidate) {
    let b = text.as_bytes();
    while c.start < c.end && b[c.start].is_ascii_whitespace() {
        c.start += 1;
    }
    if c.start > 0
        && c.start < c.end
        && b[c.start - 1] == b'\\'
        && b"nrtbfu\"\\/".contains(&b[c.start])
    {
        c.start += 1;
    }
    while c.end > c.start && (b[c.end - 1] == b'\\' || b[c.end - 1].is_ascii_whitespace()) {
        c.end -= 1;
    }
}

/// Sorts candidates and merges overlapping or touching spans. The merged
/// span keeps the rule of its first candidate: earliest start, then the
/// longer span, then the lower layer, then detection order.
fn merge(mut found: Vec<Candidate>) -> Vec<Finding> {
    found.retain(|c| c.start < c.end);
    // Stable sort: equal keys keep detection (rule) order.
    found.sort_by_key(|c| (c.start, std::cmp::Reverse(c.end), c.layer));
    let mut out: Vec<Finding> = Vec::with_capacity(found.len());
    for c in found {
        match out.last_mut() {
            Some(last) if c.start <= last.end => last.end = last.end.max(c.end),
            _ => out.push(Finding {
                rule: c.rule,
                start: c.start,
                end: c.end,
            }),
        }
    }
    out
}

/// Deterministic pseudo-random token for tests. Fixtures are built at
/// runtime so no secret-shaped literal is ever committed (GitHub push
/// protection and `GitGuardian` scan this repo).
#[cfg(test)]
pub(crate) fn test_token(seed: u64, len: usize, alphabet: &[u8]) -> String {
    let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..len)
        .map(|_| {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let i = usize::try_from(x >> 33).unwrap_or(0) % alphabet.len();
            char::from(alphabet[i])
        })
        .collect()
}

#[cfg(test)]
pub(crate) const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(rule: &'static str, start: usize, end: usize, layer: u8) -> Candidate {
        Candidate {
            rule,
            start,
            end,
            layer,
        }
    }

    fn spans(f: &[Finding]) -> Vec<(&'static str, usize, usize)> {
        f.iter().map(|f| (f.rule, f.start, f.end)).collect()
    }

    #[test]
    fn redact_path_keeps_random_components_but_catches_credentials() {
        let tmp = "/private/var/folders/xy/abcDEF123ghiJKL456/T/.tmpQ8zK2pLm9Rw4";
        assert!(
            redact(tmp)
                .findings
                .iter()
                .any(|f| f.rule == "high-entropy")
        );
        let r = redact_path(tmp);
        assert!(r.is_clean());
        assert_eq!(r.text, tmp);

        let token = format!("ghp_{}", test_token(11, 36, ALNUM));
        let cwd = format!("/work/{token}/app");
        let r = redact_path(&cwd);
        assert_eq!(r.text, "/work/[REDACTED:github-token]/app");
        let url = format!("https://user:{}@host/repo", test_token(5, 16, ALNUM));
        assert!(!redact_path(&url).is_clean());
    }

    #[test]
    fn redact_path_is_idempotent() {
        let token = format!("ghp_{}", test_token(12, 36, ALNUM));
        for input in [
            format!("/work/{token}/app"),
            format!("/a/password={}/b", test_token(3, 16, ALNUM)),
            "/private/var/folders/xy/abcDEF123ghiJKL456/T/.tmpQ8zK2pLm9Rw4".to_owned(),
            "feature/plain-branch".to_owned(),
        ] {
            let once = redact_path(&input).text.into_owned();
            let twice = redact_path(&once);
            assert!(twice.is_clean(), "{once} -> {}", twice.text);
            assert_eq!(twice.text, once);
        }
    }

    #[test]
    fn clean_text_is_borrowed() {
        let r = redact("nothing to see here, just a normal sentence.");
        assert!(r.is_clean());
        assert!(matches!(r.text, Cow::Borrowed(_)));
    }

    #[test]
    fn empty_text_is_clean() {
        let r = redact("");
        assert!(r.is_clean());
        assert_eq!(r.text, "");
    }

    #[test]
    fn merge_keeps_earliest_rule_and_unions_spans() {
        let m = merge(vec![cand("b", 5, 20, 1), cand("a", 2, 10, 3)]);
        assert_eq!(spans(&m), vec![("a", 2, 20)]);
    }

    #[test]
    fn merge_tie_prefers_longer_then_lower_layer() {
        let m = merge(vec![cand("short", 0, 5, 1), cand("long", 0, 9, 3)]);
        assert_eq!(spans(&m), vec![("long", 0, 9)]);
        let m = merge(vec![cand("l3", 0, 9, 3), cand("l1", 0, 9, 1)]);
        assert_eq!(spans(&m), vec![("l1", 0, 9)]);
    }

    #[test]
    fn merge_joins_touching_and_keeps_disjoint() {
        let m = merge(vec![
            cand("a", 0, 4, 1),
            cand("b", 4, 8, 1),
            cand("c", 9, 12, 2),
        ]);
        assert_eq!(spans(&m), vec![("a", 0, 8), ("c", 9, 12)]);
    }

    #[test]
    fn merge_drops_empty_spans() {
        assert_eq!(merge(vec![cand("a", 3, 3, 1)]), vec![]);
    }

    #[test]
    fn trim_escapes_keeps_json_escapes() {
        let text = r#"a\nSECRET\";"#;
        let mut c = cand("x", 2, 10, 3); // covers `nSECRET\`
        trim_escapes(text, &mut c);
        assert_eq!(&text[c.start..c.end], "SECRET");
    }

    /// Adversarial inputs that would be quadratic if any layer rescanned
    /// values. Linear time is the guarantee; the generous wall-clock bound
    /// (debug build, slow CI, plus the fixpoint pass over the output) only
    /// catches a regression to quadratic, which takes minutes on 1 MB.
    #[test]
    fn adversarial_inputs_stay_linear() {
        // Scaling check, not a wall-clock bound: CI runners vary a lot in
        // speed. Linear code takes ~4x as long on 4x the input; quadratic
        // takes ~16x. A ratio under 8 passes on any machine.
        type Gen = fn(usize) -> String;
        let cases: &[Gen] = &[
            |n| "token=(".repeat(n / 7),
            |n| format!("\"{}", "password=\"".repeat(n / 10)),
            |n| format!("password=\"{}", "a".repeat(n)),
            |n| "token: str ".repeat(n / 11),
            |n| "[REDACTED:x]token=s3cretValue9 ".repeat(n / 32),
            |n| format!("{}\n", "x".repeat(n)),
            |n| "token=f(".repeat(n / 8),
            |n| "token=a.b;".repeat(n / 10),
            |n| "token:token:".repeat(n / 12),
            |n| "Authorization: ".repeat(n / 15),
            |n| "Authorization: Digest a=".repeat(n / 24),
            |n| "Cookie: a=b;".repeat(n / 12),
            |n| format!("https://u:{}", "p@".repeat(n / 2)),
            |n| "mysql -u r ".repeat(n / 11),
            |n| "docker login -p x ".repeat(n / 18),
            |n| "password x ".repeat(n / 11),
            |n| format!("machine h\n{}", "  password x\n".repeat(n / 13)),
            |n| "- name: DB_PASSWORD\n  value: x\n".repeat(n / 31),
            |n| "a:1:b:c:d\n".repeat(n / 10),
            |n| format!("password: {}", "word ".repeat(n / 5)),
        ];
        let time = |text: &str| {
            let t = std::time::Instant::now();
            let r = redact(text);
            let secs = t.elapsed().as_secs_f64();
            assert_eq!(redact(&r.text).text, r.text, "not idempotent");
            secs
        };
        let big = 1 << 20;
        for (i, make) in cases.iter().enumerate() {
            let small = time(&make(big / 4));
            let large = time(&make(big));
            // Below ~20 ms the timer noise dominates; nothing to measure.
            if large < 0.02 {
                continue;
            }
            let ratio = large / small.max(1e-6);
            assert!(
                ratio < 8.0,
                "case {i}: 4x input took {ratio:.1}x longer ({small:.3}s -> {large:.3}s)"
            );
        }
    }

    #[test]
    fn marker_is_never_rematched() {
        let r = redact("token=[REDACTED:keyword-assignment] and [REDACTED:high-entropy]");
        assert!(r.is_clean(), "{:?}", r.findings);
    }
}
