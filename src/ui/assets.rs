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
const WOFF2: &str = "font/woff2";
/// Font licences (OFL-1.1), served next to the fonts they cover.
const TXT: &str = "text/plain; charset=utf-8";

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
        "/assets/palette.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/palette.js"),
        },
    ),
    (
        "/assets/views/home.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/views/home.js"),
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
        "/assets/views/sessions.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/views/sessions.js"),
        },
    ),
    (
        "/assets/views/packs.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/views/packs.js"),
        },
    ),
    (
        "/assets/views/setup.js",
        Asset {
            content_type: JS,
            body: include_bytes!("assets/views/setup.js"),
        },
    ),
    (
        "/assets/favicon.svg",
        Asset {
            content_type: SVG,
            body: include_bytes!("assets/favicon.svg"),
        },
    ),
    (
        "/assets/fonts/instrument-sans.woff2",
        Asset {
            content_type: WOFF2,
            body: include_bytes!("assets/fonts/instrument-sans.woff2"),
        },
    ),
    (
        "/assets/fonts/jetbrains-mono.woff2",
        Asset {
            content_type: WOFF2,
            body: include_bytes!("assets/fonts/jetbrains-mono.woff2"),
        },
    ),
    (
        "/assets/fonts/OFL-InstrumentSans.txt",
        Asset {
            content_type: TXT,
            body: include_bytes!("assets/fonts/OFL-InstrumentSans.txt"),
        },
    ),
    (
        "/assets/fonts/OFL-JetBrainsMono.txt",
        Asset {
            content_type: TXT,
            body: include_bytes!("assets/fonts/OFL-JetBrainsMono.txt"),
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

    /// The code and markup (fonts are binary; licences are plain text).
    fn code() -> impl Iterator<Item = (&'static str, &'static Asset)> {
        all().filter(|(_, a)| [HTML, CSS, JS, SVG].contains(&a.content_type))
    }

    #[test]
    fn every_asset_exists_with_the_right_content_type() {
        for (path, a) in all() {
            assert!(!a.body.is_empty(), "{path} is empty");
            let want = match path.rsplit('.').next().unwrap() {
                "css" => CSS,
                "js" => JS,
                "svg" => SVG,
                "woff2" => WOFF2,
                "txt" => TXT,
                _ => HTML,
            };
            assert_eq!(a.content_type, want, "{path}");
        }
        let total: usize = all().map(|(_, a)| a.body.len()).sum();
        assert!(total < 250 * 1024, "assets are {total} bytes");
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
        for (path, a) in code() {
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
        let css = text(lookup("/assets/app.css").unwrap());
        let urls: Vec<&str> = css
            .split("url(\"")
            .skip(1)
            .map(|c| c.split('"').next().unwrap())
            .collect();
        assert!(!urls.is_empty(), "app.css loads the fonts");
        for u in urls {
            assert!(
                known(&format!("/assets/{u}")),
                "app.css references missing {u}"
            );
        }
    }

    /// Every file in `assets/` is embedded (a new file must be added to TABLE),
    /// and each font ships with its licence.
    #[test]
    fn every_file_on_disk_is_embedded() {
        fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push(p.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui/assets");
        let mut files = Vec::new();
        walk(std::path::Path::new(root), &mut files);
        for f in files {
            let rel = &f[root.len()..];
            // index.html is served at `/`; dotfiles (formatter config) are not assets.
            if rel == "/index.html" || rel.rsplit('/').next().is_some_and(|n| n.starts_with('.')) {
                continue;
            }
            assert!(
                lookup(&format!("/assets{rel}")).is_some(),
                "{rel} is not in TABLE"
            );
        }
        for (font, licence) in [
            ("instrument-sans.woff2", "OFL-InstrumentSans.txt"),
            ("jetbrains-mono.woff2", "OFL-JetBrainsMono.txt"),
        ] {
            let f = lookup(&format!("/assets/fonts/{font}")).unwrap();
            assert!(f.body.starts_with(b"wOF2"), "{font} is woff2");
            let l = text(lookup(&format!("/assets/fonts/{licence}")).unwrap());
            assert!(l.contains("SIL OPEN FONT LICENSE"), "{licence}");
        }
    }
}
