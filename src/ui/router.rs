//! Tiny route table for `/api/*`.
//!
//! Extension point: add a [`Route`] to `default_routes()` in `ui/mod.rs`
//! (or call `UiHandler::with_route`). Patterns are `/`-separated; `{name}`
//! segments capture one percent-decoded segment into [`Params`].

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use super::server::{Method, Request, Response};
use super::sync::SyncState;
use crate::harness::{claude, codex};
use crate::store::StoreError;

/// Shared state for route handlers.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub data_dir: PathBuf,
    pub csrf: String,
    /// Claude Code `settings.json` the Connect page edits.
    pub settings_path: Option<PathBuf>,
    /// This binary, canonical: the target of installed hooks and MCP commands.
    pub exe: PathBuf,
    /// Where the sync job looks for sessions.
    pub claude_root: Option<PathBuf>,
    pub codex_root: Option<PathBuf>,
    pub opencode_root: Option<PathBuf>,
    pub sync: Arc<SyncState>,
}

impl Ctx {
    /// Defaults from the environment (as the CLI resolves them).
    #[must_use]
    pub fn new(data_dir: PathBuf, csrf: String) -> Self {
        let claude_root = claude::default_root();
        Self {
            data_dir,
            csrf,
            settings_path: claude_root.as_ref().map(|r| r.join("settings.json")),
            exe: crate::paths::current_exe().unwrap_or_default(),
            claude_root,
            codex_root: codex::default_root(),
            opencode_root: crate::harness::opencode::default_root(),
            sync: Arc::new(SyncState::default()),
        }
    }
}

/// A failed request, mapped to a status and a short, generic message. Internal
/// error text never reaches the client.
#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    NotFound,
    Conflict(String),
    Unavailable(&'static str),
    Internal,
}

/// What a handler returns.
pub type ApiResult = Result<Response, ApiError>;

impl ApiError {
    /// Logs the failure kind (never content) and answers a generic 500.
    #[must_use]
    pub fn internal(what: &str) -> Self {
        eprintln!("recalla ui: internal error: {what}");
        Self::Internal
    }

    fn response(&self) -> Response {
        match self {
            Self::BadRequest(m) => error(400, m),
            Self::NotFound => error(404, "not found"),
            Self::Conflict(m) => error(409, m),
            Self::Unavailable(m) => error(503, m),
            Self::Internal => error(500, "internal error"),
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Invalid(m) => Self::BadRequest(m),
            StoreError::NotFound(_) => Self::NotFound,
            StoreError::Ambiguous(m) => Self::Conflict(m),
            StoreError::Schema { found, expected } if found < expected => {
                Self::Unavailable("database needs upgrading")
            }
            StoreError::Schema { .. } => Self::Unavailable("database is newer than this build"),
            e @ StoreError::Sqlite(_) if e.is_busy() => Self::Unavailable("database busy"),
            e @ StoreError::Sqlite(_) => Self::internal(&e.to_string()),
        }
    }
}

/// Captured `{name}` segments.
#[derive(Debug, Default)]
pub struct Params(Vec<(&'static str, String)>);

impl Params {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Route handler signature.
pub type RouteFn = fn(&Ctx, &Request, &Params) -> ApiResult;

/// One route: method, pattern (e.g. `/api/memories/{uid}/confirm`), handler.
#[derive(Clone, Copy)]
pub struct Route {
    pub method: Method,
    pub pattern: &'static str,
    pub handler: RouteFn,
}

fn match_pattern(pattern: &'static str, path: &str) -> Option<Params> {
    let (mut pat, mut got) = (pattern.split('/'), path.split('/'));
    let mut params = Vec::new();
    loop {
        match (pat.next(), got.next()) {
            (None, None) => return Some(Params(params)),
            (Some(p), Some(g)) => {
                if let Some(name) = p.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                    params.push((name, super::server::percent_decode(g, false)?));
                } else if p != g {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

fn error(status: u16, msg: &str) -> Response {
    Response::json(status, &json!({ "error": msg }))
}

/// Runs the matching route. HEAD is routed as GET. Unknown path: 404 JSON;
/// known path with another method: 405 JSON.
#[must_use]
pub fn dispatch(routes: &[Route], ctx: &Ctx, req: &Request) -> Response {
    let method = if req.method == Method::Head {
        Method::Get
    } else {
        req.method
    };
    let mut path_known = false;
    for r in routes {
        if let Some(params) = match_pattern(r.pattern, &req.path) {
            if r.method == method {
                return (r.handler)(ctx, req, &params).unwrap_or_else(|e| e.response());
            }
            path_known = true;
        }
    }
    if path_known {
        error(405, "method not allowed")
    } else {
        error(404, "not found")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_wraps)]
    fn ok(_: &Ctx, _: &Request, p: &Params) -> ApiResult {
        Ok(Response::text(200, p.get("id").unwrap_or("-")))
    }

    fn fail(_: &Ctx, _: &Request, _: &Params) -> ApiResult {
        Err(ApiError::Unavailable("busy"))
    }

    fn req(method: Method, path: &str) -> Request {
        Request {
            method,
            path: path.into(),
            query: vec![],
            headers: vec![],
            body: vec![],
            cookies: vec![],
        }
    }

    #[test]
    fn matching_and_status_codes() {
        let ctx = Ctx::new(PathBuf::new(), String::new());
        let routes = [
            Route {
                method: Method::Get,
                pattern: "/api/x/{id}",
                handler: ok,
            },
            Route {
                method: Method::Get,
                pattern: "/api/fail",
                handler: fail,
            },
        ];
        let r = dispatch(&routes, &ctx, &req(Method::Get, "/api/fail"));
        assert_eq!(r.status, 503);
        assert_eq!(r.body, br#"{"error":"busy"}"#);
        let r = dispatch(&routes, &ctx, &req(Method::Get, "/api/x/a%20b"));
        assert_eq!((r.status, r.body.as_slice()), (200, b"a b".as_slice()));
        assert_eq!(
            dispatch(&routes, &ctx, &req(Method::Head, "/api/x/1")).status,
            200
        );
        assert_eq!(
            dispatch(&routes, &ctx, &req(Method::Post, "/api/x/1")).status,
            405
        );
        assert_eq!(
            dispatch(&routes, &ctx, &req(Method::Get, "/api/x")).status,
            404
        );
        assert_eq!(
            dispatch(&routes, &ctx, &req(Method::Get, "/api/x/1/2")).status,
            404
        );
        assert_eq!(
            dispatch(&routes, &ctx, &req(Method::Get, "/api/y")).status,
            404
        );
    }
}
