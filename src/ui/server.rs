//! Minimal bounded HTTP/1.1 server for the loopback dashboard (ADR D-022).
//!
//! Every connection is one request and one response (`Connection: close`),
//! parsed with `httparse`, on its own thread, with a hard cap on concurrent
//! connections. Binds `127.0.0.1` only. Stopping: the `stop` flag in
//! [`ServeOptions`] (tests); otherwise the default Ctrl-C terminates the
//! process, which is fine because the server holds no state that needs a flush.

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::security;

/// Concurrent connections served; extra ones get an immediate 503.
pub const MAX_CONNECTIONS: usize = 32;
/// Request line + headers, in bytes.
pub const MAX_HEAD: usize = 16 * 1024;
/// Request body, in bytes.
pub const MAX_BODY: usize = 64 * 1024;
/// Request target (path + query), in bytes.
pub const MAX_TARGET: usize = 2048;
/// Query pairs accepted.
pub const MAX_QUERY_PAIRS: usize = 32;
/// Default per-connection time allowed to read the whole request, and per write.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// Total time allowed to send one response (a client that reads slowly is cut off).
pub const DEFAULT_WRITE_DEADLINE: Duration = Duration::from_secs(30);
const MAX_HEADERS: usize = 64;
const WRITE_CHUNK: usize = 16 * 1024;
const DRAIN_LIMIT: usize = 256 * 1024;

/// Request methods the server accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
}

/// A parsed, validated request. Header names are lowercased; `path` is the raw
/// (still percent-encoded) path, already checked by [`validate_target`].
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub cookies: Vec<(String, String)>,
}

impl Request {
    /// First header named `name` (lowercase).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// First query value for `name`.
    #[must_use]
    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// First cookie value for `name`.
    #[must_use]
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A response. The server adds `Content-Length`, `Connection` and the
/// security headers; handlers only set what is specific to them.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    #[must_use]
    pub fn new(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".to_owned(), content_type.to_owned())],
            body,
        }
    }

    #[must_use]
    pub fn text(status: u16, body: &str) -> Self {
        Self::new(
            status,
            "text/plain; charset=utf-8",
            body.as_bytes().to_vec(),
        )
    }

    #[must_use]
    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::new(
            status,
            "application/json; charset=utf-8",
            value.to_string().into_bytes(),
        )
    }

    /// Sets `name`, replacing any existing header of that name (case-insensitive).
    pub fn set_header(&mut self, name: &str, value: &str) {
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        self.headers.push((name.to_owned(), value.to_owned()));
    }

    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.set_header(name, value);
        self
    }
}

/// Application logic behind the server. Panics are caught and become a bare 500.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, req: &Request) -> Response;
}

/// Server settings.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// 0 picks a random free port.
    pub port: u16,
    /// Data directory the handler works on (carried for the caller; the server itself never reads it).
    pub data_dir: PathBuf,
    /// Whether the caller should open a browser (the caller does, because the URL holds the token).
    pub open_browser: bool,
    /// Set to true to make [`Server::run`] return.
    pub stop: Option<Arc<AtomicBool>>,
    /// Time allowed to read one whole request.
    pub timeout: Duration,
    /// Total time allowed to write one response.
    pub write_deadline: Duration,
    /// Idle time after which the session ends (the caller passes it to the handler).
    pub session_idle: Duration,
}

impl ServeOptions {
    #[must_use]
    pub fn new(port: u16, data_dir: PathBuf) -> Self {
        Self {
            port,
            data_dir,
            open_browser: false,
            stop: None,
            timeout: DEFAULT_TIMEOUT,
            write_deadline: DEFAULT_WRITE_DEADLINE,
            session_idle: security::DEFAULT_SESSION_IDLE,
        }
    }
}

/// A bound, not yet running server.
pub struct Server {
    listener: TcpListener,
    opts: ServeOptions,
}

