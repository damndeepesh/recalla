//! Redaction corpus: recall on positives, zero findings on negatives,
//! mixed documents, idempotence and multibyte offsets.
//!
//! WHY EVERY SECRET IS BUILT AT RUNTIME: GitHub push protection and
//! `GitGuardian` scan this repo. A literal realistic credential (token
//! prefix + body, PEM header, ...) in any committed file would block the
//! push or raise an alert. So fixtures concatenate split prefixes with
//! pseudo-random bodies, e.g. `["gh", "p_", &tok(1, 36)].concat()`. Never
//! write a secret-shaped literal here, not even in a comment.

#![allow(clippy::unwrap_used)]

use std::borrow::Cow;

use recalla::redact::{Finding, redact};

const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64URL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const HEX: &[u8] = b"0123456789abcdef";

/// Deterministic pseudo-random string over `alphabet`.
fn rnd(seed: u64, len: usize, alphabet: &[u8]) -> String {
    let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..len)
        .map(|_| {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            char::from(alphabet[usize::try_from(x >> 33).unwrap() % alphabet.len()])
        })
        .collect()
}

fn tok(seed: u64, len: usize) -> String {
    rnd(seed, len, ALNUM)
}

/// Random body guaranteed to hold upper, lower and digit.
fn mixed(seed: u64, len: usize) -> String {
    let mut t = tok(seed, len - 3);
    t.insert_str(len / 2 - 1, "q7Z");
    t
}

