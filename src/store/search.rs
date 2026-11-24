//! BM25-ranked full-text search over memories.

use rusqlite::types::Value;

use super::{MEMORY_COLS, Store, StoreError, memory_from_row};
use crate::model::{Kind, Memory, Scope, Status};

/// Restricts a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Exact scope match; `None` searches every scope.
    pub scope: Option<Scope>,
    /// Allowed kinds; empty allows all.
    pub kinds: Vec<Kind>,
    /// Allowed statuses. Empty matches nothing.
    pub statuses: Vec<Status>,
}

impl Default for Filter {
    /// Any scope or kind, active and needs-reverify memories only.
    fn default() -> Self {
        Self {
            scope: None,
            kinds: Vec::new(),
            statuses: vec![Status::Active, Status::NeedsReverify],
        }
    }
}

/// A search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub memory: Memory,
    /// Higher is more relevant (negated BM25).
    pub score: f64,
}

/// Turns free text into a safe FTS5 query: every whitespace-separated token
/// becomes a quoted string literal (implicit AND), so operators and
/// punctuation in user input are matched as text, never interpreted.
/// Returns `None` when there is nothing to search for.
pub(super) fn fts_query(query: &str) -> Option<String> {
    // NUL would truncate the string inside SQLite, so treat it as a separator.
    let terms: Vec<String> = query
        .split(|c: char| c.is_whitespace() || c == '\0')
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// Like [`fts_query`] but any token may match (OR), for natural-language task
/// descriptions where requiring every word finds nothing. BM25 still ranks
/// memories sharing more or rarer words higher. Distinct tokens, capped.
pub(super) fn fts_query_any(query: &str) -> Option<String> {
    const MAX_TERMS: usize = 24;
    let mut seen = std::collections::HashSet::new();
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|t| seen.insert(t.clone()))
        .take(MAX_TERMS)
        .map(|t| format!("\"{t}\""))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

/// Appends `value` to `params` and returns its `?N` placeholder.
fn bind(params: &mut Vec<Value>, value: impl Into<Value>) -> String {
    params.push(value.into());
    format!("?{}", params.len())
}

/// Binds every value and returns the comma-joined placeholders.
fn bind_all(params: &mut Vec<Value>, values: impl Iterator<Item = String>) -> String {
    values
        .map(|v| bind(params, v))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Store {
    /// Full-text search, best match first.
    ///
    /// # Errors
    /// `Sqlite` on failure. An empty query returns an empty list.
    pub fn search(
        &self,
        query: &str,
        filter: &Filter,
        limit: usize,
    ) -> Result<Vec<Hit>, StoreError> {
        self.search_fts(fts_query(query), filter, limit)
    }

    /// Ranked search where any query word may match (see [`fts_query_any`]).
    ///
    /// # Errors
    /// `Sqlite` on failure. A query with no usable words returns an empty list.
    pub fn search_any(
        &self,
        query: &str,
        filter: &Filter,
        limit: usize,
    ) -> Result<Vec<Hit>, StoreError> {
        self.search_fts(fts_query_any(query), filter, limit)
    }

    fn search_fts(
        &self,
        fts: Option<String>,
        filter: &Filter,
        limit: usize,
    ) -> Result<Vec<Hit>, StoreError> {
        let Some(fts) = fts else {
            return Ok(Vec::new());
        };
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut params: Vec<Value> = Vec::new();
        let mut clauses = vec![format!("memories_fts MATCH {}", bind(&mut params, fts))];
        clauses.push(format!(
            "m.status IN ({})",
            bind_all(&mut params, filter.statuses.iter().map(ToString::to_string))
        ));
        if !filter.kinds.is_empty() {
            clauses.push(format!(
                "m.kind IN ({})",
                bind_all(&mut params, filter.kinds.iter().map(ToString::to_string))
            ));
        }
        if let Some(scope) = &filter.scope {
            clauses.push(format!(
                "m.scope_key = {}",
                bind(&mut params, scope.to_string())
            ));
        }
        let limit = bind(&mut params, limit);
        let sql = format!(
            "SELECT {MEMORY_COLS}, bm25(memories_fts) AS rank
             FROM memories_fts JOIN memories m ON m.id = memories_fts.rowid
             WHERE {} ORDER BY rank, m.id LIMIT {limit}",
            clauses.join(" AND ")
        );

        let mut hits = self
            .conn
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(params), |row| {
                Ok(Hit {
                    memory: memory_from_row(row)?,
                    score: -row.get::<_, f64>(14)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for hit in &mut hits {
            self.load_children(&mut hit.memory)?;
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewMemory, Origin, Provenance};

    fn add(store: &mut Store, kind: Kind, scope: Scope, text: &str) -> String {
        let new = NewMemory {
            kind,
            text: text.into(),
            scope,
            confidence: 0.8,
            importance: 1,
        };
        store
            .insert(&new, &Provenance::new(Origin::Remember))
            .unwrap()
            .uid()
            .to_owned()
    }

    fn texts(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.memory.text.as_str()).collect()
    }

    fn all(store: &Store, q: &str) -> Vec<Hit> {
        store.search(q, &Filter::default(), 10).unwrap()
    }

    #[test]
    fn fts_query_quotes_every_token() {
        assert_eq!(fts_query("  foo   bar "), Some("\"foo\" \"bar\"".into()));
        assert_eq!(
            fts_query(r#"foo" OR bar*"#),
            Some(r#""foo""" "OR" "bar*""#.into())
        );
        assert_eq!(fts_query("a\0b"), Some("\"a\" \"b\"".into()));
        assert_eq!(fts_query("  \t\n\0"), None);
    }

    #[test]
    fn any_query_ors_distinct_words_and_ranks_by_overlap() {
        assert_eq!(
            fts_query_any("Fix the Glob, glob ** a"),
            Some("\"fix\" OR \"the\" OR \"glob\"".into())
        );
        assert_eq!(fts_query_any(" a ** "), None);
        let mut s = Store::open_in_memory().unwrap();
        add(
            &mut s,
            Kind::Fact,
            Scope::Global,
            "glob builder drops separators",
        );
        add(
            &mut s,
            Kind::Fact,
            Scope::Global,
            "unrelated tokio runtime note",
        );
        let hits = s
            .search_any("fix glob separators today", &Filter::default(), 10)
            .unwrap();
        assert_eq!(texts(&hits), vec!["glob builder drops separators"]);
        assert!(
            all(&s, "fix glob separators today").is_empty(),
            "AND search unchanged"
        );
    }

    #[test]
    fn hostile_queries_do_not_error() {
        let mut s = Store::open_in_memory().unwrap();
        add(&mut s, Kind::Fact, Scope::Global, "foo bar baz");
        for q in [
            r#"foo" OR bar*"#,
            "NEAR(",
            "NEAR(foo bar, 2)",
            "-x",
            ":",
            "col:foo",
            "\"",
            "\"\"\"",
            "(",
            ")",
            "*",
            "^foo",
            "foo AND",
            "OR",
            "'; DROP TABLE memories; --",
            "foo\u{0}bar",
            "日本語",
        ] {
            assert!(s.search(q, &Filter::default(), 10).is_ok(), "query {q:?}");
        }
        // Operators are matched as plain text, not interpreted.
        assert_eq!(all(&s, "foo OR nothing").len(), 0);
        assert_eq!(all(&s, "foo bar").len(), 1);
    }

    #[test]
    fn empty_query_returns_nothing() {
        let mut s = Store::open_in_memory().unwrap();
        add(&mut s, Kind::Fact, Scope::Global, "anything");
        assert_eq!(all(&s, "").len(), 0);
        assert_eq!(all(&s, "   ").len(), 0);
    }

    #[test]
    fn bm25_ranks_more_relevant_first() {
        let mut s = Store::open_in_memory().unwrap();
        add(
            &mut s,
            Kind::Fact,
            Scope::Global,
            "migrations run on open and also somewhere else entirely unrelated filler words",
        );
        add(
            &mut s,
            Kind::Fact,
            Scope::Global,
            "migrations migrations migrations",
        );
        let hits = all(&s, "migrations");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].memory.text, "migrations migrations migrations");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn scope_filter_isolates() {
        let mut s = Store::open_in_memory().unwrap();
        let a = Scope::Project("a".into());
        let b = Scope::Project("b".into());
        add(&mut s, Kind::Fact, a.clone(), "uses tokio");
        add(&mut s, Kind::Fact, b, "uses tokio too");
        add(&mut s, Kind::Fact, Scope::Global, "tokio everywhere");
        let only_a = Filter {
            scope: Some(a),
            ..Filter::default()
        };
        assert_eq!(
            texts(&s.search("tokio", &only_a, 10).unwrap()),
            ["uses tokio"]
        );
        assert_eq!(all(&s, "tokio").len(), 3);
    }

    #[test]
    fn kind_filter_and_limit() {
        let mut s = Store::open_in_memory().unwrap();
        add(
            &mut s,
            Kind::Gotcha,
            Scope::Global,
            "cache invalidation bites",
        );
        add(&mut s, Kind::Fact, Scope::Global, "cache is in memory");
        let gotchas = Filter {
            kinds: vec![Kind::Gotcha],
            ..Filter::default()
        };
        assert_eq!(
            texts(&s.search("cache", &gotchas, 10).unwrap()),
            ["cache invalidation bites"]
        );
        assert_eq!(s.search("cache", &Filter::default(), 1).unwrap().len(), 1);
    }

    #[test]
    fn status_filtering_and_fts_sync() {
        let mut s = Store::open_in_memory().unwrap();
        let old = add(&mut s, Kind::Fact, Scope::Global, "retry uses backoff");
        let new = add(&mut s, Kind::Fact, Scope::Global, "retry uses jitter");
        let arch = add(&mut s, Kind::Fact, Scope::Global, "retry is archived");

        s.supersede(&old, &new).unwrap();
        s.archive(&arch).unwrap();
        assert_eq!(texts(&all(&s, "retry")), ["retry uses jitter"]);
        // Hidden from search, still retrievable.
        assert!(s.get(&old).unwrap().is_some());

        let history = Filter {
            statuses: vec![Status::Superseded, Status::Archived],
            ..Filter::default()
        };
        assert_eq!(s.search("retry", &history, 10).unwrap().len(), 2);

        s.set_status(&new, Status::NeedsReverify).unwrap();
        assert_eq!(texts(&all(&s, "jitter")), ["retry uses jitter"]);
        s.restore(&old).unwrap();
        s.restore(&arch).unwrap();
        assert_eq!(all(&s, "retry").len(), 3);
    }

    #[test]
    fn kind_name_is_not_searchable_text() {
        let mut s = Store::open_in_memory().unwrap();
        add(
            &mut s,
            Kind::Decision,
            Scope::Global,
            "use sqlite for storage",
        );
        assert_eq!(all(&s, "decision"), []);
    }

    #[test]
    fn hit_carries_provenance() {
        let mut s = Store::open_in_memory().unwrap();
        add(&mut s, Kind::Fact, Scope::Global, "provenance rides along");
        let hit = &all(&s, "provenance")[0];
        assert_eq!(hit.memory.provenance.len(), 1);
    }
}
