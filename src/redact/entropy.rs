//! Layer 3: a Shannon-entropy catch-all for token formats no rule knows.
//!
//! A candidate is a run of 24+ chars from `[A-Za-z0-9+/_-]` (plus trailing
//! `=` padding). It is redacted when its entropy is at least
//! [`min_entropy`] bits/char (4.0 from 32 chars up) and it mixes at least [`MIN_CLASSES`] of
//! {lower, upper, digit, symbol}, or when it is a long single-case
//! alphanumeric token (see [`single_case_secret`]). Shapes that are high-entropy but not
//! secret are excluded so provenance and code survive: hex of any length
//! (git SHAs, our uids, hashes), UUIDs, single-case identifiers, camelCase
//! identifiers, file paths, base64 of plain prose, and agent message ids.

use std::sync::LazyLock;

use regex::Regex;

use super::rules::key_words;
use super::{Candidate, re};

/// Minimum token length (bytes; tokens are ASCII).
const MIN_LEN: usize = 24;
/// Minimum Shannon entropy, bits per char, for tokens of 32+ chars.
const MIN_ENTROPY: f64 = 4.0;
/// Short tokens cannot reach high entropy (24 distinct chars is only
/// log2(24) = 4.58 bits), so below 32 chars the bar is `log2(len) - 0.85`.
/// Tuned on random base62: a flat 4.0 misses ~5% of 24-char tokens, this
/// misses ~0.2% (3.73 bits at 24, 3.96 at 28).
const SHORT_SLACK: f64 = 0.85;
/// Minimum number of char classes out of lower, upper, digit, symbol.
const MIN_CLASSES: usize = 3;
/// Single-case alphanumeric tokens (Azure `DevOps` PATs: 52 lowercase +
/// digits, base32) count from this length. Pure hex is still excluded.
const SINGLE_CASE_LEN: usize = 40;
/// Their entropy bar is `log2(len) - SINGLE_CASE_SLACK`, capped at
/// [`SINGLE_CASE_ENTROPY`]. A flat 4.5 misses ~25% of random 52-char and
/// most 40-char lowercase+digit tokens; this misses ~1% of random base36
/// and base32 tokens from 40 chars up.
const SINGLE_CASE_ENTROPY: f64 = 4.5;
const SINGLE_CASE_SLACK: f64 = 1.6;

/// Internal `=` splits a run, so `KEY=value` yields `value` alone and the
/// key name is never redacted; only trailing `=` (base64 padding) stays.
/// A JSON-escaped slash (`\/`) counts as `/`, so still-escaped base64 is
/// not split into short pieces.
static TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"(?:[A-Za-z0-9+/_-]|\\/){24,}=*"));

/// A `-`/`_`-separated part that is a plain word, number or hex: when every
/// part is one, the token is an identifier, UUID or hash, not a secret.
static PLAIN_PART: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:[a-z]+[0-9]*|[A-Z]+[0-9]*|[0-9]+|(?i:0x)?[0-9a-fA-F]+)$"));

/// Ids that agent transcripts are full of; random but not credentials.
/// Anchored to the exact shape, so a secret that merely starts with `msg_`
/// (extra parts, symbols, another length) is judged like any token.
static AGENT_ID: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(?:(?:toolu|srvtoolu|msg|req)_(?:01)?[0-9A-Za-z]{20,40}",
        r"|call_[0-9A-Za-z]{20,40}|chatcmpl-[0-9A-Za-z]{20,40})$",
    ))
});

/// Layer 3: high-entropy tokens.
pub(super) fn scan(text: &str, out: &mut Vec<Candidate>) {
    for m in TOKEN.find_iter(text) {
        // Judged on the unescaped form; the span still covers the original.
        let raw = m.as_str();
        let unescaped;
        let token = if raw.contains('\\') {
            unescaped = raw.replace("\\/", "/");
            unescaped.as_str()
        } else {
            raw
        };
        if !token.contains('/') {
            if is_secret(token) {
                push(out, m.start(), m.end());
            }
            continue;
        }
        // Base64 uses `/`, so a `/` never makes a token safe by itself. Only a
        // clear path is skipped: every segment is a word, identifier or hash,
        // and at least two are 3+ chars. Anything else is judged whole (`/`
        // included), so a key split into short segments is still caught.
        // When in doubt we redact: a lost path beats a leaked secret.
        if is_clear_path(token) {
            continue;
        }
        if is_secret(token) {
            push(out, m.start(), m.end());
        }
    }
}

/// Every `/` segment is empty or wordy, and 2+ are wordy with 3+ chars.
fn is_clear_path(token: &str) -> bool {
    let segments: Vec<&str> = token.split('/').collect();
    segments.iter().all(|s| s.is_empty() || wordy(s))
        && segments.iter().filter(|s| s.len() >= 3).count() >= 2
}

