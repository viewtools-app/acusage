use std::path::{Path, PathBuf};

use acusage::model::{AccountSource, UsageRecord};
use acusage::sources::claude::{Finalized, Profiles, default_dirs, discover, finalize, parse_file};

const A1: &str = "00000000-0000-4000-8000-000000000001";
const A2: &str = "00000000-0000-4000-8000-000000000002";
const A3: &str = "00000000-0000-4000-8000-000000000003";
const O2: &str = "00000000-0000-4000-8000-0000000000a2";

mod common;

fn run(dirs: &[PathBuf], home: &Path) -> (Finalized, Profiles) {
    let (files, _) = discover(dirs);
    let parsed = files.iter().map(parse_file).collect();
    let profiles = Profiles::load(dirs, home);
    (finalize(parsed, &profiles, home), profiles)
}

fn by_msg<'a>(f: &'a Finalized, id: &str) -> &'a UsageRecord {
    f.records
        .iter()
        .find(|r| {
            r.dedup_key
                .as_deref()
                .is_some_and(|k| k.starts_with(&format!("{id}\u{1f}")))
        })
        .unwrap_or_else(|| panic!("missing {id}"))
}

#[test]
fn default_dirs_env_and_home() {
    let tmp = common::fixture_home("claude/home");
    let h = tmp.path().to_path_buf();
    let d = default_dirs(Some(" /a/x , ,/b/y"), &h);
    assert_eq!(d, vec![PathBuf::from("/a/x"), PathBuf::from("/b/y")]);
    let d = default_dirs(None, &h);
    assert_eq!(d, vec![h.join(".claude"), h.join(".claude-work")]);
    assert_eq!(default_dirs(Some(" , "), &h), d);
}

#[test]
fn discovery_is_recursive_and_tolerates_missing_dirs() {
    let tmp = common::fixture_home("claude/home");
    let h = tmp.path().to_path_buf();
    let dirs = vec![h.join(".claude"), h.join("nope")];
    let (files, bad) = discover(&dirs);
    assert_eq!(bad, 0);
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| f.profile_dir == h.join(".claude")));
}

#[test]
fn dedup_owner_profile_and_skips() {
    let tmp = common::fixture_home("claude/home");
    let h = tmp.path().to_path_buf();
    let dirs = default_dirs(None, &h);
    let (f, _) = run(&dirs, &h);

    // 7 distinct ids/sessions + 2 id-less records; synthetic and bad-time lines dropped.
    assert_eq!(f.records.len(), 8);
    assert_eq!(f.skipped_lines, 2);
    assert_eq!(f.unreadable_files, 0);

    // Streamed + resumed copies: largest output wins, counted once.
    let m1 = by_msg(&f, "m1");
    assert_eq!(m1.output, 20);
    assert_eq!(m1.account.id, A1);
    assert_eq!(m1.account.source, AccountSource::Owner);
    assert_eq!(m1.account.tool, "claude");
    assert_eq!(
        (m1.cache_write_5m, m1.cache_write_1h, m1.cache_read),
        (40, 60, 7)
    );
    assert_eq!(m1.project.as_deref(), Some("/Users/user/proj"));

    // No cache_creation split: all cache writes are 5m.
    let m2 = by_msg(&f, "m2");
    assert_eq!((m2.cache_write_5m, m2.cache_write_1h), (100, 0));

    // Owner record appears after the usage line, in the same file.
    assert_eq!(by_msg(&f, "m4").account.id, A1);
    // Default dir, no owner: unknown (no profile fallback).
    let m5 = by_msg(&f, "m5");
    assert_eq!(m5.account.source, AccountSource::Unknown);
    assert_eq!(m5.account.id, "unknown");
    // Non-default profile, no owner: profile's current account.
    let m6 = by_msg(&f, "m6");
    assert_eq!(m6.account.source, AccountSource::Profile);
    assert_eq!(m6.account.id, A2);
    assert_eq!(m6.account.org_id.as_deref(), Some(O2));
    // Owner wins over profile.
    let m7 = by_msg(&f, "m7");
    assert_eq!(m7.account.id, A3);
    assert_eq!(m7.account.source, AccountSource::Owner);

    // Records without message.id count once each.
    assert_eq!(
        f.records.iter().filter(|r| r.dedup_key.is_none()).count(),
        2
    );
}

#[test]
fn labels_come_from_oauth_account_only() {
    let tmp = common::fixture_home("claude/home");
    let h = tmp.path().to_path_buf();
    let (_, p) = run(&default_dirs(None, &h), &h);
    let l = &p.labels[A1];
    assert_eq!(l.email.as_deref(), Some("user@example.com"));
    assert_eq!(l.display_name.as_deref(), Some("Example User"));
    assert_eq!(l.org_name.as_deref(), Some("Example Org"));
    assert_eq!(p.labels[A2].email.as_deref(), Some("work@example.com"));
    assert_eq!(p.labels.len(), 2);
}

#[test]
fn explicit_non_default_dir_uses_its_own_claude_json() {
    let tmp = common::fixture_home("claude/home");
    let h = tmp.path().to_path_buf();
    let dirs = vec![h.join(".claude-work")];
    let (f, _) = run(&dirs, &h);
    assert_eq!(by_msg(&f, "m6").account.id, A2);
    // Session s1 owner lives in the other dir, which was not given: falls back to profile.
    assert_eq!(by_msg(&f, "m1").account.source, AccountSource::Profile);
}

#[test]
fn bad_input_never_panics() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join(".claude-x");
    let proj = dir.join("projects/p");
    std::fs::create_dir_all(&proj).unwrap();
    // Invalid UTF-8, truncated JSON, empty lines, wrong types, owner without session.
    let mut bytes: Vec<u8> = vec![0xff, 0xfe, b'\n', b'\n'];
    bytes.extend_from_slice(
        b"{\"type\":\"assistant\",\"message\":{\"usage\":{\"output_tokens\":\"x\"}\n",
    );
    bytes.extend_from_slice(b"{\"type\":\"session-owner\",\"accountUuid\":\"a\"}\n");
    bytes.extend_from_slice(b"{\"type\":\"assistant\",\"message\":null}\n");
    bytes.extend_from_slice(b"[1,2,3]\n");
    bytes.extend_from_slice(b"{\"type\":\"assistant\",\"timestamp\":\"2026-11-01T00:00:00Z\",\"message\":{\"id\":\"z\",\"model\":\"m\",\"usage\":{\"output_tokens\":1}}}");
    // Cache-creation split that would overflow when added.
    bytes.extend_from_slice(b"\n{\"type\":\"assistant\",\"timestamp\":\"2026-11-01T00:00:01Z\",\"message\":{\"id\":\"big\",\"model\":\"m\",\"usage\":{\"cache_creation\":{\"ephemeral_5m_input_tokens\":18446744073709551615,\"ephemeral_1h_input_tokens\":1}}}}");
    std::fs::write(proj.join("bad.jsonl"), bytes).unwrap();
    std::fs::write(dir.join(".claude.json"), "{ not json").unwrap();
    let (f, p) = run(&[dir], t.path());
    assert!(p.labels.is_empty());
    assert_eq!(f.skipped_lines, 4);
    assert_eq!(f.records.len(), 2);
    assert!(
        f.records
            .iter()
            .all(|r| r.account.source == AccountSource::Unknown)
    );
}
