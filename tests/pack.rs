//! Context packs: budget invariant, ranking, project scoping, the injection
//! ledger, and the `pack` / `stats` / `why` commands.

#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

mod common;

use std::collections::{HashMap, HashSet};

use common::Env;
use recalla::model::{self, Kind, Memory, NewMemory, Origin, Provenance, Scope, Status};
use recalla::pack::{build, est_tokens};
use recalla::paths;
use recalla::store::Store;
use serde_json::Value;

const NOW: i64 = 1_800_000_000_000;
const WEB_API: &str = "/tmp/eval/web-api";

/// Tiny deterministic xorshift64* PRNG, so the property test needs no deps.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const PIECES: &[&str] = &[
    "use",
    "cargo test",
    "the",
    "retry",
    "`make check`",
    "src/lib.rs",
    "日本語のテキスト",
    "数据库迁移必须是幂等的",
    "🚀",
    "👩‍👩‍👧‍👦",
    "naïve café",
    "\n",
    "\n\n",
    "\t",
    "  ",
    "\r\n",
    "x",
    "async/await",
    "한국어",
    "Привет мир",
];

fn random_text(rng: &mut Rng) -> String {
    let mut text = String::new();
    for _ in 0..=rng.below(40) {
        text.push_str(rng.pick(PIECES));
        text.push(' ');
    }
    if rng.below(10) == 0 {
        text.push_str(&"長".repeat(rng.below(400)));
    }
    text
}

fn memory(uid: String, kind: Kind, text: &str) -> Memory {
    Memory {
        uid,
        kind,
        text: text.into(),
        scope: Scope::Global,
        confidence: 0.8,
        importance: 1,
        status: Status::Active,
        superseded_by: None,
        seen_count: 1,
        created_at: NOW,
        updated_at: NOW,
        valid_from: NOW,
        valid_to: None,
        provenance: Vec::new(),
        anchors: Vec::new(),
        stale_reason: None,
    }
}

fn random_memory(rng: &mut Rng, n: usize) -> Memory {
    let kind = Kind::ALL[rng.below(Kind::ALL.len())];
    let mut m = memory(
        format!("{:016x}{n:016x}", rng.next()),
        kind,
        &random_text(rng),
    );
    m.confidence = f32::from(u8::try_from(rng.below(101)).unwrap()) / 100.0;
    m.seen_count = u32::try_from(rng.below(30)).unwrap();
    m.updated_at = NOW - i64::try_from(rng.below(400)).unwrap() * 86_400_000;
    if rng.below(5) == 0 {
        m.status = Status::NeedsReverify;
    }
    m
}

fn pack_of(ms: &[Memory], budget: usize) -> recalla::pack::Pack {
    build(ms, budget, "demo", &HashMap::new(), NOW)
}

#[test]
fn budget_is_never_exceeded() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut nonempty, mut truncated) = (0, 0);
    for _ in 0..500 {
        let count = rng.below(201);
        let ms: Vec<Memory> = (0..count).map(|i| random_memory(&mut rng, i)).collect();
        let budget = 50 + rng.below(3951);
        let hits: HashMap<String, f64> = ms
            .iter()
            .filter(|_| rng.below(4) == 0)
            .map(|m| (m.uid.clone(), 0.5))
            .collect();
        let p = build(&ms, budget, "proj\nname 日本", &hits, NOW);

        let known: HashSet<&str> = ms.iter().map(|m| m.uid.as_str()).collect();
        assert!(p.included_uids.iter().all(|u| known.contains(u.as_str())));
        assert_eq!(
            p.included_uids.len(),
            p.included_uids.iter().collect::<HashSet<_>>().len()
        );
        if p.markdown.is_empty() {
            assert_eq!(p.est_tokens, 0);
            assert!(p.included_uids.is_empty());
            continue;
        }
        nonempty += 1;
        truncated += usize::from(p.included_uids.len() < count);
        let real = est_tokens(&p.markdown);
        assert!(real <= budget, "{real} > {budget}");
        assert_eq!(p.est_tokens, real);
        assert_eq!(
            p.json["memories"].as_array().unwrap().len(),
            p.included_uids.len()
        );
    }
    // The property is only meaningful if budgets actually bind sometimes.
    assert!(nonempty > 400 && truncated > 100, "{nonempty} {truncated}");
}

#[test]
fn confident_preference_outranks_weak_fact_and_seen_count_raises_rank() {
    let pref = memory("a".repeat(32), Kind::Preference, "pref");
    let mut fact = memory("b".repeat(32), Kind::Fact, "fact");
    fact.confidence = 0.3;
    let mut weak_pref = pref.clone();
    weak_pref.confidence = 0.1;
    let uids = |ms: &[Memory]| pack_of(ms, 100_000).included_uids;
    assert_eq!(
        uids(&[fact.clone(), pref.clone()]),
        [pref.uid, fact.uid.clone()]
    );
    assert_eq!(uids(&[weak_pref.clone(), fact.clone()])[0], fact.uid);

    let mut seen = weak_pref.clone();
    seen.uid = "c".repeat(32);
    seen.seen_count = 200;
    assert_eq!(uids(&[fact, weak_pref, seen.clone()])[0], seen.uid);
}

