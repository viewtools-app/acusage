use std::path::PathBuf;

use acusage::model::{AccountSource, UsageRecord};
use acusage::sources::codex::{Finalized, default_dirs, discover, finalize, parse_file};

const C1: &str = "00000000-0000-4000-8000-0000000000c1";
const C2: &str = "00000000-0000-4000-8000-0000000000c2";

mod common;

fn run() -> Finalized {
    let tmp = common::fixture_home("codex/home");
    let dirs = default_dirs(None, tmp.path());
    let (files, _) = discover(&dirs);
    finalize(files.iter().map(parse_file).collect())
}

fn by_id<'a>(f: &'a Finalized, id: &str) -> &'a UsageRecord {
    f.records
        .iter()
        .find(|r| r.dedup_key.as_deref() == Some(id))
        .unwrap_or_else(|| panic!("missing {id}"))
}

#[test]
fn default_dirs_env_and_home() {
    let tmp = common::fixture_home("codex/home");
    let h = tmp.path().to_path_buf();
    assert_eq!(
        default_dirs(Some(" /a/x "), &h),
        vec![PathBuf::from("/a/x")]
    );
    assert_eq!(default_dirs(None, &h), vec![h.join(".codex")]);
    assert_eq!(default_dirs(Some(" "), &h), vec![h.join(".codex")]);
}

#[test]
fn discovery_covers_sessions_and_archive() {
    let tmp = common::fixture_home("codex/home");
    let (files, bad) = discover(&[tmp.path().join(".codex"), tmp.path().join("nope")]);
    assert_eq!(bad, 0);
    assert_eq!(files.len(), 5);
    assert!(files.iter().all(|f| f.path.extension().unwrap() == "jsonl"));
}

#[test]
fn usage_records_split_dedup_and_models() {
    let f = run();
    // resp-1..4, one id-less record, two fallback events from s3; dups and bad lines dropped.
    assert_eq!(f.records.len(), 7);

    let r1 = by_id(&f, "resp-1");
    assert_eq!(
        (r1.input, r1.cache_read, r1.cache_write_5m, r1.output),
        (600, 400, 50, 200)
    );
    assert_eq!(r1.model, "gpt-test-a");
    assert_eq!(r1.account.id, C1);
    assert_eq!(r1.account.source, AccountSource::Owner);
    assert_eq!(r1.session_id.as_deref(), Some("s-0001"));

    // cached > input saturates to 0; model follows the latest turn_context.
    let r2 = by_id(&f, "resp-2");
    assert_eq!((r2.input, r2.cache_read, r2.output), (0, 500, 40));
    assert_eq!(r2.model, "gpt-test-b");
    assert_eq!(r2.project.as_deref(), Some("/Users/user/proj"));

    assert_eq!(by_id(&f, "resp-3").model, "gpt-test-a");

    // No account, session_meta model used, source unknown.
    let r4 = by_id(&f, "resp-4");
    assert_eq!(r4.model, "gpt-meta");
    assert_eq!(r4.account.id, "unknown");
    assert_eq!(r4.account.source, AccountSource::Unknown);
    assert_eq!(
        f.records
            .iter()
            .filter(|r| r.dedup_key.is_none() && r.account.id == "unknown")
            .count(),
        1
    );
}

#[test]
fn token_count_fallback_only_without_usage_records() {
    let f = run();
    let fb: Vec<_> = f.records.iter().filter(|r| r.account.id == C2).collect();
    // s3: repeated total skipped, null info skipped; s1's token_count ignored (has usage records).
    // s3 is also archived (identical copy): the session + event time key counts it once.
    assert_eq!(fb.len(), 2);
    assert_eq!((fb[0].input, fb[0].cache_read, fb[0].output), (40, 60, 10));
    assert_eq!((fb[1].input, fb[1].cache_read, fb[1].output), (110, 40, 20));
    assert!(
        fb.iter()
            .all(|r| r.model == "gpt-test-c" && r.dedup_key.is_some())
    );
    assert!(f.records.iter().all(|r| r.output != 9999));
}

#[test]
fn skipped_lines_are_counted() {
    let f = run();
    // not json, usage without usage, bad timestamp.
    assert_eq!(f.skipped_lines, 3);
    assert_eq!(f.unreadable_files, 0);
}

