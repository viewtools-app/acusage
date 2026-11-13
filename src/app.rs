//! End-to-end glue: sources -> config -> prices -> report. Rendering lives elsewhere.

use std::path::PathBuf;

use std::time::Instant;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use rayon::prelude::*;
use serde::Serialize;

use crate::blocks::{self, BlockOpts, BlocksReport};
use crate::cli::{Cli, Command, GroupOpts};
use crate::config;
use crate::model::{AccountSource, UsageRecord};
use crate::pricing::PriceTable;
use crate::report::{
    self, AccountsReport, Bucket, Labeler, LimitRow, Options, SessionReport, UsageReport, Zone,
};
use crate::sources::{antigravity, claude, codex, gemini};

/// Bad command line values (maps to exit code 2).
#[derive(Debug)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

/// Process environment, injectable for tests.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub claude_config_dir: Option<String>,
    pub codex_home: Option<String>,
    pub acusage_config: Option<String>,
    /// Clock, injectable so output is deterministic in tests.
    pub now: DateTime<Utc>,
}

impl Env {
    pub fn from_process() -> Self {
        Env {
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            claude_config_dir: std::env::var("CLAUDE_CONFIG_DIR").ok(),
            codex_home: std::env::var("CODEX_HOME").ok(),
            acusage_config: std::env::var("ACUSAGE_CONFIG").ok(),
            now: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Body {
    Usage(UsageReport),
    Blocks(BlocksReport),
    Session(SessionReport),
    Accounts(AccountsReport),
    Limits(Vec<LimitRow>),
}

/// Per-source counters for `--verbose`. Counts only, never line contents.
#[derive(Debug, Clone)]
pub struct SourceStats {
    pub source: &'static str,
    pub files: u64,
    pub unreadable_files: u64,
    pub skipped_lines: u64,
    pub records_before_dedup: u64,
    pub records_after_dedup: u64,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone)]
pub struct Output {
    /// Report name: daily, weekly, monthly, blocks, session, accounts or limits.
    pub name: &'static str,
    pub body: Body,
    pub generated_at: DateTime<Utc>,
    pub skipped_lines: u64,
    pub unreadable_files: u64,
    pub stats: Vec<SourceStats>,
    /// Config file path, shown in the unpriced-usage note.
    pub config_path: String,
    /// Usage with no account record in its log (`None` when there is none).
    pub unknown: Option<UnknownInfo>,
    /// Records per attribution source, per agent (`--verbose`).
    pub attribution: Vec<Attribution>,
    /// A bare run: the report covers the last 30 days only.
    pub default_window: bool,
    pub query: Query,
}

/// What the user asked for, kept for the title box.
#[derive(Debug, Clone)]
pub struct Query {
    /// Explicit `--since` / `--until` only (the default window is `default_window`).
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
    pub last: Option<usize>,
    pub breakdown: bool,
    pub instances: bool,
    pub tool: Option<&'static str>,
    pub accounts: Vec<String>,
    pub project: Option<String>,
    /// First and last day with usage in what is shown (`None` when nothing is shown).
    pub period: Option<(NaiveDate, NaiveDate)>,
}

#[derive(Debug, Clone)]
pub struct UnknownInfo {
    pub sessions: u64,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Attribution {
    pub tool: String,
    pub owner: u64,
    pub profile: u64,
    pub unknown: u64,
}

fn unknown_info(records: &[UsageRecord]) -> Option<UnknownInfo> {
    let unk: Vec<&UsageRecord> = records
        .iter()
        .filter(|r| r.account.source == AccountSource::Unknown)
        .collect();
    let first = unk.iter().map(|r| r.ts).min()?;
    let last = unk.iter().map(|r| r.ts).max()?;
    let sessions: std::collections::BTreeSet<(&str, &str)> = unk
        .iter()
        .map(|r| (r.tool.as_str(), r.session_id.as_deref().unwrap_or("")))
        .collect();
    Some(UnknownInfo {
        sessions: sessions.len() as u64,
        first,
        last,
    })
}

fn attribution(records: &[UsageRecord]) -> Vec<Attribution> {
    let mut v: Vec<Attribution> = vec![];
    for r in records {
        let i = match v.iter().position(|a| a.tool == r.tool) {
            Some(i) => i,
            None => {
                v.push(Attribution {
                    tool: r.tool.clone(),
                    owner: 0,
                    profile: 0,
                    unknown: 0,
                });
                v.len() - 1
            }
        };
        match r.account.source {
            AccountSource::Owner => v[i].owner += 1,
            AccountSource::Profile => v[i].profile += 1,
            AccountSource::Unknown => v[i].unknown += 1,
        }
    }
    v.sort_by(|a, b| a.tool.cmp(&b.tool));
    v
}

/// `YYYY-MM-DD` or `YYYYMMDD`.
pub fn parse_date_str(s: &str) -> Option<NaiveDate> {
    let fmt = if s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit()) {
        "%Y%m%d"
    } else {
        "%Y-%m-%d"
    };
    NaiveDate::parse_from_str(s, fmt).ok()
}

/// 8 digits or `NNNN-NN-NN`: the right shape, whatever the numbers are.
fn well_formed(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() == 8 && b.iter().all(u8::is_ascii_digit))
        || (b.len() == 10
            && b.iter().enumerate().all(|(i, c)| {
                if i == 4 || i == 7 {
                    *c == b'-'
                } else {
                    c.is_ascii_digit()
                }
            }))
}

fn parse_date(flag: &str, v: &Option<String>) -> Result<Option<NaiveDate>> {
    v.as_deref()
        .map(|s| {
            parse_date_str(s).ok_or_else(|| {
                UsageError(if well_formed(s) {
                    format!("{flag}: invalid date {s}")
                } else {
                    format!("{flag}: expected YYYY-MM-DD or YYYYMMDD, got {s:?}")
                })
                .into()
            })
        })
        .transpose()
}

/// Every `--account` text must match some account seen in the logs.
fn check_accounts(
    o: &Options,
    records: &[UsageRecord],
    limits: &[crate::model::LimitSnapshot],
    labeler: &Labeler,
) -> Result<()> {
    let seen = || {
        records
            .iter()
            .map(|r| &r.account)
            .chain(limits.iter().map(|l| &l.account))
    };
    for w in &o.accounts {
        if seen().any(|a| labeler.matches(a, w)) {
            continue;
        }
        let mut known: Vec<String> = seen().map(|a| labeler.label(a).0).collect();
        known.sort();
        known.dedup();
        let known = if known.is_empty() {
            "none".to_string()
        } else {
            known.join(", ")
        };
        anyhow::bail!("No usage for account {w:?}. Known: {known}");
    }
    Ok(())
}

/// First and last day of the usage shown. `--last N` keeps the newest N buckets, as the
/// usage reports do.
fn period(shown: &[UsageRecord], command: &Command, o: &Options) -> Option<(NaiveDate, NaiveDate)> {
    let bucket = match command {
        Command::Daily(_) => Some(Bucket::Day),
        Command::Weekly(_) => Some(Bucket::Week),
        Command::Monthly(_) => Some(Bucket::Month),
        Command::Limits => return None,
        _ => None,
    };
    let mut days: Vec<NaiveDate> = shown.iter().map(|r| o.zone.date(r.ts)).collect();
    if let (Some(b), Some(n)) = (bucket, o.last) {
        let keys: std::collections::BTreeSet<String> = days.iter().map(|d| b.key(*d)).collect();
        let keep: std::collections::HashSet<String> = keys.into_iter().rev().take(n).collect();
        days.retain(|d| keep.contains(&b.key(*d)));
    }
    Some((*days.iter().min()?, *days.iter().max()?))
}

pub fn run(cli: &Cli, env: &Env) -> Result<Output> {
    let o = &cli.opts;
    let (group, blk) = match &cli.command {
        Some(Command::Daily(g) | Command::Weekly(g) | Command::Monthly(g)) => (g.clone(), None),
        Some(Command::Blocks(b)) => (GroupOpts::default(), Some(b.clone())),
        _ => (GroupOpts::default(), None),
    };
    let group = GroupOpts {
        instances: group.instances || cli.group.instances,
        project: group.project.or_else(|| cli.group.project.clone()),
    };
    let daily = Command::Daily(GroupOpts::default());
    let command = cli.command.as_ref().unwrap_or(&daily);
    let zone = if o.utc { Zone::Utc } else { Zone::Local };
    let mut since = parse_date("--since", &o.since)?;
    let until = parse_date("--until", &o.until)?;
    if let (Some(s), Some(u)) = (since, until)
        && s > u
    {
        return Err(UsageError(format!("--since {s} is after --until {u}")).into());
    }
    if o.last == Some(0) {
        return Err(UsageError("--last: 0 is not a positive number".into()).into());
    }
    // A bare `acusage` shows the last 30 days; any date flag or a subcommand asks for more.
    let default_window =
        cli.command.is_none() && since.is_none() && until.is_none() && o.last.is_none();
    if default_window {
        since = Some(zone.date(env.now) - chrono::Duration::days(29));
    }
    let options = Options {
        zone,
        since,
        until,
        accounts: o.account.clone(),
        tool: o.tool,
        breakdown: o.breakdown,
        last: o.last,
        order: o.order,
        instances: group.instances,
        project: group.project.clone(),
    };

    let cfg_path = config::config_path(env.acusage_config.as_deref(), &env.home);
    let cfg = config::load(&cfg_path)?;
    let prices = PriceTable::builtin().with_overrides(cfg.prices.clone());

    let claude_dirs = if o.claude_dir.is_empty() {
        claude::default_dirs(env.claude_config_dir.as_deref(), &env.home)
    } else {
        o.claude_dir.clone()
    };
    let codex_dirs = if o.codex_dir.is_empty() {
        codex::default_dirs(env.codex_home.as_deref(), &env.home)
    } else {
        o.codex_dir.clone()
    };
    let agy_dirs = if o.antigravity_dir.is_empty() {
        antigravity::default_dirs(&env.home)
    } else {
        o.antigravity_dir.clone()
    };
    let gemini_dirs = if o.gemini_dir.is_empty() {
        gemini::default_dirs(&env.home)
    } else {
        o.gemini_dir.clone()
    };
    let t = Instant::now();
    let (cfiles, cunread) = claude::discover(&claude_dirs);
    // Parse is per file and pure; collect keeps file order, so output equals the sequential run.
    let parsed: Vec<_> = cfiles.par_iter().map(claude::parse_file).collect();
    let profiles = claude::Profiles::load(&claude_dirs, &env.home);
    let cl = claude::finalize(parsed, &profiles, &env.home);
    let claude_stats = SourceStats {
        source: "claude",
        files: cfiles.len() as u64,
        unreadable_files: cl.unreadable_files + cunread,
        skipped_lines: cl.skipped_lines,
        records_before_dedup: cl.records_before,
        records_after_dedup: cl.records.len() as u64,
        elapsed_ms: t.elapsed().as_millis(),
    };
    let t = Instant::now();
    let (xfiles, xunread) = codex::discover(&codex_dirs);
    let xl = codex::finalize(xfiles.par_iter().map(codex::parse_file).collect());
    let codex_stats = SourceStats {
        source: "codex",
        files: xfiles.len() as u64,
        unreadable_files: xl.unreadable_files + xunread,
        skipped_lines: xl.skipped_lines,
        records_before_dedup: xl.records_before,
        records_after_dedup: xl.records.len() as u64,
        elapsed_ms: t.elapsed().as_millis(),
    };
    let t = Instant::now();
    let (afiles, aunread) = antigravity::discover(&agy_dirs);
    let al = antigravity::finalize(afiles.par_iter().map(antigravity::parse_file).collect());
    let agy_stats = SourceStats {
        source: "antigravity",
        files: afiles.len() as u64,
        unreadable_files: al.unreadable_files + aunread,
        skipped_lines: al.skipped_lines,
        records_before_dedup: al.records_before,
        records_after_dedup: al.records.len() as u64,
        elapsed_ms: t.elapsed().as_millis(),
    };
    let t = Instant::now();
    let (gfiles, gunread) = gemini::discover(&gemini_dirs);
    let gl = gemini::finalize(gfiles.par_iter().map(gemini::parse_file).collect());
    let gemini_stats = SourceStats {
        source: "gemini",
        files: gfiles.len() as u64,
        unreadable_files: gl.unreadable_files + gunread,
        skipped_lines: gl.skipped_lines,
        records_before_dedup: gl.records_before,
        records_after_dedup: gl.records.len() as u64,
        elapsed_ms: t.elapsed().as_millis(),
    };

    let mut records: Vec<UsageRecord> = cl.records;
    records.extend(xl.records);
    records.extend(al.records);
    records.extend(gl.records);
    let skipped_lines = cl.skipped_lines + xl.skipped_lines + al.skipped_lines + gl.skipped_lines;
    let unreadable_files = cl.unreadable_files
        + xl.unreadable_files
        + al.unreadable_files
        + gl.unreadable_files
        + cunread
        + xunread
        + aunread
        + gunread;

    let labeler = Labeler::new(&profiles.labels, &cfg.aliases).with_plans(&xl.limits);
    check_accounts(&options, &records, &xl.limits, &labeler)?;
    let (name, body) = match command {
        Command::Daily(_) => (
            "daily",
            Body::Usage(report::usage_report(
                &records,
                &labeler,
                &prices,
                Bucket::Day,
                &options,
            )),
        ),
        Command::Weekly(_) => (
            "weekly",
            Body::Usage(report::usage_report(
                &records,
                &labeler,
                &prices,
                Bucket::Week,
                &options,
            )),
        ),
        Command::Monthly(_) => (
            "monthly",
            Body::Usage(report::usage_report(
                &records,
                &labeler,
                &prices,
                Bucket::Month,
                &options,
            )),
        ),
        Command::Blocks(_) => (
            "blocks",
            Body::Blocks(blocks::blocks_report(
                &records,
                &labeler,
                &prices,
                &options,
                &BlockOpts {
                    active: blk.as_ref().is_some_and(|b| b.active),
                    recent: blk.as_ref().is_some_and(|b| b.recent),
                    token_limit: blk.as_ref().and_then(|b| b.token_limit),
                    session_hours: blk.as_ref().map_or(5, |b| b.session_length),
                    now: env.now,
                },
            )),
        ),
        Command::Session => (
            "session",
            Body::Session(report::session_report(
                &records, &labeler, &prices, &options,
            )),
        ),
        Command::Accounts => (
            "accounts",
            Body::Accounts(report::accounts_report(
                &records, &labeler, &prices, &options,
            )),
        ),
        Command::Limits => (
            "limits",
            Body::Limits(report::limits_report(&xl.limits, &labeler, &options)),
        ),
    };
    let shown: Vec<UsageRecord> = report::filter(&records, &labeler, &options)
        .into_iter()
        .cloned()
        .collect();
    let unknown = unknown_info(&shown);
    let query = Query {
        since: parse_date("--since", &o.since)?,
        until,
        last: o.last,
        breakdown: o.breakdown,
        instances: group.instances,
        tool: o.tool.map(report::tool_name),
        accounts: o.account.clone(),
        project: group.project.clone(),
        period: period(&shown, command, &options),
    };
    let attribution = attribution(&records);
    Ok(Output {
        name,
        body,
        generated_at: env.now,
        skipped_lines,
        unreadable_files,
        stats: vec![claude_stats, codex_stats, agy_stats, gemini_stats],
        config_path: cfg_path.display().to_string(),
        unknown,
        attribution,
        default_window,
        query,
    })
}
