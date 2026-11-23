//! Pure security checks for the dashboard: tokens, Host/Origin/CSRF rules,
//! response headers and text sanitizing. No I/O except reading OS randomness.

use std::fmt::Write as _;
use std::io::{self, Read};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::server::{Request, Response};
use crate::pack;

/// Session cookie name for a server on `port`. Cookies are shared across
/// ports of the same host name, so each run uses its own name.
#[must_use]
pub fn cookie_name(port: u16) -> String {
    format!("recalla_{port}")
}

/// Default idle time after which the session ends.
pub const DEFAULT_SESSION_IDLE: Duration = Duration::from_secs(12 * 60 * 60);
/// Header that must carry the per-run CSRF value on POST.
pub const CSRF_HEADER: &str = "x-recalla-csrf";

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// 32 random bytes from the OS, hex-encoded (64 chars).
///
/// # Errors
/// `/dev/urandom` unavailable (non-unix platforms are unsupported).
pub fn random_hex() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// A secret random value (session token or CSRF value).
#[derive(Debug, Clone)]
pub struct Token(String);

impl Token {
    /// # Errors
    /// See [`random_hex`].
    pub fn generate() -> io::Result<Self> {
        random_hex().map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time comparison with a candidate.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        ct_eq(self.0.as_bytes(), candidate.as_bytes())
    }
}

/// Constant-time equality (length is not secret).
#[must_use]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// How long the printed login link works.
pub const LOGIN_TTL: Duration = Duration::from_secs(60);

/// A one-time login code: valid for a short time and for exactly one
/// successful use. Wrong, used and expired codes are indistinguishable to the
/// caller (all just `false`).
#[derive(Debug)]
pub struct LoginCode {
    value: String,
    /// `Some(deadline)` until redeemed.
    unused_until: Mutex<Option<Instant>>,
}