fn push(out: &mut Vec<Candidate>, start: usize, end: usize) {
    out.push(Candidate {
        rule: "high-entropy",
        start,
        end,
        layer: 3,
    });
}

/// True when `token` (no path handling) looks like a random secret.
fn is_secret(token: &str) -> bool {
    // The camelCase exclusion needs mixed case, so it never applies to a
    // single-case token (base32 digits split it into vowel "words").
    let mixed = classes(token) >= MIN_CLASSES
        && shannon(token) >= min_entropy(token.len())
        && !is_camel_identifier(token);
    token.len() >= MIN_LEN
        && (mixed || single_case_secret(token))
        && !AGENT_ID.is_match(token)
        && !is_plain(token)
        && !is_base64_prose(token)
}

/// 40+ chars of lowercase+digits or uppercase+digits, high entropy, not hex.
fn single_case_secret(token: &str) -> bool {
    let b = token.as_bytes();
    let lower = b
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let upper = b
        .iter()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    token.len() >= SINGLE_CASE_LEN
        && (lower || upper)
        && classes(token) == 2
        && !b.iter().all(u8::is_ascii_hexdigit)
        && shannon(token)
            >= (len_f64(token.len()).log2() - SINGLE_CASE_SLACK).min(SINGLE_CASE_ENTROPY)
}

fn len_f64(len: usize) -> f64 {
    f64::from(u32::try_from(len).unwrap_or(u32::MAX))
}

/// Entropy bar for a token of `len` chars.
fn min_entropy(len: usize) -> f64 {
    (len_f64(len).log2() - SHORT_SLACK).min(MIN_ENTROPY)
}

/// Shannon entropy over chars, in bits per char.
fn shannon(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[usize::from(b)] += 1;
    }
    let n = f64::from(u32::try_from(s.len()).unwrap_or(u32::MAX));
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / n;
            -p * p.log2()
        })
        .sum()
}

/// Number of char classes present: lower, upper, digit, symbol (`+/=_-`).
fn classes(s: &str) -> usize {
    let b = s.as_bytes();
    [
        b.iter().any(u8::is_ascii_lowercase),
        b.iter().any(u8::is_ascii_uppercase),
        b.iter().any(u8::is_ascii_digit),
        b.iter().any(|c| b"+/=_-".contains(c)),
    ]
    .iter()
    .filter(|&&x| x)
    .count()
}

/// Hex, UUIDs, `SCREAMING_SNAKE`, `kebab-case`, `snake_case`: every
/// `-`/`_` part is a single-case word, a number, or hex.
fn is_plain(token: &str) -> bool {
    token
        .trim_end_matches('=')
        .split(['-', '_'])
        .all(|p| p.is_empty() || PLAIN_PART.is_match(p))
}

/// Common consonant-only abbreviations in identifiers.
const ABBREVIATIONS: &[&str] = &[
    "db", "js", "ts", "sql", "xml", "html", "http", "https", "dns", "tcp", "ssl", "tls", "sdk",
    "cli", "ctx", "msg", "src", "pkg", "std", "str", "fn", "cmd", "url", "rpc", "grpc", "ssh",
    "css", "svg", "csv", "pdf", "png", "jpg", "gpu", "cpu", "vm", "ws", "rx", "tx", "mb", "kb",
    "gb", "ms", "ns", "dm", "pr", "mr", "md", "nfc", "nfd", "lru", "ttl", "jwt",
];

/// `getUserById2FromCacheOrDb`: words averaging 3+ letters, each with a
/// vowel (or a common abbreviation), at most two single letters, and short
/// numbers. Random
/// base62 flips case every char or two, so its "words" are short
/// consonant clusters like `mlxmx` or `kt`.
fn is_camel_identifier(token: &str) -> bool {
    !token.contains(['+', '/', '=']) && identifier_words(token).is_some_and(|alpha| alpha >= 2)
}

/// When every word of `name` is identifier-like (see
/// [`is_camel_identifier`]) and the words average 3+ letters, the number
/// of multi-letter words; `None` for random-looking soup.
pub(super) fn identifier_words(name: &str) -> Option<usize> {
    let (mut letters, mut alpha, mut singles) = (0, 0, 0);
    for w in key_words(name) {
        if w.bytes().all(|c| c.is_ascii_digit()) {
            if w.len() > 2 {
                return None;
            }
            continue;
        }
        if w.len() == 1 {
            // `OAuth`, `V5`, `x86`: a few single letters are normal.
            singles += 1;
            if singles > 2 {
                return None;
            }
            continue;
        }
        let vowel = w.bytes().any(|c| b"aeiouy".contains(&c));
        if !(vowel || ABBREVIATIONS.contains(&w.as_str())) {
            return None;
        }
        letters += w.len();
        alpha += 1;
    }
    (letters >= 3 * alpha).then_some(alpha)
}

