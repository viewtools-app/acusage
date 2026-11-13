use acusage::cli::{Cli, Command, Order, Tool};
use clap::Parser;

#[test]
fn parses_subcommand_and_flags() {
    let cli = Cli::try_parse_from([
        "acusage",
        "weekly",
        "--since",
        "2026-01-01",
        "--until",
        "2026-01-31",
        "--account",
        "a",
        "--account",
        "b",
        "--tool",
        "codex",
        "--breakdown",
        "--order",
        "desc",
        "--utc",
        "--json",
        "--verbose",
        "--claude-dir",
        "/x",
        "--claude-dir",
        "/y",
        "--codex-dir",
        "/z",
        "--antigravity-dir",
        "/w",
        "--gemini-dir",
        "/g",
    ])
    .unwrap();
    assert_eq!(cli.command, Some(Command::Weekly(Default::default())));
    assert_eq!(cli.opts.account, ["a", "b"]);
    assert_eq!(cli.opts.tool, Some(Tool::Codex));
    assert_eq!(cli.opts.order, Order::Desc);
    assert_eq!(cli.opts.claude_dir.len(), 2);
    assert_eq!(cli.opts.antigravity_dir.len(), 1);
    assert_eq!(cli.opts.gemini_dir.len(), 1);
    assert!(cli.opts.breakdown && cli.opts.utc && cli.opts.json && cli.opts.verbose);
}

#[test]
fn defaults_and_bad_args() {
    let cli = Cli::try_parse_from(["acusage"]).unwrap();
    assert_eq!(cli.command, None);
    assert_eq!(cli.opts.order, Order::Asc);
    assert!(Cli::try_parse_from(["acusage", "--tool", "gpt"]).is_err());
    assert!(Cli::try_parse_from(["acusage", "bogus"]).is_err());
}
