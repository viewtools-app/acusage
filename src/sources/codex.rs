//! Codex log parser. Read-only: files are only opened for reading.
//!
//! Flow: `default_dirs` -> `discover` -> `parse_file` per file (parallelisable)
//! -> `finalize` (response_id dedup, account attribution, latest limits).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::model::{AccountRef, AccountSource, LimitSnapshot, LimitWindow, UsageRecord};

const TOOL: &str = "codex";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: PathBuf,
}

/// `$CODEX_HOME` if set and non-empty, else `<home>/.codex`.
pub fn default_dirs(env_value: Option<&str>, home: &Path) -> Vec<PathBuf> {
    match env_value.map(str::trim) {
        Some(v) if !v.is_empty() => vec![PathBuf::from(v)],
        _ => vec![home.join(".codex")],
    }
}

/// Find `<dir>/{sessions,archived_sessions}/**/*.jsonl`. Returns the files (sorted) and the
/// number of directory entries that could not be read. A missing dir is not an error.
pub fn discover(dirs: &[PathBuf]) -> (Vec<SourceFile>, u64) {
    let mut files = Vec::new();
    let mut unreadable = 0;
    for dir in dirs {
        let mut found = Vec::new();
        for sub in ["sessions", "archived_sessions"] {
            let root = dir.join(sub);
            if !root.is_dir() {
                continue;
            }
            for entry in WalkDir::new(&root).follow_links(false) {
                match entry {
                    Ok(e) => {
                        if e.file_type().is_file()
                            && e.path().extension().is_some_and(|x| x == "jsonl")
                        {
                            found.push(e.into_path());
                        }
                    }
                    Err(_) => unreadable += 1,
                }
            }
        }
        found.sort();
        files.extend(found.into_iter().map(|path| SourceFile { path }));
    }
    (files, unreadable)
}

/// One usage line before dedup.
#[derive(Debug, Clone, PartialEq)]
pub struct RawRecord {
    pub ts: DateTime<Utc>,
    pub model: String,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub response_id: Option<String>,
    pub input: u64,
    pub output: u64,
    /// Part of `output`; shown separately, never added to totals.
    pub reasoning: u64,
    pub cache_write_5m: u64,
    pub cache_read: u64,
}

/// A `token_count` event that carried at least one non-null rate-limit window.
#[derive(Debug, Clone, PartialEq)]
pub struct RawLimit {
    pub observed_at: DateTime<Utc>,
    pub primary: Option<LimitWindow>,
    pub secondary: Option<LimitWindow>,
    pub plan_type: Option<String>,
}

#[derive(Debug, Default)]
pub struct ParsedFile {
    /// `creator_account_id` of the first `session_meta`; `None` means unknown.
    pub account_id: Option<String>,
    pub records: Vec<RawRecord>,
    /// Latest limit event in this file.
    pub limit: Option<RawLimit>,
    pub skipped_lines: u64,
    pub unreadable: bool,
}

