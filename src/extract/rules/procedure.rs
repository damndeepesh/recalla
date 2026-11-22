//! Procedures: build/test/lint commands that ran successfully.
//!
//! Emitted from the `ToolResult` event: the result arrives after its call, so
//! the rule looks back in the session window for the `Bash` call whose
//! `tool_id` equals the result's `tool_id` (the `tool_use_id` it answers).
//! The text is the normalized core command (see [`normalize_command`]), so
//! every spelling of the same command, in any session, dedups by uid and bumps
//! `seen_count`; no cross-session state is kept.

use serde_json::Value;

use super::decoded;
use crate::extract::scope::project_scope;
use crate::extract::{Candidate, EventCtx};
use crate::model::Kind;
use crate::store::{EventRow, StoredKind};

const CONFIDENCE: f32 = 0.6;
const IMPORTANCE: u8 = 1;
const MAX_COMMAND_CHARS: usize = 160;

const TOOLS: &[&str] = &[
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "npx",
    "make",
    "go",
    "pytest",
    "uv",
    "poetry",
    "tox",
    "nox",
    "terraform",
    "xcodebuild",
    "swift",
    "gradle",
    "./gradlew",
    "mvn",
    "dotnet",
    "mix",
    "bundle",
    "rake",
    "rspec",
    "deno",
    "just",
    "task",
];
/// `pytest` and `rspec` are verbs too, so `uv run pytest` and bare `pytest -x` qualify.
const VERBS: &[&str] = &[
    "test",
    "build",
    "lint",
    "check",
    "vet",
    "clippy",
    "fmt",
    "format",
    "plan",
    "typecheck",
    "tsc",
    "compile",
    "verify",
    "pytest",
    "rspec",
];
const EXCLUDED: &[&str] = &[
    "install", "add", "apply", "import", "deploy", "publish", "rm", "clean", "init",
];

/// Procedure candidates from one successful tool-result event.
#[must_use]
pub fn extract(ctx: &EventCtx<'_>, project: Option<&str>) -> Vec<Candidate> {
    let e = ctx.event;
    if e.kind != StoredKind::ToolResult || e.is_error {
        return Vec::new();
    }
    let Some(id) = e.tool_id.as_deref().filter(|id| !id.is_empty()) else {
        return Vec::new();
    };
    let Some(call) = ctx.prev.iter().rev().find(|p| is_call(p, id)) else {
        return Vec::new();
    };
    let Some(command) = bash_command(call)
        .and_then(|c| normalize_command(&c))
        .filter(|c| is_procedure(c))
    else {
        return Vec::new();
    };
    vec![Candidate {
        kind: Kind::Procedure,
        text: format!("`{command}`"),
        confidence: CONFIDENCE,
        importance: IMPORTANCE,
        scope: project_scope(project),
        anchors: Vec::new(),
    }]
}

fn is_call(e: &EventRow, id: &str) -> bool {
    e.kind == StoredKind::ToolCall
        && e.tool_name.as_deref() == Some("Bash")
        && e.tool_id.as_deref() == Some(id)
}

fn bash_command(call: &EventRow) -> Option<String> {
    let input: Value = serde_json::from_str(&call.text).ok()?;
    Some(decoded(input.get("command")?.as_str()?).trim().to_owned())
}

/// Separators that end the core command, in the order they are looked for.
const CORE_END: &[&str] = &["|", ";", "&&", " >", "2>", "&>", "<"];

/// `VAR=value` (shell-style name, no spaces in the value).
fn is_env_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Rest of `s` after the first `;` or `&&`, whichever comes first.
fn after_separator(s: &str) -> Option<&str> {
    let (i, len) = [(s.find(';'), 1), (s.find("&&"), 2)]
        .into_iter()
        .filter_map(|(i, l)| i.map(|i| (i, l)))
        .min()?;
    Some(&s[i + len..])
}

