//! Realistic credential corpus from the final security review: 108 config,
//! shell, header, connection-string and source templates, each filled with
//! 20 deterministic random secrets. No 8+ char piece of a secret may
//! survive. Plus negatives that must stay untouched and a seeded fuzz that
//! asserts idempotence.
//!
//! Secrets are built at runtime from split prefixes (see `tests/redact.rs`
//! for why). Templates hold only the surrounding text and `{S}`.

#![allow(clippy::unwrap_used)]

use recalla::redact::redact;

/// splitmix64: tiny deterministic PRNG, seeded per case and trial.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }

    fn pick(&mut self, alphabet: &[u8], n: usize) -> String {
        (0..n)
            .map(|_| char::from(alphabet[self.below(alphabet.len())]))
            .collect()
    }
}

const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const LOWNUM: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const HEX: &[u8] = b"0123456789abcdef";
const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PW: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#%^*-_.";
const BASE32: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Secret generator by kind. Vendor prefixes are split so no literal here
/// looks like a credential.
fn make(r: &mut Rng, kind: &str) -> String {
    match kind {
        "alnum16" => r.pick(ALNUM, 16),
        "alnum20" => r.pick(ALNUM, 20),
        "alnum32" => r.pick(ALNUM, 32),
        "low52" => r.pick(LOWNUM, 52),
        "hex32" => r.pick(HEX, 32),
        "hex40" => r.pick(HEX, 40),
        "hex64" => r.pick(HEX, 64),
        "b64_40" => r.pick(B64, 40),
        "b64_88" => format!("{}==", r.pick(B64, 86)),
        "pw16" => r.pick(PW, 16),
        "pw20" => r.pick(PW, 20),
        "aws_id" => ["AK", "IA", &r.pick(BASE32, 16)].concat(),
        "ghp" => ["gh", "p_", &r.pick(ALNUM, 36)].concat(),
        "ghs" => ["gh", "s_", &r.pick(ALNUM, 36)].concat(),
        "glpat" => ["gl", "pat-", &r.pick(ALNUM, 20)].concat(),
        "npm" => ["np", "m_", &r.pick(ALNUM, 36)].concat(),
        "uuid" => {
            let h = r.pick(HEX, 32);
            format!(
                "{}-{}-{}-{}-{}",
                &h[..8],
                &h[8..12],
                &h[12..16],
                &h[16..20],
                &h[20..]
            )
        }
        "sktest" => ["sk", "_te", "st_", &r.pick(ALNUM, 24)].concat(),
        "rktest" => ["rk", "_te", "st_", &r.pick(ALNUM, 24)].concat(),
        "hvs" => ["hv", "s.", &r.pick(ALNUM, 24)].concat(),
        "xoxb" => format!(
            "{}-{}-{}",
            ["xo", "xb"].concat(),
            r.pick(b"0123456789", 12),
            r.pick(ALNUM, 24)
        ),
        "phrase" => {
            let words = ["correct", "horse", "battery", "staple", "orbit", "maple"];
            (0..4)
                .map(|_| words[r.below(words.len())])
                .collect::<Vec<_>>()
                .join(" ")
        }
        _ => unreachable!("unknown secret kind {kind}"),
    }
}