impl Server {
    /// Binds `127.0.0.1:<port>` (never any other address).
    ///
    /// # Errors
    /// The bind error, e.g. the port is in use.
    pub fn bind(opts: ServeOptions) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, opts.port))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, opts })
    }

    /// The address actually bound (the real port when 0 was asked for).
    ///
    /// # Errors
    /// I/O error from the OS.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accepts connections until the stop flag is set.
    ///
    /// # Errors
    /// Never in practice; accept errors are reported on stderr and retried.
    pub fn run(self, handler: impl Handler) -> io::Result<()> {
        let handler = Arc::new(handler);
        let active = Arc::new(AtomicUsize::new(0));
        let (timeout, write_deadline) = (self.opts.timeout, self.opts.write_deadline);
        loop {
            if self
                .opts
                .stop
                .as_ref()
                .is_some_and(|s| s.load(Ordering::SeqCst))
            {
                return Ok(());
            }
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(15));
                    continue;
                }
                Err(e) => {
                    eprintln!("recalla ui: accept failed: {:?}", e.kind());
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            if stream.set_nonblocking(false).is_err() {
                continue;
            }
            if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                active.fetch_sub(1, Ordering::SeqCst);
                respond_error(stream, 503, false, Duration::from_secs(1));
                continue;
            }
            let (handler, guard) = (Arc::clone(&handler), ActiveGuard(Arc::clone(&active)));
            let spawned = thread::Builder::new()
                .name("recalla-ui-conn".into())
                .spawn(move || {
                    let _guard = guard;
                    serve_connection(stream, handler.as_ref(), (timeout, write_deadline));
                });
            if let Err(e) = spawned {
                eprintln!("recalla ui: cannot spawn connection thread: {:?}", e.kind());
            }
        }
    }
}

/// Binds and runs in one call.
///
/// # Errors
/// Bind errors.
pub fn serve(opts: ServeOptions, handler: impl Handler) -> io::Result<()> {
    Server::bind(opts)?.run(handler)
}