/// A plain word, identifier, hash or camelCase name.
fn wordy(seg: &str) -> bool {
    is_plain(seg) || is_camel_identifier(seg)
}

/// Base64 (standard or URL-safe) whose decoding is plain prose: printable
/// sentence characters with regular spaces. Encoded credentials and JSON
/// key files contain quotes, colons or no spaces, so they are not excluded.
fn is_base64_prose(token: &str) -> bool {
    let Some(bytes) = decode_base64(token.trim_end_matches('=')) else {
        return false;
    };
    let spaces = bytes.split(|&c| c == b' ').count() - 1;
    bytes.len() >= 12
        && spaces * 10 >= bytes.len()
        && bytes
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || b" .,'!?;-\n".contains(&c))
}

/// Decodes unpadded base64; `None` on a char outside one alphabet.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let url = s.contains(['-', '_']);
    if url && s.contains(['+', '/']) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::super::{ALNUM, test_token};
    use super::*;

    fn flagged(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        scan(text, &mut out);
        out.iter()
            .map(|c| text[c.start..c.end].to_owned())
            .collect()
    }

    /// Standard base64 encoder, to build prose fixtures.
    fn b64(input: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for chunk in input.chunks(3) {
            let n =
                chunk.iter().fold(0u32, |a, &b| (a << 8) | u32::from(b)) << (8 * (3 - chunk.len()));
            for i in 0..=chunk.len() {
                s.push(char::from(
                    A[usize::try_from((n >> (18 - 6 * i)) & 63).unwrap()],
                ));
            }
        }
        while s.len() % 4 != 0 {
            s.push('=');
        }
        s
    }

    #[test]
    fn random_tokens_are_flagged() {
        for (seed, len) in [(1, 24), (2, 32), (3, 40), (4, 64)] {
            let t = test_token(seed, len, ALNUM);
            assert_eq!(flagged(&format!("x {t} y")), vec![t.clone()], "{t}");
        }
        let b64url = test_token(
            5,
            43,
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_",
        );
        assert_eq!(flagged(&b64url), vec![b64url.clone()]);
    }

    /// Recall regression guard on random base62 with all three classes.
    #[test]
    fn recall_on_random_short_tokens() {
        for len in [24, 28, 32] {
            let tokens: Vec<String> = (0..2000)
                .map(|seed| test_token(seed * 7 + 3, len, ALNUM))
                .filter(|t| classes(t) >= MIN_CLASSES)
                .collect();
            let hits = tokens.iter().filter(|t| is_secret(t)).count();
            assert!(
                hits * 100 >= tokens.len() * 99,
                "len {len}: {hits}/{}",
                tokens.len()
            );
        }
    }

    /// H4: Azure-style lowercase+digit and base32 tokens, 40+ chars.
    #[test]
    fn single_case_long_tokens_are_flagged() {
        let low = b"abcdefghijklmnopqrstuvwxyz0123456789";
        for (len, alphabet) in [
            (52, &low[..]),
            (40, &low[..]),
            (56, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"),
        ] {
            let tokens: Vec<String> = (0..1000)
                .map(|seed| test_token(seed * 13 + 5, len, alphabet))
                .filter(|t| classes(t) == 2)
                .collect();
            let hits = tokens.iter().filter(|t| is_secret(t)).count();
            assert!(
                hits * 100 >= tokens.len() * 97,
                "len {len}: {hits}/{}",
                tokens.len()
            );
        }
        // Still excluded: hex, short single-case tokens, plain words.
        for t in [
            test_token(1, 64, b"0123456789abcdef"),
            test_token(2, 36, low),
            "release2024build2024release2024build2024".to_owned(),
        ] {
            assert!(!is_secret(&t), "{t}");
        }
    }

    #[test]
    fn key_name_is_not_part_of_the_token() {
        let t = test_token(6, 32, ALNUM);
        assert_eq!(flagged(&format!("SOME_KEY={t}")), vec![t]);
    }

    #[test]
    fn plain_shapes_are_not_flagged() {
        for text in [
            "0123456789abcdef0123456789abcdef01234567",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "123e4567-e89b-12d3-a456-426614174000",
            "SOME_VERY_LONG_CONSTANT_NAME_V2",
            "some-very-long-kebab-case-identifier",
            "parseHttp2ResponseHeadersAsyncWithRetry",
            "XMLHttpRequestUploadEventTarget",
            "getUserById2FromCacheOrDb",
            "handleOAuth2CallbackForGoogleSSO",
            "useQueryClientV5WithSuspenseMode",
            "aarch64-apple-darwin-v1_2_3-final",
            "toolu_01A09q90qw90lq917835lq9x",
            "/Users/someone/projects/recalla/src/store/mod.rs",
            "node_modules/@scope/some-package/dist/index.js",
        ] {
            assert_eq!(flagged(text), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn base64_prose_is_not_flagged_but_base64_json_is() {
        let prose = b64(b"The quick brown fox jumps over the lazy dog, twice.");
        assert_eq!(flagged(&prose), Vec::<String>::new());
        let json = b64(br#"{"type": "service_account", "private_key_id": "a1b2c3"}"#);
        assert_eq!(flagged(&json), vec![json.clone()]);
    }

    /// Base64 uses `/`; a key split into short segments must still go.
    #[test]
    fn base64_with_slashes_is_flagged_whole() {
        let mut checked = 0;
        for seed in 0..200u64 {
            let bytes: Vec<u8> = test_token(seed, 32, ALNUM)
                .bytes()
                .map(|b| b.wrapping_mul(37))
                .collect();
            let key = b64(&bytes);
            if !key.contains('/') {
                continue;
            }
            checked += 1;
            assert_eq!(flagged(&format!("k {key} k")), vec![key.clone()], "{key}");
        }
        assert!(checked >= 20, "{checked}");
        // Three slashes, every segment shorter than the minimum length.
        let parts: Vec<String> = (0..4).map(|i| test_token(40 + i, 9, ALNUM)).collect();
        let split = parts.join("/");
        assert_eq!(flagged(&split), vec![split.clone()]);
        let lead = format!("/{split}");
        assert_eq!(flagged(&lead), vec![lead.clone()]);
    }

    #[test]
    fn real_paths_stay_clean() {
        for text in [
            "/Users/x/projects/recalla/src/store/mod.rs",
            "src/redact/entropy.rs and crate/store/search",
            "src/redact/some_module_name/entropy_tests/fixtures",
            "~/projects/recalla/target/debug/build/recalla-0c67b109f093d3bd/out",
            "/home/dev/.cargo/registry/src/index.crates.io-6f17d22bba15001f/regex-1.13.2",
            "web/src/components/UserProfileCard/UserProfileCardHeader.tsx",
        ] {
            assert_eq!(flagged(text), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn secret_inside_url_path_is_caught() {
        // Judged whole: the path around the secret may be redacted with it.
        let t = test_token(7, 32, ALNUM);
        let text = format!("https://hooks.example.com/services/webhooks/{t}");
        let hits = flagged(&text);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains(&t), "{hits:?}");
    }

    #[test]
    fn short_random_segments_with_path_words_are_still_caught() {
        // A key split into short segments, padded with path-like words.
        let t = test_token(11, 30, ALNUM);
        let text = format!("src/store/{}/{}/{}", &t[..10], &t[10..20], &t[20..]);
        assert_eq!(flagged(&text).len(), 1, "{text}");
    }

    #[test]
    fn exact_agent_ids_are_not_flagged() {
        for id in [
            format!("msg_01{}", test_token(70, 22, ALNUM)),
            format!("req_01{}", test_token(71, 22, ALNUM)),
            format!("srvtoolu_01{}", test_token(72, 22, ALNUM)),
            format!("call_{}", test_token(73, 24, ALNUM)),
            format!("chatcmpl-{}", test_token(74, 29, ALNUM)),
        ] {
            assert_eq!(flagged(&id), Vec::<String>::new(), "{id}");
        }
    }

    /// Only the exact agent-id shapes are exempt; anything else that starts
    /// with an id prefix is judged as a normal token.
    #[test]
    fn id_prefix_lookalikes_are_judged_normally() {
        let sym = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/-_";
        for token in [
            format!("msg_{}", test_token(80, 30, sym)),
            format!(
                "msg_{}_{}",
                test_token(81, 12, ALNUM),
                test_token(82, 16, ALNUM)
            ),
            format!("call_{}", test_token(83, 48, ALNUM)),
            format!(
                "toolu_01{}-{}",
                test_token(84, 12, ALNUM),
                test_token(85, 12, ALNUM)
            ),
            format!("chatcmpl-{}", test_token(86, 60, ALNUM)),
        ] {
            assert_eq!(
                flagged(&format!("x {token} y")),
                vec![token.clone()],
                "{token}"
            );
        }
    }

    #[test]
    fn entropy_and_classes() {
        assert!(shannon("aaaaaaaa") < 0.01);
        assert!((shannon("abcdefgh") - 3.0).abs() < 1e-9);
        assert_eq!(classes("abcDEF123-"), 4);
        assert_eq!(classes("abcdef"), 1);
    }
}
