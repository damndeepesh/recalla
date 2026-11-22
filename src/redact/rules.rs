//! Layer 1 (known credential formats) and layer 2 (keyword assignments).
//!
//! All patterns are original. Prefixes follow the vendors' published token
//! formats. Secret-shaped prefixes are written with a character class
//! (e.g. `[-]`) so this file itself never looks like a leaked credential.
//!
//! When in doubt we redact: a lost word beats a leaked secret.

use std::sync::LazyLock;

use regex::Regex;

use super::entropy::identifier_words;
use super::{Candidate, Finding, MARKER, Recognised, re};

/// A layer-1 rule. The secret is capture group 1 when present, else the
/// whole match.
struct Rule {
    name: &'static str,
    re: Regex,
    /// The match must not start inside a word (checked in code, because
    /// `\b` is Unicode-aware and fails after CJK text).
    boundary: bool,
    /// Extra validation of the secret, to cut false positives.
    check: Option<fn(&str) -> bool>,
}

fn rule(
    name: &'static str,
    pattern: &str,
    boundary: bool,
    check: Option<fn(&str) -> bool>,
) -> Rule {
    Rule {
        name,
        re: re(pattern),
        boundary,
        check,
    }
}

fn has_digit(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_digit())
}

/// Upper, lower and digit all present: real random tokens, not kebab words.
fn mixed(s: &str) -> bool {
    has_digit(s)
        && s.bytes().any(|b| b.is_ascii_uppercase())
        && s.bytes().any(|b| b.is_ascii_lowercase())
}

fn bearer_value(s: &str) -> bool {
    has_digit(s) && s.bytes().any(|b| b.is_ascii_alphabetic()) && !is_placeholder(s)
}

/// A URL password running to the last `@`: not a placeholder, and not a
/// port followed by a path (`https://host:443/a/@scope/pkg`).
fn url_password(s: &str) -> bool {
    !port_path(s) && !is_placeholder(s)
}

/// A URL password running to the first `@`. Only a bare port with an
/// empty path (`host:443/@scope`) is rejected, so when the last-`@` span
/// is rejected as a port, this one still redacts `123/abc` in
/// `user:123/abc@host`.
fn url_password_first(s: &str) -> bool {
    let bare_port = s
        .strip_suffix('/')
        .is_some_and(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()));
    !bare_port && !is_placeholder(s)
}

/// `<digits>/...`: a port and a path, not a password.
fn port_path(s: &str) -> bool {
    s.split_once('/')
        .is_some_and(|(p, _)| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
}

/// Order matters only for ties: a more specific rule must precede a more
/// general one that matches the same span (the Anthropic rule before the `OpenAI` one).
static KNOWN: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        // No boundary: key ids are often glued into names (`AWS_KEY_AKIA...`).
        rule("aws-access-key", r"(?:AKIA|ASIA)[0-9A-Z]{16,}", false, None),
        rule("github-token", r"gh[pousr]_[A-Za-z0-9]{36,}", true, None),
        rule("github-token", r"github_pat_[A-Za-z0-9_]{22,}", true, None),
        rule("gitlab-token", r"glpat[-][A-Za-z0-9_-]{20,}", true, None),
        rule("anthropic-key", r"sk-ant[-][A-Za-z0-9_-]{20,}", true, None),
        rule(
            "openai-key",
            r"sk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,}",
            true,
            Some(mixed),
        ),
        rule(
            "slack-token",
            r"xox[abposr]-[A-Za-z0-9-]{10,}",
            true,
            Some(has_digit),
        ),
        rule(
            "stripe-key",
            r"[rs]k_(?:live|te[s]t)_[A-Za-z0-9]{16,}",
            true,
            None,
        ),
        rule("google-api-key", r"AIza[0-9A-Za-z_-]{35,}", true, None),
        rule("npm-token", r"npm_[A-Za-z0-9]{36,}", true, None),
        rule(
            "huggingface-token",
            r"hf_[A-Za-z0-9]{30,}",
            true,
            Some(mixed),
        ),
        rule("digitalocean-token", r"do[opr]_v1_[0-9a-f]{64}", true, None),
        rule("mailgun-key", r"key[-][0-9a-f]{32}", true, None),
        rule(
            "sendgrid-key",
            r"S[G]\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}",
            true,
            None,
        ),
        rule(
            "jwt",
            r"eyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*",
            true,
            None,
        ),
        // A complete PEM block, header to footer. Both PEM rules also take
        // any text glued to their end (up to whitespace or an escape, like
        // an unquoted value): the header's spaces would otherwise join it to
        // a value before the block on the next pass, breaking idempotence.
        rule(
            "private-key",
            concat!(
                r"-----BEGIN[A-Z0-9 ]*PRIVATE KEY[A-Z ]*-----(?s:.*?)-----END[A-Z0-9 ]*PRIVATE KEY[A-Z ]*-----",
                r#"(?:[^\s\\]|\\[^"'nrt\\\s])*"#,
            ),
            false,
            None,
        ),
        // A truncated block (no footer): header plus the base64 body that
        // follows, so a cut-off transcript still leaks nothing.
        rule(
            "private-key",
            concat!(
                r"-----BEGIN[A-Z0-9 ]*PRIVATE KEY[A-Z ]*-----(?:[A-Za-z0-9+/= \t\r\n\\]*[A-Za-z0-9+/=\\])?",
                r#"(?:[^\s\\]|\\[^"'nrt\\\s])*"#,
            ),
            false,
            None,
        ),
        // Only the password part of `scheme://user:pass@host`. Passwords
        // hold raw `@`, `/`, quotes and escapes, so it runs to the LAST `@`
        // of the whitespace-delimited run; over-redacting a path beats a
        // leak. The first-`@` span always fires too (merge unions them), so
        // a port-shaped rejection never leaves the password unredacted.
        rule(
            "url-credentials",
            r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s/:@"'\\]*:((?:[^\s\\]|\\\S)+)@"#,
            false,
            Some(url_password),
        ),
        rule(
            "url-credentials",
            r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s/:@"'\\]*:((?:[^\s@\\]|\\[^@\s])+)@"#,
            false,
            Some(url_password_first),
        ),
        rule(
            "bearer",
            r"(?i)bearer[ \t]+([A-Za-z0-9._~+/-]{20,}=*)",
            true,
            Some(bearer_value),
        ),
        // A bare token as URL userinfo (`https://<token>@host`): no colon,
        // so it is not `user:pass`, and 20+ chars, so not a user name.
        rule(
            "url-credentials",
            r#"[A-Za-z][A-Za-z0-9+.-]*://([^\s/:@"'\\]{20,})@"#,
            false,
            Some(not_placeholder),
        ),
        // Passwords in CLI flags, anchored to the command on the same line
        // (up to `;`, `&`, `|`), so `ls -p` and `grep -a` stay clean.
        rule(
            "cli-password",
            r"\b(?:mysql|mysqldump|mysqladmin|mariadb|mariadb-dump)\b[^\n;&|]*?[ \t]-p(\S+)",
            false,
            Some(not_placeholder),
        ),
        rule(
            "cli-password",
            concat!(
                r"\b(?:(?:docker|podman)\b[^\n;&|]*?\blogin|helm|sshpass|psql|mongo|mongosh)\b",
                r"[^\n;&|]*?[ \t](?:-p|--password)[ \t]+(\S+)",
            ),
            false,
            Some(cli_value),
        ),
        rule(
            "cli-password",
            r"\bredis-cli\b[^\n;&|]*?[ \t](?:-a|--pass)[ \t]+(\S+)",
            false,
            Some(cli_value),
        ),
        // `curl -u user:pass` / `--user=user:pass`: only the password.
        rule(
            "cli-password",
            r#"\b(?:curl|wget)\b[^\n;&|]*?[ \t](?:-u|--user)(?:[ \t]+|=)?['"]?[^\s:'"]*:([^\s'"]+)"#,
            false,
            Some(not_placeholder),
        ),
        // `.pgpass`: `host:port:database:user:password` on its own line.
        rule(
            "pgpass",
            r"(?m)^[ \t]*[A-Za-z0-9.*_-]+:(?:[0-9]+|\*):[^:\s]+:[^:\s]+:(\S+)",
            false,
            Some(not_placeholder),
        ),
    ]
});

fn not_placeholder(s: &str) -> bool {
    !is_placeholder(s)
}

/// A space-separated flag value: not a port (`psql -p 5432`), another flag,
/// or a placeholder.
fn cli_value(s: &str) -> bool {
    !s.bytes().all(|c| c.is_ascii_digit()) && !is_flag(s) && !is_placeholder(s)
}

/// True when the match at `start` begins a word. A preceding `\n`/`\t`/`\r`
/// escape counts as a boundary, for still-escaped JSON text.
fn at_boundary(text: &str, start: usize) -> bool {
    let b = text.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    match start {
        0 => true,
        1 => !word(b[0]),
        _ => !word(b[start - 1]) || (b[start - 2] == b'\\' && b"nrt".contains(&b[start - 1])),
    }
}

