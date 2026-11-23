//! Integration and attack tests for the dashboard server (raw TCP client, no HTTP dep).

#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

mod common;

use common::http::{self, Resp};
use recalla::ui::UiHandler;
use recalla::ui::router::{ApiResult, Ctx, Params, Route};
use recalla::ui::security::{CSRF_HEADER, LOGIN_TTL, LoginCode, Token};
use recalla::ui::server::{Handler, Method, Request, Response, ServeOptions, Server};

struct Wrapped(UiHandler);

impl Handler for Wrapped {
    fn handle(&self, req: &Request) -> Response {
        assert!(req.path != "/api/panic", "boom");
        self.0.handle(req)
    }
}

#[allow(clippy::unnecessary_wraps)] // route signature
fn echo(_: &Ctx, req: &Request, _: &Params) -> ApiResult {
    Ok(Response::text(200, &format!("echo {}", req.body.len())))
}

#[allow(clippy::unnecessary_wraps)] // route signature
fn big(_: &Ctx, _: &Request, _: &Params) -> ApiResult {
    Ok(Response::text(200, &"x".repeat(32 * 1024 * 1024)))
}

struct Fixture {
    addr: SocketAddr,
    token: String,
    code: String,
    csrf: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Fixture {
    fn start(timeout: Duration) -> Self {
        Self::start_with(timeout, LOGIN_TTL)
    }

    fn start_with(timeout: Duration, code_ttl: Duration) -> Self {
        Self::start_cfg(timeout, code_ttl, (Duration::from_secs(30), LOGIN_TTL))
    }

    fn start_cfg(
        timeout: Duration,
        code_ttl: Duration,
        (write_deadline, session_idle): (Duration, Duration),
    ) -> Self {
        let (token, csrf) = (Token::generate().unwrap(), Token::generate().unwrap());
        let code = Arc::new(LoginCode::generate(code_ttl).unwrap());
        let (t, c) = (token.as_str().to_owned(), csrf.as_str().to_owned());
        let code_text = code.as_str().to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let mut opts = ServeOptions::new(0, PathBuf::new());
        opts.stop = Some(Arc::clone(&stop));
        opts.timeout = timeout;
        opts.write_deadline = write_deadline;
        let server = Server::bind(opts).unwrap();
        let addr = server.local_addr().unwrap();
        let handler = UiHandler::new(token, csrf, code, addr.port(), PathBuf::new())
            .with_session_idle(session_idle)
            .with_route(Route {
                method: Method::Post,
                pattern: "/api/echo",
                handler: echo,
            })
            .with_route(Route {
                method: Method::Get,
                pattern: "/api/big",
                handler: big,
            });
        let thread = std::thread::spawn(move || server.run(Wrapped(handler)).unwrap());
        Self {
            addr,
            token: t,
            code: code_text,
            csrf: c,
            stop,
            thread: Some(thread),
        }
    }

    fn port(&self) -> u16 {
        self.addr.port()
    }

    fn cookie(&self) -> String {
        format!("recalla_{}={}", self.port(), self.token)
    }

    fn raw(&self, bytes: &[u8]) -> Resp {
        http::raw(self.addr, bytes)
    }

    fn get(&self, target: &str, extra: &[(&str, &str)]) -> Resp {
        http::get(self.addr, target, extra)
    }

    fn request(
        &self,
        method: &str,
        target: &str,
        host: Option<&str>,
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> Resp {
        http::request(self.addr, method, target, host, extra, body)
    }

    fn post(&self, extra: &[(&str, &str)]) -> Resp {
        self.request(
            "POST",
            "/api/echo",
            Some(&format!("127.0.0.1:{}", self.port())),
            extra,
            b"{}",
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn assert_no_leak(f: &Fixture, r: &Resp) {
    assert!(!r.body.contains(&f.token) && !r.body.contains(&f.csrf) && !r.body.contains(&f.code));
    assert!(!r.body.contains("<html") && !r.body.contains("root:"));
}

#[test]
fn binds_loopback_only() {
    let f = Fixture::start(Duration::from_secs(5));
    assert_eq!(f.addr.ip().to_string(), "127.0.0.1");
    assert_ne!(f.port(), 0);
}

#[test]
fn happy_path_code_cookie_page_assets_api() {
    let f = Fixture::start(Duration::from_secs(5));
    let r = f.get(&format!("/?c={}", f.code), &[]);
    assert_eq!(r.status, 302);
    assert_eq!(r.header("location"), Some("/"));
    let cookie = r.header("set-cookie").unwrap();
    assert!(
        cookie.starts_with(&f.cookie())
            && cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Lax")
    );
    let ck = [("Cookie", f.cookie())];
    let ck: Vec<(&str, &str)> = ck.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let page = f.get("/", &ck);
    assert_eq!(page.status, 200);
    assert!(
        page.header("content-type")
            .unwrap()
            .starts_with("text/html")
    );
    assert!(page.body.contains("Recalla"));
    let css = f.get("/assets/app.css", &ck);
    assert_eq!(css.status, 200);
    assert!(css.header("content-type").unwrap().starts_with("text/css"));
    assert_eq!(f.get("/assets/nope.css", &ck).status, 404);
    let ping = f.get("/api/ping", &ck);
    assert_eq!((ping.status, ping.body.as_str()), (200, "{\"ok\":true}"));
    let bearer = format!("Bearer {}", f.token);
    assert_eq!(
        f.get("/api/ping", &[("Authorization", &bearer)]).status,
        401,
        "bearer auth is gone"
    );
    let sess = f.get("/api/session", &ck);
    assert!(sess.body.contains(&f.csrf));
    assert_eq!(f.get("/api/unknown", &ck).status, 404);
    let head = f.request(
        "HEAD",
        "/api/ping",
        Some(&format!("127.0.0.1:{}", f.port())),
        &ck,
        b"",
    );
    assert_eq!((head.status, head.body.as_str()), (200, ""));
}

#[test]
fn attack_no_credentials_is_401() {
    let f = Fixture::start(Duration::from_secs(5));
    for p in [
        "/",
        "/assets/app.css",
        "/assets/app.js",
        "/api/ping",
        "/api/session",
    ] {
        let r = f.get(p, &[]);
        assert_eq!(r.status, 401, "{p}");
        assert_no_leak(&f, &r);
    }
}

fn flip_last(s: &str) -> String {
    let mut s = s.to_owned();
    let flipped = if s.pop() == Some('0') { '1' } else { '0' };
    s.push(flipped);
    s
}

#[test]
fn attack_wrong_code_or_token_is_401() {
    let f = Fixture::start(Duration::from_secs(5));
    for t in [
        flip_last(&f.code),
        flip_last(&f.token),
        "x".repeat(64),
        String::new(),
        f.code[..63].to_owned(),
        f.token.clone(), // the session token is not a login code
    ] {
        let r = f.get(&format!("/?c={t}"), &[]);
        assert_eq!(r.status, 401);
        assert!(r.header("set-cookie").is_none());
        assert_no_leak(&f, &r);
        if t == f.token {
            continue; // the real token is a valid cookie, just not a login code
        }
        assert_eq!(
            f.get(
                "/api/ping",
                &[("Cookie", &format!("recalla_{}={t}", f.port()))]
            )
            .status,
            401
        );
        assert_eq!(
            f.get("/api/ping", &[("Authorization", &format!("Bearer {t}"))])
                .status,
            401
        );
    }
    // The removed `?t=` login must not work, even with the real token.
    for t in [f.token.as_str(), f.code.as_str()] {
        let r = f.get(&format!("/?t={t}"), &[]);
        assert_eq!(r.status, 401);
        assert!(r.header("set-cookie").is_none());
    }
    let ok = f.get(&format!("/?c={}", f.code), &[]);
    assert_eq!(ok.status, 302, "wrong guesses must not burn the real code");
}

#[test]
fn login_code_works_once() {
    let f = Fixture::start(Duration::from_secs(5));
    let first = f.get(&format!("/?c={}", f.code), &[]);
    assert_eq!(first.status, 302);
    let cookie = http::cookie_pair(&first).unwrap();
    assert_eq!(cookie, f.cookie());
    let again = f.get(&format!("/?c={}", f.code), &[]);
    assert_eq!(again.status, 401);
    assert!(again.header("set-cookie").is_none());
    let wrong = f.get(&format!("/?c={}", flip_last(&f.code)), &[]);
    assert_eq!(
        (wrong.status, wrong.body.as_str()),
        (401, again.body.as_str())
    );
    assert_eq!(f.get("/api/ping", &[("Cookie", &cookie)]).status, 200);
}

#[test]
fn expired_login_code_is_401() {
    let f = Fixture::start_with(Duration::from_secs(5), Duration::ZERO);
    let r = f.get(&format!("/?c={}", f.code), &[]);
    assert_eq!(r.status, 401);
    assert!(r.header("set-cookie").is_none());
}

#[test]
fn login_code_races_to_exactly_one_winner() {
    let f = Arc::new(Fixture::start(Duration::from_secs(5)));
    let wins = (0..12)
        .map(|_| {
            let f = Arc::clone(&f);
            std::thread::spawn(move || f.get(&format!("/?c={}", f.code), &[]).status)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|s| *s == 302)
        .count();
    assert_eq!(wins, 1);
}

#[test]
fn attack_dns_rebinding_wrong_host() {
    let f = Fixture::start(Duration::from_secs(5));
    let ck = f.cookie();
    let hosts = [
        Some("evil.com".to_owned()),
        Some("127.0.0.1:1".to_owned()),
        Some(format!("localhost.evil.com:{}", f.port())),
        Some(format!("127.0.0.1:{}.evil.com", f.port())),
        Some("127.0.0.1".to_owned()),
        None,
    ];
    for h in hosts {
        for target in [
            format!("/?c={}", f.code),
            "/api/ping".to_owned(),
            "/assets/app.css".to_owned(),
        ] {
            let r = f.request("GET", &target, h.as_deref(), &[("Cookie", &ck)], b"");
            assert!(
                matches!(r.status, 400 | 403),
                "{h:?} {target} -> {}",
                r.status
            );
            assert_no_leak(&f, &r);
            assert!(r.header("set-cookie").is_none());
        }
    }
    let ok = f.request(
        "GET",
        "/api/ping",
        Some(&format!("LOCALHOST:{}", f.port())),
        &[("Cookie", &ck)],
        b"",
    );
    assert_eq!(ok.status, 200);
}

#[test]
fn attack_post_origin_and_csrf() {
    let f = Fixture::start(Duration::from_secs(5));
    let ck = f.cookie();
    let origin = format!("http://127.0.0.1:{}", f.port());
    let base = ("Cookie", ck.as_str());
    assert_eq!(f.post(&[]).status, 401);
    assert_eq!(
        f.post(&[base, (CSRF_HEADER, &f.csrf)]).status,
        403,
        "missing origin"
    );
    for o in [
        "null",
        "http://evil.com",
        "https://127.0.0.1:1",
        &format!("http://evil.com:{}", f.port()),
    ] {
        assert_eq!(
            f.post(&[base, ("Origin", o), (CSRF_HEADER, &f.csrf)])
                .status,
            403,
            "{o}"
        );
    }
    assert_eq!(
        f.post(&[base, ("Origin", &origin)]).status,
        403,
        "missing csrf"
    );
    assert_eq!(
        f.post(&[base, ("Origin", &origin), (CSRF_HEADER, "wrong")])
            .status,
        403
    );
    let ok = f.post(&[base, ("Origin", &origin), (CSRF_HEADER, &f.csrf)]);
    assert_eq!((ok.status, ok.body.as_str()), (200, "echo 2"));
    assert_eq!(
        f.request(
            "POST",
            "/",
            Some(&format!("127.0.0.1:{}", f.port())),
            &[base],
            b""
        )
        .status,
        405
    );
}

#[test]
fn attack_options_and_other_methods_405_no_cors() {
    let f = Fixture::start(Duration::from_secs(5));
    for m in ["OPTIONS", "PUT", "DELETE", "PATCH", "TRACE", "CONNECT"] {
        let r = f.request(
            m,
            "/api/ping",
            Some(&format!("127.0.0.1:{}", f.port())),
            &[("Origin", "http://evil.com")],
            b"",
        );
        assert_eq!(r.status, 405, "{m}");
        assert!(
            !r.headers
                .iter()
                .any(|(k, _)| k.starts_with("access-control"))
        );
    }
    assert!(
        !f.get("/api/ping", &[("Origin", "http://evil.com")])
            .headers
            .iter()
            .any(|(k, _)| k.starts_with("access-control"))
    );
}

#[test]
fn attack_path_traversal_and_bad_targets() {
    let f = Fixture::start(Duration::from_secs(5));
    let ck = f.cookie();
    let long = format!("/{}", "a".repeat(5000));
    for t in [
        "/assets/../../etc/passwd",
        "/assets/%2e%2e/x",
        "/assets/%2E%2E%2Fx",
        "/assets/..%2f..%2fetc/passwd",
        "//",
        "//etc/passwd",
        "http://127.0.0.1/",
        "/assets/%00",
        "/assets/app.css%00.png",
        "/assets/\\..\\x",
        long.as_str(),
    ] {
        let r = f.get(t, &[("Cookie", &ck)]);
        assert!(matches!(r.status, 400 | 404), "{t:?} -> {}", r.status);
        assert!(!r.body.contains("root:"));
    }
    let r = f.raw(
        format!(
            "GET /assets/app\0.css HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {ck}\r\n\r\n",
            f.port()
        )
        .as_bytes(),
    );
    assert!(matches!(r.status, 400 | 404));
}

#[test]
fn attack_request_limits() {
    let f = Fixture::start(Duration::from_secs(5));
    let host = format!("127.0.0.1:{}", f.port());
    let big = "a".repeat(20 * 1024);
    let r = f.raw(format!("GET / HTTP/1.1\r\nHost: {host}\r\nX-Big: {big}\r\n\r\n").as_bytes());
    assert!(matches!(r.status, 431 | 400), "{}", r.status);
    let ck = f.cookie();
    let body = vec![b'a'; 70 * 1024];
    let r = f.request("POST", "/api/echo", Some(&host), &[("Cookie", &ck)], &body);
    assert_eq!(r.status, 413);
    let r = f.raw(
        format!(
            "POST /api/echo HTTP/1.1\r\nHost: {host}\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"
        )
        .as_bytes(),
    );
    assert_eq!(r.status, 400);
    let r = f.raw(format!("POST /api/echo HTTP/1.1\r\nHost: {host}\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab").as_bytes());
    assert_eq!(r.status, 400);
    let r = f.raw(format!("POST /api/echo HTTP/1.1\r\nHost: {host}\r\nExpect: 100-continue\r\nContent-Length: 1\r\n\r\n").as_bytes());
    assert_eq!(r.status, 400);
    assert_eq!(
        f.raw(format!("GET / HTTP/1.0\r\nHost: {host}\r\n\r\n").as_bytes())
            .status,
        505
    );
    assert_eq!(f.raw(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n").status, 400);
    assert_eq!(
        f.get("/api/ping", &[("Cookie", &ck)]).status,
        200,
        "server still serving"
    );
}

#[test]
fn attack_slow_client_is_dropped() {
    let f = Fixture::start(Duration::from_millis(400));
    let mut s = TcpStream::connect(f.addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"GET / HTT").unwrap();
    let start = std::time::Instant::now();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    assert!(start.elapsed() < Duration::from_secs(4), "not dropped");
    assert!(buf.is_empty() || http::parse(&buf).status == 408);
    assert_eq!(f.get("/api/ping", &[("Cookie", &f.cookie())]).status, 200);
}

#[test]
fn attack_connection_cap_503() {
    let f = Fixture::start(Duration::from_secs(20));
    let mut idle: Vec<TcpStream> = (0..40)
        .map(|_| TcpStream::connect(f.addr).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(400));
    let (mut refused, mut held) = (0, 0);
    for s in &mut idle {
        s.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut buf = [0u8; 256];
        match s.read(&mut buf) {
            Ok(n) if n > 0 && String::from_utf8_lossy(&buf[..n]).contains(" 503 ") => refused += 1,
            Ok(0) => refused += 1,
            _ => held += 1,
        }
    }
    assert!(refused >= 8, "refused {refused}");
    assert!(held <= 32, "held {held}");
    drop(idle);
    std::thread::sleep(Duration::from_millis(500));
    let ck = f.cookie();
    assert_eq!(f.get("/api/ping", &[("Cookie", &ck)]).status, 200);
}

#[test]
fn handler_panic_is_500_and_server_survives() {
    let f = Fixture::start(Duration::from_secs(5));
    let ck = f.cookie();
    let r = f.get("/api/panic", &[("Cookie", &ck)]);
    assert_eq!(r.status, 500);
    assert!(!r.body.contains("boom"));
    assert_eq!(f.get("/api/ping", &[("Cookie", &ck)]).status, 200);
}

#[test]
fn every_response_has_security_headers() {
    let f = Fixture::start(Duration::from_secs(5));
    let ck = f.cookie();
    let host = format!("127.0.0.1:{}", f.port());
    let responses = [
        f.get("/", &[]),
        f.get("/", &[("Cookie", &ck)]),
        f.get(&format!("/?c={}", f.code), &[]),
        f.get("/?c=wrong", &[]),
        f.get("/assets/app.js", &[("Cookie", &ck)]),
        f.get("/api/ping", &[("Cookie", &ck)]),
        f.get("/nope", &[]),
        f.request("GET", "/", Some("evil.com"), &[], b""),
        f.request("OPTIONS", "/", Some(&host), &[], b""),
        f.raw(b"garbage\r\n\r\n"),
        f.get("/api/panic", &[("Cookie", &ck)]),
    ];
    for r in &responses {
        assert!(r.header("content-security-policy").unwrap().contains("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"));
        assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(r.header("referrer-policy"), Some("no-referrer"));
        assert_eq!(r.header("x-frame-options"), Some("DENY"));
        assert_eq!(
            r.header("cross-origin-resource-policy"),
            Some("same-origin")
        );
        assert_eq!(r.header("cache-control"), Some("no-store"));
        assert_eq!(r.header("permissions-policy"), Some("interest-cohort=()"));
        assert_eq!(r.header("connection"), Some("close"));
    }
}

#[test]
fn session_ends_after_idle_and_activity_extends_it() {
    let f = Fixture::start_cfg(
        Duration::from_secs(5),
        LOGIN_TTL,
        (Duration::from_secs(30), Duration::from_millis(600)),
    );
    let ck = f.cookie();
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(f.get("/api/ping", &[("Cookie", &ck)]).status, 200);
    }
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(f.get("/api/ping", &[("Cookie", &ck)]).status, 401);
    assert_eq!(f.get("/", &[("Cookie", &ck)]).status, 401);
}

#[test]
fn cookie_of_another_port_or_the_old_name_is_not_accepted() {
    let f = Fixture::start(Duration::from_secs(5));
    for name in [
        format!("recalla_{}", f.port() ^ 1),
        "recalla_session".into(),
    ] {
        let ck = format!("{name}={}", f.token);
        assert_eq!(f.get("/api/ping", &[("Cookie", &ck)]).status, 401, "{name}");
    }
}

#[test]
fn attack_slow_reader_is_cut_off_by_the_write_deadline() {
    let f = Fixture::start_cfg(
        Duration::from_secs(5),
        LOGIN_TTL,
        (Duration::from_millis(800), LOGIN_TTL),
    );
    let mut s = TcpStream::connect(f.addr).unwrap();
    let req = format!(
        "GET /api/big HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
        f.port(),
        f.cookie()
    );
    s.write_all(req.as_bytes()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    // A steady trickle (~2.5 MB/s): every single write makes progress, so only
    // a TOTAL deadline can stop it. The 32 MiB body would take ~13 s.
    let start = std::time::Instant::now();
    let (mut got, mut buf) = (0usize, vec![0u8; 256 * 1024]);
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(e) => panic!("server kept the connection open: {e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(start.elapsed() < Duration::from_secs(6), "never cut off");
    }
    assert!(got < 32 * 1024 * 1024, "full body sent: {got}");
    // Deadline 0.8 s plus the socket buffers still queued; far below the ~13 s a full send needs.
    assert!(
        start.elapsed() < Duration::from_secs(6),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(f.get("/api/ping", &[("Cookie", &f.cookie())]).status, 200);
}
