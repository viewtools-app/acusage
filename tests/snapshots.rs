//! Snapshot tests: run the whole pipeline on the synthetic fixture homes and compare the
//! rendered output with tests/snapshots/<name>.txt. Set UPDATE_SNAPSHOTS=1 to rewrite them.

use acusage::app::{self, Env};
use acusage::cli::Cli;
use acusage::pricing::{PriceSource, PriceTable};
use acusage::render::{self, RenderOpts};
use chrono::{TimeZone, Utc};
use clap::Parser;

mod common;

use common::fixtures;

fn render_args(args: &[&str]) -> String {
    render_cfg(args, "/nonexistent/acusage.toml")
}

fn render_cfg(args: &[&str], config: &str) -> String {
    let (out, cli) = run_cfg(args, config).unwrap();
    render_out(&out, &cli, None)
}

fn run_cfg(args: &[&str], config: &str) -> anyhow::Result<(app::Output, Cli)> {
    run_full(args, config, false)
}

/// `agy`: also read the synthetic Antigravity home.
fn run_full(args: &[&str], config: &str, agy: bool) -> anyhow::Result<(app::Output, Cli)> {
    run_all(args, config, agy, false)
}

/// `agy` / `gemini`: also read the synthetic Antigravity / Gemini CLI homes.
fn run_all(
    args: &[&str],
    config: &str,
    agy: bool,
    gemini: bool,
) -> anyhow::Result<(app::Output, Cli)> {
    let gemini_tmp = common::fixture_home("gemini/home");
    let agy_tmp = common::fixture_home("antigravity/home");
    let claude_tmp = common::fixture_home("claude/home");
    let codex_tmp = common::fixture_home("codex/home");
    let claude_home = claude_tmp.path().to_path_buf();
    let codex_home = codex_tmp.path().to_path_buf();
    let mut argv: Vec<String> = ["acusage", "--utc"].map(String::from).to_vec();
    argv.extend(args.iter().map(|s| s.to_string()));
    for (flag, path) in [
        ("--claude-dir", claude_home.join(".claude")),
        ("--claude-dir", claude_home.join(".claude-work")),
        ("--codex-dir", codex_home.join(".codex")),
    ] {
        argv.push(flag.into());
        argv.push(path.display().to_string());
    }
    if agy {
        argv.push("--antigravity-dir".into());
        argv.push(
            agy_tmp
                .path()
                .join(".gemini/antigravity-cli")
                .display()
                .to_string(),
        );
    }
    if gemini {
        argv.push("--gemini-dir".into());
        argv.push(gemini_tmp.path().join(".gemini").display().to_string());
    }
    let cli = Cli::try_parse_from(argv)?;
    let env = Env {
        home: claude_home,
        claude_config_dir: None,
        codex_home: None,
        acusage_config: Some(config.into()),
        now: Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap(),
    };
    let out = app::run(&cli, &env)?;
    Ok((out, cli))
}

fn render_out(out: &app::Output, cli: &Cli, width: Option<usize>) -> String {
    let opts = RenderOpts {
        json: cli.opts.json,
        utc: true,
        no_cost: cli.opts.no_cost,
        color: false,
        compact: cli.opts.compact,
        full: cli.opts.full,
        width,
    };
    // The fixture path differs per machine; keep it out of the snapshots.
    let root = fixtures().display().to_string();
    render::render(out, &opts).replace(&root, "<fixtures>")
}

fn check(name: &str, args: &[&str]) {
    check_cfg(name, args, "/nonexistent/acusage.toml");
}

fn check_cfg(name: &str, args: &[&str], config: &str) {
    let got = render_cfg(args, config);
    compare(name, got);
}

/// Snapshot of a run that includes the Antigravity fixture.
fn check_agy(name: &str, args: &[&str]) {
    let (out, cli) = run_full(args, "/nonexistent/acusage.toml", true).unwrap();
    compare(name, render_out(&out, &cli, None));
}

/// Snapshot of a run that includes the Gemini CLI fixture.
fn check_gemini(name: &str, args: &[&str]) {
    let (out, cli) = run_all(args, "/nonexistent/acusage.toml", false, true).unwrap();
    compare(name, render_out(&out, &cli, None));
}

fn compare(name: &str, got: String) {
    let path = fixtures()
        .parent()
        .unwrap()
        .join("snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
    }
    let want = std::fs::read_to_string(&path).expect("snapshot missing");
    assert_eq!(got, want, "snapshot {name} differs");
}

