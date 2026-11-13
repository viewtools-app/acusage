use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "acusage",
    version,
    about = "Usage report for Claude Code, Codex, Antigravity and Gemini CLI logs",
    after_help = concat!(
        "Account: the login a session ran under; \"unknown\" means the log has no account record.\n\
API Value: what the usage would cost at API list prices; subscriptions are not billed per token.\n\
Blocks: 5-hour windows that start on the hour of an account's first activity (estimates).\n\
Config: ~/.config/acusage/config.toml, or $ACUSAGE_CONFIG ([aliases] and [prices]).\n\
Homepage: ",
        crate::homepage!()
    )
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Used when no subcommand is given (daily)
    #[command(flatten)]
    pub group: GroupOpts,
    #[command(flatten)]
    pub opts: Options,
}

/// Flags of the daily, weekly and monthly reports.
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupOpts {
    /// Group by project first
    #[arg(short = 'i', long)]
    pub instances: bool,
    /// Only projects whose path contains TEXT (case-insensitive)
    #[arg(short = 'p', long, value_name = "TEXT")]
    pub project: Option<String>,
}

/// Flags of the blocks report.
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct BlockFlags {
    /// Only the active block, with projections
    #[arg(short = 'a', long)]
    pub active: bool,
    /// Only blocks that started in the last 3 days, and the active one
    #[arg(short = 'r', long)]
    pub recent: bool,
    /// Show % of a token limit: a number, or "max" (busiest earlier block of each account)
    #[arg(short = 't', long, value_name = "N|max")]
    pub token_limit: Option<TokenLimit>,
    /// Block length in hours
    #[arg(
        short = 'n',
        long,
        value_name = "HOURS",
        default_value_t = 5,
        value_parser = clap::value_parser!(u32).range(1..=168)
    )]
    pub session_length: u32,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Usage per day (default)
    Daily(GroupOpts),
    /// Usage per ISO week (Monday start)
    Weekly(GroupOpts),
    /// Usage per month
    Monthly(GroupOpts),
    /// One row per session
    Session,
    /// Billing blocks per account (5-hour windows)
    Blocks(BlockFlags),
    /// Every account seen
    Accounts,
    /// Codex rate-limit windows
    Limits,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Claude,
    Codex,
    Antigravity,
    Gemini,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Asc,
    Desc,
}

/// `--token-limit`: a fixed number or `max` (the account's busiest block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenLimit {
    Max,
    Tokens(u64),
}

impl std::str::FromStr for TokenLimit {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        if s.eq_ignore_ascii_case("max") {
            return Ok(TokenLimit::Max);
        }
        s.replace(',', "")
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .map(TokenLimit::Tokens)
            .ok_or_else(|| format!("expected a positive number or \"max\", got {s:?}"))
    }
}

#[derive(Args, Debug)]
pub struct Options {
    /// First day to include (YYYY-MM-DD or YYYYMMDD, inclusive)
    #[arg(short = 's', long, global = true, value_name = "DATE")]
    pub since: Option<String>,
    /// Last day to include (YYYY-MM-DD or YYYYMMDD, inclusive)
    #[arg(short = 'u', long, global = true, value_name = "DATE")]
    pub until: Option<String>,
    /// Filter by label, email or id prefix (repeatable)
    #[arg(long, global = true, value_name = "LABEL")]
    pub account: Vec<String>,
    /// Only one tool
    #[arg(long, global = true, value_enum)]
    pub tool: Option<Tool>,
    /// Add one sub-row per model under each row
    #[arg(short = 'b', long, global = true)]
    pub breakdown: bool,
    /// Compact columns (Input and Output only); chosen automatically when the terminal is narrow
    #[arg(long, global = true, conflicts_with = "full")]
    pub compact: bool,
    /// Never switch to compact columns or drop columns; session shows full ids
    #[arg(long, global = true)]
    pub full: bool,
    /// Show only the most recent N days, weeks or months
    #[arg(long, global = true, value_name = "N")]
    pub last: Option<usize>,
    /// Sort order of buckets
    #[arg(short = 'o', long, global = true, value_enum, default_value = "asc")]
    pub order: Order,
    /// Bucket in UTC instead of local time
    #[arg(long, global = true)]
    pub utc: bool,
    /// Machine-readable JSON output
    #[arg(short = 'j', long, global = true)]
    pub json: bool,
    /// Hide the money column in tables
    #[arg(long, global = true)]
    pub no_cost: bool,
    /// Force colored output
    #[arg(long, global = true, conflicts_with = "no_color")]
    pub color: bool,
    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Print diagnostics
    #[arg(long, global = true)]
    pub verbose: bool,
    /// Claude config dir (repeatable)
    #[arg(long, global = true, value_name = "PATH")]
    pub claude_dir: Vec<PathBuf>,
    /// Codex dir (repeatable)
    #[arg(long, global = true, value_name = "PATH")]
    pub codex_dir: Vec<PathBuf>,
    /// Antigravity dir (repeatable)
    #[arg(long, global = true, value_name = "PATH")]
    pub antigravity_dir: Vec<PathBuf>,
    /// Gemini CLI dir, usually ~/.gemini (repeatable)
    #[arg(long, global = true, value_name = "PATH")]
    pub gemini_dir: Vec<PathBuf>,
}