/// The core command of a shell line: leading `cd <path> &&`, `export V=x;`,
/// `env V=x` and `V=x` prefixes are stripped (repeatably), then the first
/// segment up to a pipe, `;`, `&&`, `||` or redirect is kept. `None` when the
/// line is multi-line, has nothing left, or the core uses command
/// substitution or backgrounding.
#[must_use]
pub fn normalize_command(command: &str) -> Option<String> {
    if command.contains(['\n', '\r']) {
        return None;
    }
    let mut rest = command.trim();
    loop {
        let first = rest.split_whitespace().next().unwrap_or_default();
        if first == "cd" || first == "export" {
            rest = after_separator(rest)?.trim_start();
        } else if first == "env" || is_env_assignment(first) {
            rest = rest[first.len()..].trim_start();
        } else {
            break;
        }
    }
    let end = CORE_END.iter().filter_map(|p| rest.find(p)).min();
    let core = end.map_or(rest, |i| &rest[..i]).trim();
    let bad = core.is_empty() || core.contains(['`', '&']) || core.contains("$(");
    (!bad).then(|| core.to_owned())
}

/// A known tool with a build/test/check verb (input is a normalized core).
fn is_procedure(command: &str) -> bool {
    if command.chars().count() > MAX_COMMAND_CHARS {
        return false;
    }
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let Some(first) = tokens.first() else {
        return false;
    };
    let is_python_pytest = tokens.starts_with(&["python", "-m", "pytest"])
        || tokens.starts_with(&["python3", "-m", "pytest"]);
    if !TOOLS.contains(first) && !is_python_pytest {
        return false;
    }
    let words: Vec<&str> = tokens.iter().map(|t| base(t)).collect();
    if words.iter().skip(1).any(|w| EXCLUDED.contains(w)) {
        return false;
    }
    let has_verb = |w: &&str| VERBS.contains(w);
    if let Some(i) = words.iter().position(|w| *w == "run") {
        return words.get(i + 1).is_some_and(has_verb);
    }
    words.iter().any(has_verb)
}

