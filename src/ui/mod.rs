//! Local dashboard server (`recalla ui`): bounded HTTP core, security layer,
//! embedded assets and the `/api` route table. See ARCHITECTURE.md.

pub mod actions;
pub mod api;
pub mod assets;
pub mod login;
pub mod router;
pub mod security;
pub mod server;
pub mod sync;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;

use crate::ops;
use crate::paths;
use crate::store::{DEFAULT_BUSY, Store, StoreError};
use router::{ApiResult, Ctx, Params, Route, dispatch};
use security::{Guard, LoginCode, Token};
use server::{Handler, Method, Request, Response};

/// The dashboard's request handler: enforces the security rules, then serves
/// the page, assets and API routes.
pub struct UiHandler {
    guard: Guard,
    ctx: Ctx,
    routes: Vec<Route>,
}

/// Startup step: creates the data dir and database if missing (a first-time
/// user gets an empty dashboard, not errors) and migrates an older schema
/// once, explicitly, calling `on_upgrade(new_version)` first.
///
/// # Errors
/// A message when the directory or database can't be prepared.
pub fn prepare(data_dir: &Path, on_upgrade: impl FnOnce(u32)) -> Result<(), String> {
    let probe = Store::open_read_only(paths::db_path(data_dir), DEFAULT_BUSY);
    if let Err(StoreError::Schema { found, expected }) = probe
        && found > 0
        && found < expected
    {
        on_upgrade(expected);
    }
    ops::open_or_create(data_dir)
        .map(drop)
        .map_err(|e| e.to_string())
}

/// The one-time login link (the only secret that ever reaches stdout or argv;
/// the long-lived session token stays inside the process).
#[must_use]
pub fn login_url(port: u16, code: &str) -> String {
    format!("http://127.0.0.1:{port}/?c={code}")
}

/// Program and arguments that open `target` (a file path, never a URL that
/// carries the login code) in the default browser.
#[must_use]
pub fn opener_command(target: &Path) -> (&'static str, Vec<String>) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    (program, vec![target.to_string_lossy().into_owned()])
}

/// Routes every deployment serves: session/ping here, data and actions from
/// `api` and `actions`.
fn default_routes() -> Vec<Route> {
    let mut routes = vec![
        Route {
            method: Method::Get,
            pattern: "/api/session",
            handler: session,
        },
        Route {
            method: Method::Get,
            pattern: "/api/ping",
            handler: ping,
        },
    ];
    routes.extend(api::routes());
    routes.extend(actions::routes());
    routes
}

#[allow(clippy::unnecessary_wraps)] // route signature
fn session(ctx: &Ctx, _: &Request, _: &Params) -> ApiResult {
    Ok(Response::json(
        200,
        &json!({ "csrf": ctx.csrf, "version": crate::VERSION }),
    ))
}

#[allow(clippy::unnecessary_wraps)] // route signature
fn ping(_: &Ctx, _: &Request, _: &Params) -> ApiResult {
    Ok(Response::json(200, &json!({ "ok": true })))
}

fn unauthorized() -> Response {
    Response::text(
        401,
        "Unauthorized. Run `recalla ui` again for a new login link.\n",
    )
}

fn forbidden() -> Response {
    Response::text(403, "Forbidden\n")
}

impl UiHandler {
    #[must_use]
    pub fn new(
        token: Token,
        csrf: Token,
        code: Arc<LoginCode>,
        port: u16,
        data_dir: PathBuf,
    ) -> Self {
        let ctx = Ctx::new(data_dir, csrf.as_str().to_owned());
        Self {
            guard: Guard::new(token, csrf, code, port),
            ctx,
            routes: default_routes(),
        }
    }

    /// Sets the idle time after which the session ends.
    #[must_use]
    pub fn with_session_idle(mut self, idle: std::time::Duration) -> Self {
        self.guard.idle = idle;
        self
    }

    /// Adjusts the handler context (tests inject temp paths and roots).
    #[must_use]
    pub fn with_ctx(mut self, f: impl FnOnce(&mut Ctx)) -> Self {
        f(&mut self.ctx);
        self
    }

    /// Adds a route (tests).
    #[must_use]
    pub fn with_route(mut self, route: Route) -> Self {
        self.routes.push(route);
        self
    }

    fn page(&self, req: &Request) -> Response {
        if let Some(code) = req.query_param("c") {
            // Wrong, used and expired codes all look the same.
            if !self.guard.code.redeem(code) {
                return unauthorized();
            }
            return Response::text(302, "")
                .with_header("Location", "/")
                .with_header("Set-Cookie", &self.guard.set_cookie_value());
        }
        if !self.guard.authenticated(req) {
            return unauthorized();
        }
        asset_response(assets::index())
    }

    fn api(&self, req: &Request) -> Response {
        if !self.guard.authenticated(req) {
            return unauthorized();
        }
        if req.method == Method::Post && !self.guard.mutation_ok(req) {
            return forbidden();
        }
        dispatch(&self.routes, &self.ctx, req)
    }
}

fn asset_response(a: &assets::Asset) -> Response {
    Response::new(200, a.content_type, a.body.to_vec())
}

impl Handler for UiHandler {
    fn handle(&self, req: &Request) -> Response {
        if !self.guard.host_ok(req) {
            return forbidden();
        }
        let path = req.path.as_str();
        let is_api = path == "/api" || path.starts_with("/api/");
        if !is_api && req.method == Method::Post {
            return Response::text(405, "Method not allowed\n");
        }
        if path == "/" {
            self.page(req)
        } else if path.starts_with("/assets/") {
            if !self.guard.authenticated(req) {
                return unauthorized();
            }
            assets::lookup(path).map_or_else(|| Response::text(404, "Not found\n"), asset_response)
        } else if is_api {
            self.api(req)
        } else {
            Response::text(404, "Not found\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opener_argv_holds_only_the_page_path() {
        let code = "ab".repeat(32);
        let page = Path::new("/data/run/login-1234.html");
        let (program, args) = opener_command(page);
        assert!(matches!(program, "open" | "xdg-open"));
        assert_eq!(args, vec!["/data/run/login-1234.html".to_owned()]);
        assert!(!args.join(" ").contains(&code));
    }
}