impl LoginCode {
    /// A fresh random code that works for `ttl` from now.
    ///
    /// # Errors
    /// See [`random_hex`].
    pub fn generate(ttl: Duration) -> io::Result<Self> {
        Ok(Self {
            value: random_hex()?,
            unused_until: Mutex::new(Some(Instant::now() + ttl)),
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// True once the code was redeemed or its time ran out.
    #[must_use]
    pub fn is_spent(&self) -> bool {
        let slot = self
            .unused_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        slot.is_none_or(|deadline| Instant::now() >= deadline)
    }

    /// Consumes the code if `candidate` matches and it is unused and unexpired.
    #[must_use]
    pub fn redeem(&self, candidate: &str) -> bool {
        let matches = ct_eq(self.value.as_bytes(), candidate.as_bytes());
        let mut slot = self
            .unused_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if matches && slot.is_some_and(|deadline| Instant::now() < deadline) {
            *slot = None;
            return true;
        }
        false
    }
}

/// Accepts exactly `127.0.0.1:<port>` or `localhost:<port>` (host case-insensitive).
#[must_use]
pub fn check_host(host: Option<&str>, port: u16) -> bool {
    host.is_some_and(|h| {
        let h = h.to_ascii_lowercase();
        h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
    })
}

/// Accepts exactly `http://127.0.0.1:<port>` or `http://localhost:<port>`.
#[must_use]
pub fn check_origin(origin: Option<&str>, port: u16) -> bool {
    origin.is_some_and(|o| {
        let o = o.to_ascii_lowercase();
        o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")
    })
}

/// The per-run secrets and port the request checks need.
#[derive(Debug, Clone)]
pub struct Guard {
    pub token: Token,
    pub csrf: Token,
    pub code: Arc<LoginCode>,
    pub port: u16,
    /// The session ends after this long without an authenticated request.
    pub idle: Duration,
    last_seen: Arc<Mutex<Instant>>,
}

impl Guard {
    #[must_use]
    pub fn new(token: Token, csrf: Token, code: Arc<LoginCode>, port: u16) -> Self {
        Self {
            token,
            csrf,
            code,
            port,
            idle: DEFAULT_SESSION_IDLE,
            last_seen: Arc::new(Mutex::new(Instant::now())),
        }
    }

    #[must_use]
    pub fn host_ok(&self, req: &Request) -> bool {
        check_host(req.header("host"), self.port)
    }

    /// Valid session cookie on a session that has not idled out; refreshes
    /// the idle timer. The token never appears anywhere else.
    #[must_use]
    pub fn authenticated(&self, req: &Request) -> bool {
        if !req
            .cookie(&cookie_name(self.port))
            .is_some_and(|c| self.token.matches(c))
        {
            return false;
        }
        let mut last = self
            .last_seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if now.duration_since(*last) > self.idle {
            return false;
        }
        *last = now;
        true
    }

    /// POST only: matching `Origin` and CSRF header.
    #[must_use]
    pub fn mutation_ok(&self, req: &Request) -> bool {
        check_origin(req.header("origin"), self.port)
            && req
                .header(CSRF_HEADER)
                .is_some_and(|c| self.csrf.matches(c))
    }

    #[must_use]
    pub fn set_cookie_value(&self) -> String {
        format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/",
            cookie_name(self.port),
            self.token.as_str()
        )
    }
}

/// Sets the headers every response carries. Never adds CORS headers.
pub fn apply_headers(resp: &mut Response) {
    for (k, v) in [
        ("Content-Security-Policy", CSP),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        ("X-Frame-Options", "DENY"),
        ("Cross-Origin-Resource-Policy", "same-origin"),
        ("Cache-Control", "no-store"),
        ("Permissions-Policy", "interest-cohort=()"),
    ] {
        resp.set_header(k, v);
    }
}

/// Single-line text with control and invisible characters removed.
#[must_use]
pub fn sanitize(s: &str) -> String {
    pack::one_line(s, usize::MAX)
}

/// Like [`sanitize`] but keeps newlines and tabs.
#[must_use]
pub fn sanitize_multiline(s: &str) -> String {
    s.chars()
        .filter(|c| matches!(c, '\n' | '\t') || !(c.is_control() || pack::is_invisible(*c)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::server::Method;

    #[test]
    fn ct_eq_cases() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(ct_eq(b"", b""));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }

    #[test]
    fn host_rules() {
        assert!(check_host(Some("127.0.0.1:8080"), 8080));
        assert!(check_host(Some("LocalHost:8080"), 8080));
        for bad in [
            None,
            Some(""),
            Some("127.0.0.1"),
            Some("127.0.0.1:1"),
            Some("evil.com"),
            Some("evil.com:8080"),
            Some("localhost.evil.com:8080"),
            Some("localhost.:8080"),
            Some("127.0.0.1:8080.evil.com"),
            Some("127.0.0.1:08080"),
            Some("0.0.0.0:8080"),
            Some("[::1]:8080"),
        ] {
            assert!(!check_host(bad, 8080), "{bad:?}");
        }
    }

    #[test]
    fn origin_rules() {
        assert!(check_origin(Some("http://127.0.0.1:9"), 9));
        assert!(check_origin(Some("http://localhost:9"), 9));
        for bad in [
            None,
            Some("null"),
            Some("https://127.0.0.1:9"),
            Some("http://evil.com"),
            Some("http://127.0.0.1:9/"),
            Some("http://127.0.0.1:8"),
        ] {
            assert!(!check_origin(bad, 9), "{bad:?}");
        }
    }

    #[test]
    fn token_shape_and_uniqueness() {
        let (a, b) = (Token::generate().unwrap(), Token::generate().unwrap());
        assert_eq!(a.as_str().len(), 64);
        assert!(
            a.as_str()
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
        assert_ne!(a.as_str(), b.as_str());
        assert!(a.matches(a.as_str()));
        assert!(!a.matches(b.as_str()));
        assert!(!a.matches(""));
    }

    fn req(headers: &[(&str, &str)], cookie: Option<&str>) -> Request {
        Request {
            method: Method::Post,
            path: "/api/x".into(),
            query: vec![],
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            body: vec![],
            cookies: cookie
                .map(|c| vec![(cookie_name(7), c.into())])
                .unwrap_or_default(),
        }
    }

    fn guard() -> Guard {
        Guard::new(
            Token::generate().unwrap(),
            Token::generate().unwrap(),
            Arc::new(LoginCode::generate(LOGIN_TTL).unwrap()),
            7,
        )
    }

    #[test]
    fn cookie_is_named_per_port_and_other_ports_do_not_match() {
        let g = guard();
        assert_eq!(cookie_name(7), "recalla_7");
        assert!(g.set_cookie_value().starts_with("recalla_7="));
        assert!(!g.set_cookie_value().contains("Secure"));
        let t = g.token.as_str().to_owned();
        let mut other = req(&[], Some(&t));
        other.cookies = vec![(cookie_name(8), t.clone()), ("recalla_session".into(), t)];
        assert!(!g.authenticated(&other));
    }

    #[test]
    fn session_idles_out_but_activity_keeps_it() {
        let mut g = guard();
        let t = g.token.as_str().to_owned();
        g.idle = Duration::from_millis(300);
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(150));
            assert!(g.authenticated(&req(&[], Some(&t))), "active session");
        }
        std::thread::sleep(Duration::from_millis(450));
        assert!(!g.authenticated(&req(&[], Some(&t))), "idle session");
        assert!(!g.authenticated(&req(&[], Some(&t))), "stays ended");
    }

    #[test]
    fn guard_auth_and_mutation() {
        let g = guard();
        let t = g.token.as_str().to_owned();
        assert!(g.authenticated(&req(&[], Some(&t))));
        assert!(!g.authenticated(&req(&[("authorization", &format!("Bearer {t}"))], None)));
        assert!(!g.authenticated(&req(&[], None)));
        assert!(!g.authenticated(&req(&[], Some(&format!("{}x", &t[..63])))));
        let c = g.csrf.as_str().to_owned();
        let good = [("origin", "http://127.0.0.1:7"), (CSRF_HEADER, c.as_str())];
        assert!(g.mutation_ok(&req(&good, None)));
        assert!(!g.mutation_ok(&req(&good[..1], None)));
        assert!(!g.mutation_ok(&req(&good[1..], None)));
    }

    #[test]
    fn login_code_is_single_use_and_expires() {
        let c = LoginCode::generate(LOGIN_TTL).unwrap();
        let v = c.as_str().to_owned();
        assert_eq!(v.len(), 64);
        assert!(!c.redeem("wrong"));
        assert!(!c.redeem(""));
        assert!(c.redeem(&v), "wrong tries must not burn the code");
        assert!(!c.redeem(&v), "second use");
        let expired = LoginCode::generate(Duration::ZERO).unwrap();
        assert!(!expired.redeem(expired.as_str()));
        let (a, b) = (
            LoginCode::generate(LOGIN_TTL).unwrap(),
            LoginCode::generate(LOGIN_TTL).unwrap(),
        );
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn login_code_redeems_once_under_contention() {
        let c = Arc::new(LoginCode::generate(LOGIN_TTL).unwrap());
        let wins: usize = (0..16)
            .map(|_| {
                let c = Arc::clone(&c);
                std::thread::spawn(move || c.redeem(c.as_str()))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum();
        assert_eq!(wins, 1);
    }

    #[test]
    fn headers_applied_without_cors() {
        let mut r = Response::text(200, "x");
        apply_headers(&mut r);
        assert!(
            r.headers
                .iter()
                .any(|(k, v)| k == "X-Frame-Options" && v == "DENY")
        );
        assert!(
            !r.headers
                .iter()
                .any(|(k, _)| k.to_ascii_lowercase().starts_with("access-control"))
        );
    }

    #[test]
    fn sanitize_strips_hidden_and_control() {
        assert_eq!(sanitize("a\u{200b}b\x1b[31m\n c"), "ab[31m c");
        assert_eq!(sanitize_multiline("a\u{202e}b\x07\nc\td\r"), "ab\nc\td");
    }
}