#[test]
fn query_boost_lifts_a_matching_memory() {
    let top = memory("a".repeat(32), Kind::Preference, "unrelated");
    let hit = memory("b".repeat(32), Kind::Fact, "matches the query");
    let ms = [top.clone(), hit.clone()];
    assert_eq!(pack_of(&ms, 100_000).included_uids[0], top.uid);
    let boosts = HashMap::from([(hit.uid.clone(), 1.0)]);
    let boosted = build(&ms, 100_000, "demo", &boosts, NOW);
    assert_eq!(boosted.included_uids[0], hit.uid);
}

#[test]
fn needs_reverify_gets_its_own_labeled_section() {
    let mut stale = memory("a".repeat(32), Kind::Gotcha, "might be outdated");
    stale.status = Status::NeedsReverify;
    let fresh = memory("b".repeat(32), Kind::Gotcha, "still true");
    let p = pack_of(&[stale, fresh], 4000);
    let (gotchas, reverify) = p.markdown.split_once("## Needs re-verify ⚠").unwrap();
    assert!(gotchas.contains("## Gotchas") && gotchas.contains("still true"));
    assert!(!gotchas.contains("might be outdated"));
    assert!(reverify.contains("might be outdated"));
}

#[test]
fn budget_too_small_gives_empty_output() {
    let ms = [memory("a".repeat(32), Kind::Preference, "short")];
    for budget in [0, 1, 10, 30] {
        let p = pack_of(&ms, budget);
        assert_eq!((p.markdown.as_str(), p.est_tokens), ("", 0), "{budget}");
    }
    assert!(!pack_of(&ms, 80).markdown.is_empty());
}

fn insert(s: &mut Store, kind: Kind, scope: Scope, text: &str) {
    let new = NewMemory {
        kind,
        text: text.into(),
        scope,
        confidence: 0.9,
        importance: 1,
    };
    s.insert(&new, &Provenance::new(Origin::Remember)).unwrap();
}

#[test]
fn project_union_excludes_other_projects() {
    let mut s = Store::open_in_memory().unwrap();
    insert(&mut s, Kind::Preference, Scope::Global, "global rule");
    insert(&mut s, Kind::Fact, Scope::Project("/a".into()), "fact of a");
    insert(&mut s, Kind::Fact, Scope::Project("/b".into()), "fact of b");
    let module = Scope::Module {
        project: "/a".into(),
        path: "src".into(),
    };
    insert(&mut s, Kind::Gotcha, module, "gotcha in a/src");

    let statuses = [Status::Active, Status::NeedsReverify];
    let ms = s.memories_for_project(Some("/a"), &statuses).unwrap();
    let p = pack_of(&ms, 4000);
    for want in ["global rule", "fact of a", "gotcha in a/src"] {
        assert!(p.markdown.contains(want), "{want}");
    }
    assert!(!p.markdown.contains("fact of b"));
    let none = s.memories_for_project(None, &statuses).unwrap();
    assert_eq!(pack_of(&none, 4000).included_uids.len(), 1);
}

fn json(out: &str) -> Value {
    serde_json::from_str(out).unwrap()
}

fn backfilled() -> Env {
    let env = Env::new();
    env.write_all();
    env.backfill();
    env
}

