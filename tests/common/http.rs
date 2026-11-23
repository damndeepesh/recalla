//! Raw-TcpStream HTTP/1.1 client for the dashboard tests (no HTTP dependency,
//! so malformed requests can be sent byte for byte).

#![allow(dead_code, clippy::unwrap_used)]

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::Value;

pub struct Resp {
    pub status: u16,
    /// Names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Resp {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("bad json {e}: {}", self.body))
    }
}

/// Sends raw bytes, reads until close.
pub fn raw(addr: SocketAddr, bytes: &[u8]) -> Resp {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let _ = s.write_all(bytes);
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    parse(&buf)
}

/// One request with an explicit `Host` (or none) and extra headers.
pub fn request(
    addr: SocketAddr,
    method: &str,
    target: &str,
    host: Option<&str>,
    extra: &[(&str, &str)],
    body: &[u8],
) -> Resp {
    let mut req = format!("{method} {target} HTTP/1.1\r\n");
    if let Some(h) = host {
        let _ = write!(req, "Host: {h}\r\n");
    }
    for (k, v) in extra {
        let _ = write!(req, "{k}: {v}\r\n");
    }
    if method == "POST" {
        let _ = write!(req, "Content-Length: {}\r\n", body.len());
    }
    req += "\r\n";
    let mut bytes = req.into_bytes();
    bytes.extend_from_slice(body);
    raw(addr, &bytes)
}

/// GET with the correct `Host` for `addr`.
pub fn get(addr: SocketAddr, target: &str, extra: &[(&str, &str)]) -> Resp {
    request(addr, "GET", target, Some(&host(addr)), extra, b"")
}

pub fn host(addr: SocketAddr) -> String {
    format!("127.0.0.1:{}", addr.port())
}

pub fn parse(buf: &[u8]) -> Resp {
    let text = String::from_utf8_lossy(buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    assert!(!head.replace("\r\n", "").contains('\n'), "bare LF in head");
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Resp {
        status,
        headers,
        body: body.to_owned(),
    }
}

/// The `name=value` part of a `Set-Cookie` header.
pub fn cookie_pair(resp: &Resp) -> Option<String> {
    resp.header("set-cookie")
        .and_then(|c| c.split(';').next())
        .map(str::to_owned)
}