/// `test:unit` is the verb `test`.
fn base(token: &str) -> &str {
    token.split(':').next().unwrap_or(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::testutil::event;
    use crate::model::Scope;
    use serde_json::json;

    fn call(id: &str, name: &str, command: &str) -> EventRow {
        let mut e = event(
            StoredKind::ToolCall,
            &json!({ "command": command }).to_string(),
        );
        e.tool_name = Some(name.into());
        e.tool_id = Some(id.into());
        e
    }

    fn result(id: &str, is_error: bool) -> EventRow {
        let mut e = event(StoredKind::ToolResult, "output");
        e.tool_id = Some(id.into());
        e.is_error = is_error;
        e
    }

    fn run_pair(command: &str) -> Vec<Candidate> {
        let prev = [call("t1", "Bash", command)];
        let r = result("t1", false);
        extract(
            &EventCtx {
                event: &r,
                prev: &prev,
            },
            Some("/work/app"),
        )
    }

    #[test]
    fn positives() {
        for c in [
            "cargo test",
            "cargo clippy --all-targets -- -D warnings",
            "cargo fmt --check",
            "npm run test",
            "pnpm run build",
            "npm test",
            "yarn lint",
            "make check",
            "go vet ./...",
            "go test ./pkg/...",
            "pytest -x tests/",
            "python -m pytest tests/unit",
            "uv run pytest",
            "terraform plan",
            "npx tsc --noEmit",
            "./gradlew build",
            "npm run test:unit",
            "just verify",
        ] {
            let got = run_pair(c);
            assert_eq!(got.len(), 1, "{c}");
            assert_eq!(got[0].kind, Kind::Procedure);
            assert_eq!(got[0].text, format!("`{c}`"));
            assert!((got[0].confidence - 0.6).abs() < 1e-6);
            assert_eq!(got[0].scope, Scope::Project("/work/app".into()));
        }
    }

    #[test]
    fn normalizes_compound_commands() {
        for (raw, core) in [
            (
                "cd ~/projects/recalla && make check > \"$TMPDIR/rc.log\" 2>&1; echo \"exit=$?\"",
                "make check",
            ),
            ("export PATH=/x:$PATH; cargo test -q", "cargo test -q"),
            ("cargo build && ./target/release/app", "cargo build"),
            ("cd /repo && make check 2>&1 | tail -5", "make check"),
            (
                "cd /a; cd b && export A=1 && B=2 env C=3 cargo test",
                "cargo test",
            ),
            ("RUST_LOG=debug cargo test", "cargo test"),
            (
                "env RUST_LOG=debug cargo build --release",
                "cargo build --release",
            ),
            ("cargo test; echo done", "cargo test"),
            ("cargo test || true", "cargo test"),
            ("cargo test<in.txt", "cargo test"),
            ("  cargo test  ", "cargo test"),
        ] {
            assert_eq!(normalize_command(raw).as_deref(), Some(core), "{raw}");
        }
        for raw in [
            "",
            "cd /repo",
            "export A=1",
            "cargo test $(pwd)",
            "cargo test `pwd`",
            "cargo test &",
            "cargo build\ncargo test",
            "cd /repo && $(make) check",
        ] {
            assert_eq!(normalize_command(raw), None, "{raw}");
        }
    }

    #[test]
    fn compound_spellings_share_one_memory_text() {
        for raw in [
            "make check",
            "cd /repo && make check 2>&1 | tail -5",
            "export PATH=/x:$PATH; make check",
            "CI=1 make check > out.log",
        ] {
            let got = run_pair(raw);
            assert_eq!(got.len(), 1, "{raw}");
            assert_eq!(got[0].text, "`make check`");
        }
        assert!(run_pair("npm run dev | tee log").is_empty());
        assert!(run_pair("cd /repo && npm install").is_empty());
    }

    #[test]
    fn negatives() {
        let long = format!("cargo test {}", "x".repeat(160));
        for c in [
            "cargo test &",
            "cargo test\ncargo build",
            "npm install",
            "npm run dev",
            "cargo run",
            "cargo add serde",
            "terraform apply",
            "make clean",
            "make deploy",
            "npm publish",
            "ls -la",
            "git commit -m test",
            "echo cargo test",
            "cargo install cargo-test",
            "make",
            "python script.py test",
            "cargo test $(pwd)",
            "",
            long.as_str(),
        ] {
            assert!(run_pair(c).is_empty(), "{c}");
        }
    }

    #[test]
    fn sidechain_results_still_emit() {
        let prev = [call("t1", "Bash", "cargo test")];
        let mut r = result("t1", false);
        r.sidechain = true;
        let ctx = EventCtx {
            event: &r,
            prev: &prev,
        };
        assert_eq!(extract(&ctx, None).len(), 1);
    }

    #[test]
    fn pairs_by_tool_id() {
        let prev = [call("a", "Bash", "cargo test"), call("b", "Bash", "ls -la")];
        let r = result("b", false);
        assert!(
            extract(
                &EventCtx {
                    event: &r,
                    prev: &prev
                },
                None
            )
            .is_empty()
        );
        let r = result("a", false);
        let got = extract(
            &EventCtx {
                event: &r,
                prev: &prev,
            },
            None,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].scope, Scope::Global);
        // Unknown id, empty id and missing id pair with nothing.
        for id in [Some("zzz"), Some(""), None] {
            let mut r = result("a", false);
            r.tool_id = id.map(Into::into);
            assert!(
                extract(
                    &EventCtx {
                        event: &r,
                        prev: &prev
                    },
                    None
                )
                .is_empty()
            );
        }
    }

    #[test]
    fn failed_results_and_other_tools_yield_nothing() {
        let prev = [call("t1", "Bash", "cargo test")];
        let r = result("t1", true);
        assert!(
            extract(
                &EventCtx {
                    event: &r,
                    prev: &prev
                },
                None
            )
            .is_empty()
        );
        let prev = [call("t1", "Read", "cargo test")];
        let r = result("t1", false);
        assert!(
            extract(
                &EventCtx {
                    event: &r,
                    prev: &prev
                },
                None
            )
            .is_empty()
        );
        // The call event itself never emits (the result may still fail).
        let c = call("t1", "Bash", "cargo test");
        assert!(
            extract(
                &EventCtx {
                    event: &c,
                    prev: &[]
                },
                None
            )
            .is_empty()
        );
    }

    #[test]
    fn malformed_input_is_ignored() {
        let mut bad = call("t1", "Bash", "cargo test");
        bad.text = "not json".into();
        let r = result("t1", false);
        assert!(
            extract(
                &EventCtx {
                    event: &r,
                    prev: &[bad.clone()]
                },
                None
            )
            .is_empty()
        );
        bad.text = json!({ "command": 5 }).to_string();
        assert!(
            extract(
                &EventCtx {
                    event: &r,
                    prev: &[bad]
                },
                None
            )
            .is_empty()
        );
    }
}