#[derive(Deserialize)]
struct Head {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Envelope<P> {
    timestamp: Option<String>,
    payload: Option<P>,
}

#[derive(Deserialize)]
struct MetaPayload {
    id: Option<String>,
    session_id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    creator_account_id: Option<String>,
}

#[derive(Deserialize)]
struct TurnPayload {
    model: Option<String>,
    cwd: Option<String>,
}

#[derive(Deserialize, Clone, PartialEq, Default)]
struct Usage {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_output_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct UsagePayload {
    session_id: Option<String>,
    response_id: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct EventHead {
    payload: Option<EventKind>,
}

#[derive(Deserialize)]
struct EventKind {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct CountPayload {
    info: Option<Info>,
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
struct Info {
    total_token_usage: Option<Usage>,
    last_token_usage: Option<Usage>,
}

#[derive(Deserialize)]
struct RateLimits {
    primary: Option<Window>,
    secondary: Option<Window>,
    plan_type: Option<String>,
}

#[derive(Deserialize)]
struct Window {
    used_percent: Option<f64>,
    window_minutes: Option<f64>,
    resets_at: Option<f64>,
}

impl Window {
    fn convert(self) -> Option<LimitWindow> {
        Some(LimitWindow {
            used_percent: self.used_percent?,
            window_minutes: self.window_minutes.map(|m| m as u64),
            resets_at: self.resets_at.map(|r| r as i64),
        })
    }
}

fn parse_ts(t: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(t?)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Per-file parse state.
#[derive(Default)]
struct State {
    out: ParsedFile,
    model: Option<String>,
    meta_model: Option<String>,
    meta_session: Option<String>,
    seen_meta: bool,
    cwd: Option<String>,
    fallback: Vec<RawRecord>,
    has_usage_records: bool,
    prev_total: Option<Usage>,
}

impl State {
    fn record(
        &self,
        ts: DateTime<Utc>,
        session: Option<String>,
        id: Option<String>,
        u: &Usage,
    ) -> RawRecord {
        let cached = u.cached_input_tokens.unwrap_or(0);
        RawRecord {
            ts,
            model: self
                .model
                .clone()
                .or_else(|| self.meta_model.clone())
                .unwrap_or_else(|| "unknown".into()),
            session_id: session.or_else(|| self.meta_session.clone()),
            project: self.cwd.clone(),
            response_id: id,
            input: u.input_tokens.unwrap_or(0).saturating_sub(cached),
            output: u.output_tokens.unwrap_or(0),
            reasoning: u.reasoning_output_tokens.unwrap_or(0),
            cache_write_5m: u.cache_write_input_tokens.unwrap_or(0),
            cache_read: cached,
        }
    }

    fn line(&mut self, bytes: &[u8]) {
        let Ok(head) = serde_json::from_slice::<Head>(bytes) else {
            self.out.skipped_lines += 1;
            return;
        };
        let ok = match head.kind.as_deref() {
            Some("session_meta") => self.meta(bytes),
            Some("turn_context") => self.turn(bytes),
            Some("token_usage_record") => self.usage(bytes),
            Some("event_msg") => self.event(bytes),
            _ => true,
        };
        if !ok {
            self.out.skipped_lines += 1;
        }
    }

    fn meta(&mut self, bytes: &[u8]) -> bool {
        let Ok(Envelope {
            payload: Some(p), ..
        }) = serde_json::from_slice::<Envelope<MetaPayload>>(bytes)
        else {
            return false;
        };
        // Only the first meta describes this file; later ones (forked parents) are ignored.
        if !self.seen_meta {
            self.seen_meta = true;
            self.out.account_id = p.creator_account_id.filter(|s| !s.is_empty());
            self.meta_model = p.model;
            self.meta_session = p.session_id.or(p.id);
            self.cwd = self.cwd.take().or(p.cwd);
        }
        true
    }

    fn turn(&mut self, bytes: &[u8]) -> bool {
        let Ok(Envelope {
            payload: Some(p), ..
        }) = serde_json::from_slice::<Envelope<TurnPayload>>(bytes)
        else {
            return false;
        };
        if let Some(m) = p.model.filter(|m| !m.is_empty()) {
            self.model = Some(m);
        }
        if p.cwd.is_some() {
            self.cwd = p.cwd;
        }
        true
    }

    fn usage(&mut self, bytes: &[u8]) -> bool {
        let Ok(env) = serde_json::from_slice::<Envelope<UsagePayload>>(bytes) else {
            return false;
        };
        let (Some(ts), Some(p)) = (parse_ts(env.timestamp.as_deref()), env.payload) else {
            return false;
        };
        let Some(u) = p.usage else {
            return false;
        };
        self.has_usage_records = true;
        let r = self.record(ts, p.session_id, p.response_id, &u);
        self.out.records.push(r);
        true
    }

    fn event(&mut self, bytes: &[u8]) -> bool {
        match serde_json::from_slice::<EventHead>(bytes) {
            Ok(EventHead {
                payload: Some(EventKind { kind: Some(k) }),
            }) if k == "token_count" => {}
            Ok(_) => return true,
            Err(_) => return false,
        }
        let Ok(env) = serde_json::from_slice::<Envelope<CountPayload>>(bytes) else {
            return false;
        };
        let (Some(ts), Some(p)) = (parse_ts(env.timestamp.as_deref()), env.payload) else {
            return false;
        };
        if let Some(rl) = p.rate_limits {
            let primary = rl.primary.and_then(Window::convert);
            let secondary = rl.secondary.and_then(Window::convert);
            let newer = self.out.limit.as_ref().is_none_or(|l| ts >= l.observed_at);
            if (primary.is_some() || secondary.is_some()) && newer {
                self.out.limit = Some(RawLimit {
                    observed_at: ts,
                    primary,
                    secondary,
                    plan_type: rl.plan_type,
                });
            }
        }
        if let Some(info) = p.info {
            if info.total_token_usage.is_some() && info.total_token_usage == self.prev_total {
                return true;
            }
            if info.total_token_usage.is_some() {
                self.prev_total = info.total_token_usage;
            }
            if let Some(last) = info.last_token_usage {
                let mut r = self.record(ts, None, None, &last);
                // Same session archived next to the live copy: key on session + event time.
                r.response_id = r.session_id.as_ref().map(|sid| {
                    format!(
                        "fallback\u{1f}{sid}\u{1f}{}",
                        ts.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
                    )
                });
                self.fallback.push(r);
            }
        }
        true
    }
}

/// Stream one file line by line. Never panics; bad lines and unreadable files are counted.
pub fn parse_file(file: &SourceFile) -> ParsedFile {
    let mut st = State::default();
    let Ok(f) = File::open(&file.path) else {
        st.out.unreadable = true;
        return st.out;
    };
    let mut reader = BufReader::new(f);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => {
                st.out.unreadable = true;
                break;
            }
        }
        let line = buf.trim_ascii();
        if !line.is_empty() {
            st.line(line);
        }
    }
    if !st.has_usage_records {
        st.out.records = st.fallback;
    }
    st.out
}

#[derive(Debug, Default)]
pub struct Finalized {
    pub records: Vec<UsageRecord>,
    /// Latest snapshot per account, sorted by account id.
    pub limits: Vec<LimitSnapshot>,
    pub skipped_lines: u64,
    pub unreadable_files: u64,
    /// Usage records before cross-file dedup.
    pub records_before: u64,
}

fn account(id: Option<&String>) -> AccountRef {
    AccountRef {
        tool: TOOL.into(),
        id: id.cloned().unwrap_or_else(|| "unknown".into()),
        org_id: None,
        source: if id.is_some() {
            AccountSource::Owner
        } else {
            AccountSource::Unknown
        },
    }
}

/// Dedup by `response_id` across files (largest `output` wins; records without an id are
/// kept as they are), attribute accounts and pick the latest limits per account.
pub fn finalize(files: Vec<ParsedFile>) -> Finalized {
    let mut out = Finalized::default();
    let mut slots: Vec<(RawRecord, Option<String>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut limits: HashMap<Option<String>, RawLimit> = HashMap::new();
    for f in files {
        out.skipped_lines += f.skipped_lines;
        out.unreadable_files += u64::from(f.unreadable);
        if let Some(mut l) = f.limit {
            match limits.get_mut(&f.account_id) {
                Some(cur) if cur.observed_at > l.observed_at => {
                    // Keep the newest snapshot but remember any plan type seen.
                    if cur.plan_type.is_none() {
                        cur.plan_type = l.plan_type;
                    }
                }
                Some(cur) => {
                    if l.plan_type.is_none() {
                        l.plan_type = cur.plan_type.take();
                    }
                    *cur = l;
                }
                None => {
                    limits.insert(f.account_id.clone(), l);
                }
            }
        }
        out.records_before += f.records.len() as u64;
        for r in f.records {
            let Some(id) = r.response_id.clone() else {
                slots.push((r, f.account_id.clone()));
                continue;
            };
            match index.get(&id) {
                Some(&i) => {
                    if r.output > slots[i].0.output {
                        slots[i] = (r, f.account_id.clone());
                    }
                }
                None => {
                    index.insert(id, slots.len());
                    slots.push((r, f.account_id.clone()));
                }
            }
        }
    }
    for (r, acct) in slots {
        out.records.push(UsageRecord {
            ts: r.ts,
            tool: TOOL.into(),
            account: account(acct.as_ref()),
            model: r.model,
            session_id: r.session_id,
            project: r.project,
            input: r.input,
            output: r.output,
            reasoning: r.reasoning,
            cache_write_5m: r.cache_write_5m,
            cache_write_1h: 0,
            cache_read: r.cache_read,
            dedup_key: r.response_id,
        });
    }
    let mut snaps: Vec<LimitSnapshot> = limits
        .into_iter()
        .map(|(id, l)| LimitSnapshot {
            tool: TOOL.into(),
            account: account(id.as_ref()),
            primary: l.primary,
            secondary: l.secondary,
            plan_type: l.plan_type,
            observed_at: l.observed_at,
        })
        .collect();
    snaps.sort_by(|a, b| a.account.id.cmp(&b.account.id));
    out.limits = snaps;
    out
}