/// Decrements the connection count when the connection thread ends.
struct ActiveGuard(Arc<AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn serve_connection(
    mut stream: TcpStream,
    handler: &dyn Handler,
    (timeout, write_deadline): (Duration, Duration),
) {
    let mut resp = match read_request(&mut stream, timeout) {
        Ok(req) => {
            let head_only = req.method == Method::Head;
            let mut resp = catch_unwind(AssertUnwindSafe(|| handler.handle(&req)))
                .unwrap_or_else(|_| Response::text(500, "Internal error\n"));
            if head_only {
                resp.set_header("Content-Length", &resp.body.len().to_string());
                resp.body.clear();
            }
            resp
        }
        Err(ReadError::Drop) => return,
        Err(ReadError::Reject(status)) => {
            return respond_error(stream, status, true, write_deadline);
        }
    };
    finish(&mut resp);
    // A failed write means the client went away; nothing to do about it.
    let _ = write_response(&mut stream, &resp, write_deadline);
    let _ = stream.shutdown(Shutdown::Both);
}

/// Sends a bodyless-ish error and closes. With `drain`, first discards a
/// bounded amount of unread input so the client sees the response, not a reset.
fn respond_error(mut stream: TcpStream, status: u16, drain: bool, budget: Duration) {
    let mut resp = Response::text(status, &format!("{status} {}\n", reason(status)));
    if status == 405 {
        resp.set_header("Allow", "GET, HEAD, POST");
    }
    finish(&mut resp);
    if write_response(&mut stream, &resp, budget).is_err() {
        return;
    }
    let _ = stream.shutdown(Shutdown::Write);
    if drain {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
        let (mut sink, mut total) = ([0u8; 4096], 0);
        while total < DRAIN_LIMIT {
            match stream.read(&mut sink) {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
        }
    }
}

/// Applies the security headers shared by every response.
fn finish(resp: &mut Response) {
    security::apply_headers(resp);
}

/// Sends `bytes` using single `write` calls, recomputing the socket timeout
/// from the remaining time before each one, so the deadline is a true total.
fn write_within(stream: &mut TcpStream, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    let mut sent = 0;
    while sent < bytes.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_write_timeout(Some(remaining))?;
        let end = bytes.len().min(sent + WRITE_CHUNK);
        match stream.write(&bytes[sent..end]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => sent += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Writes the whole response within `budget` in total: chunks are sent while
/// checking the deadline, so a client that reads one byte at a time is cut off.
fn write_response(stream: &mut TcpStream, resp: &Response, budget: Duration) -> io::Result<()> {
    let clean = |s: &str| !s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
    let fallback;
    let resp = if resp.headers.iter().all(|(k, v)| clean(k) && clean(v)) {
        resp
    } else {
        // A handler bug must never become header injection.
        let mut r = Response::text(500, "Internal error\n");
        security::apply_headers(&mut r);
        fallback = r;
        &fallback
    };
    let declared = resp
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .map_or_else(|| resp.body.len().to_string(), |(_, v)| v.clone());
    let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, reason(resp.status));
    for (k, v) in &resp.headers {
        if !k.eq_ignore_ascii_case("content-length") && !k.eq_ignore_ascii_case("connection") {
            let _ = write!(out, "{k}: {v}\r\n");
        }
    }
    let _ = write!(
        out,
        "Content-Length: {declared}\r\nConnection: close\r\n\r\n"
    );
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&resp.body);
    write_within(stream, &bytes, Instant::now() + budget)?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Error",
    }
}

enum ReadError {
    /// Close without answering (client vanished before sending anything).
    Drop,
    /// Answer with this status, then close.
    Reject(u16),
}

struct Head {
    method: Method,
    target: String,
    headers: Vec<(String, String)>,
}

fn read_request(stream: &mut TcpStream, timeout: Duration) -> Result<Request, ReadError> {
    let deadline = Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let (head, head_len) = loop {
        if let Some(parsed) = parse_head(&buf).map_err(ReadError::Reject)? {
            break parsed;
        }
        if buf.len() >= MAX_HEAD {
            return Err(ReadError::Reject(431));
        }
        read_some(stream, &mut buf, deadline)?;
    };
    if head_len > MAX_HEAD {
        return Err(ReadError::Reject(431));
    }
    let body_len = body_length(&head).map_err(ReadError::Reject)?;
    let mut body = buf.split_off(head_len);
    body.truncate(body_len);
    while body.len() < body_len {
        let mut chunk = Vec::new();
        read_some(stream, &mut chunk, deadline)?;
        body.extend_from_slice(&chunk);
        body.truncate(body_len);
    }
    let (path, query) = validate_target(&head.target).ok_or(ReadError::Reject(400))?;
    let cookies = parse_cookies(&head.headers);
    Ok(Request {
        method: head.method,
        path,
        query,
        headers: head.headers,
        body,
        cookies,
    })
}

/// Reads one chunk onto `buf`, honouring the overall request deadline.
fn read_some(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
    deadline: Instant,
) -> Result<(), ReadError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(ReadError::Reject(408));
    }
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|_| ReadError::Drop)?;
    let mut chunk = [0u8; 4096];
    match stream.read(&mut chunk) {
        Ok(0) if buf.is_empty() => Err(ReadError::Drop),
        Ok(0) => Err(ReadError::Reject(400)),
        Ok(n) => {
            buf.extend_from_slice(&chunk[..n]);
            Ok(())
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Err(ReadError::Reject(408))
        }
        Err(_) => Err(ReadError::Drop),
    }
}

