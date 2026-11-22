# Plan: redaction

## Goal

A `redact` module that scrubs secrets from any text before it reaches the store, the mirror, or archive metadata (AGENTS.md rule: redact before persist). It is deterministic, offline, and linear-time. Not wired into ingest yet; ingest lands in its own TODO item.

## Design

- **API:**
  - `redact(text: &str) -> Redacted { text: Cow<str>, findings: Vec<Finding { rule: &'static str, start, end }> }`
  - Replacement: `[REDACTED:<rule>]`.
  - Clean text is borrowed, so there is no allocation in the common case.
- **Layer 1, known formats** (one combined `regex`):
  - AWS access key ids: AKIA/ASIA
  - GitHub: `ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`, `github_pat_`
  - GitLab: `glpat-`
  - Anthropic: `sk-ant-`
  - OpenAI: `sk-` / `sk-proj-`
  - Slack: `xox[abpors]-`
  - Stripe: `sk_live_` / `rk_live_`
  - Google API: `AIza`
  - npm: `npm_`
  - Hugging Face: `hf_`
  - JWT: `eyJ…`
  - PEM private key blocks
  - URL credentials: `scheme://user:pass@`
  - `Authorization: Bearer|Basic <value>`
- **Layer 2, keyword assignments:** `(api[_-]?key|secret|token|passw(or)?d|pwd|access[_-]?key|client[_-]?secret|private[_-]?key|auth)` followed by `[:=]` (also `=>`, JSON `":"`, YAML `: `). Only the value is redacted.
  - Values under 8 chars are skipped.
  - Placeholders are allowlisted: `xxx…`, `***`, `<…>`, `${…}`, `$VAR`, `your-…`, `changeme`, `example`, `dummy`, `test`, `null`, `none`, `true`/`false`.
- **Layer 3, entropy:** any token of 24+ chars from `[A-Za-z0-9+/=_-]`, with Shannon entropy ≥4.0 bits/char, that mixes at least 3 character classes.
  - **Excluded:** pure hex (git SHAs, our blake3 uids, hashes), UUIDs, and tokens containing `/` that look like paths.
  - This avoids breaking provenance (commit hashes must survive).
- **Overlaps:** findings are sorted, and overlapping spans merge into the earliest rule. Idempotent: `redact(redact(x)) == redact(x)`. The marker itself never matches.
- **Performance:** the `regex` crate guarantees linear time (no ReDoS). Patterns compile once via `std::sync::LazyLock`.
- **Fixture corpus:** fake secrets are **built at runtime** by string concatenation (e.g. `["gh", "p_", &"A1b2".repeat(9)].concat()`). Literal secret-looking strings are never committed, because GitHub push protection and GitGuardian are on and would block or flag the repo.
  - Negative corpus: commit SHAs, UUIDs, uids, base64 prose, file paths, long identifiers, minified code snippets, URLs without credentials.

## Steps

- [x] 1. Add `regex` dep + D-007 (why `regex`, why no gitleaks/trufflehog dependency)
- [x] 2. `redact` core: `Redacted`/`Finding` types, span merge, replacement, idempotence; layer 1 known-format rules
- [x] 3. Layer 2: keyword-assignment values + placeholder allowlist
- [x] 4. Layer 3: entropy detector with hex/UUID/path exclusions
- [x] 5. Corpus tests (`tests/redact.rs`):
  - Every positive is fully removed (no 8+ char substring of the secret survives).
  - Zero findings on the negative corpus.
  - Idempotence; offsets valid at UTF-8 char boundaries.
- [x] 6. Docs: ARCHITECTURE redact section, TODO tick, CHANGELOG, state.yaml; delete the merged `core-model-store` plan

## Files touched

- `Cargo.toml`, `Cargo.lock`, `src/lib.rs`, `src/redact.rs` (split into `src/redact/{mod,rules,entropy}.rs` if over ~300 lines)
- `tests/redact.rs`
- DESIGN_DECISIONS.md, ARCHITECTURE.md, TODO.md, CHANGELOG.md, `.agent/state.yaml`, `.agent/plans/core-model-store.md` (deleted)

## Reused modules

- None needed. The store stays untouched (ingest will call `redact` before `Store::insert`).

## Risks

- **False positives eating real content**, e.g. long identifiers or base64 in code. Mitigation: a 3-class + entropy threshold, a negative corpus, and the keyword layer only redacts values.
- **False negatives on novel formats.** Layered design plus entropy as a catch-all; rules are easy to extend. This is documented as best-effort, not a guarantee.
- **Breaking provenance by redacting commit hashes or uids.** Pure hex is excluded, and a test asserts this.
- **Push protection triggered by fixtures.** Runtime-built secrets only, plus a test-file comment explaining why.
- **Unicode offset bugs.** Spans come from `regex` byte offsets, which are always char boundaries; a test with multibyte text covers it.

## Test plan

- Unit tests per layer (positive + negative per rule).
- Integration tests over the corpus:
  - Recall = 100% on positives.
  - 0 findings on negatives.
  - Idempotence.
  - Mixed text: secrets embedded in JSON, YAML, shell, `.env`, and a stack trace.
- `make check` green.