/// Layer 1: known credential formats and credential-bearing headers.
pub(super) fn known(text: &str, out: &mut Vec<Candidate>) {
    for r in KNOWN.iter() {
        for caps in r.re.captures_iter(text) {
            let whole = caps.get(0).map_or(0, |m| m.start());
            let Some(m) = caps.get(1).or_else(|| caps.get(0)) else {
                continue;
            };
            if r.boundary && !at_boundary(text, whole) {
                continue;
            }
            if r.check.is_some_and(|check| !check(m.as_str())) || already_redacted(m.as_str()) {
                continue;
            }
            out.push(Candidate {
                rule: r.name,
                start: m.start(),
                end: m.end(),
                layer: 1,
            });
        }
    }
    headers(text, &AUTH, true, out);
    headers(text, &COOKIE, false, out);
    netrc(text, out);
}

/// `password <pw>` / `passwd <pw>` with whitespace syntax (`.netrc`).
static NETRC: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:^|[ \t\n])(?:password|passwd)[ \t]+([^\s]+)"));

/// Bytes before a netrc line searched for its `machine`/`default` header.
const NETRC_SCAN: usize = 256;

/// One line's netrc facts, computed once per line so many matches on a
/// long line stay linear.
struct NetrcLine {
    end: usize,
    /// Byte offset of the first non-blank char.
    first: usize,
    /// Holds `machine ` or starts with `default`.
    entry: bool,
    /// An entry starts on an earlier line within [`NETRC_SCAN`] bytes, or
    /// the line is exactly `password <value>`.
    block: bool,
}

fn netrc_entry(l: &str) -> bool {
    let l = l.trim_start();
    l.starts_with("machine ") || l == "default" || l.starts_with("default ")
}

/// Netrc passwords, only in netrc shape: the line holds `machine ` or
/// starts with `default`; or `password` starts the line and either an
/// earlier line (within [`NETRC_SCAN`] bytes) starts a netrc entry or the
/// line holds exactly that one value.
fn netrc(text: &str, out: &mut Vec<Candidate>) {
    let mut line: Option<NetrcLine> = None;
    let mut from = 0;
    for caps in NETRC.captures_iter(text) {
        let (Some(all), Some(m)) = (caps.get(0), caps.get(1)) else {
            continue;
        };
        let cur = match line.take() {
            Some(l) if m.start() < l.end => l,
            _ => {
                // `from` only moves forward, so each byte is searched once.
                let start = text[from..m.start()]
                    .rfind(['\n', '\r'])
                    .map_or(from, |i| from + i + 1);
                let end = text[m.end()..]
                    .find(['\n', '\r'])
                    .map_or(text.len(), |i| m.end() + i);
                let s = &text[start..end];
                let lo = char_limit(text, start.saturating_sub(NETRC_SCAN));
                from = end;
                NetrcLine {
                    end,
                    first: start + (s.len() - s.trim_start().len()),
                    entry: s.contains("machine ") || netrc_entry(s),
                    block: text[lo..start].lines().any(netrc_entry)
                        || s.split_whitespace().count() == 2,
                }
            }
        };
        let kw = all.start() + all.as_str().len() - all.as_str().trim_start().len();
        let shaped = cur.entry || (kw == cur.first && cur.block);
        if shaped && !is_placeholder(m.as_str()) && !already_redacted(m.as_str()) {
            out.push(Candidate {
                rule: "netrc",
                start: m.start(),
                end: m.end(),
                layer: 1,
            });
        }
        line = Some(cur);
    }
}

/// `Authorization:` / `Proxy-Authorization=` header name, any case,
/// optionally inside (escaped) JSON quotes. `q` is an opening quote on the
/// value.
static AUTH: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?i)authorization\\?["']?[ \t]*[:=][ \t]*(?P<q>\\?["'])?"#));

/// `Cookie:` / `Cookies:` / `Set-Cookie:` header name, like [`AUTH`].
static COOKIE: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?i)(?:set-)?cookies?\\?["']?[ \t]*[:=][ \t]*(?P<q>\\?["'])?"#));

/// `Set-Cookie` attributes: kept with their values, not cookie values.
const COOKIE_ATTRS: &[&str] = &[
    "path",
    "domain",
    "expires",
    "max-age",
    "samesite",
    "secure",
    "httponly",
    "priority",
    "partitioned",
    "version",
    "comment",
];

/// End of a header value that starts at `start`: the end of the line, an
/// escape (`\n`, `\"`), or (when the header sits inside a string) the
/// closing quote. Not capped: like layer-2 values, a long value fails
/// closed and the scan resumes after it, so it stays linear.
fn header_end(text: &str, start: usize, in_string: bool) -> usize {
    let b = text.as_bytes();
    let escape = |i: usize| b[i] == b'\\' && b.get(i + 1).is_some_and(|n| b"nr\"'".contains(n));
    let mut i = start;
    while i < b.len()
        && !(b"\r\n".contains(&b[i]) || escape(i) || (in_string && b"\"'`".contains(&b[i])))
    {
        i += 1;
    }
    i
}

/// `end` clamped to the text and moved back to a char boundary.
fn char_limit(text: &str, end: usize) -> usize {
    let mut limit = end.min(text.len());
    while !text.is_char_boundary(limit) {
        limit -= 1;
    }
    limit
}

/// True when the header name at `m_start` or its value is inside quotes.
fn header_in_string(text: &str, m_start: usize, value_quote: bool) -> bool {
    value_quote || (m_start > 0 && b"\"'`".contains(&text.as_bytes()[m_start - 1]))
}

/// Every header matched by `pattern` (layer 1): the whole `Authorization`
/// value (`auth`), else each cookie value.
fn headers(text: &str, pattern: &Regex, auth: bool, out: &mut Vec<Candidate>) {
    let mut pos = 0;
    while let Some(caps) = pattern.captures_at(text, pos) {
        let Some(m) = caps.get(0) else { break };
        pos = m.end();
        if !auth && !at_boundary(text, m.start()) {
            continue;
        }
        let in_string = header_in_string(text, m.start(), caps.name("q").is_some());
        let end = header_end(text, m.end(), in_string);
        pos = end.max(pos);
        if auth {
            whole_value(text, m.end(), end, "bearer", 1, out);
        } else {
            cookie_values(text, m.end(), end, 1, out);
        }
    }
}

/// Redacts `text[start..end]` without trailing whitespace, whatever its
/// length or shape, unless it is empty or a whole-value placeholder. Used
/// for `Authorization` values: the scheme word goes with the credential.
fn whole_value(
    text: &str,
    start: usize,
    end: usize,
    rule: &'static str,
    layer: u8,
    out: &mut Vec<Candidate>,
) {
    let v = text[start..end].trim_end();
    if !v.is_empty() && !is_placeholder(v) && !already_redacted(v) {
        out.push(Candidate {
            rule,
            start,
            end: start + v.len(),
            layer,
        });
    }
}

/// Each `;`-separated part of the cookie header value `start..end`,
/// whole (`name=value` or a bare token), except `Set-Cookie` attributes.
/// Shared by layers 1 and 2.
fn cookie_values(text: &str, start: usize, end: usize, layer: u8, out: &mut Vec<Candidate>) {
    let mut off = start;
    for part in text[start..end].split(';') {
        let s = off + part.len() - part.trim_start().len();
        off += part.len() + 1;
        let name = part.split('=').next().unwrap_or(part).trim();
        if !COOKIE_ATTRS.contains(&name.to_ascii_lowercase().as_str()) {
            whole_value(text, s, off - 1, "cookie", layer, out);
        }
    }
}

/// `key<sep>` where sep is `=`, `:`, `=>` or `:=`, with an optional closing
/// quote on the key (`q`: JSON, possibly still escaped). The value is parsed
/// in code from the match end, so a long value is never rescanned.
static ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"(?P<key>[A-Za-z_][A-Za-z0-9_.-]*)(?P<q>\\?["'])?[ \t]*(?P<sep>=>|:=|=|:)[ \t]*"#)
});

/// `--flag value` (space-separated CLI flag).
static FLAG: LazyLock<Regex> = LazyLock::new(|| re(r"--(?P<key>[A-Za-z][A-Za-z0-9_-]*)[ \t]+"));

/// A key word ENDING in one of these marks the value as a secret, so glued
/// names count too (`PGPASSWORD`, `SSHPASS`, `apitoken`, `MYSQL_PWD`).
const SECRET_SUFFIXES: &[&str] = &[
    "secret",
    "token",
    "password",
    "passwd",
    "pwd",
    "pass",
    "passphrase",
    "auth",
    "apikey",
    "secretkey",
    "accesskey",
    "privatekey",
    "authorization",
    "credential",
    "credentials",
    "cookie",
    "cookies",
];
/// Two-word keys anywhere in the name (`aws_access_key_id`).
const SECRET_PAIRS: &[(&str, &str)] = &[("api", "key"), ("access", "key"), ("private", "key")];
/// Whole key words that mark a secret (`HMAC_SECRET`, `JWT_SIGNING_KEY`,
/// `ENCRYPTION`, `STRIPE_WEBHOOK_SALT`), matched exactly to avoid
/// `webmaster`-style false positives.
const SECRET_WORDS: &[&str] = &[
    "salt",
    "hmac",
    "signing",
    "encryption",
    "master",
    "fernet",
    "seed",
];
/// A final `key`/`keys` word is secret unless the word before it is one of
/// these (`primary_key`, `ssh_public_key`, `idempotency_key`).
const PUBLIC_KEYS: &[&str] = &[
    "public",
    "primary",
    "foreign",
    "sort",
    "partition",
    "cache",
    "idempotency",
    "object",
    "s3",
    "hot",
    "map",
    "unique",
    "composite",
    "lookup",
    "publishable",
];
/// A final key word that describes the secret instead of holding it
/// (`token_url`, `password_file`, `auth_type`, `max_token_count`).
const DESCRIPTORS: &[&str] = &[
    "type", "types", "count", "len", "length", "size", "limit", "limits", "budget", "usage", "url",
    "uri", "endpoint", "file", "path", "dir", "name", "method", "mode", "kind", "provider",
    "header", "env", "var", "field", "policy", "required", "enabled", "expiry", "expires", "ttl",
    "timeout", "stdin", "prefix", "suffix", "format", "version", "index", "offset",
];