/// `Ok(None)` while the head is incomplete.
fn parse_head(buf: &[u8]) -> Result<Option<(Head, usize)>, u16> {
    let mut slots = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut slots);
    let len = match req.parse(buf) {
        Ok(httparse::Status::Complete(n)) => n,
        Ok(httparse::Status::Partial) => return Ok(None),
        Err(httparse::Error::TooManyHeaders) => return Err(431),
        Err(_) => return Err(400),
    };
    if req.version != Some(1) {
        return Err(505);
    }
    let method = match req.method {
        Some("GET") => Method::Get,
        Some("HEAD") => Method::Head,
        Some("POST") => Method::Post,
        _ => return Err(405),
    };
    let mut headers = Vec::with_capacity(req.headers.len());
    for h in req.headers.iter() {
        let value = std::str::from_utf8(h.value).map_err(|_| 400u16)?;
        headers.push((h.name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    let count = |name: &str| headers.iter().filter(|(k, _)| k == name).count();
    let forbidden = ["transfer-encoding", "expect", "upgrade"];
    if forbidden.iter().any(|n| count(n) > 0) || count("host") > 1 || count("content-length") > 1 {
        return Err(400);
    }
    let target = req.path.ok_or(400u16)?.to_owned();
    Ok(Some((
        Head {
            method,
            target,
            headers,
        },
        len,
    )))
}

/// Declared body length: only POST may carry one.
fn body_length(head: &Head) -> Result<usize, u16> {
    let Some((_, raw)) = head.headers.iter().find(|(k, _)| k == "content-length") else {
        return Ok(0);
    };
    if raw.is_empty() || raw.len() > 10 || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(400);
    }
    let len: usize = raw.parse().map_err(|_| 400u16)?;
    if len > MAX_BODY {
        return Err(413);
    }
    if len > 0 && head.method != Method::Post {
        return Err(400);
    }
    Ok(len)
}

fn parse_cookies(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(k, _)| k == "cookie")
        .flat_map(|(_, v)| v.split(';'))
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((k.trim().to_owned(), v.trim().to_owned()))
        })
        .collect()
}

/// Checks a request target and splits it into `(raw path, decoded query)`.
///
/// Rejects anything but origin-form: no absolute-form, `//` prefixes or empty
/// segments, `.`/`..` segments (also percent-encoded), backslashes, fragments,
/// control or non-ASCII bytes, encoded `/`, `\` or NUL in a segment, bad
/// UTF-8, over-long targets and oversized queries.
#[must_use]
pub fn validate_target(target: &str) -> Option<(String, Vec<(String, String)>)> {
    if target.len() > MAX_TARGET
        || !target.starts_with('/')
        || !target
            .bytes()
            .all(|b| (0x21..0x7f).contains(&b) && !matches!(b, b'\\' | b'#'))
    {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" {
        for seg in path[1..].split('/') {
            let decoded = percent_decode(seg, false)?;
            let bad = decoded.is_empty()
                || decoded == "."
                || decoded == ".."
                || decoded
                    .chars()
                    .any(|c| c.is_control() || c == '/' || c == '\\');
            if bad {
                return None;
            }
        }
    }
    Some((path.to_owned(), parse_query(query)?))
}

/// Parses `a=b&c=d`; `+` is a space. `None` on invalid escapes or UTF-8, control
/// characters, or more than [`MAX_QUERY_PAIRS`] pairs.
#[must_use]
pub fn parse_query(query: &str) -> Option<Vec<(String, String)>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let (k, v) = (percent_decode(k, true)?, percent_decode(v, true)?);
        if out.len() == MAX_QUERY_PAIRS || k.chars().chain(v.chars()).any(char::is_control) {
            return None;
        }
        out.push((k, v));
    }
    Some(out)
}

