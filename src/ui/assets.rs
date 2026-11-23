//! Embedded static files. Requests are matched against a fixed table and never
//! touch the filesystem, so path traversal is impossible by construction.

/// One embedded file.
pub struct Asset {
    pub content_type: &'static str,
    pub body: &'static [u8],
}

const INDEX: Asset = Asset {
    content_type: HTML,
    body: include_bytes!("assets/index.html"),
};

const HTML: &str = "text/html; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
const SVG: &str = "image/svg+xml";

/// Every embedded file, keyed by its request path.
pub const TABLE: &[(&str, Asset)] = &[
    (
        "/assets/app.css",
        Asset {
            content_type: CSS,
            body: include_bytes!("assets/app.css"),
        },
    ),
    (
        "/assets/app.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/app.js"),
        },
    ),
    (
        "/assets/theme.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/theme.js"),
        },
    ),
    (
        "/assets/dom.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/dom.js"),
        },
    ),
    (
        "/assets/api.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/api.js"),
        },
    ),
    (
        "/assets/ui.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/ui.js"),
        },
    ),
    (
        "/assets/router.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/router.js"),
        },
    ),
    (
        "/assets/filters.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/filters.js"),
        },
    ),
    (
        "/assets/views/overview.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/views/overview.js"),
        },
    ),
    (
        "/assets/views/sessions.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/views/sessions.js"),
        },
    ),
    (
        "/assets/views/memories.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/views/memories.js"),
        },
    ),
    (
        "/assets/views/injections.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/views/injections.js"),
        },
    ),
    (
        "/assets/views/connect.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/views/connect.js"),
        },
    ),
    (
        "/assets/favicon.svg",
        Asset {
            content_type: SVG,
            body: include_bytes!("assets/favicon.svg"),
        },
    ),
];

/// The page served at `/`.
#[must_use]
pub fn index() -> &'static Asset {
    &INDEX
}

/// Looks up an exact request path such as `/assets/app.css`.
#[must_use]
pub fn lookup(path: &str) -> Option<&'static Asset> {
    TABLE.iter().find(|(p, _)| *p == path).map(|(_, a)| a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_resolves_exact_paths_only() {
        assert_eq!(
            lookup("/assets/app.css").unwrap().content_type,
            "text/css; charset=utf-8"
        );
        assert!(lookup("/assets/app.js").is_some());
        assert!(lookup("/assets/favicon.svg").is_some());
        for bad in [
            "/assets/",
            "/assets/index.html",
            "/assets/../app.css",
            "/assets/app.css/",
            "/app.css",
        ] {
            assert!(lookup(bad).is_none(), "{bad}");
        }
        assert!(!index().body.is_empty());
    }

    fn text(a: &Asset) -> &str {
        std::str::from_utf8(a.body).expect("assets are UTF-8")
    }

    fn all() -> impl Iterator<Item = (&'static str, &'static Asset)> {
        TABLE
            .iter()
            .map(|(p, a)| (*p, a))
            .chain(std::iter::once(("/", index())))
    }

    #[test]
    fn every_asset_exists_with_the_right_content_type() {
        for (path, a) in all() {
            assert!(!a.body.is_empty(), "{path} is empty");
            let want = match path.rsplit('.').next().unwrap() {
                "css" => CSS,
                "js" => JS,
                "svg" => SVG,
                _ => HTML,
            };
            assert_eq!(a.content_type, want, "{path}");
        }
        let total: usize = all().map(|(_, a)| a.body.len()).sum();
        assert!(total < 150 * 1024, "assets are {total} bytes");
    }

    /// Guard like `scripts/web-check.sh`: the dashboard renders untrusted text
    /// and runs under a strict CSP, so these constructs must never appear.
    #[test]
    fn assets_avoid_unsafe_constructs() {
        const BANNED: &[&str] = &[
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "new Function",
            "javascript:",
            "setTimeout(\"",
            "srcdoc",
            "createContextualFragment",
        ];
        for (path, a) in all() {
            let src = text(a);
            for b in BANNED {
                assert!(!src.contains(b), "{path} contains {b}");
            }
            // Third-party URLs: only the SVG namespace identifier is allowed.
            let cleaned = src.replace("http://www.w3.org/2000/svg", "");
            assert!(
                !cleaned.contains("http://") && !cleaned.contains("https://"),
                "{path} references an external URL"
            );
            assert!(!src.contains("@import"), "{path} imports remotely");
        }
        let html = text(index());
        assert!(!html.contains(" style="), "inline style attribute");
        for attr in html.split_whitespace() {
            let name = attr.split('=').next().unwrap_or_default();
            assert!(
                !(name.starts_with("on") && attr.contains('=')),
                "inline handler {attr}"
            );
        }
        for tag in html.split("<script").skip(1) {
            let head = tag.split('>').next().unwrap();
            assert!(
                head.contains("src=\"/assets/"),
                "inline script: <script{head}>"
            );
        }
    }

    /// Every HTML reference and every `import` resolves to an embedded file.
    #[test]
    fn references_resolve_to_embedded_files() {
        let known = |p: &str| lookup(p).is_some();
        for part in text(index()).split(['"', '\'']) {
            if part.starts_with("/assets/") {
                assert!(known(part), "index.html references missing {part}");
            }
        }
        for (path, a) in TABLE.iter().filter(|(_, a)| a.content_type == JS) {
            let dir = &path[..path.rfind('/').unwrap()];
            for chunk in text(a)
                .split("from \"")
                .skip(1)
                .chain(text(a).split("import(\"").skip(1))
            {
                let spec = chunk.split('"').next().unwrap();
                if !spec.starts_with('.') {
                    continue;
                }
                let mut parts: Vec<&str> = dir.split('/').collect();
                for seg in spec.split('/') {
                    match seg {
                        "." => {}
                        ".." => {
                            parts.pop();
                        }
                        s => parts.push(s),
                    }
                }
                let target = parts.join("/");
                assert!(known(&target), "{path} imports missing {target}");
            }
        }
    }
}
