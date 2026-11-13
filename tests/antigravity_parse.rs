use acusage::model::{AccountSource, UsageRecord};
use acusage::sources::antigravity::{Finalized, default_dirs, discover, finalize, parse_file};

mod common;

fn uid(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn run() -> Finalized {
    let tmp = common::fixture_home("antigravity/home");
    let (files, _) = discover(&default_dirs(tmp.path()));
    finalize(files.iter().map(parse_file).collect())
}

fn of(f: &Finalized, n: u32) -> Vec<&UsageRecord> {
    let id = uid(n);
    let mut v: Vec<_> = f
        .records
        .iter()
        .filter(|r| r.session_id.as_deref() == Some(id.as_str()))
        .collect();
    v.sort_by_key(|r| r.ts);
    v
}

#[test]
fn default_dir_is_under_home() {
    let tmp = common::fixture_home("antigravity/home");
    assert_eq!(
        default_dirs(tmp.path()),
        vec![tmp.path().join(".gemini/antigravity-cli")]
    );
}

#[test]
fn discovery_finds_one_transcript_per_session() {
    let tmp = common::fixture_home("antigravity/home");
    let dir = tmp.path().join(".gemini/antigravity-cli");
    let (files, bad) = discover(&[dir, tmp.path().join("nope")]);
    assert_eq!(bad, 0);
    assert_eq!(files.len(), 8);
    assert!(files.iter().all(|f| f.path.ends_with("transcript.jsonl")));
}

#[test]
fn counts_dedup_and_skips() {
    let f = run();
    // A:3 (dup step dropped) B:1 C:40 D:1 E:3 F:2 G:1 H:1
    assert_eq!(f.records.len(), 52);
    assert_eq!(f.records_before, 52);
    assert_eq!(f.skipped_lines, 1);
    assert_eq!(f.unreadable_files, 0);
}

#[test]
fn tokens_are_per_request_and_uncached_input_is_separate() {
    let f = run();
    let a = of(&f, 1);
    let t: Vec<_> = a
        .iter()
        .map(|r| (r.input, r.output, r.cache_read))
        .collect();
    // step 3 appears twice; the larger output wins.
    assert_eq!(t, vec![(100, 10, 0), (200, 20, 50), (300, 30, 100)]);
    assert!(a.iter().all(|r| r.reasoning == 0 && r.cache_write_5m == 0));
}

#[test]
fn account_is_unknown_and_tool_is_antigravity() {
    let f = run();
    for r in &f.records {
        assert_eq!(r.tool, "antigravity");
        assert_eq!(r.account.id, "unknown");
        assert_eq!(r.account.source, AccountSource::Unknown);
    }
}

#[test]
fn models_come_from_the_conversation_db_in_order() {
    let f = run();
    let m: Vec<_> = of(&f, 1).iter().map(|r| r.model.clone()).collect();
    assert_eq!(m, ["model-a", "model-a", "model-b"]);
    // a row with only the model code takes the id seen with that code in the same database
    // (row 2 above); a code never seen with an id becomes the lowercased code
    assert_eq!(of(&f, 8)[0].model, "model_placeholder_x9");
    // overflow page chain
    assert_eq!(of(&f, 2)[0].model, "model-big");
    // multi-level b-tree: 40 rows over small pages
    let c = of(&f, 3);
    assert_eq!(c.len(), 40);
    assert_eq!(c[0].model, "model-c");
    assert_eq!(c[38].model, "model-c");
    assert_eq!(c[39].model, "model-c-last");
}

#[test]
fn model_is_never_guessed() {
    let f = run();
    // no db, corrupt db
    assert_eq!(of(&f, 4)[0].model, "unknown");
    assert_eq!(of(&f, 7)[0].model, "unknown");
    // 2 rows, 3 requests: the two matching requests get their rows' models, the third is unknown
    let m: Vec<_> = of(&f, 5).iter().map(|r| r.model.clone()).collect();
    assert_eq!(m, ["model-a", "model-b", "unknown"]);
    // an all-zero row (another model) is skipped; the request without a row is unknown even
    // though the only model in the database is model-f
    let m: Vec<_> = of(&f, 6).iter().map(|r| r.model.clone()).collect();
    assert_eq!(m, ["model-f", "unknown"]);
}

#[test]
fn same_dir_twice_does_not_double_count() {
    let tmp = common::fixture_home("antigravity/home");
    let d = tmp.path().join(".gemini/antigravity-cli");
    let (files, _) = discover(&[d.clone(), d]);
    let f = finalize(files.iter().map(parse_file).collect());
    assert_eq!(f.records.len(), 52);
}
