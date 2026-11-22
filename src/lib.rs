//! Recalla: local-first memory engine for coding agents.
//!
//! Modules land one MVP item at a time (see `TODO.md`).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::assert_is_empty))]

pub mod archive;
pub mod cli;
pub mod extract;
pub mod harness;
pub mod ingest;
pub mod model;
pub mod pack;
pub mod paths;
pub mod redact;
pub mod store;

/// Crate version, as reported by `recalla --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Text printed by `recalla --version`.
#[must_use]
pub fn version_line() -> String {
    format!("recalla {VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_includes_crate_version() {
        assert_eq!(
            version_line(),
            format!("recalla {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