/// (format, secret kind, template). `{S}` is the secret.
const CASES: &[(&str, &str, &str)] = &[
    // .env / shell
    ("env", "alnum32", "DATABASE_PASSWORD={S}"),
    ("env", "pw16", "DB_PASS='{S}'"),
    ("env", "hex64", "SECRET_KEY_BASE={S}"),
    ("env", "hex64", "ENCRYPTION_KEY={S}"),
    ("env", "hex32", "MASTER_KEY={S}"),
    ("env", "hex64", "JWT_SIGNING_KEY={S}"),
    ("env", "alnum32", "NEXTAUTH_SECRET=\"{S}\""),
    ("env", "hex32", "HMAC_KEY={S}"),
    ("env", "hex32", "LICENSE_KEY={S}"),
    ("env", "alnum20", "MAPBOX_KEY={S}"),
    ("env", "hex32", "SENDGRID_KEY={S}"),
    ("env", "hex40", "CLIENT_KEY={S}"),
    ("env", "uuid", "HEROKU_API_KEY={S}"),
    ("env", "hex32", "DD_APP_KEY={S}"),
    ("env", "alnum32", "OPENAI_API_KEY={S}"),
    ("env", "hex32", "SESSION_KEY={S}"),
    ("env", "hex64", "COOKIE_SIGNING_KEY={S}"),
    ("shell", "ghp", "export GITHUB_TOKEN={S}"),
    ("shell", "pw16", "export PGPASSWORD='{S}'"),
    ("shell", "hex40", "export SIGNING_KEY={S}"),
    (
        "shell",
        "pw16",
        "curl -u admin:{S} https://api.example.com/v1",
    ),
    (
        "shell",
        "pw16",
        "curl --user admin:{S} https://api.example.com/v1",
    ),
    (
        "shell",
        "pw16",
        "docker login -u deploy -p {S} registry.example.com",
    ),
    ("shell", "pw16", "mysql -u root -p{S} mydb"),
    ("shell", "pw16", "redis-cli -h cache -a {S} ping"),
    ("shell", "pw16", "sshpass -p {S} ssh root@host"),
    ("shell", "pw16", "mysql --password={S} mydb"),
    ("shell", "pw16", "psql \"postgresql://app:{S}@db:5432/app\""),
    // YAML
    ("yaml", "pw16", "      POSTGRES_PASSWORD: {S}"),
    ("yaml", "pw16", "      - POSTGRES_PASSWORD={S}"),
    ("yaml", "b64_40", "  password: {S}"),
    ("yaml", "b64_40", "  api-key: {S}"),
    ("yaml", "hex64", "  encryption_key: {S}"),
    ("yaml", "alnum32", "  client_secret: \"{S}\""),
    ("yaml", "alnum32", "        value: {S}"),
    ("yaml", "hex32", "  masterKey: {S}"),
    // JSON
    ("json", "alnum32", "{\"password\": \"{S}\"}"),
    ("json", "alnum32", "{\"apiKey\":\"{S}\",\"region\":\"us\"}"),
    ("json", "hex64", "{\"encryptionKey\": \"{S}\"}"),
    ("json", "hex32", "{\"key\": \"{S}\"}"),
    ("json", "alnum32", "{\"client_secret\":\"{S}\"}"),
    ("json", "hex40", "{\"signingKey\": \"{S}\"}"),
    ("json", "b64_40", "{\"credentials\": {\"token\": \"{S}\"}}"),
    // curl headers
    (
        "header",
        "alnum32",
        "curl -H \"Authorization: Bearer {S}\" https://x",
    ),
    ("header", "hex32", "curl -H \"X-API-Key: {S}\" https://x"),
    ("header", "hex32", "curl -H 'x-auth-token: {S}' https://x"),
    ("header", "alnum32", "curl -H \"Api-Key: {S}\" https://x"),
    (
        "header",
        "glpat",
        "curl --header \"PRIVATE-TOKEN: {S}\" https://x",
    ),
    (
        "header",
        "hex40",
        "curl -H \"Authorization: token {S}\" https://x",
    ),
    (
        "header",
        "b64_40",
        "curl -H \"Authorization: Basic {S}\" https://x",
    ),
    ("header", "hvs", "curl -H \"X-Vault-Token: {S}\" https://x"),
    ("header", "hex32", "curl -H \"X-Secret: {S}\" https://x"),
    ("header", "alnum32", "Cookie: session={S}; theme=dark"),
    // connection strings
    ("conn", "pw16", "postgres://app:{S}@db.internal:5432/app"),
    ("conn", "pw16", "mysql://root:{S}@127.0.0.1:3306/db"),
    (
        "conn",
        "pw16",
        "mongodb+srv://admin:{S}@cluster0.abc.mongodb.net/test",
    ),
    ("conn", "pw16", "redis://:{S}@cache:6379/0"),
    ("conn", "pw16", "rediss://default:{S}@cache:6380"),
    ("conn", "pw16", "amqp://guest:{S}@rabbit:5672/vhost"),
    (
        "conn",
        "pw16",
        "Server=db;Database=app;User Id=sa;Password={S};",
    ),
    ("conn", "pw16", "host=db user=app password={S} dbname=app"),
    (
        "conn",
        "b64_88",
        "DefaultEndpointsProtocol=https;AccountName=acct;AccountKey={S};EndpointSuffix=core.windows.net",
    ),
    ("conn", "pw16", "DATABASE_URL=postgres://app:{S}@db/app"),
    // git remotes
    (
        "git",
        "ghp",
        "git remote add origin https://{S}@github.com/o/r.git",
    ),
    (
        "git",
        "ghs",
        "https://x-access-token:{S}@github.com/o/r.git",
    ),
    ("git", "glpat", "https://oauth2:{S}@gitlab.com/o/r.git"),
    ("git", "alnum32", "https://user:{S}@bitbucket.org/o/r.git"),
    ("git", "hex40", "git clone https://{S}@github.com/o/r.git"),
    (
        "git",
        "alnum32",
        "git clone https://{S}@dev.azure.com/o/p/_git/r",
    ),
    // aws
    ("aws", "aws_id", "aws_access_key_id = {S}"),
    ("aws", "b64_40", "aws_secret_access_key = {S}"),
    ("aws", "b64_88", "aws_session_token = {S}"),
    ("aws", "b64_40", "AWS_SECRET_ACCESS_KEY={S}"),
    // npmrc
    ("npmrc", "npm", "//registry.npmjs.org/:_authToken={S}"),
    ("npmrc", "uuid", "//registry.npmjs.org/:_authToken={S}"),
    ("npmrc", "b64_40", "_auth={S}"),
    ("npmrc", "b64_40", "//npm.pkg.github.com/:_password={S}"),
    // pip
    (
        "pip",
        "pw16",
        "index-url = https://user:{S}@pypi.example.com/simple",
    ),
    (
        "pip",
        "pw16",
        "extra-index-url=https://__token__:{S}@pypi.example.com/simple",
    ),
    // netrc
    (
        "netrc",
        "pw16",
        "machine api.example.com login deploy password {S}",
    ),
    (
        "netrc",
        "pw20",
        "machine github.com\n  login bot\n  password {S}",
    ),
    ("netrc", "ghp", "machine github.com login bot password {S}"),
    // GitHub Actions logs
    ("gha", "ghs", "  GITHUB_TOKEN: {S}"),
    ("gha", "alnum32", "Run echo \"token={S}\""),
    (
        "gha",
        "alnum32",
        "##[debug]Evaluating: secrets.DEPLOY_KEY => {S}",
    ),
    // tfvars
    ("tfvars", "pw16", "db_password = \"{S}\""),
    ("tfvars", "alnum32", "cloudflare_api_token = \"{S}\""),
    ("tfvars", "hex32", "datadog_app_key = \"{S}\""),
    ("tfvars", "hex64", "encryption_key = \"{S}\""),
    // source
    ("py", "alnum32", "openai.api_key = \"{S}\""),
    ("py", "alnum32", "client = Client(api_key=\"{S}\")"),
    (
        "py",
        "pw16",
        "conn = psycopg2.connect(host=\"db\", password=\"{S}\")",
    ),
    ("py", "hex64", "SECRET_KEY = '{S}'"),
    ("py", "hex64", "FERNET_KEY = b'{S}'"),
    ("py", "sktest", "stripe.api_key = \"{S}\""),
    (
        "py",
        "alnum32",
        "headers = {\"Authorization\": \"Bearer {S}\"}",
    ),
    ("js", "alnum32", "const apiKey = \"{S}\";"),
    ("js", "hex32", "const KEY = '{S}';"),
    ("js", "sktest", "const stripe = require('stripe')('{S}');"),
    ("js", "pw16", "  auth: { user: 'mailer', pass: '{S}' },"),
    (
        "js",
        "alnum32",
        "fetch(url, { headers: { 'x-api-key': '{S}' } })",
    ),
    ("js", "xoxb", "const slack = new WebClient('{S}');"),
    ("js", "hex64", "const secret = Buffer.from('{S}', 'hex');"),
    // second review harness (fuzz.rs)
    (
        "azure",
        "low52",
        "git remote add origin https://{S}@dev.azure.com/o/p/_git/r",
    ),
    ("azure", "low52", "AZURE_DEVOPS_EXT_PAT={S}"),
    ("k8s", "hex64", "- name: DB_PASSWORD\n  value: {S}"),
    ("yaml", "phrase", "password: {S}"),
    ("pgpass", "alnum16", "db.internal:5432:app:app:{S}"),
];