/// Minimum value length (chars); shorter values are rarely real secrets.
const MIN_VALUE: usize = 8;
/// Minimum length for a `session` value: short ones are ids or flags.
const MIN_SESSION: usize = 16;

/// Splits an identifier into lowercase words on separators, camelCase and
/// letter/digit boundaries: `xApiKey2` -> `x`, `api`, `key`, `2`.
pub(super) fn key_words(key: &str) -> Vec<String> {
    let b = key.as_bytes();
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &c) in b.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some(&p) = i.checked_sub(1).and_then(|j| b.get(j)) {
            let next_lower = b.get(i + 1).is_some_and(u8::is_ascii_lowercase);
            let split = (c.is_ascii_uppercase()
                && (p.is_ascii_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_ascii_uppercase() && next_lower)))
                || (c.is_ascii_digit() && p.is_ascii_alphabetic())
                || (c.is_ascii_alphabetic() && p.is_ascii_digit());
            if split && !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.push(char::from(c.to_ascii_lowercase()));
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// When `key` names a secret (and not a descriptor of one), the minimum
/// value length to redact.
fn secret_key(key: &str) -> Option<usize> {
    let words = key_words(key);
    let last = words.last()?;
    if DESCRIPTORS.contains(&last.as_str()) {
        return None;
    }
    // `s3_key` splits as `s`, `3`, `key`, so the glued pair counts too.
    let final_key = match words.as_slice() {
        [.., a, prev, k] if matches!(k.as_str(), "key" | "keys") => {
            !PUBLIC_KEYS.contains(&prev.as_str())
                && !PUBLIC_KEYS.contains(&format!("{a}{prev}").as_str())
        }
        [.., prev, k] if matches!(k.as_str(), "key" | "keys") => {
            !PUBLIC_KEYS.contains(&prev.as_str())
        }
        [k] => matches!(k.as_str(), "key" | "keys"),
        _ => false,
    };
    // `masterkey`, `signingkey`: a glued secret word plus `key`.
    let glued = last
        .strip_suffix("keys")
        .or_else(|| last.strip_suffix("key"))
        .is_some_and(|w| SECRET_WORDS.contains(&w));
    let secret = final_key
        || glued
        || last == "pat"
        || words
            .windows(2)
            .any(|p| SECRET_PAIRS.contains(&(p[0].as_str(), p[1].as_str())))
        || words.iter().any(|w| {
            SECRET_WORDS.contains(&w.as_str()) || SECRET_SUFFIXES.iter().any(|s| w.ends_with(s))
        });
    if secret {
        Some(MIN_VALUE)
    } else {
        (last == "session").then_some(MIN_SESSION)
    }
}

/// Longest residual region inspected, in bytes. Layer-2 and header values
/// are not capped: they fail closed and are redacted to the end of their
/// run, and the scan resumes after the run, so it stays linear.
const MAX_VALUE: usize = 4096;

/// Longest prefix inspected to decide that a value is code or a type
/// annotation. Longer expressions are treated as data (redacted).
const CODE_SCAN: usize = 256;

/// A parsed value: byte span of its content (quotes excluded).
#[derive(Debug, Clone, Copy)]
struct Value {
    start: usize,
    end: usize,
    /// Where the raw run ended, before trailing punctuation was trimmed.
    run_end: usize,
    quoted: bool,
}

/// True when `next` (the byte after a `,` or `}`) continues JSON structure.
fn json_structure(next: Option<&u8>) -> bool {
    next.is_none_or(|c| c.is_ascii_whitespace() || b"\"}],\\".contains(c))
}

/// Length of `s` without trailing `;`, `,`, quotes, and closing brackets
/// that have no opener in `s` (`f(password=abc)`, `{token: abc}`).
fn trimmed_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut end = b.len();
    while let Some(&c) = b[..end].last() {
        let opener = match c {
            b')' => b'(',
            b']' => b'[',
            b'}' => b'{',
            b';' | b',' | b'"' | b'\'' | b'`' => 0,
            _ => break,
        };
        if opener != 0 && b[..end].contains(&opener) {
            break;
        }
        end -= 1;
    }
    end
}

/// Parses the value starting at `pos`, never past `limit` (a char
/// boundary). Quoted values end at the matching quote (or end of line if
/// unterminated). Unquoted values (dotenv, shell, YAML) run to whitespace or
/// an escape such as `\"`/`\n`, so `;`, `,` and quotes inside a password
/// never split it; only in JSON (`json`: the key was `"`-quoted) do `,` and
/// `}` end the value, and only when JSON structure follows.
fn parse_value(text: &str, pos: usize, json: bool, limit: usize) -> Value {
    let bytes = text.as_bytes();
    let rest = &bytes[pos..limit];
    let delim: &[u8] = match rest {
        [b'\\', q @ (b'"' | b'\''), ..] => &[b'\\', *q][..],
        [q @ (b'"' | b'\'' | b'`'), ..] => std::slice::from_ref(q),
        _ => &[],
    };
    if !delim.is_empty() {
        // Ends at the closing delimiter or the end of the line, whichever
        // comes first; one pass, never past `limit`.
        let start = pos + delim.len();
        let mut i = start;
        while i < limit {
            let c = bytes[i];
            if c == b'\n' || c == b'\r' || bytes[i..limit].starts_with(delim) {
                break;
            }
            i += if c == b'\\' && delim.len() == 1 { 2 } else { 1 };
        }
        let end = i.min(limit);
        return Value {
            start,
            end,
            run_end: end,
            quoted: true,
        };
    }
    let mut i = pos;
    while i < limit {
        let c = bytes[i];
        let escape = c == b'\\' && bytes.get(i + 1).is_some_and(|n| b"\"'nrt\\".contains(n));
        let json_end = json && (c == b',' || c == b'}') && json_structure(bytes.get(i + 1));
        if c.is_ascii_whitespace() || escape || json_end {
            break;
        }
        i += 1;
    }
    // Non-ASCII bytes never stop the scan, and `limit` is a char boundary,
    // so `i` always ends on a char boundary; trimming removes ASCII only.
    Value {
        start: pos,
        end: pos + trimmed_len(&text[pos..i]),
        run_end: i,
        quoted: false,
    }
}

/// True when the parsed value should be redacted.
fn secret_value(text: &str, v: Value, min: usize) -> bool {
    let s = &text[v.start..v.end];
    unmarked_len(s) >= min && !is_placeholder(s)
}

/// A match that holds one of our markers plus fewer than [`MIN_VALUE`]
/// other chars: a second pass over text we already redacted.
fn already_redacted(s: &str) -> bool {
    s.contains("[REDACTED:") && unmarked_len(s) < MIN_VALUE
}

/// Chars of `s` outside our markers: a marker plus a short tail
/// (`[REDACTED:x]@/`) is already redacted, so a second pass keeps it.
fn unmarked_len(s: &str) -> usize {
    if !s.contains("[REDACTED:") {
        return s.chars().count();
    }
    let marked: usize = MARKER
        .find_iter(s)
        .map(|m| m.as_str().chars().count())
        .sum();
    s.chars().count() - marked
}

/// Longest string literal allowed inside a code group: a longer one is
/// data (`Buffer.from('<key>', 'hex')`), so the value is not code.
const MAX_CODE_LITERAL: usize = 15;