#[test]
fn daily() {
    check("daily", &["daily"]);
}

#[test]
fn daily_breakdown() {
    check("daily_breakdown", &["daily", "--breakdown"]);
}

#[test]
fn session() {
    check("session", &["session"]);
}

#[test]
fn accounts() {
    check("accounts", &["accounts"]);
}

#[test]
fn limits() {
    check("limits", &["limits"]);
}

#[test]
fn daily_json() {
    check("daily_json", &["daily", "--json"]);
}

#[test]
fn no_data() {
    let got = render_args(&["daily", "--since", "2030-01-01"]);
    assert_eq!(got, "No usage found.\n");
}

fn priced() -> String {
    fixtures().join("priced.toml").display().to_string()
}

#[test]
fn daily_priced_partial_costs() {
    check_cfg("daily_priced", &["daily"], &priced());
}

#[test]
fn session_priced() {
    check_cfg("session_priced", &["session"], &priced());
}

#[test]
fn limits_filtered_by_tool() {
    let got = render_args(&["limits", "--tool", "claude"]);
    assert!(got.starts_with("No limit data found."));
    assert!(got.contains("Claude limits are not shown"));
}

#[test]
fn limits_json() {
    check("limits_json", &["limits", "--json"]);
}

#[test]
fn session_no_cost() {
    check("session_no_cost", &["session", "--no-cost"]);
}

#[test]
fn daily_compact() {
    check("daily_compact", &["daily", "--compact"]);
}

#[test]
fn daily_filtered_box() {
    // The title box names the filters, the breakdown and the --last window.
    check(
        "daily_filtered",
        &["daily", "--tool", "codex", "-b", "--last", "1"],
    );
}

#[test]
fn daily_codex_reasoning() {
    check("daily_codex", &["daily", "--tool", "codex"]);
}

#[test]
fn weekly() {
    check("weekly", &["weekly"]);
}

fn err(args: &[&str]) -> String {
    format!(
        "{:#}",
        run_cfg(args, "/nonexistent/acusage.toml").err().unwrap()
    )
}

#[test]
fn contradictory_filters_are_usage_errors() {
    let e = run_cfg(
        &["daily", "--since", "2026-11-02", "--until", "2026-11-01"],
        "/nonexistent/acusage.toml",
    )
    .err()
    .unwrap();
    assert!(e.downcast_ref::<app::UsageError>().is_some());
    assert!(format!("{e}").contains("is after --until"));
    assert!(err(&["daily", "--last", "0"]).contains("--last"));
}

#[test]
fn well_formed_but_impossible_date() {
    assert!(err(&["daily", "--since", "2026-02-30"]).contains("invalid date 2026-02-30"));
    assert!(err(&["daily", "--since", "yesterday"]).contains("expected YYYY-MM-DD"));
}

#[test]
fn unknown_account_lists_known_labels() {
    let e = err(&["daily", "--account", "nobody"]);
    assert!(
        e.contains("No usage for account \"nobody\". Known: "),
        "{e}"
    );
    assert!(e.contains("user@example.com"), "{e}");
}

#[test]
fn bare_run_is_limited_to_30_days() {
    let (bare, _) = run_cfg(&[], "/nonexistent/acusage.toml").unwrap();
    assert!(bare.default_window);
    let (daily, _) = run_cfg(&["daily"], "/nonexistent/acusage.toml").unwrap();
    assert!(!daily.default_window);
}

#[test]
fn flags_belong_to_their_subcommands() {
    use acusage::cli::Cli;
    let ok = |a: &[&str]| Cli::try_parse_from([&["acusage"], a].concat()).is_ok();
    assert!(ok(&["daily", "-i", "-p", "x"]));
    assert!(ok(&["blocks", "-a", "-r", "-t", "max", "-n", "4"]));
    assert!(!ok(&["daily", "-a"]));
    assert!(!ok(&["limits", "-i"]));
    assert!(!ok(&["session", "-t", "5"]));
}

fn render_at(args: &[&str], config: &str, width: usize) -> String {
    let (out, cli) = run_cfg(args, config).unwrap();
    render_out(&out, &cli, Some(width))
}

fn fixture_cfg(name: &str) -> String {
    fixtures().join(name).display().to_string()
}