#[test]
fn cli_pack_logs_an_injection_unless_no_log_and_stats_reflects_it() {
    let env = backfilled();
    let (code, md, err) = env.cli(&["pack", "--project", WEB_API, "--budget", "600"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        md.starts_with("# Recalla memory · project web-api · "),
        "{md}"
    );
    assert!(md.contains("## Preferences") && md.contains("token bucket"));
    assert!(est_tokens(md.trim_end()) <= 600);
    println!("{md}");

    let st = env.store().injection_stats().unwrap();
    assert_eq!(st.injections, 1);
    assert_eq!(st.per_project[0].project.as_deref(), Some(WEB_API));
    assert!(!st.top_memories.is_empty());

    let (code, again, _) = env.cli(&["pack", "--project", WEB_API, "--budget", "600", "--no-log"]);
    assert_eq!((code, again.as_str()), (0, md.as_str()));
    assert_eq!(env.store().injection_stats().unwrap().injections, 1);

    let (code, out, _) = env.cli(&["stats"]);
    assert_eq!(code, 0);
    assert!(out.contains("injections  1 (avg"), "{out}");
    assert!(
        out.contains(WEB_API) && out.contains("most injected"),
        "{out}"
    );
    assert!(out.contains("preference/active"), "{out}");
    let (_, out, _) = env.cli(&["stats", "--json"]);
    let v = json(&out);
    assert_eq!(v["injections"], 1);
    assert!(v["top_memories"][0]["text"].is_string());
    assert!(v["memories"].as_array().unwrap().len() > 3);
}

#[test]
fn cli_pack_json_query_and_other_projects() {
    let env = backfilled();
    let (code, out, _) = env.cli(&["pack", "--project", WEB_API, "--format", "json", "--no-log"]);
    let v = json(&out);
    assert_eq!(code, 0);
    assert!(v["memories"].as_array().unwrap().len() > 3);
    assert!(v["est_tokens"].as_u64().unwrap() <= 1500);
    let texts = out.to_lowercase();
    assert!(
        !texts.contains("pnpm"),
        "frontend memory leaked into web-api"
    );

    // A tiny budget with a query: the matching memory is the one that survives.
    let args = ["pack", "--project", WEB_API, "--no-log", "--format", "json"];
    let (_, out, _) =
        env.cli(&[&args[..], &["--budget", "90", "--query", "SlidingWindow"]].concat());
    let v = json(&out);
    let first = v["memories"][0]["text"].as_str().unwrap();
    assert!(first.contains("SlidingWindow"), "{first}");

    let (code, out, _) = env.cli(&["pack", "--project", "/no/such/project", "--no-log"]);
    assert_eq!(code, 0);
    assert!(!out.contains("token bucket"));
}

#[test]
fn cli_pack_with_missing_db_is_silent_success() {
    let env = Env::new();
    let (code, out, err) = env.cli(&["pack", "--project", WEB_API]);
    assert_eq!((code, out.as_str()), (0, ""));
    assert!(err.contains("no database"), "{err}");
    assert!(!env.data.exists());
}

#[test]
fn why_shows_provenance_and_source_text() {
    let env = backfilled();
    let mem = env
        .active()
        .into_iter()
        .find(|m| m.kind == Kind::Preference && m.text.contains("token bucket"))
        .unwrap();
    let id = &mem.uid[..8];
    let (code, out, err) = env.cli(&["why", id]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains(&mem.uid) && out.contains("preference"),
        "{out}"
    );
    assert!(out.contains("eval-web "), "session prefix: {out}");
    assert!(out.contains("eval-web-api.jsonl line 1"), "{out}");
    assert!(
        !out.contains("/eval-web-api.jsonl"),
        "full path leaked: {out}"
    );
    assert!(
        out.contains("    > ") && out.contains("token bucket"),
        "{out}"
    );
    assert!(out.contains("src/limits/bucket.rs"), "anchor: {out}");

    let (code, out, _) = env.cli(&["why", id, "--json"]);
    let v = json(&out);
    assert_eq!(code, 0);
    assert_eq!(v["uid"], mem.uid.as_str());
    let p = &v["provenance"][0];
    assert_eq!(p["source_file"], "eval-web-api.jsonl");
    assert!(p["events"][0].as_str().unwrap().contains("token bucket"));
    assert!(v["anchors"][0]["path"].is_string());
}

#[test]
fn why_ambiguous_prefix_exits_2_and_missing_exits_1() {
    let env = Env::new();
    // Find two texts whose uids share a 4-hex prefix, then store just those.
    let mut seen: HashMap<String, String> = HashMap::new();
    let (a, b, shared) = (0..)
        .find_map(|i| {
            let text = format!("memory number {i}");
            let uid = model::uid(Kind::Fact, &Scope::Global, &text);
            let key = uid[..4].to_owned();
            seen.insert(key.clone(), text.clone())
                .map(|prev| (prev, text, key))
        })
        .unwrap();
    paths::ensure_dir(&env.data).unwrap();
    let mut store = Store::open(paths::db_path(&env.data)).unwrap();
    insert(&mut store, Kind::Fact, Scope::Global, &a);
    insert(&mut store, Kind::Fact, Scope::Global, &b);
    drop(store);

    let (code, out, err) = env.cli(&["why", &shared]);
    assert_eq!((code, out.as_str()), (2, ""));
    assert!(err.contains("matches 2 memories"), "{err}");
    assert!(err.contains(&a) && err.contains(&b), "{err}");

    let (code, _, err) = env.cli(&["why", "abc"]);
    assert_eq!(code, 2);
    assert!(err.contains("at least 4 hex"), "{err}");
    let (code, _, err) = env.cli(&["why", "zzzz"]);
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = env.cli(&["why", "00000000ffff"]);
    assert_eq!(code, 1);
    assert!(err.contains("no memory matches"), "{err}");
}

#[test]
fn stats_and_why_without_a_database_exit_2() {
    let env = Env::new();
    for args in [&["stats"][..], &["why", "abcd1234"]] {
        let (code, out, err) = env.cli(args);
        assert_eq!((code, out.as_str()), (2, ""));
        assert!(err.contains("no database"), "{err}");
    }
}

#[test]
fn stats_on_a_fresh_ledger_is_empty_not_an_error() {
    let env = backfilled();
    let (code, out, _) = env.cli(&["stats"]);
    assert_eq!(code, 0);
    assert!(out.contains("injections  0 (avg 0 tok"), "{out}");
}
