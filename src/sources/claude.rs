//! Claude Code log parser. Read-only: files are only opened for reading.
//!
//! Flow: `default_dirs` -> `discover` -> `parse_file` per file (parallelisable)
//! -> `Profiles::load` -> `finalize` (owner map, profile fallback, dedup).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::model::{AccountRef, AccountSource, UsageRecord};

const TOOL: &str = "claude";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: PathBuf,
    /// The config dir this log was found under (e.g. `~/.claude-work`).
    pub profile_dir: PathBuf,
}

/// Config dirs: `$CLAUDE_CONFIG_DIR` (comma separated) if set and non-empty, else
/// `<home>/.claude` plus every `<home>/.claude-*` directory.
pub fn default_dirs(env_value: Option<&str>, home: &Path) -> Vec<PathBuf> {
    if let Some(v) = env_value {
        let dirs: Vec<PathBuf> = v
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        if !dirs.is_empty() {
            return dirs;
        }
    }
    let mut dirs = Vec::new();
    let main = home.join(".claude");
    if main.is_dir() {
        dirs.push(main);
    }
    let mut extra: Vec<PathBuf> = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".claude-"))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    extra.sort();
    dirs.extend(extra);
    dirs
}

/// Find `<dir>/projects/**/*.jsonl`. Returns the files (sorted, deterministic) and the
/// number of directory entries that could not be read. A missing dir is not an error.
pub fn discover(dirs: &[PathBuf]) -> (Vec<SourceFile>, u64) {
    let mut files = Vec::new();
    let mut unreadable = 0;
    for dir in dirs {
        let root = dir.join("projects");
        if !root.is_dir() {
            continue;
        }
        let mut found = Vec::new();
        for entry in WalkDir::new(&root).follow_links(false) {
            match entry {
                Ok(e) => {
                    if e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl")
                    {
                        found.push(e.into_path());
                    }
                }
                Err(_) => unreadable += 1,
            }
        }
        found.sort();
        files.extend(found.into_iter().map(|path| SourceFile {
            path,
            profile_dir: dir.clone(),
        }));
    }
    (files, unreadable)
}