fn widest(s: &str) -> usize {
    s.lines().map(|l| l.chars().count()).max().unwrap_or(0)
}

fn check_at(name: &str, args: &[&str], width: usize) {
    let got = render_at(args, "/nonexistent/acusage.toml", width);
    let path = fixtures()
        .parent()
        .unwrap()
        .join("snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &got).unwrap();
    }
    let want = std::fs::read_to_string(&path).expect("snapshot missing");
    assert_eq!(got, want, "snapshot {name} differs");
}

#[test]
fn every_view_fits_80_100_120_columns() {
    let plain = "/nonexistent/acusage.toml";
    let long = fixture_cfg("alias_long.toml");
    let priced = priced();
    let views: &[&[&str]] = &[
        &["daily"],
        &["daily", "-b"],
        &["daily", "-i"],
        &["weekly"],
        &["monthly"],
        &["session"],
        &["accounts"],
        &["limits"],
        &["blocks"],
        &["blocks", "-t", "max"],
        &["blocks", "-a", "-t", "1000"],
        &["daily", "--tool", "codex"],
    ];
    for cfg in [plain, long.as_str(), priced.as_str()] {
        for args in views {
            for width in [80, 100, 120] {
                let got = render_at(args, cfg, width);
                assert!(
                    widest(&got) <= width,
                    "{args:?} ({cfg}) is {} columns at {width}:\n{got}",
                    widest(&got)
                );
            }
        }
    }
}

#[test]
fn full_keeps_every_column() {
    let narrow = render_at(&["daily"], "/nonexistent/acusage.toml", 80);
    assert!(
        narrow.contains("compact layout (terminal is 80 columns); use --full or a wider terminal")
    );
    let (out, cli) = run_cfg(&["daily", "--full"], "/nonexistent/acusage.toml").unwrap();
    let full = render_out(&out, &cli, Some(80));
    assert!(full.contains("Cache Create"));
    assert!(!full.contains("compact layout"));
}

#[test]
fn long_alias_is_cut_in_the_middle_not_dropped() {
    let got = render_at(&["daily"], &fixture_cfg("alias_long.toml"), 80);
    assert!(got.contains("a-very-l…-the-lab"), "{got}");
    assert!(got.contains('…'));
}

#[test]
fn colliding_short_labels_get_a_note() {
    let got = render_at(&["daily"], &fixture_cfg("alias_collide.toml"), 120);
    assert!(got.contains("look alike"), "{got}");
    let plain = render_at(&["daily"], "/nonexistent/acusage.toml", 120);
    assert!(!plain.contains("look alike"));
}

#[test]
fn blocks_have_one_section_per_account() {
    let got = render_at(&["blocks"], "/nonexistent/acusage.toml", 200);
    assert!(got.contains("user (Example) (Claude Code)"), "{got}");
    assert!(got.contains("unknown (Claude Code)"));
    assert!(!got.contains("│ Account "));
    assert!(got.contains("idle "));
    assert!(got.contains("Blocks start on the hour of the first activity (UTC time shown)."));
}

#[test]
fn codex_blocks_say_they_are_estimates_and_skip_unknown() {
    let got = render_at(
        &["blocks", "--tool", "codex"],
        "/nonexistent/acusage.toml",
        200,
    );
    assert!(
        got.contains("estimated 5h blocks; real Codex windows: acusage limits"),
        "{got}"
    );
    assert!(!got.contains("unknown (Codex)"));
}

#[test]
fn snapshot_blocks() {
    check("blocks", &["blocks"]);
    check("blocks_active_max", &["blocks", "-a", "-t", "max"]);
    check(
        "blocks_compact_limit",
        &["blocks", "--compact", "-t", "1000"],
    );
    check("blocks_json", &["blocks", "--json"]);
}

#[test]
fn snapshots_at_80_columns() {
    check_at("daily_80", &["daily"], 80);
    check_at("session_80", &["session"], 80);
    check_at("blocks_80", &["blocks"], 80);
    check_at("accounts_80", &["accounts"], 80);
    check_at("limits_80", &["limits"], 80);
}

#[test]
fn daily_antigravity() {
    check_agy(
        "daily_antigravity",
        &["daily", "--tool", "antigravity", "--breakdown"],
    );
}

#[test]
fn daily_three_agents() {
    check_agy("daily_three_agents", &["daily"]);
}

