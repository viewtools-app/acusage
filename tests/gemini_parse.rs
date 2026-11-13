use acusage::model::{AccountSource, UsageRecord};
use acusage::sources::gemini::{Finalized, default_dirs, discover, finalize, parse_file};

mod common;

fn run() -> Finalized {
    let tmp = common::fixture_home("gemini/home");
    let (files, _) = discover(&default_dirs(tmp.path()));
    finalize(files.iter().map(parse_file).collect())
}

fn by_model<'a>(f: &'a Finalized, model: &str) -> Vec<&'a UsageRecord> {
    let mut v: Vec<_> = f.records.iter().filter(|r| r.model == model).collect();
    v.sort_by_key(|r| r.ts);
    v
}

#[test]
fn default_dir_is_dot_gemini() {
    let tmp = common::fixture_home("gemini/home");
    assert_eq!(default_dirs(tmp.path()), vec![tmp.path().join(".gemini")]);
}

#[test]
fn discovery_reads_only_session_files_under_tmp_chats() {
    let tmp = common::fixture_home("gemini/home");
    let (files, bad) = discover(&[tmp.path().join(".gemini"), tmp.path().join("nope")]);
    assert_eq!(bad, 0);
    assert_eq!(files.len(), 3);
    assert!(files.iter().all(|f| {
        let p = f.path.to_string_lossy();
        p.contains("/tmp/") && !p.contains("antigravity") && !p.ends_with("notes.json")
    }));
    assert_eq!(files.iter().filter(|f| f.jsonl).count(), 1);
}

#[test]
fn tokens_map_to_input_cache_output_and_reasoning() {
    let f = run();
    let r = by_model(&f, "gemini-3.8-flash");
    // input 1000 incl. 300 cached, +10 tool; output 200 + thoughts 50.
    assert_eq!((r[0].input, r[0].cache_read), (710, 300));
    assert_eq!((r[0].output, r[0].reasoning), (250, 50));
    assert_eq!((r[1].input, r[1].cache_read, r[1].output), (500, 1500, 100));
    assert_eq!((r[0].cache_write_5m, r[0].cache_write_1h), (0, 0));
}

#[test]
fn jsonl_header_gives_the_session_and_bad_lines_are_counted() {
    let f = run();
    let r = by_model(&f, "gemini-3.8-flash-high");
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].session_id.as_deref(),
        Some("00000000-0000-4000-8000-000000000002")
    );
    assert_eq!((r[0].input, r[0].output), (100, 25));
    assert_eq!(f.skipped_lines, 1);
}

#[test]
fn a_session_saved_twice_is_counted_once() {
    let f = run();
    assert_eq!(f.records_before, 6);
    assert_eq!(f.records.len(), 5);
    assert_eq!(by_model(&f, "gemini-3.8-flash").len(), 2);
    assert_eq!(by_model(&f, "gemini-3.5-flash-lite").len(), 1);
}

#[test]
fn antigravity_tree_is_not_read_and_account_is_unknown() {
    let f = run();
    assert!(f.records.iter().all(|r| r.input < 999_999));
    for r in &f.records {
        assert_eq!(r.tool, "gemini");
        assert_eq!(r.account.source, AccountSource::Unknown);
    }
}

#[test]
fn missing_or_broken_files_do_not_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let chats = tmp.path().join("tmp/h/chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(chats.join("session-a.json"), "{not json").unwrap();
    std::fs::write(chats.join("session-b.jsonl"), "").unwrap();
    let (files, _) = discover(&[tmp.path().to_path_buf()]);
    let f = finalize(files.iter().map(parse_file).collect());
    assert!(f.records.is_empty());
    assert_eq!(f.skipped_lines, 1);
}
