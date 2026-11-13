//! Parallel scan equals the sequential result, and a run never modifies the logs it reads.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use acusage::app::{self, Env};
use acusage::cli::Cli;
use chrono::{TimeZone, Utc};
use clap::Parser;

mod common;

fn env_for(home: &Path) -> Env {
    Env {
        home: home.to_path_buf(),
        claude_config_dir: None,
        codex_home: None,
        acusage_config: Some("/nonexistent/acusage.toml".into()),
        now: Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap(),
    }
}

fn cli(report: &str, claude: &Path, codex: &Path) -> Cli {
    let mut argv: Vec<String> = ["acusage", report, "--utc"].map(String::from).to_vec();
    for (flag, p) in [
        ("--claude-dir", claude.join(".claude")),
        ("--claude-dir", claude.join(".claude-work")),
        ("--codex-dir", codex.join(".codex")),
    ] {
        argv.push(flag.into());
        argv.push(p.display().to_string());
    }
    Cli::try_parse_from(argv).unwrap()
}

const REPORTS: [&str; 6] = [
    "daily", "weekly", "monthly", "session", "accounts", "limits",
];

fn json_of(report: &str, claude: &Path, codex: &Path) -> String {
    let out = app::run(&cli(report, claude, codex), &env_for(claude)).unwrap();
    serde_json::to_string(&out.body).unwrap()
}

#[test]
fn thread_count_does_not_change_output() {
    let claude_tmp = common::fixture_home("claude/home");
    let codex_tmp = common::fixture_home("codex/home");
    let (claude, codex) = (
        claude_tmp.path().to_path_buf(),
        codex_tmp.path().to_path_buf(),
    );
    for report in REPORTS {
        let run_with = |n: usize| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build()
                .unwrap()
                .install(|| json_of(report, &claude, &codex))
        };
        assert_eq!(run_with(1), run_with(8), "{report}");
    }
}

fn set_mode(root: &Path, dir_mode: u32, file_mode: u32) {
    for e in fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            set_mode(&p, dir_mode, file_mode);
        } else {
            fs::set_permissions(&p, fs::Permissions::from_mode(file_mode)).unwrap();
        }
    }
    fs::set_permissions(root, fs::Permissions::from_mode(dir_mode)).unwrap();
}

fn snapshot(root: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
    let mut entries: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        let m = fs::symlink_metadata(&p).unwrap();
        out.push((p.clone(), m.len(), m.modified().unwrap()));
        if m.is_dir() {
            snapshot(&p, out);
        }
    }
}

#[test]
fn run_leaves_a_read_only_tree_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join("claude");
    let codex = tmp.path().join("codex");
    common::copy_tree(&common::fixtures().join("claude/home"), &claude);
    common::copy_tree(&common::fixtures().join("codex/home"), &codex);
    let agy = tmp.path().join("agy");
    let gem = tmp.path().join("gem");
    common::copy_tree(&common::fixtures().join("antigravity/home"), &agy);
    common::copy_tree(&common::fixtures().join("gemini/home"), &gem);
    set_mode(tmp.path(), 0o555, 0o444);

    let mut before = Vec::new();
    snapshot(tmp.path(), &mut before);
    let result = std::panic::catch_unwind(|| {
        for report in REPORTS {
            app::run(&cli(report, &claude, &codex), &env_for(&claude)).unwrap();
        }
        // The Antigravity and Gemini fixtures (databases included), read through their own flags.
        for tool in ["antigravity", "gemini"] {
            let mut argv: Vec<String> = ["acusage", "session", "--utc", "--tool", tool]
                .map(String::from)
                .to_vec();
            argv.extend([
                "--antigravity-dir".into(),
                agy.join(".gemini/antigravity-cli").display().to_string(),
                "--gemini-dir".into(),
                gem.join(".gemini").display().to_string(),
            ]);
            let c = Cli::try_parse_from(argv).unwrap();
            let out = app::run(&c, &env_for(&claude)).unwrap();
            assert!(!serde_json::to_string(&out.body).unwrap().is_empty());
        }
    });
    let mut after = Vec::new();
    snapshot(tmp.path(), &mut after);
    // Restore permissions so the tempdir can be removed.
    set_mode(tmp.path(), 0o755, 0o644);

    result.unwrap();
    assert!(before.len() > 10);
    assert_eq!(before, after);
}