#[test]
fn session_antigravity_json() {
    check_agy(
        "session_antigravity_json",
        &["session", "--tool", "antigravity", "--json"],
    );
}

#[test]
fn daily_gemini_breakdown() {
    check_gemini(
        "daily_gemini",
        &["daily", "--tool", "gemini", "--breakdown"],
    );
}

#[test]
fn session_gemini_json() {
    check_gemini(
        "session_gemini_json",
        &["session", "--tool", "gemini", "--json"],
    );
}

/// Run on the synthetic local-model home (an Ollama-style model plus a `-thinking` variant).
fn run_local(args: &[&str]) -> (app::Output, Cli) {
    let tmp = common::fixture_home("local/home");
    let home = tmp.path().to_path_buf();
    let mut argv: Vec<String> = ["acusage", "--utc"].map(String::from).to_vec();
    argv.extend(args.iter().map(|s| s.to_string()));
    argv.push("--claude-dir".into());
    argv.push(home.join(".claude").display().to_string());
    let cli = Cli::try_parse_from(argv).unwrap();
    let env = Env {
        home,
        claude_config_dir: None,
        codex_home: None,
        acusage_config: Some("/nonexistent/acusage.toml".into()),
        now: Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap(),
    };
    let out = app::run(&cli, &env).unwrap();
    (out, cli)
}

#[test]
fn local_model_table_note_and_no_partial_mark() {
    let (out, cli) = run_local(&["daily"]);
    compare("daily_local", render_out(&out, &cli, None));
    let got = render_out(&out, &cli, None);
    assert!(got.contains("local models counted at $0: qwen3-coder:30b-a3b-q4_K_M"));
    assert!(!got.contains('*'), "{got}");
}

#[test]
fn local_model_json_field() {
    let (out, cli) = run_local(&["daily", "--json"]);
    let got = render_out(&out, &cli, None);
    let v: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(
        v["local_models"],
        serde_json::json!(["qwen3-coder:30b-a3b-q4_K_M"])
    );
    assert_eq!(v["unpriced_models"], serde_json::json!([]));
    assert_eq!(v["totals"]["partial_cost"], false);
}

#[test]
fn local_model_json_field_session() {
    let (out, cli) = run_local(&["session", "--json"]);
    let got = render_out(&out, &cli, None);
    let v: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(
        v["local_models"],
        serde_json::json!(["qwen3-coder:30b-a3b-q4_K_M"])
    );
    assert_eq!(v["unpriced_models"], serde_json::json!([]));
}

/// Run on the synthetic marker home: a built-in model, a LiteLLM-only model and a local model.
fn run_marker(args: &[&str]) -> (app::Output, Cli) {
    run_marker_cfg(args, "/nonexistent/acusage.toml")
}

fn run_marker_cfg(args: &[&str], config: &str) -> (app::Output, Cli) {
    let t = PriceTable::builtin();
    assert_eq!(
        t.source("gemini-3.7-flash"),
        Some(PriceSource::Litellm),
        "marker fixtures need gemini-3.7-flash priced by the LiteLLM snapshot only (not BUILTIN); \
         a snapshot refresh or a BUILTIN change broke this precondition"
    );
    assert_eq!(
        t.source("claude-opus-5-5"),
        Some(PriceSource::Builtin),
        "marker fixtures need claude-opus-5-5 in BUILTIN"
    );
    let tmp = common::fixture_home("marker/home");
    let home = tmp.path().to_path_buf();
    let mut argv: Vec<String> = ["acusage", "--utc"].map(String::from).to_vec();
    argv.extend(args.iter().map(|s| s.to_string()));
    argv.push("--claude-dir".into());
    argv.push(home.join(".claude").display().to_string());
    let cli = Cli::try_parse_from(argv).unwrap();
    let env = Env {
        home,
        claude_config_dir: None,
        codex_home: None,
        acusage_config: Some(config.into()),
        now: Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap(),
    };
    (app::run(&cli, &env).unwrap(), cli)
}

const MARK_NOTE: &str = "\u{2020} priced from LiteLLM snapshot (third-party, not checked)";

fn marker_text(args: &[&str]) -> String {
    let (out, cli) = run_marker(args);
    render_out(&out, &cli, None)
}