/// Index just past the bracket group that opens at `open` (`(`, `[` or
/// `<`), or `None` if it does not close on this line before `limit` or
/// holds a string literal over [`MAX_CODE_LITERAL`] bytes. Quoted strings
/// and escapes inside `(`/`[` groups are skipped.
fn close_group(b: &[u8], open: usize, limit: usize) -> Option<usize> {
    let (open_b, close_b, quotes): (u8, u8, &[u8]) = match b[open] {
        b'(' => (b'(', b')', b"\"'`"),
        b'[' => (b'[', b']', b"\"'`"),
        _ => (b'<', b'>', b""),
    };
    let (mut depth, mut quote, mut i, mut lit) = (0usize, None, open, 0);
    while i < limit {
        let byte = b[i];
        match byte {
            b'\n' | b'\r' => return None,
            b'\\' => {
                i += 2;
                continue;
            }
            _ if quote == Some(byte) => {
                if i - lit > MAX_CODE_LITERAL {
                    return None;
                }
                quote = None;
            }
            _ if quote.is_some() => {}
            _ if quotes.contains(&byte) => (quote, lit) = (Some(byte), i + 1),
            _ if byte == open_b => depth += 1,
            _ if byte == close_b => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// When the unquoted value at `pos` is a source-code expression, its end.
/// Code is a call, index or generic type (`get_token(&cfg)?`,
/// `env::var("X").unwrap()`, `os.environ["X"]`, `Option<String>`) or a
/// member path (`self.config.password`, but not `Title.Case` words). All
/// names must be identifier words (lowercase/snake/camel words with vowels,
/// no digits), and the expression must end at `;`, `)`, `,` or the end of
/// the line. A bare name or reference (`&cfg`, `*cfg`) is data.
fn code_end(text: &str, pos: usize) -> Option<usize> {
    let b = text.as_bytes();
    let limit = (pos + CODE_SCAN).min(b.len());
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    let mut i = pos;
    if matches!(b.get(i), Some(b'&' | b'*')) {
        i += 1;
    }
    let (mut parts, mut group, mut title) = (0usize, false, false);
    let mut names = String::new();
    loop {
        let s = i;
        if !(i < limit && (b[i].is_ascii_alphabetic() || b[i] == b'_' || b[i] == b'$')) {
            return None;
        }
        while i < limit && ident(b[i]) {
            i += 1;
        }
        parts += 1;
        let name = &text[s..i];
        title |= name.starts_with(|c: char| c.is_ascii_uppercase())
            && name.bytes().any(|c| c.is_ascii_lowercase());
        names.push_str(name);
        names.push('.');
        if b[i..limit].starts_with(b"!(") {
            i += 1; // macro call
        }
        while i < limit && matches!(b[i], b'(' | b'[' | b'<') {
            i = close_group(b, i, limit)?;
            group = true;
            while i < limit && b[i] == b'?' {
                i += 1;
            }
        }
        let seps: [&[u8]; 4] = [b"?.", b"::", b"->", b"."];
        let Some(sep) = seps.into_iter().find(|s| b[i..limit].starts_with(s)) else {
            break;
        };
        i += sep.len();
    }
    let mut j = i;
    while j < limit && matches!(b[j], b' ' | b'\t') {
        j += 1;
    }
    if j == limit && limit < b.len() {
        return None;
    }
    let terminated = match b.get(j) {
        None => true,
        Some(b'\\') => b.get(j + 1).is_some_and(|n| b"nr\"'".contains(n)),
        Some(c) => b";,)\r\n".contains(c),
    };
    let shape = group || (parts >= 2 && !title);
    let words = !has_digit(&names) && identifier_words(&names).is_some_and(|n| n >= 1);
    (terminated && shape && words).then_some(i)
}

/// Bytes before a key inspected to classify its context.
const CONTEXT_SCAN: usize = 64;

/// Words that declare a source-code binding, field or function.
const DECLS: &[&str] = &[
    "let", "const", "var", "val", "mut", "final", "static", "pub", "private", "public", "def", "fn",
];

/// True when the assignment of `key` (at `key_start`) is in a real
/// source-code context, so its value may be exempted by [`code_end`]: a
/// type annotation between key and `=` (`annotated`: `token: str = ...`),
/// a member key (`self.x`, `obj.x`), a declaration keyword earlier on the
/// same line, or an argument (inside a `(` still open on the line).
/// Everything else, including a line-start `key = value`, is data.
fn code_context(text: &str, key_start: usize, key: &str, annotated: bool) -> bool {
    if annotated || key.contains('.') {
        return true;
    }
    let mut lo = key_start.saturating_sub(CONTEXT_SCAN);
    while !text.is_char_boundary(lo) {
        lo -= 1;
    }
    let window = &text[lo..key_start];
    let line = window
        .rfind(['\n', '\r'])
        .map_or(window, |i| &window[i + 1..]);
    let line = line.rfind("\\n").map_or(line, |i| &line[i + 2..]);
    let open = line.bytes().fold(0usize, |d, c| match c {
        b'(' => d + 1,
        b')' => d.saturating_sub(1),
        _ => d,
    });
    open > 0
        || line
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| DECLS.contains(&w))
}

/// For `key: Type = value` (`token: str = "..."`), the position of `value`.
fn annotation_value(text: &str, pos: usize) -> Option<usize> {
    let b = text.as_bytes();
    let limit = (pos + CODE_SCAN).min(b.len());
    let mut i = pos;
    if b.get(i) == Some(&b'&') {
        i += 1;
    }
    if !b
        .get(i)
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
    {
        return None;
    }
    while i < limit && (b[i].is_ascii_alphanumeric() || b"_.:[]|<>".contains(&b[i])) {
        i += 1;
    }
    while i < limit && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    if b.get(i) != Some(&b'=') || matches!(b.get(i + 1), Some(b'=' | b'>')) {
        return None;
    }
    i += 1;
    while i < limit && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    Some(i)
}

/// Layer 2: values of keyword assignments. Only the value is redacted; the
/// key and separator stay readable.
pub(super) fn assignments(text: &str, out: &mut Vec<Candidate>, seen: &mut Vec<Recognised>) {
    scan_assignments(text, &ASSIGN, out, seen);
    scan_assignments(text, &FLAG, out, seen);
    name_values(text, out, seen);
}

fn scan_assignments(
    text: &str,
    pattern: &Regex,
    out: &mut Vec<Candidate>,
    seen: &mut Vec<Recognised>,
) {
    let b = text.as_bytes();
    let mut pos = 0;
    while let Some(caps) = pattern.captures_at(text, pos) {
        let (Some(key), Some(all)) = (caps.name("key"), caps.get(0)) else {
            break;
        };
        // A rejected key resumes right after itself: the separator region
        // may hold the real assignment (`name: token=...`). Code resumes at
        // its own start, so call arguments (`f(password="...")`) are still
        // seen; code checks look at most CODE_SCAN bytes, so this stays
        // linear. Once a data value is parsed, resume after it whether
        // accepted or not, so no byte is value-parsed twice. Unquoted
        // values only stop at whitespace, so an assignment nested in a
        // rejected value (`token=dummy&secret=...`) is redacted with it.
        pos = key.end();
        // `scheme://user:pass@`: `oauth2:` is a URL user name, not a key.
        if text[..key.start()].ends_with("://") {
            continue;
        }
        let Some(min) = secret_key(key.as_str()) else {
            continue;
        };
        let sep = caps.name("sep").map_or("", |s| s.as_str());
        let next = caps.name("sep").and_then(|s| b.get(s.end()));
        // `==` and `=~` compare and `::` is a path: not assignments.
        if (sep == "=" && matches!(next, Some(b'=' | b'~'))) || (sep == ":" && next == Some(&b':'))
        {
            continue;
        }
        let mut start = all.end();
        let mut annotated = false;
        if sep == ":" {
            if let Some(p) = annotation_value(text, start) {
                (start, annotated) = (p, true);
            }
        }
        let quoted = matches!(b.get(start), Some(b'"' | b'\'' | b'`'))
            || (b.get(start) == Some(&b'\\') && matches!(b.get(start + 1), Some(b'"' | b'\'')));
        if !quoted
            && !sep.is_empty()
            && code_context(text, key.start(), key.as_str(), annotated)
            && code_end(text, start).is_some()
        {
            pos = pos.max(start);
            continue;
        }
        let json = caps.name("q").is_some_and(|q| q.as_str().ends_with('"'));
        let v = if sep == ":" && !json && !quoted && !annotated && line_start_key(b, key.start()) {
            line_value(text, start)
        } else {
            parse_value(text, start, json, text.len())
        };
        // `--password --verbose`: the next flag, not a value. Resume at the
        // flag so it is scanned as a key itself.
        if sep.is_empty()
            && !v.quoted
            && is_flag(&text[v.start..v.run_end])
            && b.get(v.run_end).is_none_or(u8::is_ascii_whitespace)
        {
            continue;
        }
        pos = pos.max(v.run_end);
        // Header-shaped keys get the same whole-value rules as the layer-1
        // header rules, independently of whether those fired.
        let header = key_words(key.as_str()).pop().unwrap_or_default();
        let auth = matches!(header.as_str(), "authorization" | "auth");
        let cookie = !v.quoted && matches!(header.as_str(), "cookie" | "cookies");
        if matches!(sep, ":" | "=") && (auth || cookie) {
            let end = if v.quoted {
                v.end
            } else {
                header_end(text, v.start, header_in_string(text, key.start(), false))
            };
            if auth {
                whole_value(text, v.start, end, "bearer", 2, out);
            } else {
                cookie_values(text, v.start, end, 2, out);
            }
            pos = pos.max(end);
            continue;
        }
        seen.push(Recognised {
            start: v.start,
            end: v.run_end,
            min,
        });
        if secret_value(text, v, min) {
            out.push(Candidate {
                rule: "keyword-assignment",
                start: v.start,
                end: v.end,
                layer: 2,
            });
        }
    }
}

/// True when only indentation or a YAML list dash precedes `key_start` on
/// its line. Scans back over `[ \t-]` only, so it stays linear.
fn line_start_key(b: &[u8], key_start: usize) -> bool {
    let mut i = key_start;
    while i > 0 && matches!(b[i - 1], b' ' | b'\t' | b'-') {
        i -= 1;
    }
    i == 0 || matches!(b[i - 1], b'\n' | b'\r')
}

/// An unquoted YAML value: the rest of the line (`password: correct horse
/// battery staple`), without trailing whitespace and punctuation.
fn line_value(text: &str, start: usize) -> Value {
    let end = header_end(text, start, false);
    let s = text[start..end].trim_end();
    Value {
        start,
        end: start + trimmed_len(s),
        run_end: end,
        quoted: false,
    }
}

/// A k8s-style env entry: `- name: DB_PASSWORD` then `value: ...` on the
/// next line.
static NAME_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r#"(?m)^[ \t]*(?:-[ \t]+)?name:[ \t]*["']?(?P<key>[A-Za-z_][A-Za-z0-9_.-]*)["']?[ \t]*\r?\n"#,
        r"[ \t]*value:[ \t]*",
    ))
});