#[test]
fn latest_limits_per_account() {
    let f = run();
    assert_eq!(f.limits.len(), 3);
    let c1 = f.limits.iter().find(|l| l.account.id == C1).unwrap();
    // The newest C1 event (s2) has only a secondary window; null-null events do not count.
    assert!(c1.primary.is_none());
    let s = c1.secondary.as_ref().unwrap();
    assert_eq!(
        (s.used_percent, s.window_minutes, s.resets_at),
        (40.0, Some(10080), Some(1793900000))
    );
    assert_eq!(c1.plan_type.as_deref(), Some("plus"));
    assert_eq!(c1.observed_at.to_rfc3339(), "2026-11-02T09:03:00+00:00");

    let c2 = f.limits.iter().find(|l| l.account.id == C2).unwrap();
    assert_eq!(c2.primary.as_ref().unwrap().used_percent, 7.0);
    assert_eq!(c2.secondary.as_ref().unwrap().used_percent, 1.0);
    assert_eq!(c2.plan_type.as_deref(), Some("pro"));

    let u = f.limits.iter().find(|l| l.account.id == "unknown").unwrap();
    assert_eq!(u.account.source, AccountSource::Unknown);
    assert!(u.secondary.is_none());
    assert_eq!(u.primary.as_ref().unwrap().window_minutes, Some(300));
}

#[test]
fn archived_copy_counts_once() {
    let f = run();
    assert!(f.records_before > f.records.len() as u64);
    let fb = f.records.iter().filter(|r| r.account.id == C2).count();
    assert_eq!(fb, 2);
}

#[test]
fn only_the_first_session_meta_counts() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join(".codex/sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let lines = [
        r#"{"timestamp":"2026-11-01T10:00:00Z","type":"session_meta","payload":{"id":"s-first"}}"#,
        r#"{"timestamp":"2026-11-01T10:00:01Z","type":"session_meta","payload":{"id":"s-parent","creator_account_id":"acct-x"}}"#,
        r#"{"timestamp":"2026-11-01T10:00:02Z","type":"turn_context","payload":{"model":"gpt-test-c"}}"#,
        r#"{"timestamp":"2026-11-01T10:00:03Z","type":"token_usage_record","payload":{"response_id":"r1","usage":{"input_tokens":5,"output_tokens":1}}}"#,
    ];
    std::fs::write(dir.join("f.jsonl"), lines.join("\n")).unwrap();
    let (files, _) = discover(&[t.path().join(".codex")]);
    let f = finalize(files.iter().map(parse_file).collect());
    assert_eq!(f.records.len(), 1);
    assert_eq!(f.records[0].session_id.as_deref(), Some("s-first"));
    assert_eq!(f.records[0].account.source, AccountSource::Unknown);
}

fn write_rollout(dir: &std::path::Path, name: &str, lines: &[String]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), lines.join("\n")).unwrap();
}

fn tc(ts: &str, total: u64, plan: Option<&str>) -> String {
    let plan = plan.map_or(String::new(), |p| format!(r#","plan_type":"{p}""#));
    format!(
        r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{total},"output_tokens":1}},"last_token_usage":{{"input_tokens":{total},"output_tokens":1}}}},"rate_limits":{{"primary":{{"used_percent":1.0,"window_minutes":300,"resets_at":1793530000}}{plan}}}}}}}"#
    )
}

fn meta(id: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-11-01T10:00:00Z","type":"session_meta","payload":{{"id":"{id}","creator_account_id":"acct-p"}}}}"#
    )
}

fn turn() -> String {
    r#"{"timestamp":"2026-11-01T10:00:01Z","type":"turn_context","payload":{"model":"gpt-test-c"}}"#
        .to_string()
}

#[test]
fn plan_type_survives_a_newer_snapshot_without_one() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join(".codex/sessions");
    let old = [
        meta("s-a"),
        turn(),
        tc("2026-11-01T10:01:00Z", 5, Some("plus")),
    ];
    let new = [meta("s-b"), turn(), tc("2026-11-02T10:01:00Z", 6, None)];
    write_rollout(&dir, "a.jsonl", &old);
    write_rollout(&dir, "b.jsonl", &new);
    let (files, _) = discover(&[t.path().join(".codex")]);
    let f = finalize(files.iter().map(parse_file).collect());
    assert_eq!(f.limits.len(), 1);
    assert_eq!(f.limits[0].plan_type.as_deref(), Some("plus"));
    assert_eq!(
        f.limits[0].observed_at.to_rfc3339(),
        "2026-11-02T10:01:00+00:00"
    );
}

#[test]
fn forked_rollout_replaying_parent_events_counts_them_once() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join(".codex/sessions");
    let parent = [
        meta("s-p"),
        turn(),
        tc("2026-11-01T10:01:00Z", 5, Some("plus")),
    ];
    let fork = [
        meta("s-p"),
        turn(),
        tc("2026-11-01T10:01:00Z", 5, Some("plus")),
        tc("2026-11-01T10:05:00Z", 9, Some("plus")),
    ];
    write_rollout(&dir, "parent.jsonl", &parent);
    write_rollout(&dir, "fork.jsonl", &fork);
    let (files, _) = discover(&[t.path().join(".codex")]);
    let f = finalize(files.iter().map(parse_file).collect());
    assert_eq!(f.records.len(), 2);
}