#[test]
fn litellm_model_gets_marker_and_note_in_every_table() {
    for args in [
        &["daily"][..],
        &["weekly"],
        &["monthly"],
        &["session"],
        &["daily", "-b"],
        &["blocks"],
        &["accounts"],
    ] {
        let got = marker_text(args);
        if args[0] != "accounts" {
            assert!(got.contains("gemini-3.7-flash\u{2020}"), "{args:?}\n{got}");
            assert!(!got.contains("opus-5-5\u{2020}"), "{args:?}\n{got}");
            assert!(!got.contains("qwen3-coder:30b-a3b-q4_K_M\u{2020}"), "{got}");
        }
        assert!(got.contains(MARK_NOTE), "{args:?}\n{got}");
    }
}

#[test]
fn daily_breakdown_sub_rows_carry_the_marker() {
    let got = marker_text(&["daily", "-b"]);
    assert!(
        got.contains("\u{2514}\u{2500} gemini-3.7-flash\u{2020}"),
        "{got}"
    );
    assert!(!got.contains("\u{2514}\u{2500} opus-5-5\u{2020}"), "{got}");
}

#[test]
fn compact_layout_keeps_the_note() {
    let got = marker_text(&["daily", "--compact"]);
    assert!(got.contains(MARK_NOTE), "{got}");
}

#[test]
fn no_cost_hides_marker_and_note() {
    let got = marker_text(&["daily", "--no-cost"]);
    assert!(!got.contains('\u{2020}'), "{got}");
    assert!(!got.contains("LiteLLM"), "{got}");
}

#[test]
fn config_priced_model_is_not_marked() {
    // A config price for the LiteLLM-only model moves it to the config layer: no marker, no note.
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("acusage.toml");
    std::fs::write(
        &cfg,
        "[prices.\"gemini-3.7-flash\"]\ninput = 1.0\noutput = 2.0\ncache_write_5m = 0.0\ncache_write_1h = 0.0\ncache_read = 0.0\n",
    )
    .unwrap();
    let (out, cli) = run_marker_cfg(&["daily"], &cfg.display().to_string());
    let got = render_out(&out, &cli, None);
    assert!(!got.contains('\u{2020}'), "{got}");
    assert!(!got.contains("LiteLLM"), "{got}");
    let (out, cli) = run_marker_cfg(&["daily", "--json"], &cfg.display().to_string());
    let v: serde_json::Value = serde_json::from_str(&render_out(&out, &cli, None)).unwrap();
    assert_eq!(v["price_sources"]["gemini-3.7-flash"], "config");
}

#[test]
fn marker_is_never_lost_in_a_cut_cell_and_rows_fit() {
    for w in 80..=120 {
        let (out, cli) = run_marker(&["session"]);
        let got = render_out(&out, &cli, Some(w));
        assert!(widest(&got) <= w, "{w}\n{got}");
        for l in got.lines().filter(|l| l.starts_with('\u{2502}')) {
            if l.contains("- gemini") {
                assert!(l.contains('\u{2020}'), "{w}\n{got}");
            }
        }
    }
}

#[test]
fn price_sources_json() {
    let (out, cli) = run_marker(&["daily", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&render_out(&out, &cli, None)).unwrap();
    assert_eq!(
        v["price_sources"],
        serde_json::json!({
            "claude-opus-5-5": "builtin",
            "gemini-3.7-flash": "litellm",
            "qwen3-coder:30b-a3b-q4_K_M": "local",
        })
    );
    assert_eq!(
        v["local_models"],
        serde_json::json!(["qwen3-coder:30b-a3b-q4_K_M"])
    );
    assert_eq!(v["unpriced_models"], serde_json::json!([]));
    assert_eq!(v["schema"], 1);
    for args in [
        &["session", "--json"][..],
        &["blocks", "--json"],
        &["accounts", "--json"],
    ] {
        let (out, cli) = run_marker(args);
        let v: serde_json::Value = serde_json::from_str(&render_out(&out, &cli, None)).unwrap();
        assert_eq!(
            v["price_sources"]["gemini-3.7-flash"], "litellm",
            "{args:?}"
        );
    }
}

#[test]
fn price_sources_json_empty_and_limits() {
    let got = render_args(&["limits", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(v["price_sources"], serde_json::json!({}));
    let got = render_args(&[
        "daily",
        "--json",
        "--tool",
        "codex",
        "--since",
        "2030-01-01",
    ]);
    let v: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(v["price_sources"], serde_json::json!({}));
}