/// Values of `name:`/`value:` pairs whose name is a secret key.
fn name_values(text: &str, out: &mut Vec<Candidate>, seen: &mut Vec<Recognised>) {
    let b = text.as_bytes();
    for caps in NAME_VALUE.captures_iter(text) {
        let (Some(key), Some(all)) = (caps.name("key"), caps.get(0)) else {
            continue;
        };
        let Some(min) = secret_key(key.as_str()) else {
            continue;
        };
        let start = all.end();
        let v = if matches!(b.get(start), Some(b'"' | b'\'' | b'`')) {
            parse_value(text, start, false, text.len())
        } else {
            line_value(text, start)
        };
        seen.push(Recognised {
            start: v.start,
            end: v.run_end,
            min,
        });
        if secret_value(text, v, min) {
            out.push(Candidate {
                rule: "keyword-assignment",
                start: v.start,
                end: v.end,
                layer: 2,
            });
        }
    }
}

/// A CLI flag word: `--long-name` or `-x`.
fn is_flag(w: &str) -> bool {
    match w.as_bytes() {
        [b'-', c] => c.is_ascii_alphabetic(),
        [b'-', b'-', c, rest @ ..] => {
            c.is_ascii_lowercase()
                && rest
                    .iter()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        }
        _ => false,
    }
}

/// Residual check, run after merge: every stretch of a value region some
/// layer recognised as secret-bearing (a secret key) that no finding
/// covers, byte for byte, is redacted (at most [`MAX_VALUE`] bytes, never
/// past the region), unless it is short, only whitespace and punctuation,
/// or a whole-value placeholder. An untouched region uses its key's
/// minimum length; a partly covered one, [`MIN_VALUE`]. "Recognised key,
/// value exempted" is thus impossible by construction; only code values in
/// a code context are never recognised.
pub(super) fn residual(
    text: &str,
    seen: &[Recognised],
    findings: &[Finding],
    out: &mut Vec<Candidate>,
) {
    for r in seen {
        let end = char_limit(text, r.end.min(r.start + MAX_VALUE)).max(r.start);
        let i = findings.partition_point(|f| f.end <= r.start);
        let overlapping = findings[i..].iter().take_while(|f| f.start < end);
        let min = if overlapping.clone().next().is_some() {
            MIN_VALUE
        } else {
            r.min
        };
        let mut at = r.start;
        for f in overlapping {
            uncovered(text, at, f.start.max(at), min, out);
            at = at.max(f.end);
        }
        uncovered(text, at, end.max(at), min, out);
    }
}

/// Redacts the gap `start..end` of a recognised region (see [`residual`]).
fn uncovered(text: &str, start: usize, end: usize, min: usize, out: &mut Vec<Candidate>) {
    let gap = text[start..end].trim_start();
    let s = gap.trim_end();
    let s = &s[..trimmed_len(s)];
    if unmarked_len(s) >= min && s.chars().any(char::is_alphanumeric) && !is_placeholder(s) {
        let start = end - gap.len();
        out.push(Candidate {
            rule: "keyword-assignment",
            start,
            end: start + s.len(),
            layer: 2,
        });
    }
}

/// Values that are obviously not real secrets. The WHOLE value must be a
/// placeholder shape: a template or env reference, a mask, `your-...`
/// without digits, an exact stock word, or one of our markers. A value
/// mixing upper, lower and digits is never a template.
pub(super) fn is_placeholder(v: &str) -> bool {
    let lower = v.to_ascii_lowercase();
    if STOCK.contains(&lower.as_str()) || is_marker(v) || is_mask(v) {
        return true;
    }
    if mixed(v) {
        return false;
    }
    (v.len() > 2 && v.starts_with('<') && v.ends_with('>'))
        || (v.starts_with("${") && v.ends_with('}'))
        || (v.starts_with("$(") && v.ends_with(')'))
        || (v.starts_with("{{") && v.ends_with("}}"))
        || is_percent_var(v)
        || is_env_ref(v)
        || (lower
            .strip_prefix("your")
            .is_some_and(|r| r.starts_with(['-', '_', ' ']))
            && !has_digit(v))
}

/// Exact (case-insensitive) values that mark a non-secret.
const STOCK: &[&str] = &[
    "changeme",
    "example",
    "dummy",
    "placeholder",
    "redacted",
    "password",
    "secret",
    "test",
    "fake",
    "sample",
    "null",
    "none",
    "nil",
    "undefined",
    "true",
    "false",
];

