//! Gemini CLI parser. Read-only: files are only opened for reading.
//!
//! Format (from Gemini CLI's documented chat recording, not yet verified on real logs):
//! `<dir>/tmp/<projectHash>/chats/session-*.json` is one object
//! `{sessionId, projectHash, startTime, lastUpdated, messages: [...]}`; the `.jsonl` variant is a
//! header line with `sessionId` followed by one message per line. A message with
//! `type: "gemini"` and a `tokens` object is one model request:
//! `{id, timestamp, type, model, tokens: {input, output, cached, thoughts, tool, total}}`.
//!
//! Mapping: Gemini's `input` includes the cached part and `output` excludes thinking, so
//! input = input - cached + tool, cache read = cached, output = output + thoughts (the thoughts
//! count is also kept as `reasoning`, a display-only part of output). Only the `tmp/*/chats`
//! tree is read: the Antigravity directories next to it and every auth file are never opened.
//! The logs hold no account record, so every record is attributed to `unknown`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::model::{AccountRef, AccountSource, UsageRecord};

const TOOL: &str = "gemini";
/// A session file larger than this is not read.
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: PathBuf,
    pub jsonl: bool,
}

/// `<home>/.gemini`.
pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    vec![home.join(".gemini")]
}

/// Find `<dir>/tmp/<hash>/chats/session-*.json[l]`. Returns the files (sorted) and the number of
/// directory entries that could not be read. A missing dir is not an error.
pub fn discover(dirs: &[PathBuf]) -> (Vec<SourceFile>, u64) {
    let mut files = Vec::new();
    let mut unreadable = 0;
    for dir in dirs {
        let Ok(projects) = std::fs::read_dir(dir.join("tmp")) else {
            continue;
        };
        let mut found = Vec::new();
        for p in projects {
            let Ok(p) = p else {
                unreadable += 1;
                continue;
            };
            let Ok(chats) = std::fs::read_dir(p.path().join("chats")) else {
                continue;
            };
            for e in chats {
                let Ok(e) = e else {
                    unreadable += 1;
                    continue;
                };
                let path = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                let jsonl = name.ends_with(".jsonl");
                if name.starts_with("session-")
                    && (jsonl || name.ends_with(".json"))
                    && path.is_file()
                {
                    found.push(SourceFile { path, jsonl });
                }
            }
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        files.extend(found);
    }
    (files, unreadable)
}

/// One usage message before cross-file dedup.
#[derive(Debug, Clone, PartialEq)]
pub struct RawRecord {
    pub ts: DateTime<Utc>,
    pub model: String,
    pub session_id: String,
    pub message_id: Option<String>,
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
}

#[derive(Debug, Default)]
pub struct ParsedFile {
    pub records: Vec<RawRecord>,
    pub skipped_lines: u64,
    pub unreadable: bool,
}

#[derive(Deserialize, Default)]
struct Tokens {
    input: Option<u64>,
    output: Option<u64>,
    cached: Option<u64>,
    thoughts: Option<u64>,
    tool: Option<u64>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    tokens: Option<Tokens>,
}

#[derive(Deserialize)]
struct Session {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    messages: Option<Vec<serde_json::Value>>,
}

/// Parse one session file. Messages that are not model requests with tokens are other events and
/// are ignored; malformed messages are counted. Never panics.
pub fn parse_file(file: &SourceFile) -> ParsedFile {
    let mut out = ParsedFile::default();
    let too_big = std::fs::metadata(&file.path).is_ok_and(|m| m.len() > MAX_FILE_BYTES);
    let bytes = if too_big {
        None
    } else {
        std::fs::read(&file.path).ok()
    };
    let Some(bytes) = bytes else {
        out.unreadable = true;
        return out;
    };
    let stem = file
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut session_id: Option<String> = None;
    let mut values: Vec<serde_json::Value> = Vec::new();
    if file.jsonl {
        for line in bytes.split(|b| *b == b'\n') {
            let line = line.trim_ascii();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice::<serde_json::Value>(line) {
                Ok(v) if v.get("type").is_none() && v.get("sessionId").is_some() => {
                    if let Ok(h) = serde_json::from_value::<Session>(v) {
                        session_id = session_id.or(h.session_id);
                    }
                }
                Ok(v) => values.push(v),
                Err(_) => out.skipped_lines += 1,
            }
        }
    } else {
        match serde_json::from_slice::<Session>(&bytes) {
            Ok(s) => {
                session_id = s.session_id;
                values = s.messages.unwrap_or_default();
            }
            Err(_) => {
                out.skipped_lines += 1;
                return out;
            }
        }
    }
    let session_id = session_id.unwrap_or(stem);
    for v in values {
        if v.get("type").and_then(|t| t.as_str()) != Some("gemini") || v.get("tokens").is_none() {
            continue;
        }
        let Ok(m) = serde_json::from_value::<Message>(v) else {
            out.skipped_lines += 1;
            continue;
        };
        let ts = m
            .timestamp
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|d| d.with_timezone(&Utc));
        let (Some(ts), Some(t), Some("gemini")) = (ts, m.tokens, m.kind.as_deref()) else {
            out.skipped_lines += 1;
            continue;
        };
        let cached = t.cached.unwrap_or(0);
        let thoughts = t.thoughts.unwrap_or(0);
        out.records.push(RawRecord {
            ts,
            model: m.model.unwrap_or_else(|| "unknown".into()),
            session_id: session_id.clone(),
            message_id: m.id,
            input: t
                .input
                .unwrap_or(0)
                .saturating_sub(cached)
                .saturating_add(t.tool.unwrap_or(0)),
            output: t.output.unwrap_or(0).saturating_add(thoughts),
            reasoning: thoughts,
            cache_read: cached,
        });
    }
    out
}

#[derive(Debug, Default)]
pub struct Finalized {
    pub records: Vec<UsageRecord>,
    pub skipped_lines: u64,
    pub unreadable_files: u64,
    /// Usage records before cross-file dedup.
    pub records_before: u64,
}

fn account() -> AccountRef {
    AccountRef {
        tool: TOOL.into(),
        id: "unknown".into(),
        org_id: None,
        source: AccountSource::Unknown,
    }
}

/// Dedup by `(session, message id)` across files (a session saved twice), attribute every
/// record to the unknown account.
pub fn finalize(files: Vec<ParsedFile>) -> Finalized {
    let mut out = Finalized::default();
    let mut seen: HashSet<String> = HashSet::new();
    for f in files {
        out.skipped_lines += f.skipped_lines;
        out.unreadable_files += u64::from(f.unreadable);
        out.records_before += f.records.len() as u64;
        for r in f.records {
            let key = r
                .message_id
                .as_ref()
                .map(|m| format!("{}:{m}", r.session_id));
            if let Some(k) = &key
                && !seen.insert(k.clone())
            {
                continue;
            }
            out.records.push(UsageRecord {
                ts: r.ts,
                tool: TOOL.into(),
                account: account(),
                model: r.model,
                session_id: Some(r.session_id),
                project: None,
                input: r.input,
                output: r.output,
                reasoning: r.reasoning,
                cache_write_5m: 0,
                cache_write_1h: 0,
                cache_read: r.cache_read,
                dedup_key: key,
            });
        }
    }
    out
}