/// More shapes per fix: short CLI flags, basic auth, netrc, `*_KEY`/`*_PAT`
/// names, YAML lists, pgpass, token-only URL userinfo, Stripe test keys.
const EXTRA: &[(&str, &str, &str)] = &[
    ("azure", "low52", "export GH_PAT={S}"),
    ("azure", "low52", "token is {S} for the pipeline"),
    (
        "k8s",
        "hex32",
        "env:\n  - name: \"API_KEY\"\n    value: \"{S}\"",
    ),
    ("yaml", "phrase", "  - passphrase: {S}"),
    ("pgpass", "pw16", "*:*:*:replicator:{S}"),
    ("url", "alnum32", "https://{S}@example.com/repo.git"),
    (
        "shell",
        "pw16",
        "mysqldump -h db -u root -p{S} app > dump.sql",
    ),
    ("shell", "pw16", "mariadb -uroot -p'{S}' app"),
    (
        "shell",
        "pw16",
        "podman login -u bot --password {S} quay.io",
    ),
    ("shell", "pw16", "helm registry login reg.io -u bot -p {S}"),
    ("shell", "pw16", "mongosh --host db -u admin -p {S}"),
    ("shell", "pw16", "mongo -u admin -p {S} admin"),
    ("shell", "pw16", "wget --user=bot --password={S} https://x"),
    ("shell", "pw16", "curl -u 'admin:{S}' https://x"),
    ("netrc", "pw16", "default login anonymous password {S}"),
    (
        "netrc",
        "pw16",
        "machine ftp.example.com\nlogin bot\npasswd {S}",
    ),
    ("env", "hex32", "STRIPE_WEBHOOK_SALT={S}"),
    ("env", "hex32", "ENCRYPTION={S}"),
    ("env", "alnum16", "PUSHER_APP_KEY={S}"),
    ("js", "hex64", "const sessionKey = '{S}';"),
    ("js", "hex64", "const key = Buffer.from('{S}', 'hex');"),
    ("stripe", "sktest", "charge with {S} now"),
    ("stripe", "rktest", "restricted {S} too"),
];