/// One usage line before account attribution and dedup.
#[derive(Debug, Clone, PartialEq)]
pub struct RawRecord {
    pub ts: DateTime<Utc>,
    pub model: String,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub message_id: Option<String>,
    pub request_id: Option<String>,
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerEntry {
    pub session_id: String,
    pub account_uuid: String,
    pub org_uuid: Option<String>,
}

#[derive(Debug, Default)]
pub struct ParsedFile {
    pub profile_dir: PathBuf,
    pub records: Vec<RawRecord>,
    pub owners: Vec<OwnerEntry>,
    /// Lines that were not valid JSON, or relevant records with unusable fields.
    pub skipped_lines: u64,
    pub unreadable: bool,
}

#[derive(Deserialize)]
struct Head {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct OwnerLine {
    #[serde(rename = "accountUuid")]
    account_uuid: Option<String>,
    #[serde(rename = "organizationUuid")]
    org_uuid: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct AssistantLine {
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize)]
struct CacheCreation {
    ephemeral_1h_input_tokens: Option<u64>,
    ephemeral_5m_input_tokens: Option<u64>,
}

enum Line {
    Ignored,
    Usage(Box<RawRecord>),
    Owner(OwnerEntry),
    Bad,
}

fn parse_line(bytes: &[u8]) -> Line {
    let Ok(head) = serde_json::from_slice::<Head>(bytes) else {
        return Line::Bad;
    };
    match head.kind.as_deref() {
        Some("assistant") => match serde_json::from_slice::<AssistantLine>(bytes) {
            Ok(l) => assistant(l),
            Err(_) => Line::Bad,
        },
        Some("session-owner") => match serde_json::from_slice::<OwnerLine>(bytes) {
            Ok(OwnerLine {
                account_uuid: Some(account_uuid),
                org_uuid,
                session_id: Some(session_id),
            }) => Line::Owner(OwnerEntry {
                session_id,
                account_uuid,
                org_uuid,
            }),
            _ => Line::Bad,
        },
        _ => Line::Ignored,
    }
}

fn assistant(l: AssistantLine) -> Line {
    let Some(msg) = l.message else {
        return Line::Ignored;
    };
    let Some(usage) = msg.usage else {
        return Line::Ignored;
    };
    let Some(model) = msg.model else {
        return Line::Bad;
    };
    if model == "<synthetic>" {
        return Line::Ignored;
    }
    let Some(ts) = l
        .timestamp
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
    else {
        return Line::Bad;
    };
    let total_write = usage.cache_creation_input_tokens.unwrap_or(0);
    let (w5, w1) = match usage.cache_creation {
        Some(c) => {
            let (a, b) = (
                c.ephemeral_5m_input_tokens.unwrap_or(0),
                c.ephemeral_1h_input_tokens.unwrap_or(0),
            );
            if a.saturating_add(b) > 0 {
                (a, b)
            } else {
                (total_write, 0)
            }
        }
        None => (total_write, 0),
    };
    Line::Usage(Box::new(RawRecord {
        ts: ts.with_timezone(&Utc),
        model,
        session_id: l.session_id,
        project: l.cwd,
        message_id: msg.id,
        request_id: l.request_id,
        input: usage.input_tokens.unwrap_or(0),
        output: usage.output_tokens.unwrap_or(0),
        cache_write_5m: w5,
        cache_write_1h: w1,
        cache_read: usage.cache_read_input_tokens.unwrap_or(0),
    }))
}

/// Stream one file line by line. Never panics; bad lines and unreadable files are counted.
pub fn parse_file(file: &SourceFile) -> ParsedFile {
    let mut out = ParsedFile {
        profile_dir: file.profile_dir.clone(),
        ..Default::default()
    };
    let Ok(f) = File::open(&file.path) else {
        out.unreadable = true;
        return out;
    };
    let mut reader = BufReader::new(f);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => {
                out.unreadable = true;
                break;
            }
        }
        let line = buf.trim_ascii();
        if line.is_empty() {
            continue;
        }
        match parse_line(line) {
            Line::Ignored => {}
            Line::Usage(r) => out.records.push(*r),
            Line::Owner(o) => out.owners.push(o),
            Line::Bad => out.skipped_lines += 1,
        }
    }
    out
}

/// The only fields read from `oauthAccount` in `.claude.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OAuthAccount {
    #[serde(rename = "accountUuid")]
    pub account_uuid: String,
    #[serde(rename = "emailAddress")]
    pub email: Option<String>,
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(rename = "organizationUuid")]
    pub org_uuid: Option<String>,
    #[serde(rename = "organizationName")]
    pub org_name: Option<String>,
}

#[derive(Deserialize)]
struct ClaudeJson {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<OAuthAccount>,
}

/// `<home>/.claude.json` for the default dir `<home>/.claude`, else `<dir>/.claude.json`.
pub fn claude_json_path(dir: &Path, home: &Path) -> PathBuf {
    if is_default_dir(dir, home) {
        home.join(".claude.json")
    } else {
        dir.join(".claude.json")
    }
}

fn is_default_dir(dir: &Path, home: &Path) -> bool {
    dir == home.join(".claude")
}