/// Percent-decodes `s`. `None` on a malformed escape or invalid UTF-8.
#[must_use]
pub fn percent_decode(s: &str, plus_is_space: bool) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                let hi = char::from(hex[0]).to_digit(16)?;
                let lo = char::from(hex[1]).to_digit(16)?;
                out.push(u8::try_from(hi * 16 + lo).ok()?);
                i += 3;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> Option<Vec<(String, String)>> {
        parse_query(s)
    }

    #[test]
    fn query_decodes_percent_and_plus() {
        assert_eq!(
            q("a=b%20c&d=e+f&g").unwrap(),
            vec![
                ("a".into(), "b c".into()),
                ("d".into(), "e f".into()),
                ("g".into(), String::new())
            ]
        );
    }

    #[test]
    fn query_rejects_bad_escapes_utf8_controls_and_excess() {
        assert!(q("a=%zz").is_none());
        assert!(q("a=%4").is_none());
        assert!(q("a=%ff").is_none());
        assert!(q("a=%c3%28").is_none());
        assert!(q("a=%00").is_none());
        assert!(q("a=%0a").is_none());
        let many = (0..=MAX_QUERY_PAIRS)
            .map(|i| format!("k{i}=v"))
            .collect::<Vec<_>>();
        assert!(q(&many.join("&")).is_none());
        assert_eq!(
            q(&many[..MAX_QUERY_PAIRS].join("&")).unwrap().len(),
            MAX_QUERY_PAIRS
        );
    }

    #[test]
    fn query_accepts_valid_multibyte() {
        assert_eq!(q("n=%C3%A9").unwrap()[0].1, "é");
    }

    #[test]
    fn target_accepts_plain_paths() {
        assert_eq!(validate_target("/").unwrap().0, "/");
        assert_eq!(
            validate_target("/api/memories/ab12?x=1").unwrap().1.len(),
            1
        );
        assert!(validate_target("/assets/app.css").is_some());
        assert!(validate_target("/api/s/a%20b").is_some());
    }

    #[test]
    fn target_rejects_hostile_shapes() {
        for bad in [
            "",
            "*",
            "api",
            "//",
            "//evil.com/x",
            "http://127.0.0.1/",
            "/a//b",
            "/a/",
            "/../x",
            "/a/./b",
            "/a/..",
            "/assets/%2e%2e/x",
            "/assets/%2E%2E/x",
            "/a%2fb",
            "/a%2Fb",
            "/a%5cb",
            "/a%00",
            "/a\\b",
            "/a#frag",
            "/a b",
            "/a\x01b",
            "/\u{e9}",
            "/a%zz",
            "/a%c3%28",
            "/a?x=%zz",
        ] {
            assert!(validate_target(bad).is_none(), "accepted {bad:?}");
        }
        assert!(validate_target(&format!("/{}", "a".repeat(MAX_TARGET))).is_none());
    }

    #[test]
    fn head_parsing_enforces_rules() {
        let ok = b"GET /x HTTP/1.1\r\nHost: a\r\n\r\n";
        assert!(parse_head(ok).unwrap().is_some());
        assert!(parse_head(b"GET /x HTTP/1.1\r\nHo").unwrap().is_none());
        assert_eq!(parse_head(b"GET /x HTTP/1.0\r\n\r\n").err(), Some(505));
        assert_eq!(parse_head(b"PUT /x HTTP/1.1\r\n\r\n").err(), Some(405));
        assert_eq!(parse_head(b"OPTIONS /x HTTP/1.1\r\n\r\n").err(), Some(405));
        assert_eq!(parse_head(b"PRI * HTTP/2.0\r\n\r\n").err(), Some(400));
        for bad in [
            "Transfer-Encoding: chunked",
            "Expect: 100-continue",
            "Upgrade: websocket",
        ] {
            let raw = format!("POST /x HTTP/1.1\r\n{bad}\r\n\r\n");
            assert_eq!(parse_head(raw.as_bytes()).err(), Some(400), "{bad}");
        }
        let dup = b"POST /x HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\n";
        assert_eq!(parse_head(dup).err(), Some(400));
        let dup_host = b"GET /x HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n";
        assert_eq!(parse_head(dup_host).err(), Some(400));
    }

    fn head(method: Method, cl: &str) -> Head {
        Head {
            method,
            target: "/".into(),
            headers: vec![("content-length".into(), cl.into())],
        }
    }

    #[test]
    fn body_length_rules() {
        assert_eq!(body_length(&head(Method::Post, "5")), Ok(5));
        assert_eq!(
            body_length(&head(Method::Post, &(MAX_BODY + 1).to_string())),
            Err(413)
        );
        assert_eq!(body_length(&head(Method::Post, "99999999999999")), Err(400));
        for bad in ["", "-1", "+1", "1 2", "0x10", "1e3"] {
            assert_eq!(body_length(&head(Method::Post, bad)), Err(400), "{bad:?}");
        }
        assert_eq!(body_length(&head(Method::Get, "3")), Err(400));
        assert_eq!(body_length(&head(Method::Get, "0")), Ok(0));
    }

    #[test]
    fn cookies_parse() {
        let h = vec![("cookie".to_owned(), "a=1; b=2;c".to_owned())];
        assert_eq!(
            parse_cookies(&h),
            vec![("a".into(), "1".into()), ("b".into(), "2".into())]
        );
    }
}