const TRIALS: u64 = 20;

/// True when any 8-char substring of `secret` survives in `out`.
fn leaks(out: &str, secret: &str) -> bool {
    let chars: Vec<char> = secret.chars().collect();
    chars.len() >= 8
        && chars
            .windows(8)
            .any(|w| out.contains(&w.iter().collect::<String>()))
}

/// Runs one template table; returns (passing templates, failure messages).
fn run(table: &[(&str, &str, &str)], salt: u64) -> (usize, Vec<String>) {
    let mut failures = Vec::new();
    for (i, &(fmt, kind, tpl)) in table.iter().enumerate() {
        let mut leaked = 0;
        let mut example = String::new();
        for trial in 0..TRIALS {
            let mut r = Rng(salt ^ (u64::try_from(i).unwrap() << 8) ^ trial);
            let secret = make(&mut r, kind);
            let text = tpl.replace("{S}", &secret);
            let out = redact(&text);
            assert_eq!(redact(&out.text).text, out.text, "not idempotent: {text:?}");
            if leaks(&out.text, &secret) {
                leaked += 1;
                if example.is_empty() {
                    example = out.text.replace(&secret, "<SECRET>");
                }
            }
        }
        if leaked > 0 {
            failures.push(format!(
                "[{fmt}/{kind}] {leaked}/{TRIALS} leaked  tpl={tpl:?}  out={example:?}"
            ));
        }
    }
    (table.len() - failures.len(), failures)
}

#[test]
fn realistic_templates_leak_nothing() {
    assert_eq!(CASES.len(), 108);
    let (pass, failures) = run(CASES, 0x5EED_0001);
    let (extra_pass, extra_failures) = run(EXTRA, 0x5EED_0002);
    println!(
        "corpus: {pass}/{} templates, extra: {extra_pass}/{}",
        CASES.len(),
        EXTRA.len()
    );
    let all: Vec<String> = failures.into_iter().chain(extra_failures).collect();
    assert!(all.is_empty(), "{} leaking:\n{}", all.len(), all.join("\n"));
}

/// Text that must come back unchanged: anchored CLI rules, `*_key` names
/// that are not secrets, and netrc-like prose.
fn negatives() -> Vec<String> {
    vec![
        "ls -p /tmp".into(),
        "ls -la -p src/redact".into(),
        "grep -a pattern file.bin".into(),
        "grep -rna needle_in_haystack src".into(),
        "docker run -p 8080:80 -p 127.0.0.1:9443:443/tcp nginx".into(),
        "docker exec -u 1000:1000 app sh".into(),
        "docker run -u www-data:www-data app".into(),
        "psql -h db -p 5432 -U app appdb".into(),
        "mysql -u root -p appdb".into(),
        "redis-cli -h cache ping".into(),
        "primary_key: customer_identifier".into(),
        "sort_key = created_at_descending".into(),
        "partition_key: tenant_identifier".into(),
        "foreign_key = orders.customer_id".into(),
        "cache_key = \"user:profile:avatar\"".into(),
        "idempotency_key: order-creation-request".into(),
        "s3_key: uploads/avatars/profile.png".into(),
        "object_key = backups/nightly/archive.tar".into(),
        "ssh_public_key: ~/.ssh/id_ed25519.pub".into(),
        "gpg_public_key = keys/release-signing.asc".into(),
        "map_key = location_identifier".into(),
        "unique_key: email_and_tenant".into(),
        "lookup_key = normalized_email".into(),
        "composite_key: tenant_and_user".into(),
        "hot_key: ctrl+shift+p".into(),
        "the password reset flow is broken again".into(),
        "password reset emails are delayed".into(),
        "machine learning models are trained nightly".into(),
        "compat = legacy-adapter-version".into(),
        "src/main.rs:12:5: warning: unused variable".into(),
    ]
}