/// Parse the `oauthAccount` object only. Missing/unreadable/invalid file gives `None`.
pub fn read_oauth(path: &Path) -> Option<OAuthAccount> {
    let f = File::open(path).ok()?;
    serde_json::from_reader::<_, ClaudeJson>(BufReader::new(f))
        .ok()?
        .oauth_account
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLabel {
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub org_uuid: Option<String>,
    pub org_name: Option<String>,
}

#[derive(Debug, Default)]
pub struct Profiles {
    /// Current account per config dir.
    pub current: HashMap<PathBuf, OAuthAccount>,
    /// account_uuid -> label.
    pub labels: HashMap<String, AccountLabel>,
}

impl Profiles {
    pub fn load(dirs: &[PathBuf], home: &Path) -> Self {
        let mut p = Profiles::default();
        for dir in dirs {
            if let Some(o) = read_oauth(&claude_json_path(dir, home)) {
                p.labels.insert(
                    o.account_uuid.clone(),
                    AccountLabel {
                        email: o.email.clone(),
                        display_name: o.display_name.clone(),
                        org_uuid: o.org_uuid.clone(),
                        org_name: o.org_name.clone(),
                    },
                );
                p.current.insert(dir.clone(), o);
            }
        }
        p
    }
}

#[derive(Debug, Default)]
pub struct Finalized {
    pub records: Vec<UsageRecord>,
    pub skipped_lines: u64,
    pub unreadable_files: u64,
    /// Usage records before dedup.
    pub records_before: u64,
}

/// Apply the global session-owner map, the profile fallback and dedup.
/// Dedup keeps the record with the largest `output` per `(message.id, requestId)`;
/// records without a message id are kept as they are.
pub fn finalize(files: Vec<ParsedFile>, profiles: &Profiles, home: &Path) -> Finalized {
    let mut out = Finalized::default();
    let mut owners: HashMap<String, OwnerEntry> = HashMap::new();
    for f in &files {
        for o in &f.owners {
            owners
                .entry(o.session_id.clone())
                .or_insert_with(|| o.clone());
        }
    }

    let mut slots: Vec<(RawRecord, PathBuf)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for f in files {
        out.skipped_lines += f.skipped_lines;
        out.unreadable_files += u64::from(f.unreadable);
        out.records_before += f.records.len() as u64;
        for r in f.records {
            let Some(mid) = &r.message_id else {
                slots.push((r, f.profile_dir.clone()));
                continue;
            };
            let key = format!("{mid}\u{1f}{}", r.request_id.as_deref().unwrap_or(""));
            match index.get(&key) {
                Some(&i) => {
                    if r.output > slots[i].0.output {
                        slots[i] = (r, f.profile_dir.clone());
                    }
                }
                None => {
                    index.insert(key, slots.len());
                    slots.push((r, f.profile_dir.clone()));
                }
            }
        }
    }

    for (r, dir) in slots {
        let account = attribute(&r, &dir, &owners, profiles, home);
        let dedup_key = r
            .message_id
            .as_ref()
            .map(|m| format!("{m}\u{1f}{}", r.request_id.as_deref().unwrap_or("")));
        out.records.push(UsageRecord {
            ts: r.ts,
            tool: TOOL.into(),
            account,
            model: r.model,
            session_id: r.session_id,
            project: r.project,
            input: r.input,
            output: r.output,
            reasoning: 0,
            cache_write_5m: r.cache_write_5m,
            cache_write_1h: r.cache_write_1h,
            cache_read: r.cache_read,
            dedup_key,
        });
    }
    out
}

fn attribute(
    r: &RawRecord,
    dir: &Path,
    owners: &HashMap<String, OwnerEntry>,
    profiles: &Profiles,
    home: &Path,
) -> AccountRef {
    let make = |id: &str, org: Option<&String>, source| AccountRef {
        tool: TOOL.into(),
        id: id.into(),
        org_id: org.cloned(),
        source,
    };
    if let Some(o) = r.session_id.as_ref().and_then(|s| owners.get(s)) {
        return make(&o.account_uuid, o.org_uuid.as_ref(), AccountSource::Owner);
    }
    if !is_default_dir(dir, home)
        && let Some(c) = profiles.current.get(dir)
    {
        return make(&c.account_uuid, c.org_uuid.as_ref(), AccountSource::Profile);
    }
    make("unknown", None, AccountSource::Unknown)
}