/// Standard base64 with padding.
fn b64(input: &[u8]) -> String {
    let mut s = String::new();
    for chunk in input.chunks(3) {
        let n = chunk.iter().fold(0u32, |a, &b| (a << 8) | u32::from(b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            s.push(char::from(
                B64[usize::try_from((n >> (18 - 6 * i)) & 63).unwrap()],
            ));
        }
    }
    while s.len() % 4 != 0 {
        s.push('=');
    }
    s
}

/// Standard base64 of 32 pseudo-random bytes that contains at least one `/`.
fn b64_with_slash(seed: u64) -> String {
    (seed..seed + 1000)
        .map(|s| {
            b64(&tok(s, 32)
                .bytes()
                .map(|b| b.wrapping_mul(37))
                .collect::<Vec<_>>())
        })
        .find(|k| k.contains('/'))
        .unwrap()
}

/// A PEM private-key block; `kind` is empty or ends with a space.
fn pem(kind: &str, seed: u64) -> String {
    let lines: Vec<String> = (0..4).map(|i| rnd(seed + i, 64, B64)).collect();
    let tail = concat!("PRIVATE", " KEY-----");
    format!(
        "-----BEGIN {kind}{tail}\n{}\n-----END {kind}{tail}",
        lines.join("\n")
    )
}

/// A secret placed in surrounding text.
struct Case {
    text: String,
    secret: String,
}

fn case(template: &str, secret: &str) -> Case {
    Case {
        text: template.replace("{}", secret),
        secret: secret.to_owned(),
    }
}

/// Every positive: at least two per known-format rule, each keyword syntax,
/// and high-entropy tokens.
#[allow(clippy::too_many_lines)] // a fixture table, clearer flat
fn positives() -> Vec<Case> {
    let base32 = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    vec![
        // aws-access-key
        case(
            "aws_access_key_id = {}",
            &["AK", "IA", &rnd(1, 16, base32)].concat(),
        ),
        case(
            "creds {} expired",
            &["AS", "IA", &rnd(2, 16, base32)].concat(),
        ),
        // github-token
        case("push with {} now", &["gh", "p_", &tok(3, 36)].concat()),
        case("GITHUB_TOKEN={}", &["gh", "s_", &tok(4, 36)].concat()),
        case(
            "pat: {}",
            &["github", "_pat_", &tok(5, 22), "_", &tok(6, 59)].concat(),
        ),
        // gitlab-token
        case(
            "git push https://oauth2:{}@gitlab.example.com/g/r.git",
            &["gl", "pat-", &tok(7, 20)].concat(),
        ),
        case("GL={}", &["gl", "pat-", &tok(8, 26)].concat()),
        // anthropic-key
        case(
            "key {} in env",
            &["sk-", "ant-", "api03-", &tok(9, 93)].concat(),
        ),
        case(
            "ANTHROPIC_API_KEY='{}'",
            &["sk-", "ant-", "admin01-", &tok(10, 60)].concat(),
        ),
        // openai-key
        case("openai {}", &["sk-", &mixed(11, 48)].concat()),
        case(
            "using {} for gpt",
            &["sk-", "proj-", &mixed(12, 120)].concat(),
        ),
        // slack-token
        case(
            "slack {} posted",
            &["xo", "xb-", "2462", "1835", "1234-", &tok(13, 24)].concat(),
        ),
        case(
            "SLACK={}",
            &["xo", "xp-", "1111-2222-3333-", &tok(14, 32)].concat(),
        ),
        // stripe-key
        case("stripe {}", &["sk", "_live_", &tok(15, 24)].concat()),
        case("restricted {}", &["rk", "_live_", &tok(16, 99)].concat()),
        // google-api-key
        case("maps key {}", &["AI", "za", &rnd(17, 35, B64URL)].concat()),
        case("?key={}&q=x", &["AI", "za", &tok(18, 35)].concat()),
        // `key=` is now a secret key, so the case above reports
        // keyword-assignment (the longer span); this one keeps the rule.
        case("google maps {}", &["AI", "za", &tok(23, 35)].concat()),
        // npm-token
        case(
            "//registry.npmjs.org/:_authToken={}",
            &["np", "m_", &tok(19, 36)].concat(),
        ),
        case("npm {}", &["np", "m_", &tok(20, 36)].concat()),
        // huggingface-token
        case("hf login {}", &["h", "f_", &mixed(21, 34)].concat()),
        case("HF_TOKEN={}", &["h", "f_", &mixed(22, 34)].concat()),
        // jwt
        case(
            "session token {} expired",
            &[
                "ey",
                "JhbGciOiJIUzI1NiJ9.",
                "ey",
                "J",
                &rnd(23, 40, B64URL),
                ".",
                &rnd(24, 43, B64URL),
            ]
            .concat(),
        ),
        case(
            "id_token {}",
            &[
                "ey",
                "JhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.",
                "ey",
                "J",
                &tok(25, 60),
                ".",
                &rnd(26, 86, B64URL),
            ]
            .concat(),
        ),
        // private-key
        case("deploy key:\n{}\nend", &pem("RSA ", 27)),
        case("{}", &pem("OPENSSH ", 31)),
        case("pkcs8 {} done", &pem("", 35)),
        // url-credentials
        case("postgres://admin:{}@db.internal:5432/app", &tok(40, 16)),
        case("redis://:{}@cache:6379/0", &mixed(41, 20)),
        // bearer
        case("Authorization: Bearer {}", &rnd(42, 48, B64URL)),
        case(
            r#"headers = {"Authorization": "Basic {}"}"#,
            &b64(b"someone:pa55word-of-mine"),
        ),
        // keyword-assignment, each syntax
        case("DB_PASSWORD={}", &["hunter", "2-", &tok(43, 8)].concat()),
        case("client_secret: {}", &["s3cr3t", "Value", "Long"].concat()),
        case(
            r#"{"api_key": "{}", "region": "eu"}"#,
            &["k9", "-local-", "value"].concat(),
        ),
        case("config.secret => '{}',", &["ruby", "Secret", "99"].concat()),
        case(
            r#"export AUTH_TOKEN="{}""#,
            &["shell", "Token", "7x"].concat(),
        ),
        case(
            "mysql -u root --password {} db",
            &["root", "pass", "2024"].concat(),
        ),
        case(
            "tool --token={} --verbose",
            &["flag", "Token", "42z"].concat(),
        ),
        case(r#"password = "{}""#, "correct horse battery staple"),
        case(
            "let token: &str = \"{}\";",
            &["typed", "Value", "77"].concat(),
        ),
        // high-entropy
        case("random {} in prose", &mixed(50, 32)),
        case("x-signature: {}", &rnd(51, 43, B64URL)),
        case(
            "blob {} end",
            &["wJal", &rnd(52, 14, B64), "/K7", &rnd(53, 19, B64)].concat(),
        ),
        case("short {}!", &mixed(54, 24)),
        case("long {}.", &tok(55, 64)),
        case(
            "https://hooks.example.com/services/webhook/{}",
            &mixed(56, 32),
        ),
        // base64 keys containing `/`, including one split into short segments
        case("key {} end", &b64_with_slash(57)),
        case("key {} end", &b64_with_slash(58)),
        case(
            "blob {} end",
            &(0..4).map(|i| tok(59 + i, 9)).collect::<Vec<_>>().join("/"),
        ),
    ]
}

fn negatives() -> Vec<String> {
    let sentence = b"Recalla keeps memories local, small, and easy to audit.";
    vec![
        format!("commit {} (HEAD -> main)", rnd(60, 40, HEX)),
        format!("uid {}", rnd(61, 32, HEX)),
        format!(
            "session {}-{}-4{}-a{}-{}",
            rnd(62, 8, HEX),
            rnd(63, 4, HEX),
            rnd(64, 3, HEX),
            rnd(65, 3, HEX),
            rnd(66, 12, HEX)
        ),
        format!("sha256:{}", rnd(67, 64, HEX)),
        "see crate::store::search::fts_query for details".into(),
        "edited /Users/someone/projects/recalla/src/store/mod.rs".into(),
        "see /Users/x/projects/recalla/src/store/mod.rs and src/redact/entropy.rs".into(),
        "module crate/store/search and src/redact/some_module_name/entropy_tests/fixtures".into(),
        "web/src/components/UserProfileCard/UserProfileCardHeader.tsx".into(),
        "docs at https://docs.rs/regex/latest/regex/struct.Regex.html#method.find_iter".into(),
        format!("encoded: {}", b64(sentence)),
        "MAX_CONTEXT_WINDOW_TOKENS_PER_REQUEST and DEFAULT_SESSION_TIMEOUT_SECONDS".into(),
        "call handleOAuth2CallbackForGoogleSSO then getUserById2FromCacheOrDb".into(),
        r#"!function(e,t){"use strict";var n=function(e){return e&&e.__esModule?e:{default:e}};a.authToken=r.headers.get("x-auth");t.exports=n(e)}(this,function(){return{version:"3.7.1"}});"#.into(),
        r#"{"name": "recalla", "version": "0.1.0", "private": true, "tokens_used": 12345, "max_tokens": 4096, "author": "Someone Dev"}"#.into(),
        "api_key=YOUR_API_KEY".into(),
        "password=<password>".into(),
        "token=${TOKEN}".into(),
        "secret=changeme".into(),
        "password=***".into(),
        "token_url: https://auth.example.org/oauth/token".into(),
        "let token = get_token(&config)?;".into(),
        "x86_64-unknown-linux-gnu and aarch64-apple-darwin targets".into(),
        "[REDACTED:github-token] and token=[REDACTED:keyword-assignment]".into(),
        // Real code must survive the narrowed code heuristic.
        "let token = get_token(&cfg);".into(),
        "self.password = self.config.password".into(),
        "token: str = Field(...)".into(),
        "const secret = process.env.SECRET".into(),
        "let token = std::env::var(\"TOKEN\").expect(\"no token\");".into(),
        format!("tool_use_id: toolu_01{}", tok(68, 22)),
        "https://registry.npmjs.org:443/@scope/pkg".into(),
    ]
}

/// Asserts no 8+ char substring of `secret` survives in `out`.
fn assert_gone(out: &str, secret: &str, ctx: &str) {
    let chars: Vec<char> = secret.chars().collect();
    for w in chars.windows(8) {
        let piece: String = w.iter().collect();
        assert!(
            !out.contains(&piece),
            "leaked {piece:?} of secret in {ctx:?} -> {out:?}"
        );
    }
}

/// Offsets are valid char boundaries in the original, sorted, disjoint.
fn assert_offsets(text: &str, findings: &[Finding]) {
    let mut last = 0;
    for f in findings {
        assert!(f.start < f.end && f.start >= last, "{findings:?}");
        assert!(text.is_char_boundary(f.start) && text.is_char_boundary(f.end));
        last = f.end;
    }
}

fn assert_idempotent(text: &str) {
    let once = redact(text);
    let twice = redact(&once.text);
    assert!(
        twice.is_clean(),
        "second pass found {:?} in {:?}",
        twice.findings,
        once.text
    );
    assert_eq!(twice.text, once.text);
}

#[test]
fn every_positive_is_fully_removed() {
    for c in positives() {
        let r = redact(&c.text);
        assert!(!r.is_clean(), "missed {:?}", c.text);
        assert_gone(&r.text, &c.secret, &c.text);
        assert_offsets(&c.text, &r.findings);
    }
}

#[test]
fn known_formats_report_their_rule() {
    let expect = [
        "aws-access-key",
        "github-token",
        "gitlab-token",
        "anthropic-key",
        "openai-key",
        "slack-token",
        "stripe-key",
        "google-api-key",
        "npm-token",
        "huggingface-token",
        "jwt",
        "private-key",
        "url-credentials",
        "bearer",
        "keyword-assignment",
        "high-entropy",
    ];
    let rules: Vec<&str> = positives()
        .iter()
        .flat_map(|c| redact(&c.text).findings)
        .map(|f| f.rule)
        .collect();
    for rule in expect {
        assert!(
            rules.iter().filter(|r| **r == rule).count() >= 2,
            "{rule}: {rules:?}"
        );
    }
}

#[test]
fn negatives_have_zero_findings() {
    for text in negatives() {
        let r = redact(&text);
        assert!(r.is_clean(), "false positive {:?} in {text:?}", r.findings);
        assert!(matches!(r.text, Cow::Borrowed(_)));
    }
}

#[test]
fn keyword_layer_keeps_key_and_separator() {
    let v = ["hunter", "2-", &tok(70, 8)].concat();
    for (text, kept) in [
        (format!("DB_PASSWORD={v}"), "DB_PASSWORD="),
        (format!("db_password: {v}"), "db_password: "),
        (format!(r#""password": "{v}""#), r#""password": ""#),
        (format!("password => '{v}'"), "password => '"),
        (
            format!(r#"export DB_PASSWORD="{v}""#),
            r#"export DB_PASSWORD=""#,
        ),
        (format!("--password {v}"), "--password "),
        (format!("--token={v}"), "--token="),
    ] {
        let out = redact(&text).text.into_owned();
        assert!(out.starts_with(kept), "{text} -> {out}");
        assert!(
            out.contains("[REDACTED:keyword-assignment]"),
            "{text} -> {out}"
        );
    }
}

struct Doc {
    text: String,
    secrets: Vec<String>,
    kept: Vec<&'static str>,
}

#[allow(clippy::too_many_lines)] // a fixture table, clearer flat
fn docs() -> Vec<Doc> {
    let env_pw = tok(80, 18);
    let openai = ["sk-", "proj-", &mixed(81, 80)].concat();
    let gh = ["gh", "p_", &tok(82, 36)].concat();
    let akia = [
        "AK",
        "IA",
        &rnd(83, 16, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"),
    ]
    .concat();
    let aws_secret = rnd(84, 40, B64);
    let stripe = ["sk", "_live_", &tok(85, 32)].concat();
    let yaml_pw = ["Yaml", "Pass", "w0rd!"].concat();
    let slack = ["xo", "xb-", "9876543210-", &tok(86, 24)].concat();
    let hf = ["h", "f_", &mixed(87, 34)].concat();
    let bearer = rnd(88, 40, B64URL);
    let ant = ["sk-", "ant-", "api03-", &tok(89, 93)].concat();
    let npm = ["np", "m_", &tok(90, 36)].concat();
    let google = ["AI", "za", &tok(91, 35)].concat();
    let url_pw = mixed(92, 14);
    let jwt = [
        "ey",
        "JhbGciOiJIUzI1NiJ9.",
        "ey",
        "J",
        &tok(93, 40),
        ".",
        &tok(94, 43),
    ]
    .concat();
    let sha = rnd(95, 7, HEX);
    // Bypasses found in security review, one per line.
    let parens = format!("{}({}#{}", tok(96, 3), tok(97, 4), tok(98, 4));
    let bang = format!("!{}", tok(99, 12));
    let split = format!("{};{}", tok(110, 7), tok(111, 5));
    let yaml_call = format!("{}(123){}", tok(112, 3), tok(113, 8));
    let angle = format!("<{}", tok(114, 12));
    let pg = ["Hunter", "2", "Blue"].concat();
    let token_hex = rnd(115, 40, HEX);
    let curl_hex = rnd(116, 40, HEX);
    let at_pw = format!("{}@{}", mixed(117, 6), mixed(118, 12));
    let marker_pw = rnd(119, 12, b"abcdefghijklmnopqrstuvwxyz0123456789");
    let test_key = format!("test_{}", tok(120, 30));
    let msg_like = format!("msg_{}", rnd(121, 30, B64));
    let basic = b64(b"deploy:correct-horse-9");
    vec![
        Doc {
            text: format!(
                "DB_PASSWORD={parens}\nADMIN_PASSWORD={bang}\nMYSQL_PWD={split}\nPGPASSWORD={pg} psql -h db\nDATABASE_URL=postgres://app:{at_pw}@db.internal:5432/app\n"
            ),
            secrets: vec![parens, bang, split, pg, at_pw],
            kept: vec![
                "DB_PASSWORD=[REDACTED",
                "ADMIN_PASSWORD=[REDACTED",
                "MYSQL_PWD=[REDACTED",
                "PGPASSWORD=[REDACTED:keyword-assignment] psql -h db",
                "DATABASE_URL=postgres://app:[REDACTED:url-credentials]@db.internal:5432/app",
            ],
        },
        Doc {
            text: format!(
                "db:\n  password: {yaml_call}\n  replica_password: {angle}\nAuthorization: Token {token_hex}\n$ curl -H \"Authorization: token {curl_hex}\" https://api.example.com\n"
            ),
            secrets: vec![yaml_call, angle, token_hex, curl_hex],
            kept: vec![
                "db:\n  password: [REDACTED",
                "Authorization: [REDACTED:bearer]\n",
                "$ curl -H \"Authorization: [REDACTED:bearer]\" https://api.example.com",
            ],
        },
        Doc {
            text: format!(
                r#"{{"type":"tool_result","content":"{{\"headers\": {{\"Authorization\": \"Basic {basic}\"}}, \"cfg\": \"token=[REDACTED:x]{marker_pw}\", \"api_key\": \"{test_key}\", \"note\": \"{msg_like}\"}}"}}"#
            ),
            secrets: vec![basic, marker_pw, test_key, msg_like],
            kept: vec![
                r#"{"type":"tool_result","content":"{\"headers\": {\"Authorization\": \"[REDACTED:bearer]\"}"#,
                r#"\"api_key\": \"[REDACTED:keyword-assignment]\""#,
            ],
        },
        Doc {
            text: format!(
                "# database\nDATABASE_URL=postgres://app:{env_pw}@localhost:5432/app\nREDIS_HOST=127.0.0.1\nOPENAI_API_KEY={openai}\nGITHUB_TOKEN={gh}\nLOG_LEVEL=debug\n"
            ),
            secrets: vec![env_pw, openai, gh],
            kept: vec![
                "# database",
                "DATABASE_URL=postgres://app:",
                "@localhost:5432/app",
                "REDIS_HOST=127.0.0.1",
                "OPENAI_API_KEY=",
                "GITHUB_TOKEN=",
                "LOG_LEVEL=debug",
            ],
        },
        Doc {
            text: format!(
                r#"{{"service": "billing", "port": 8080, "aws": {{"access_key_id": "{akia}", "secret_access_key": "{aws_secret}"}}, "stripe": {{"secret_key": "{stripe}"}}, "retries": 3}}"#
            ),
            secrets: vec![akia, aws_secret, stripe],
            kept: vec![
                r#"{"service": "billing", "port": 8080, "aws": {"access_key_id": ""#,
                r#""secret_access_key": ""#,
                r#""stripe": {"secret_key": ""#,
                r#""retries": 3}"#,
            ],
        },
        Doc {
            text: format!(
                "app:\n  name: recalla\n  debug: false\ndatabase:\n  host: db.internal\n  password: \"{yaml_pw}\"\nslack:\n  bot_token: {slack}\nhuggingface:\n  token: {hf}\n"
            ),
            secrets: vec![yaml_pw, slack, hf],
            kept: vec![
                "app:\n  name: recalla\n  debug: false\ndatabase:\n  host: db.internal\n  password: \"",
                "\"\nslack:\n  bot_token: ",
                "\nhuggingface:\n  token: ",
            ],
        },
        Doc {
            text: format!(
                "$ curl -s -H \"Authorization: Bearer {bearer}\" https://api.example.com/v1/items\n{{\"items\": [], \"count\": 0}}\n$ git log --oneline -1\n{sha} feat: add redaction\n$ export ANTHROPIC_API_KEY=\"{ant}\"\n$ npm publish --token={npm}\n"
            ),
            secrets: vec![bearer, ant, npm],
            kept: vec![
                "$ curl -s -H \"Authorization: [REDACTED:bearer]",
                "\" https://api.example.com/v1/items\n{\"items\": [], \"count\": 0}\n$ git log --oneline -1\n",
                " feat: add redaction\n$ export ANTHROPIC_API_KEY=\"",
                "\"\n$ npm publish --token=",
            ],
        },
        Doc {
            text: format!(
                "Traceback (most recent call last):\n  File \"/home/dev/app/client.py\", line 42, in connect\n    session = Client(api_key=\"{google}\", timeout=30)\n  File \"/usr/lib/python3.12/site-packages/httpx/_client.py\", line 1014, in send\nhttpx.HTTPStatusError: 401 Unauthorized for url 'https://user:{url_pw}@api.example.com/v2/login'\nRuntimeError: token rejected: {jwt}\n"
            ),
            secrets: vec![google, url_pw, jwt],
            kept: vec![
                "Traceback (most recent call last):\n  File \"/home/dev/app/client.py\", line 42, in connect\n    session = [REDACTED:keyword-assignment]",
                "\", timeout=30)\n  File \"/usr/lib/python3.12/site-packages/httpx/_client.py\", line 1014, in send\nhttpx.HTTPStatusError: 401 Unauthorized for url 'https://user:",
                "@api.example.com/v2/login'\nRuntimeError: token rejected: ",
            ],
        },
    ]
}

#[test]
fn mixed_documents_lose_secrets_and_keep_text() {
    for d in docs() {
        let r = redact(&d.text);
        assert_offsets(&d.text, &r.findings);
        assert_eq!(
            r.findings.len(),
            d.secrets.len(),
            "{:?} -> {:?}",
            d.text,
            r.text
        );
        for s in &d.secrets {
            assert_gone(&r.text, s, &d.text);
        }
        for k in &d.kept {
            assert!(r.text.contains(k), "lost {k:?}: {}", r.text);
        }
    }
}

#[test]
fn redaction_is_idempotent_across_the_corpus() {
    for c in positives() {
        assert_idempotent(&c.text);
    }
    for t in negatives() {
        assert_idempotent(&t);
    }
    for d in docs() {
        assert_idempotent(&d.text);
    }
}

#[test]
fn multibyte_text_around_secrets() {
    let gh = ["gh", "p_", &tok(100, 36)].concat();
    let pw = ["秘密", "のパスワード", "値です"].concat();
    let e = mixed(101, 32);
    let text = format!("🔑 密钥：{gh} 🚀 ok\nパスワード password={pw} 終わり\n— ünïcödé {e} ✅");
    let r = redact(&text);
    assert_offsets(&text, &r.findings);
    let found: Vec<&str> = r.findings.iter().map(|f| &text[f.start..f.end]).collect();
    assert_eq!(found, vec![gh.as_str(), pw.as_str(), e.as_str()]);
    assert_eq!(
        r.text,
        "🔑 密钥：[REDACTED:github-token] 🚀 ok\nパスワード password=[REDACTED:keyword-assignment] 終わり\n— ünïcödé [REDACTED:high-entropy] ✅"
    );
    assert_idempotent(&text);
}

// Review round 3: hand-offs between layers and exemptions.

fn letters(seed: u64, len: usize) -> String {
    rnd(seed, len, b"abcdefghijklmnopqrstuvwxyz")
}

/// Lowercase + digits: below the entropy layer's class bar, so only the
/// key or header rules can catch it.
fn low(seed: u64, len: usize) -> String {
    let mut t = rnd(seed, len - 1, b"abcdefghijklmnopqrstuvwxyz0123456789");
    t.push('7');
    t
}

fn title(seed: u64, len: usize) -> String {
    let w = letters(seed, len);
    w[..1].to_ascii_uppercase() + &w[1..]
}

fn assert_redacts(text: &str, secret: &str) {
    let r = redact(text);
    assert!(!r.is_clean(), "missed {text:?}");
    assert_gone(&r.text, secret, text);
    assert_offsets(text, &r.findings);
    assert_idempotent(text);
}

/// Finding 1: layer 2 never skips a header key on the assumption that
/// layer 1 handles it.
#[test]
fn header_keys_are_redacted_without_layer_one() {
    let a = low(200, 24);
    // All lowercase, under 20 chars: the shared header parser calls it
    // prose, so only the residual check catches it.
    let low = letters(201, 16);
    for (text, secret) in [
        (format!("cookies: sid={a}"), &a),
        (format!("cookies={a}"), &a),
        (format!("my_cookie=sid={a}"), &a),
        (format!("x_cookie: {a}"), &a),
        (format!(r#"{{"cookies": "{a}"}}"#), &a),
        (format!("Authorization={a}"), &a),
        (format!("proxy_authorization: Bearer {a}"), &a),
        (format!("Authorization={low}"), &low),
        (format!("authorization: {low}"), &low),
    ] {
        assert_redacts(&text, secret);
    }
}

/// Finding 2: a cookie header whose value is one bare token.
#[test]
fn bare_cookie_tokens_are_redacted() {
    let t = low(210, 32);
    for text in [
        format!("Cookie: {t}"),
        format!("Cookie: {t}; Path=/"),
        format!("Set-Cookie: {t}; HttpOnly; Secure"),
        format!("cookie: theme=dark; {t}"),
    ] {
        assert_redacts(&text, &t);
    }
    let out = redact(&format!(
        "Set-Cookie: sid={t}; Path=/; HttpOnly; Secure; SameSite=Lax; Partitioned"
    ))
    .text
    .into_owned();
    assert!(
        out.ends_with("; Path=/; HttpOnly; Secure; SameSite=Lax; Partitioned"),
        "{out}"
    );
}

/// Finding 3: code lookalikes in dotenv/shell/YAML are data.
#[test]
fn code_lookalikes_in_config_are_redacted() {
    let call = format!("{}({})", tok(220, 4), tok(221, 2));
    let reference = format!("&{}", letters(222, 12));
    let star = format!("*{}", letters(223, 12));
    let get = format!("get{}({})", tok(224, 3), letters(225, 4));
    let dotted = format!("{}.{}", title(226, 6), title(227, 5));
    let bare_call = format!("{}({})", letters(228, 8), letters(229, 4));
    let member = format!("self.{}", letters(230, 9));
    for (text, secret) in [
        (format!("API_TOKEN={call}"), &call),
        (format!("API_TOKEN={call}x{}", tok(231, 6)), &call),
        (format!("SECRET={reference}"), &reference),
        (format!("SECRET={star}"), &star),
        (format!("PASSWORD={get}"), &get),
        (format!("DB_PASSWORD={dotted}"), &dotted),
        (format!("AUTH_TOKEN={bare_call}"), &bare_call),
        (format!("export API_TOKEN={bare_call}"), &bare_call),
        (format!("db:\n  password: {member}"), &member),
        (format!("- token: {bare_call}"), &bare_call),
        (format!("x\\nTOKEN={bare_call}\\n"), &bare_call),
        // Source context, but not identifier-word structure.
        (format!("let password = {dotted};"), &dotted),
        (format!("password = {get}"), &get),
        (format!("const token = {call};"), &call),
    ] {
        assert_redacts(&text, secret);
    }
    // A line-start assignment without a declaration keyword is data, even
    // when its value is member-shaped.
    assert_eq!(
        redact("password = self.config.password").text,
        "password = [REDACTED:keyword-assignment]"
    );
    // Real code still survives.
    for text in [
        "let token = get_token(&cfg);",
        "self.password = self.config.password",
        "token: str = Field(...)",
        "const secret = process.env.SECRET",
        "let token = std::env::var(\"TOKEN\").expect(\"no token\");",
        "pub auth_token: Option<String>,",
        "fn f(password: &cfg)",
    ] {
        assert!(redact(text).is_clean(), "{text}");
    }
}

/// Finding 4: a port-shaped password still loses its secret part.
#[test]
fn url_passwords_shaped_like_ports_are_redacted() {
    let pw = mixed(240, 12);
    let abc = tok(241, 12);
    for (text, secret) in [
        (
            format!("https://user:{pw}/x@host.example.com/p"),
            pw.clone(),
        ),
        (
            format!("https://user:123/{abc}@host.example.com"),
            abc.clone(),
        ),
        (format!("redis://u:6379/{abc}@cache"), abc.clone()),
    ] {
        assert_redacts(&text, &secret);
    }
    assert!(redact("https://registry.npmjs.org:443/@scope/pkg").is_clean());
}

/// Escaped quotes and backslashes inside a URL password.
#[test]
fn url_passwords_with_escapes_are_redacted() {
    let (a, b) = (tok(250, 10), tok(251, 10));
    for text in [
        format!(r#"postgres://app:{a}\"{b}@db.internal/app"#),
        format!(r"postgres://app:{a}\\{b}@db.internal/app"),
    ] {
        let out = redact(&text).text.into_owned();
        assert_gone(&out, &a, &text);
        assert_gone(&out, &b, &text);
    }
}

/// Oversized values fail closed instead of being dropped.
#[test]
fn oversized_values_are_redacted() {
    let long = "hunter2-".repeat(700);
    for text in [
        format!("password={long}"),
        format!("password=\"{long}\""),
        format!(r#"{{"api_key": "{long}"}}"#),
    ] {
        assert_redacts(&text, &long);
    }
}

/// A JSON-escaped `\/` does not split a base64 secret into short pieces.
#[test]
fn escaped_slashes_do_not_split_tokens() {
    let parts: Vec<String> = (0..4).map(|i| tok(260 + i, 9)).collect();
    let escaped = parts.join(r"\/");
    assert_redacts(&format!(r#"{{"blob": "{escaped}"}}"#), &escaped);
    let key = b64_with_slash(264).replace('/', r"\/");
    assert_redacts(&format!(r#"{{\"k\": \"{key}\"}}"#), &key);
    // Escaped paths stay clean.
    assert!(
        redact(r#"{"file": "\/Users\/someone\/projects\/recalla\/src\/store\/mod.rs"}"#).is_clean()
    );
}