#[test]
fn negatives_stay_untouched() {
    for text in negatives() {
        let r = redact(&text);
        assert!(r.is_clean(), "{text:?} -> {:?}", r.text);
    }
}

/// The review's idempotence repro, built from pieces.
#[test]
fn review_idempotence_repro() {
    let pem = ["-----BEGIN ", "RSA ", "PRIVATE KEY-----"].concat();
    let v = "Qx7Lm2Pz9Rt4Wk8N";
    for text in [
        format!("password=-\"=Y\\Z1_@a9a0b:b{pem}@/"),
        format!("token: -=@X:{v}"),
        format!("password={pem}abc"),
        format!("password=zz{pem}abc"),
        format!("password={v}@/"),
        format!("password={}@{pem}x", ["a", "bc"].concat()),
        format!("token: ab{pem}"),
        // A marker swallowing the quote that bounded a header value.
        format!(
            "_Y/@'Cookie: {pem}-.''.\\}}++/@日:日'c=.+\"/1@@\\é1Y+0> c日:@bX//aZ91=é+1é日X/\\日\n"
        ),
    ] {
        let once = redact(&text).text.into_owned();
        assert_eq!(redact(&once).text, once, "{text:?}");
    }
}

/// Seeded fuzz over credential-shaped fragments and random bytes: offsets
/// are valid and a second pass changes nothing.
#[test]
fn seeded_fuzz_is_idempotent() {
    let pem = ["-----BEGIN ", "RSA ", "PRIVATE KEY-----"].concat();
    let gh = ["gh", "p_"].concat();
    let pieces: Vec<&str> = vec![
        "password=",
        "token: ",
        "Authorization: ",
        "Cookie: ",
        "://",
        "@",
        ":",
        "\\\"",
        "\\n",
        "\"",
        "'",
        "`",
        "é",
        "日本",
        "🔑",
        "\u{200b}",
        "\r\n",
        " ",
        ";",
        "=",
        &pem,
        "[REDACTED:x]",
        "{",
        "}",
        "(",
        ")",
        "<",
        ">",
        "sk-",
        &gh,
        "AKIA",
        "eyJ",
        "$",
        "${",
        "%",
        "--password ",
        "api_key",
        "\\",
        "\t",
        ",",
        "secret_key",
        "machine ",
        " password ",
        "mysql -p",
        "redis-cli -a ",
        "curl -u a:",
        "- name: DB_PASSWORD\n  value: ",
        "h:5432:d:u:",
        "https://",
        "MASTER_KEY=",
        "GH_PAT=",
    ];
    let alpha: Vec<char> = "abcXYZ019+/_-=.é日🔑\\\"' :@\n".chars().collect();
    let mut r = Rng(0x1D3_0001);
    let check = |s: &str| {
        let red = redact(s);
        for f in &red.findings {
            assert!(s.is_char_boundary(f.start) && s.is_char_boundary(f.end) && f.start < f.end);
        }
        assert!(red.findings.windows(2).all(|w| w[0].end <= w[1].start));
        assert_eq!(redact(&red.text).text, red.text, "input {s:?}");
    };
    for _ in 0..20000 {
        let mut s = String::new();
        for _ in 0..r.below(40) {
            if r.below(3) == 0 {
                s.push_str(pieces[r.below(pieces.len())]);
            } else {
                for _ in 0..r.below(30) {
                    s.push(alpha[r.below(alpha.len())]);
                }
            }
        }
        check(&s);
    }
    for _ in 0..1000 {
        let len = r.below(200);
        let bytes: Vec<u8> = (0..len).map(|_| r.next().to_le_bytes()[0]).collect();
        check(&String::from_utf8_lossy(&bytes));
    }
}