/// A whole `[REDACTED:<rule>]` marker.
fn is_marker(v: &str) -> bool {
    v.strip_prefix("[REDACTED:")
        .and_then(|r| r.strip_suffix(']'))
        .is_some_and(|r| {
            !r.is_empty()
                && r.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
}

/// `***`, `xxxx-xxxx`, `........`: only mask chars and separators.
fn is_mask(v: &str) -> bool {
    let mask = |c: char| matches!(c, 'x' | 'X' | '*' | '.' | '•');
    v.chars().any(mask) && v.chars().all(|c| mask(c) || matches!(c, '-' | '_' | ' '))
}

/// `%VAR%`: a Windows env reference. Upper-case names only: a mixed-case
/// body between `%` signs can be a real password.
fn is_percent_var(v: &str) -> bool {
    v.strip_prefix('%')
        .and_then(|r| r.strip_suffix('%'))
        .is_some_and(|n| {
            !n.is_empty()
                && n.bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        })
}

/// `$VAR`: a shell or env variable reference.
fn is_env_ref(v: &str) -> bool {
    let Some(name) = v.strip_prefix('$') else {
        return false;
    };
    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

#[cfg(test)]
mod tests {
    use super::super::{ALNUM, merge, test_token};
    use super::*;

    /// Merged layer-1 findings as (rule, matched text).
    fn rules_of(text: &str) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        known(text, &mut out);
        merge(out)
            .iter()
            .map(|f| (f.rule, text[f.start..f.end].to_owned()))
            .collect()
    }

    fn tok(seed: u64, len: usize) -> String {
        test_token(seed, len, ALNUM)
    }

    /// Ensures digits and both cases, as real vendor tokens have.
    fn mixed_tok(seed: u64, len: usize) -> String {
        let mut t = tok(seed, len - 3);
        t.push_str("Q7z");
        t
    }

    #[test]
    fn detects_each_known_format() {
        let cases = [
            (
                "aws-access-key",
                [
                    "AK",
                    "IA",
                    &test_token(1, 16, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"),
                ]
                .concat(),
            ),
            (
                "aws-access-key",
                [
                    "AS",
                    "IA",
                    &test_token(2, 16, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"),
                ]
                .concat(),
            ),
            ("github-token", ["gh", "p_", &tok(3, 36)].concat()),
            ("github-token", ["gh", "s_", &tok(4, 36)].concat()),
            (
                "github-token",
                ["github", "_pat_", &tok(5, 22), "_", &tok(6, 59)].concat(),
            ),
            ("gitlab-token", ["gl", "pat-", &tok(7, 20)].concat()),
            (
                "anthropic-key",
                ["sk-", "ant-", "api03-", &tok(8, 90)].concat(),
            ),
            ("openai-key", ["sk-", &mixed_tok(9, 48)].concat()),
            ("openai-key", ["sk-", "proj-", &mixed_tok(10, 60)].concat()),
            (
                "slack-token",
                ["xo", "xb-", "1234567890-", &tok(11, 24)].concat(),
            ),
            ("stripe-key", ["sk", "_live_", &tok(12, 24)].concat()),
            ("stripe-key", ["rk", "_live_", &tok(13, 24)].concat()),
            ("stripe-key", ["sk", "_te", "st_", &tok(25, 24)].concat()),
            ("stripe-key", ["rk", "_te", "st_", &tok(26, 24)].concat()),
            ("google-api-key", ["AI", "za", &tok(14, 35)].concat()),
            ("npm-token", ["np", "m_", &tok(15, 36)].concat()),
            (
                "huggingface-token",
                ["h", "f_", &mixed_tok(16, 34)].concat(),
            ),
            (
                "jwt",
                [
                    "ey",
                    "JhbGciOiJIUzI1NiJ9.",
                    "ey",
                    "J",
                    &tok(17, 30),
                    ".",
                    &tok(18, 43),
                ]
                .concat(),
            ),
        ];
        for (rule, secret) in cases {
            let text = format!("value is {secret} here");
            assert_eq!(rules_of(&text), vec![(rule, secret.clone())], "{rule}");
        }
    }

    #[test]
    fn pem_block_with_and_without_footer() {
        let body = test_token(
            20,
            64,
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/",
        );
        let head = concat!("-----BEGIN ", "RSA PRIVATE", " KEY-----");
        let foot = concat!("-----END ", "RSA PRIVATE", " KEY-----");
        let full = format!("{head}\n{body}\n{body}\n{foot}");
        let found = rules_of(&format!("key:\n{full}\nafter"));
        assert!(found.contains(&("private-key", full.clone())), "{found:?}");

        // The span ends at the last body char; trailing whitespace stays.
        let cut = format!("{head}\n{body}");
        let found = rules_of(&format!("{cut}\n. done"));
        assert!(found.contains(&("private-key", cut.clone())), "{found:?}");
        // Text glued to the block goes with it, so a second pass is a no-op.
        let glued = format!("{head}@/x");
        assert_eq!(rules_of(&glued), vec![("private-key", glued.clone())]);
    }

    #[test]
    fn url_credentials_redact_only_password() {
        let pw = tok(21, 12);
        let text = format!("postgres://admin:{pw}@db.internal:5432/app");
        assert_eq!(rules_of(&text), vec![("url-credentials", pw)]);
        assert_eq!(rules_of("postgres://admin:${DB_PASS}@db:5432/app"), vec![]);
        assert_eq!(rules_of("https://github.com/rust-lang/regex"), vec![]);
        assert_eq!(rules_of("ssh://git@github.com:22/org/repo.git"), vec![]);
    }

    #[test]
    fn bearer_and_basic_values() {
        let t = tok(22, 40);
        assert_eq!(
            rules_of(&format!("Authorization: Bearer {t}")),
            vec![("bearer", format!("Bearer {t}"))]
        );
        assert_eq!(
            rules_of(&format!(r#""authorization": "basic {t}""#)),
            vec![("bearer", format!("basic {t}"))]
        );
        assert_eq!(rules_of("Authorization: $TOKEN"), vec![]);
        assert_eq!(rules_of("Authorization: YOUR_TOKEN_HERE"), vec![]);
        assert_eq!(rules_of("we use bearer tokens for authentication"), vec![]);
    }

    #[test]
    fn prefixes_inside_words_do_not_match() {
        let body = tok(23, 40);
        assert_eq!(rules_of(&format!("task-{body}")), vec![]);
        assert_eq!(rules_of(&format!("xgh{}", ["p_", &body].concat())), vec![]);
        // kebab-case words after `sk-` are not keys.
        assert_eq!(rules_of("sk-learn-compatible-estimator-wrapper"), vec![]);
    }

    #[test]
    fn boundary_after_cjk_and_json_escape() {
        let key = ["gh", "p_", &tok(24, 36)].concat();
        assert_eq!(rules_of(&format!("密钥{key}")).len(), 1);
        assert_eq!(rules_of(&format!(r"line\n{key}")).len(), 1);
    }

    #[test]
    fn placeholders() {
        for v in [
            "***",
            "xxxxxxxx",
            "xxxx-xxxx",
            "<password>",
            "${TOKEN}",
            "$TOKEN",
            "$(cat f)",
            "{{ secret }}",
            "%DB_PASS%",
            "YOUR_API_KEY",
            "your-token-here",
            "changeme",
            "EXAMPLE",
            "[REDACTED:bearer]",
            "true",
            "null",
        ] {
            assert!(is_placeholder(v), "{v}");
        }
        assert!(!is_placeholder("秘密のパスワード値です"));
        for v in [
            "hunter2hunter2",
            "s3cr3t-Pa55word",
            "$2b$12$abcdefghijkl",
            "latestKey9x",
        ] {
            assert!(!is_placeholder(v), "{v}");
        }
    }
    /// Values found by layer 2, as text.
    fn values(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        assignments(text, &mut out, &mut Vec::new());
        out.iter()
            .map(|c| text[c.start..c.end].to_owned())
            .collect()
    }

    #[test]
    fn assignment_syntaxes_redact_only_the_value() {
        let v = mixed_tok(30, 20);
        let cases = [
            format!("API_KEY={v}"),
            format!("db_password: {v}"),
            format!(r#"{{"client_secret": "{v}", "name": "app"}}"#),
            format!("'auth_token' => '{v}',"),
            format!(r#"export GITHUB_TOKEN="{v}""#),
            format!("mytool --password {v} --verbose"),
            format!("mytool --token={v}"),
            format!("secretKey := \"{v}\""),
            format!("token: str = \"{v}\""),
            format!(r#"{{\"apiKey\": \"{v}\"}}"#),
            format!("PRIVATE_KEY = {v};"),
        ];
        for text in cases {
            assert_eq!(values(&text), vec![v.clone()], "{text}");
        }
    }

    #[test]
    fn quoted_values_keep_spaces_and_escapes() {
        assert_eq!(
            values(r#"password = "correct horse battery""#),
            vec!["correct horse battery"]
        );
        assert_eq!(
            values(r#"password = "pa\"ss word 9""#),
            vec![r#"pa\"ss word 9"#]
        );
        // Unterminated: redact to end of line rather than leak.
        assert_eq!(values("password='abc def ghi\nnext"), vec!["abc def ghi"]);
    }

    /// Fails closed: a value of any length is redacted to the end of its run.
    #[test]
    fn values_past_the_cap_are_redacted_whole() {
        let long = "Ab1".repeat(MAX_VALUE);
        assert_eq!(values(&format!("password=\"{long}\"")), vec![long.clone()]);
        assert_eq!(values(&format!("password={long}")), vec![long.clone()]);
        let query = format!("token={long}&next={long} tail");
        assert_eq!(values(&query), vec![format!("{long}&next={long}")]);
        let fits = "Ab1".repeat(100);
        assert_eq!(values(&format!("password=\"{fits}\"")), vec![fits]);
    }

    #[test]
    fn rejected_key_does_not_hide_a_later_assignment() {
        assert_eq!(values("name: token=s3cretValue9"), vec!["s3cretValue9"]);
    }

    #[test]
    fn non_secret_keys_and_values_are_kept() {
        for text in [
            "api_key=YOUR_API_KEY",
            "password=<password>",
            "token=${TOKEN}",
            "secret=changeme",
            "password=***",
            "password=short",
            "author: Jane Q. Developer",
            "tokenizer: bert-base-uncased",
            "max_tokens: 4096000000",
            "token_url: https://auth.example.org/oauth/token",
            "password_file=/run/secrets/db_password",
            "let token = get_token(&config);",
            "let token = self.current_token;",
            "let secret = std::env::var(\"SECRET\")?;",
            "if token == other_token {",
            "pub auth_token: Option<String>,",
            "crate::auth::token::verify(x)",
            "docker login --password-stdin --username someone",
            "password: [REDACTED:keyword-assignment]",
        ] {
            assert_eq!(values(text), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn key_words_split_camel_snake_and_digits() {
        assert_eq!(key_words("xApiKey2"), ["x", "api", "key", "2"]);
        assert_eq!(
            key_words("AWS_SECRET_ACCESS_KEY"),
            ["aws", "secret", "access", "key"]
        );
        assert_eq!(key_words("HTTPAuthToken"), ["http", "auth", "token"]);
        assert!(secret_key("x-api-key").is_some());
        assert!(secret_key("monkey").is_none());
        assert!(secret_key("author").is_none());
    }

    const HEX: &[u8] = b"0123456789abcdef";
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn hex(seed: u64, len: usize) -> String {
        test_token(seed, len, HEX)
    }

    /// Lowercase + digits only: low entropy, no case mix.
    fn low(seed: u64, len: usize) -> String {
        test_token(seed, len, b"abcdefghijklmnopqrstuvwxyz0123456789")
    }

    // Fix 1: only a WHOLE call/path/reference/type counts as code.

    #[test]
    fn code_lookalike_passwords_are_redacted() {
        let parens = format!("{}({}#{}", tok(100, 3), tok(101, 4), tok(102, 4));
        assert_eq!(parens.len(), 13);
        let bang = format!("!{}", tok(103, 12));
        let call_then_data = format!("{}(123){}", tok(104, 3), tok(105, 6));
        let angle = format!("<{}", tok(106, 12));
        for (text, secret) in [
            (format!("DB_PASSWORD={parens}"), &parens),
            (format!("DB_PASSWORD={bang}"), &bang),
            (format!("password: {call_then_data}"), &call_then_data),
            (format!("password: {angle}"), &angle),
            (
                format!("token=*{}", tok(107, 12)),
                &format!("*{}", tok(107, 12)),
            ),
            (
                format!("secret=&{}9", tok(108, 10)),
                &format!("&{}9", tok(108, 10)),
            ),
        ] {
            assert_eq!(values(&text), vec![secret.clone()], "{text}");
        }
    }

    #[test]
    fn real_code_values_are_kept() {
        for text in [
            "let token = get_token(&cfg);",
            "self.password = self.config.password",
            "token: str = Field(...)",
            "const secret = process.env.SECRET",
            "let token = std::env::var(\"TOKEN\").expect(\"no token\");",
            "    private final String token = os.environ[\"API_TOKEN\"];",
            "pub auth_token: HashMap<String, Token>,",
            "let secret = format!(\"{a}{b}\");",
            "items.map(token => token.trim())",
            "send({ token: req.headers.authorization, user })",
            "fn f(password: &cfg)",
            "def login(password=self.config.password):",
        ] {
            assert_eq!(values(text), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn arguments_inside_code_values_are_still_scanned() {
        let v = mixed_tok(109, 16);
        let short = mixed_tok(109, 12);
        assert_eq!(
            values(&format!(
                "let token = make_client(password=\"{short}\", retries=3)"
            )),
            vec![short]
        );
        // M1: a 16+ char literal makes the call data, redacted with it.
        assert_eq!(
            values(&format!(
                "let token = make_client(password=\"{v}\", retries=3)"
            )),
            vec![format!("make_client(password=\"{v}")]
        );
        assert_eq!(
            values(&format!("let token=get();password={v}")),
            vec![v.clone()]
        );
        // At line start (dotenv/shell) the same text is data, redacted whole.
        assert_eq!(
            values(&format!("token=get();password={v}")),
            vec![format!("get();password={v}")]
        );
    }

    // Fix 2: unquoted values are not split by `;`, `,`, quotes or `}`.

    #[test]
    fn unquoted_values_run_to_whitespace() {
        let split = format!("{};{}", tok(110, 7), tok(111, 5));
        let quoted_mid = format!("{}'{}\"{}", tok(112, 4), tok(113, 4), tok(114, 4));
        let brace = format!("{}}}{}", tok(115, 5), tok(116, 5));
        for (text, secret) in [
            (format!("DB_PASSWORD={split}"), split.clone()),
            (format!("DB_PASSWORD={split} # rotated"), split.clone()),
            (format!("password: {quoted_mid}"), quoted_mid.clone()),
            (format!("token={brace}"), brace.clone()),
            (format!("export PASSWORD={split}\nNEXT=1"), split.clone()),
        ] {
            assert_eq!(values(&text), vec![secret], "{text}");
        }
    }

    #[test]
    fn json_values_stop_only_at_structure() {
        let v = mixed_tok(117, 12);
        assert_eq!(
            values(&format!(r#"{{"password":{v},"user":"bob"}}"#)),
            vec![v.clone()]
        );
        assert_eq!(values(&format!(r#"{{"password": {v}}}"#)), vec![v.clone()]);
        // `,` not followed by JSON structure is part of the value.
        let joined = format!("{},{}", tok(118, 3), tok(119, 6));
        assert_eq!(
            values(&format!(r#"{{"password": {joined} }}"#)),
            vec![joined]
        );
    }

    #[test]
    fn trailing_punctuation_is_trimmed_from_the_span() {
        let v = mixed_tok(120, 12);
        for text in [
            format!("f(password={v})"),
            format!("{{token: {v}}}"),
            format!("PRIVATE_KEY = {v};"),
            format!("[secret={v}],"),
        ] {
            assert_eq!(values(&text), vec![v.clone()], "{text}");
        }
    }

    // Fix 3: glued key names.

    #[test]
    fn glued_secret_key_names() {
        for key in [
            "PGPASSWORD",
            "SSHPASS",
            "ACCESSTOKEN",
            "apitoken",
            "authtoken",
            "secretkey",
            "dbpassword",
            "MYSQL_PWD",
            "REDISCLI_AUTH",
            "oauthToken",
            "Authorization",
            "aws_credentials",
            "Cookie",
        ] {
            assert_eq!(secret_key(key), Some(MIN_VALUE), "{key}");
        }
        for key in [
            "PGPASSWORD_FILE",
            "dbpassword_url",
            "apitoken_name",
            "author",
        ] {
            assert_eq!(secret_key(key), None, "{key}");
        }
        assert_eq!(secret_key("session"), Some(MIN_SESSION));
        assert_eq!(secret_key("session_id"), None);
        let pw = ["Hunter", "2", "Blue"].concat();
        assert_eq!(values(&format!("PGPASSWORD={pw} psql -h db")), vec![pw]);
    }

    #[test]
    fn session_values_need_sixteen_chars() {
        assert_eq!(values("session=abc123def4"), Vec::<String>::new());
        let long = mixed_tok(121, 20);
        assert_eq!(values(&format!("session={long}")), vec![long]);
    }

    // Fix 4: any Authorization scheme, cookies, escaped JSON, hex values.

    #[test]
    fn any_authorization_value_is_redacted_whole() {
        let h = hex(130, 40);
        let b = test_token(131, 28, B64) + "==";
        let raw = mixed_tok(132, 24);
        for (text, secret) in [
            (format!("Authorization: Token {h}"), format!("Token {h}")),
            (
                format!("curl -H \"Authorization: token {h}\" https://api"),
                format!("token {h}"),
            ),
            (
                format!(r#"{{\"Authorization\": \"Basic {b}\"}}"#),
                format!("Basic {b}"),
            ),
            (format!("authorization: {raw}"), raw.clone()),
            (
                format!("Authorization: {raw} was rejected"),
                format!("{raw} was rejected"),
            ),
            (
                format!("HTTP_AUTHORIZATION=Bearer {h}"),
                format!("Bearer {h}"),
            ),
            (
                format!(r#"Authorization: Digest username="u", response="{h}""#),
                format!(r#"Digest username="u", response="{h}""#),
            ),
            // Prose is redacted too (D-008).
            (
                "Authorization: required for this endpoint".to_owned(),
                "required for this endpoint".to_owned(),
            ),
        ] {
            assert_eq!(rules_of(&text), vec![("bearer", secret)], "{text}");
        }
        for text in [
            "Authorization: <token>",
            "Authorization: [REDACTED:bearer]",
            "Authorization:",
        ] {
            assert_eq!(rules_of(text), vec![], "{text}");
        }
    }

    #[test]
    fn layer_two_redacts_quoted_authorization_whole() {
        let h = hex(133, 40);
        assert_eq!(
            values(&format!(r#""Authorization": "Token {h}""#)),
            vec![format!("Token {h}")]
        );
        assert_eq!(
            values(&format!("auth: basic {h} trailing")),
            vec![format!("basic {h} trailing")]
        );
        // Hex is not excluded after a secret key.
        assert_eq!(values(&format!("api_token={h}")), vec![h]);
    }

    #[test]
    fn cookie_header_values_are_redacted() {
        let a = mixed_tok(134, 24);
        let b = mixed_tok(135, 32);
        let found = rules_of(&format!("Cookie: theme=dark; sid={a}; csrftoken={b}"));
        let parts = [
            "theme=dark".to_owned(),
            format!("sid={a}"),
            format!("csrftoken={b}"),
        ];
        assert_eq!(found, parts.map(|p| ("cookie", p)).to_vec());
        let found = rules_of(&format!(
            "Set-Cookie: session=\"{a}\"; Path=/; Expires=Wed, 21 Oct 2026 07:28:00 GMT; HttpOnly"
        ));
        assert_eq!(found, vec![("cookie", format!("session=\"{a}\""))]);
        assert_eq!(
            values(&format!(r#""cookie": "sid={b}""#)),
            vec![format!("sid={b}")]
        );
    }

    // Fix 5: URL passwords containing `@`, `/` and quotes.

    #[test]
    fn url_password_runs_to_the_last_at() {
        let pw = format!("{}@{}", mixed_tok(140, 6), mixed_tok(141, 12));
        let text = format!("postgres://app:{pw}@db.internal:5432/app");
        assert_eq!(rules_of(&text), vec![("url-credentials", pw)]);
        let repeated = format!("aaaa@{}", tok(142, 12));
        let text = format!("redis://u:{repeated}@cache:6379");
        assert_eq!(rules_of(&text), vec![("url-credentials", repeated)]);
        let slashy = format!("{}/{}\"{}", tok(143, 4), tok(144, 4), tok(145, 4));
        let text = format!("mysql://root:{slashy}@db/x");
        assert_eq!(rules_of(&text), vec![("url-credentials", slashy)]);
        // A port before an `@` path segment is not a password.
        assert_eq!(
            rules_of("https://registry.npmjs.org:443/@scope/pkg"),
            vec![]
        );
    }

    // Fix 6: placeholders must be WHOLE placeholder shapes.

    #[test]
    fn placeholder_words_inside_secrets_do_not_exempt_them() {
        for v in [
            format!("test_{}", tok(150, 30)),
            format!("example{}", low(151, 10)),
            format!("{}dummy", low(152, 10)),
            format!("[REDACTED:x]{}", low(153, 10)),
            format!("your_{}", low(154, 10)),
            format!("<{}>", mixed_tok(155, 12)),
            format!("${{{}}}", mixed_tok(156, 12)),
            "aaaa@bbbb".to_owned(),
        ] {
            assert!(!is_placeholder(&v), "{v}");
        }
        let pw = low(157, 10);
        assert_eq!(
            values(&format!("token=[REDACTED:x]{pw}")),
            vec![format!("[REDACTED:x]{pw}")]
        );
        let t = format!("test_{}", tok(158, 30));
        assert_eq!(values(&format!("api_key={t}")), vec![t]);
    }

    // Fix 8: more vendor formats; AWS ids glued into names.

    #[test]
    fn vendor_hex_formats() {
        for (rule, secret) in [
            (
                "digitalocean-token",
                ["do", "p_v1_", &hex(160, 64)].concat(),
            ),
            (
                "digitalocean-token",
                ["do", "o_v1_", &hex(161, 64)].concat(),
            ),
            (
                "digitalocean-token",
                ["do", "r_v1_", &hex(162, 64)].concat(),
            ),
            ("mailgun-key", ["ke", "y-", &hex(163, 32)].concat()),
            (
                "sendgrid-key",
                ["S", "G.", &tok(164, 22), ".", &tok(165, 43)].concat(),
            ),
        ] {
            let text = format!("value is {secret} here");
            assert_eq!(rules_of(&text), vec![(rule, secret.clone())], "{rule}");
        }
        assert_eq!(rules_of(&format!("monkey-{}", hex(166, 32))), vec![]);
    }

    #[test]
    fn aws_key_id_glued_to_a_name() {
        let id = [
            "AK",
            "IA",
            &test_token(170, 16, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"),
        ]
        .concat();
        for prefix in ["AWS_KEY_", "key9", "x"] {
            let text = format!("{prefix}{id}");
            assert_eq!(
                rules_of(&text),
                vec![("aws-access-key", id.clone())],
                "{text}"
            );
        }
    }

    // Review round 4: bypasses fixed by deleting heuristics.

    fn scrubbed(text: &str) -> String {
        super::super::redact(text).text.into_owned()
    }

    #[test]
    fn authorization_word_with_digits_is_not_a_scheme() {
        let word = format!("{}7", low(180, 7));
        let cred = mixed_tok(181, 24);
        let out = scrubbed(&format!("Authorization: {word} {cred}"));
        assert_eq!(out, "Authorization: [REDACTED:bearer]");
    }

    #[test]
    fn authorization_short_lowercase_credential_is_redacted() {
        let cred = test_token(182, 16, b"abcdefghijklmnopqrstuvwxyz");
        let out = scrubbed(&format!("Authorization: Bearer {cred}"));
        assert_eq!(out, "Authorization: [REDACTED:bearer]");
    }

    #[test]
    fn line_start_code_shaped_value_is_data() {
        let w = |seed| test_token(seed, 6, b"aeioubcdfg");
        let dotted = format!("{}.{}.{}", w(183), w(184), w(185));
        let call = format!("{}_{}({})", w(186), w(187), w(188));
        for v in [dotted, call] {
            for text in [format!("secret = {v}"), format!("  password = {v};")] {
                assert_eq!(values(&text), vec![v.clone()], "{text}");
            }
        }
    }

    #[test]
    fn flag_value_starting_with_a_dash_is_redacted() {
        let v = format!("-{}", mixed_tok(189, 14));
        assert_eq!(values(&format!("mytool --password {v} db")), vec![v]);
        assert_eq!(values("mytool --password -p db"), Vec::<String>::new());
        let w = mixed_tok(190, 14);
        assert_eq!(
            values(&format!("x --password --verbose --token {w}")),
            vec![w]
        );
    }

    #[test]
    fn partly_covered_region_redacts_the_uncovered_stretch() {
        let (a, b) = (mixed_tok(191, 12), low(192, 12));
        let text = format!("token={a}{b} tail");
        let seen = [Recognised {
            start: 6,
            end: 6 + a.len() + b.len(),
            min: MIN_VALUE,
        }];
        let covered = [Finding {
            rule: "x",
            start: 6,
            end: 6 + a.len(),
        }];
        let mut out = Vec::new();
        residual(&text, &seen, &covered, &mut out);
        let got: Vec<&str> = out.iter().map(|c| &text[c.start..c.end]).collect();
        assert_eq!(got, vec![b.as_str()]);
        // Short or punctuation-only gaps are left alone.
        let text = format!("token={a};;;;;;;;;;");
        let seen = [Recognised {
            start: 6,
            end: text.len(),
            min: MIN_VALUE,
        }];
        let mut out = Vec::new();
        residual(&text, &seen, &covered, &mut out);
        assert!(out.is_empty());
    }

    // Final review: H1-H4, M1-M5 (M6 is in `detects_each_known_format`).

    #[test]
    fn h1_final_key_word_is_secret_unless_public() {
        for key in [
            "ENCRYPTION_KEY",
            "MASTER_KEY",
            "JWT_SIGNING_KEY",
            "HMAC_KEY",
            "LICENSE_KEY",
            "SENDGRID_KEY",
            "CLIENT_KEY",
            "DD_APP_KEY",
            "SESSION_KEY",
            "MAPBOX_KEY",
            "key",
            "keys",
            "masterKey",
            "signingkey",
            "STRIPE_WEBHOOK_SALT",
            "HMAC",
            "ENCRYPTION",
            "FERNET",
            "wallet_seed",
        ] {
            assert_eq!(secret_key(key), Some(MIN_VALUE), "{key}");
        }
        for key in [
            "public_key",
            "PRIMARY_KEY",
            "foreign_key",
            "sortKey",
            "partition_key",
            "cache_key",
            "idempotency_key",
            "object_key",
            "s3_key",
            "ssh_public_key",
            "gpg_public_key",
            "hot_key",
            "map_key",
            "unique_key",
            "composite_key",
            "lookup_key",
            "api_public_key",
            "publishable_key",
            "key_file",
            "keyboard",
            "monkey",
            "webmaster",
        ] {
            assert_eq!(secret_key(key), None, "{key}");
        }
        let h = hex(200, 32);
        for text in [
            format!("MASTER_KEY={h}"),
            format!(r#"{{"key": "{h}"}}"#),
            format!("datadog_app_key = \"{h}\""),
        ] {
            assert_eq!(values(&text), vec![h.clone()], "{text}");
        }
    }

    #[test]
    fn h2_cli_flag_passwords_are_redacted() {
        let pw = mixed_tok(201, 14);
        for text in [
            format!("curl -u admin:{pw} https://x"),
            format!("curl --user=admin:{pw} https://x"),
            format!("curl -u 'admin:{pw}' https://x"),
            format!("mysql -u root -p{pw} db"),
            format!("mysqldump -uroot -p{pw} db > d.sql"),
            format!("mariadb -p{pw}"),
            format!("docker login -u bot -p {pw} reg.io"),
            format!("podman login --password {pw} quay.io"),
            format!("helm registry login r.io -u bot -p {pw}"),
            format!("sshpass -p {pw} ssh host"),
            format!("mongosh -u admin -p {pw}"),
            format!("redis-cli -h cache -a {pw} ping"),
        ] {
            let out = scrubbed(&text);
            assert!(!out.contains(&pw), "{text} -> {out}");
        }
        for text in [
            "ls -p /tmp",
            "grep -a pattern file.bin",
            "docker run -p 8080:80 nginx",
            "docker exec -u 1000:1000 app sh",
            "psql -h db -p 5432 -U app",
            "mysql -u root -p db",
            "echo done; grep -a x f",
        ] {
            assert_eq!(rules_of(text), vec![], "{text}");
        }
    }

    #[test]
    fn h3_netrc_passwords_are_redacted() {
        let pw = mixed_tok(202, 12);
        for text in [
            format!("machine api.example.com login bot password {pw}"),
            format!("machine github.com\n  login bot\n  password {pw}\n"),
            format!("default login anon passwd {pw}"),
            format!("password {pw}"),
        ] {
            assert_eq!(rules_of(&text), vec![("netrc", pw.clone())], "{text}");
        }
        for text in [
            "the password reset flow is broken",
            "password reset emails are delayed",
            "machine learning models are trained nightly",
        ] {
            assert_eq!(rules_of(text), vec![], "{text}");
        }
    }

    #[test]
    fn h4_pat_is_a_secret_final_word() {
        for key in ["AZURE_DEVOPS_EXT_PAT", "GH_PAT", "gitlabPat"] {
            assert_eq!(secret_key(key), Some(MIN_VALUE), "{key}");
        }
        for key in ["compat", "pat_count", "spat"] {
            assert_eq!(secret_key(key), None, "{key}");
        }
    }

    #[test]
    fn m1_long_literal_makes_a_call_data() {
        let h = hex(203, 64);
        assert_eq!(
            values(&format!("const secret = Buffer.from('{h}', 'hex');")),
            vec![format!("Buffer.from('{h}")]
        );
        assert_eq!(
            values("const secret = Buffer.from(raw, 'hex');"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn m2_yaml_line_value_runs_to_end_of_line() {
        for text in [
            "password: correct horse battery staple",
            "  - passphrase: correct horse battery staple  ",
        ] {
            assert_eq!(values(text), vec!["correct horse battery staple"], "{text}");
        }
        // Mid-line keys keep whitespace-delimited values.
        // Value built at runtime: a literal `password: <word>` trips GitGuardian.
        let v = ["abcd", "efgh"].concat();
        assert_eq!(values(&format!("x password: {v} tail")), vec![v.clone()]);
    }

    #[test]
    fn m3_name_value_pairs_with_secret_names() {
        let h = hex(204, 40);
        for text in [
            format!("- name: DB_PASSWORD\n  value: {h}"),
            format!("  - name: \"API_KEY\"\n    value: \"{h}\""),
        ] {
            assert_eq!(values(&text), vec![h.clone()], "{text}");
        }
        assert_eq!(
            values("- name: LOG_LEVEL\n  value: debug-verbose-mode"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn m4_pgpass_lines() {
        let pw = mixed_tok(205, 12);
        for text in [
            format!("db.internal:5432:app:app:{pw}"),
            format!("*:*:*:replicator:{pw}"),
        ] {
            assert_eq!(rules_of(&text), vec![("pgpass", pw.clone())], "{text}");
        }
        assert_eq!(rules_of("src/main.rs:12:5: warning: unused"), vec![]);
    }

    #[test]
    fn m5_token_only_url_userinfo() {
        let t = hex(206, 40);
        assert_eq!(
            rules_of(&format!("git clone https://{t}@github.com/o/r.git")),
            vec![("url-credentials", t)]
        );
        assert_eq!(rules_of("ssh://git@github.com/o/r.git"), vec![]);
        assert_eq!(rules_of("https://deploy-bot@example.com"), vec![]);
    }
}
