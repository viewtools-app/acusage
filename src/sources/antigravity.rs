//! Antigravity CLI (`agy`) parser. Read-only: files are only opened for reading.
//!
//! Tokens come from `brain/<session>/.system_generated/logs/transcript.jsonl`: one record per
//! model request with `input_tokens` (uncached), `output_tokens` (includes reasoning) and
//! `cache_read_tokens` (separate from input). The model name is not in the JSONL; it is in the
//! session's `conversations/<session>.db` (SQLite, table `gen_metadata`, protobuf `data`). Each row
//! carries a model code (field 1 -> repeated field 20 entry named `model_enum`) and sometimes the
//! model id itself (field 3 -> field 28). A code seen with exactly one id in the same database
//! names every row with that code. A row also carries its request's usage (field 1 -> field 4:
//! input, output, cache read as fields 2, 3, 5), so requests are matched to rows in order by
//! `(input, output, cache_read)`: rows without a request (all-zero rows) are skipped and a request
//! without a matching row is `unknown`.
//!
//! The database is read with a tiny built-in SQLite table reader (no SQLite crate): it walks the
//! `gen_metadata` b-tree in the main file only (a write-ahead log is ignored). Memory and time are
//! bounded by the file size (payload budget, row cap, page-visit cap). Any problem means the model
//! is `unknown`; a model is never guessed. No account record exists in these files,
//! so every record is attributed to `unknown`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::model::{AccountRef, AccountSource, UsageRecord};

const TOOL: &str = "antigravity";
const UNKNOWN_MODEL: &str = "unknown";
/// A database larger than this is not read (the model stays unknown). Real ones are well below.
const MAX_DB_BYTES: u64 = 64 * 1024 * 1024;
/// More `gen_metadata` rows than this means a malformed file.
const MAX_ROWS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: PathBuf,
    pub session_id: String,
    /// `<dir>/conversations/<session>.db`; may not exist.
    pub db: PathBuf,
}

/// `<home>/.gemini/antigravity-cli`.
pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    vec![home.join(".gemini").join("antigravity-cli")]
}

/// Find `<dir>/brain/<session>/.system_generated/logs/transcript.jsonl`. Returns the files
/// (sorted) and the number of directory entries that could not be read. A missing dir is not
/// an error. Only the `transcript.jsonl` stream is used: `transcript_full.jsonl` and the
/// `chunks` files repeat it.
pub fn discover(dirs: &[PathBuf]) -> (Vec<SourceFile>, u64) {
    let mut files = Vec::new();
    let mut unreadable = 0;
    for dir in dirs {
        let brain = dir.join("brain");
        let Ok(rd) = std::fs::read_dir(&brain) else {
            continue;
        };
        let mut found = Vec::new();
        for entry in rd {
            let Ok(e) = entry else {
                unreadable += 1;
                continue;
            };
            let path = e
                .path()
                .join(".system_generated")
                .join("logs")
                .join("transcript.jsonl");
            if !path.is_file() {
                continue;
            }
            let session_id = e.file_name().to_string_lossy().into_owned();
            let db = dir.join("conversations").join(format!("{session_id}.db"));
            found.push(SourceFile {
                path,
                session_id,
                db,
            });
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        files.extend(found);
    }
    (files, unreadable)
}

/// One usage line before cross-file dedup.
#[derive(Debug, Clone, PartialEq)]
pub struct RawRecord {
    pub ts: DateTime<Utc>,
    pub model: String,
    pub session_id: String,
    pub step_index: Option<u64>,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
}

#[derive(Debug, Default)]
pub struct ParsedFile {
    pub records: Vec<RawRecord>,
    pub skipped_lines: u64,
    pub unreadable: bool,
}

#[derive(Deserialize)]
struct Line {
    step_index: Option<u64>,
    created_at: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
}

/// Stream one transcript. Lines without token counts are other events and are ignored.
/// Never panics; bad lines and unreadable files are counted.
pub fn parse_file(file: &SourceFile) -> ParsedFile {
    let mut out = ParsedFile::default();
    let Ok(f) = File::open(&file.path) else {
        out.unreadable = true;
        return out;
    };
    let mut reader = BufReader::new(f);
    let mut buf = Vec::new();
    let mut rows: Vec<(DateTime<Utc>, Line)> = Vec::new();
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
        if line.is_empty() || !contains(line, b"\"input_tokens\"") {
            continue;
        }
        let Ok(l) = serde_json::from_slice::<Line>(line) else {
            out.skipped_lines += 1;
            continue;
        };
        let ts = l
            .created_at
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|d| d.with_timezone(&Utc));
        match (ts, l.input_tokens) {
            (Some(ts), Some(_)) => rows.push((ts, l)),
            _ => out.skipped_lines += 1,
        }
    }
    // A step can be written twice; keep the larger output (the later, complete one).
    let mut by_step: HashMap<u64, usize> = HashMap::new();
    let mut kept: Vec<(DateTime<Utc>, Line)> = Vec::new();
    for (ts, l) in rows {
        match l.step_index.and_then(|s| by_step.get(&s).copied()) {
            Some(i) => {
                if l.output_tokens > kept[i].1.output_tokens {
                    kept[i] = (ts, l);
                }
            }
            None => {
                if let Some(s) = l.step_index {
                    by_step.insert(s, kept.len());
                }
                kept.push((ts, l));
            }
        }
    }
    kept.sort_by_key(|(ts, l)| (l.step_index.unwrap_or(u64::MAX), *ts));
    let usage: Vec<Usage> = kept
        .iter()
        .map(|(_, l)| {
            (
                l.input_tokens.unwrap_or(0),
                l.output_tokens.unwrap_or(0),
                l.cache_read_tokens.unwrap_or(0),
            )
        })
        .collect();
    let models = if kept.is_empty() {
        vec![]
    } else {
        resolve_models(&file.db, &usage)
    };
    out.records = kept
        .into_iter()
        .enumerate()
        .map(|(i, (ts, l))| RawRecord {
            ts,
            model: models
                .get(i)
                .cloned()
                .unwrap_or_else(|| UNKNOWN_MODEL.into()),
            session_id: file.session_id.clone(),
            step_index: l.step_index,
            input: l.input_tokens.unwrap_or(0),
            output: l.output_tokens.unwrap_or(0),
            cache_read: l.cache_read_tokens.unwrap_or(0),
        })
        .collect();
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// `(input, output, cache_read)` of one request.
type Usage = (u64, u64, u64);

/// One model per request, or empty when the database cannot be read. Requests are matched to
/// rows in order by usage; a request with no matching row (or a row naming no model) is
/// `unknown`.
fn resolve_models(db: &Path, requests: &[Usage]) -> Vec<String> {
    let Some(rows) = read_gen_metadata(db) else {
        return vec![];
    };
    // code -> id, only when every row that shows both agrees.
    let mut by_code: HashMap<&str, Option<&str>> = HashMap::new();
    for (_, i) in &rows {
        if let (Some(c), Some(id)) = (i.code.as_deref(), i.id.as_deref()) {
            by_code
                .entry(c)
                .and_modify(|e| {
                    if *e != Some(id) {
                        *e = None;
                    }
                })
                .or_insert(Some(id));
        }
    }
    let name = |i: &RowModel| -> Option<String> {
        let mapped = i
            .code
            .as_deref()
            .and_then(|c| by_code.get(c).copied().flatten());
        match (mapped, i.id.as_deref(), i.code.as_deref()) {
            (Some(m), _, _) | (None, Some(m), _) => Some(m.to_string()),
            (None, None, Some(c)) => Some(c.to_ascii_lowercase()),
            _ => None,
        }
    };
    // usage -> ascending row indexes. The cursor only moves forward, so indexes below it are
    // dropped as they are met and each request costs O(1) amortised.
    let mut index: HashMap<Usage, VecDeque<usize>> = HashMap::new();
    for (i, (u, _)) in rows.iter().enumerate() {
        index.entry(*u).or_default().push_back(i);
    }
    let mut next = 0;
    requests
        .iter()
        .map(|u| {
            let Some(queue) = index.get_mut(u) else {
                return UNKNOWN_MODEL.to_string();
            };
            while queue.front().is_some_and(|&i| i < next) {
                queue.pop_front();
            }
            let Some(i) = queue.pop_front() else {
                return UNKNOWN_MODEL.to_string();
            };
            next = i + 1;
            name(&rows[i].1).unwrap_or_else(|| UNKNOWN_MODEL.to_string())
        })
        .collect()
}

// ---------------------------------------------------------------- protobuf (one field path)

fn varint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *b.get(*pos)?;
        *pos += 1;
        if shift < 64 {
            v |= u64::from(byte & 0x7f) << shift;
        }
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// First length-delimited field `num` of a protobuf message; `None` on any malformed byte.
fn field_bytes(msg: &[u8], num: u64) -> Option<&[u8]> {
    let mut pos = 0;
    while pos < msg.len() {
        let tag = varint(msg, &mut pos)?;
        match tag & 7 {
            0 => {
                varint(msg, &mut pos)?;
            }
            1 => pos = pos.checked_add(8)?,
            5 => pos = pos.checked_add(4)?,
            2 => {
                let len = usize::try_from(varint(msg, &mut pos)?).ok()?;
                let end = pos.checked_add(len)?;
                let body = msg.get(pos..end)?;
                if tag >> 3 == num {
                    return Some(body);
                }
                pos = end;
            }
            _ => return None,
        }
    }
    None
}

/// First varint field `num` of a protobuf message.
fn field_varint(msg: &[u8], num: u64) -> Option<u64> {
    let mut pos = 0;
    while pos < msg.len() {
        let tag = varint(msg, &mut pos)?;
        match tag & 7 {
            0 => {
                let v = varint(msg, &mut pos)?;
                if tag >> 3 == num {
                    return Some(v);
                }
            }
            1 => pos = pos.checked_add(8)?,
            5 => pos = pos.checked_add(4)?,
            2 => {
                let len = usize::try_from(varint(msg, &mut pos)?).ok()?;
                pos = pos.checked_add(len)?;
            }
            _ => return None,
        }
    }
    None
}

/// Every length-delimited field `num` of a message, in order; stops quietly at a malformed byte.
fn all_fields(msg: &[u8], num: u64) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < msg.len() {
        let Some(tag) = varint(msg, &mut pos) else {
            break;
        };
        match tag & 7 {
            0 => {
                if varint(msg, &mut pos).is_none() {
                    break;
                }
            }
            1 | 5 => {
                let n = if tag & 7 == 1 { 8 } else { 4 };
                let Some(p) = pos.checked_add(n) else { break };
                pos = p;
            }
            2 => {
                let Some(len) = varint(msg, &mut pos).and_then(|l| usize::try_from(l).ok()) else {
                    break;
                };
                let Some(body) = pos.checked_add(len).and_then(|end| msg.get(pos..end)) else {
                    break;
                };
                pos += len; // in range: `get` above succeeded
                if tag >> 3 == num {
                    out.push(body);
                }
            }
            _ => break,
        }
    }
    out
}

fn printable(b: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(b).ok()?;
    let ok = !s.is_empty() && s.len() <= 100 && s.bytes().all(|c| c.is_ascii_graphic());
    ok.then(|| s.to_string())
}

/// What one `gen_metadata` row says about its model.
#[derive(Default)]
struct RowModel {
    /// `data` -> 1 -> 20 entry named `model_enum` (value in its field 2).
    code: Option<String>,
    /// `data` -> 3 -> 28.
    id: Option<String>,
}

/// Usage of one row: `data` -> 1 -> 4 with input (2), output (3) and cache read (5); a count
/// that is missing is zero (protobuf omits zeros).
fn row_usage(data: &[u8]) -> Option<Usage> {
    let u = field_bytes(field_bytes(data, 1)?, 4)?;
    let n = |f| field_varint(u, f).unwrap_or(0);
    Some((n(2), n(3), n(5)))
}

fn row_model(data: &[u8]) -> RowModel {
    let mut m = RowModel::default();
    if let Some(top) = field_bytes(data, 1) {
        for e in all_fields(top, 20) {
            if field_bytes(e, 1) == Some(b"model_enum".as_slice()) {
                m.code = field_bytes(e, 2).and_then(printable);
            }
        }
    }
    m.id = field_bytes(data, 3)
        .and_then(|t| field_bytes(t, 28))
        .and_then(printable);
    m
}

// ---------------------------------------------------------------- SQLite (table b-tree only)

struct Db {
    bytes: Vec<u8>,
    page_size: usize,
    usable: usize,
}

fn be16(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn be32(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// SQLite varint (1 to 9 bytes, big endian, the 9th byte has 8 bits).
fn sqlite_varint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..9 {
        let byte = *b.get(*pos)?;
        *pos += 1;
        if i == 8 {
            return Some((v << 8) | u64::from(byte));
        }
        v = (v << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

impl Db {
    fn open(path: &Path) -> Option<Db> {
        let f = File::open(path).ok()?;
        if !f.metadata().ok()?.is_file() {
            return None;
        }
        // `take` bounds the read even when the file grows after the check.
        let mut bytes = Vec::new();
        f.take(MAX_DB_BYTES + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > MAX_DB_BYTES {
            return None;
        }
        if bytes.get(..16)? != b"SQLite format 3\0" || bytes.get(56..60)? != [0, 0, 0, 1] {
            return None; // not SQLite, or not UTF-8 text
        }
        let page_size = match be16(&bytes, 16)? {
            1 => 65536,
            n if n >= 512 && n.is_power_of_two() => n,
            _ => return None,
        };
        let usable = page_size.checked_sub(usize::from(*bytes.get(20)?))?;
        (usable >= 480).then_some(Db {
            bytes,
            page_size,
            usable,
        })
    }

    fn page(&self, n: usize) -> Option<&[u8]> {
        let start = n.checked_sub(1)?.checked_mul(self.page_size)?;
        self.bytes.get(start..start.checked_add(self.page_size)?)
    }

    /// Call `visit` with the payload of every row of the table b-tree rooted at page `root`, in
    /// rowid order. `None` when the tree is malformed or exceeds a budget. Legitimate content
    /// never needs more payload bytes than the file has, more page visits than it has pages, or
    /// more than `MAX_ROWS` rows, so a hostile file cannot make this use unbounded memory or time.
    fn table_rows(&self, root: usize, visit: &mut dyn FnMut(&[u8])) -> Option<()> {
        let mut w = Walk {
            seen: HashSet::new(),
            payload_left: self.bytes.len(),
            pages_left: self.bytes.len() / self.page_size,
            rows_left: MAX_ROWS,
        };
        self.walk(root, 0, &mut w, visit)
    }

    fn walk(&self, n: usize, depth: u32, w: &mut Walk, visit: &mut dyn FnMut(&[u8])) -> Option<()> {
        if depth > 20 || !w.seen.insert(n) {
            return None;
        }
        w.pages_left = w.pages_left.checked_sub(1)?;
        let page = self.page(n)?;
        let base = if n == 1 { 100 } else { 0 };
        let count = be16(page, base + 3)?;
        match *page.get(base)? {
            0x0d => {
                for i in 0..count {
                    let cell = be16(page, base + 8 + 2 * i)?;
                    w.rows_left = w.rows_left.checked_sub(1)?;
                    let payload = self.leaf_payload(page, cell, w)?;
                    visit(&payload);
                }
                Some(())
            }
            0x05 => {
                for i in 0..count {
                    let cell = be16(page, base + 12 + 2 * i)?;
                    self.walk(be32(page, cell)?, depth + 1, w, visit)?;
                }
                self.walk(be32(page, base + 8)?, depth + 1, w, visit)
            }
            _ => None,
        }
    }

    /// Payload of a table leaf cell, following the overflow chain.
    fn leaf_payload(&self, page: &[u8], cell: usize, w: &mut Walk) -> Option<Vec<u8>> {
        let mut pos = cell;
        let total = usize::try_from(sqlite_varint(page, &mut pos)?).ok()?;
        sqlite_varint(page, &mut pos)?; // rowid
        w.payload_left = w.payload_left.checked_sub(total)?;
        let max_local = self.usable - 35;
        let local = if total <= max_local {
            total
        } else {
            let min_local = (self.usable - 12) * 32 / 255 - 23;
            let k = min_local + (total - min_local) % (self.usable - 4);
            if k <= max_local { k } else { min_local }
        };
        let mut out = page.get(pos..pos.checked_add(local)?)?.to_vec();
        if total > local {
            let mut next = be32(page, pos + local)?;
            while out.len() < total {
                if next == 0 {
                    return None;
                }
                // Overflow pages are not in `seen`; the page budget stops cycles and sharing.
                w.pages_left = w.pages_left.checked_sub(1)?;
                let p = self.page(next)?;
                let take = (total - out.len()).min(self.usable - 4);
                out.extend_from_slice(p.get(4..4 + take)?);
                next = be32(p, 0)?;
            }
        }
        Some(out)
    }
}

/// Budgets and visited pages of one `table_rows` walk.
struct Walk {
    seen: HashSet<usize>,
    payload_left: usize,
    pages_left: usize,
    rows_left: usize,
}

enum Val<'a> {
    Null,
    Int(u64),
    Bytes(&'a [u8]),
}

/// Columns of a SQLite record (floats are read as integers of the same width; unused here).
fn record_values(rec: &[u8]) -> Option<Vec<Val<'_>>> {
    let mut pos = 0;
    let hdr = usize::try_from(sqlite_varint(rec, &mut pos)?).ok()?;
    let mut types = Vec::new();
    while pos < hdr {
        types.push(sqlite_varint(rec, &mut pos)?);
    }
    let mut at = hdr;
    let mut vals = Vec::new();
    for t in types {
        let len = match t {
            0 | 8 | 9 => 0,
            1..=4 => t as usize,
            5 => 6,
            6 | 7 => 8,
            t if t >= 12 => usize::try_from((t - 12) / 2).ok()?,
            _ => return None,
        };
        let bytes = rec.get(at..at.checked_add(len)?)?;
        at += len;
        vals.push(match t {
            0 => Val::Null,
            8 => Val::Int(0),
            9 => Val::Int(1),
            1..=7 => Val::Int(bytes.iter().fold(0, |a, &x| (a << 8) | u64::from(x))),
            _ => Val::Bytes(bytes),
        });
    }
    Some(vals)
}

/// Usage and model info of every `gen_metadata` row, in rowid order (rows that do not decode are
/// left out: they cannot match a request); `None` when the file is not a readable SQLite
/// database with that table.
fn read_gen_metadata(path: &Path) -> Option<Vec<(Usage, RowModel)>> {
    let db = Db::open(path)?;
    // sqlite_master columns: type, name, tbl_name, rootpage, sql.
    let mut root = None;
    db.table_rows(1, &mut |r| {
        if let Some(vals) = record_values(r)
            && let [
                Val::Bytes(b"table"),
                Val::Bytes(b"gen_metadata"),
                _,
                Val::Int(n),
                ..,
            ] = vals.as_slice()
        {
            root = root.or(usize::try_from(*n).ok());
        }
    })?;
    // gen_metadata columns: idx (rowid alias, stored as NULL), data, size.
    let mut rows = Vec::new();
    db.table_rows(root?, &mut |r| {
        if let Some(vals) = record_values(r)
            && let [Val::Null, Val::Bytes(d), ..] = vals.as_slice()
            && let Some(u) = row_usage(d)
        {
            rows.push((u, row_model(d)));
        }
    })?;
    Some(rows)
}

// ---------------------------------------------------------------- finalize

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

/// Dedup by `(session, step_index)` across files (the same directory given twice), attribute
/// every record to the unknown account.
pub fn finalize(files: Vec<ParsedFile>) -> Finalized {
    let mut out = Finalized::default();
    let mut seen: HashSet<String> = HashSet::new();
    for f in files {
        out.skipped_lines += f.skipped_lines;
        out.unreadable_files += u64::from(f.unreadable);
        out.records_before += f.records.len() as u64;
        for r in f.records {
            let key = r.step_index.map(|s| format!("{}:{s}", r.session_id));
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
                reasoning: 0,
                cache_write_5m: 0,
                cache_write_1h: 0,
                cache_read: r.cache_read,
                dedup_key: key,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: usize = 512;

    fn fixture(n: u32) -> Vec<u8> {
        let p = ["tests", "fixtures", "antigravity", "ho", "dot-gemini"]
            .iter()
            .fold(PathBuf::from(env!("CARGO_MANIFEST_DIR")), |p, d| {
                p.join(if *d == "ho" { "home" } else { d })
            })
            .join("antigravity-cli/conversations")
            .join(format!("00000000-0000-4000-8000-{n:012}.db"));
        std::fs::read(p).unwrap()
    }

    fn read_bytes(bytes: &[u8]) -> Option<Vec<(Usage, RowModel)>> {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.db");
        std::fs::write(&p, bytes).unwrap();
        read_gen_metadata(&p)
    }

    fn vi(mut n: u64) -> Vec<u8> {
        let mut v = vec![(n & 0x7f) as u8];
        n >>= 7;
        while n > 0 {
            v.insert(0, ((n & 0x7f) as u8) | 0x80);
            n >>= 7;
        }
        v
    }

    /// A leaf page (`first` = page 1, header after the 100 byte file header) whose cell
    /// pointers all point at the one cell.
    fn leaf(first: bool, cell: &[u8], pointers: usize) -> Vec<u8> {
        let base = if first { 100 } else { 0 };
        let mut p = vec![0u8; PS];
        let at = PS - cell.len();
        p[at..].copy_from_slice(cell);
        p[base] = 0x0d;
        p[base + 3..base + 5].copy_from_slice(&(pointers as u16).to_be_bytes());
        for i in 0..pointers {
            p[base + 8 + 2 * i..base + 10 + 2 * i].copy_from_slice(&(at as u16).to_be_bytes());
        }
        p
    }

    /// Page 1 (master with `gen_metadata` at page 2) plus the given pages.
    fn db(pages: Vec<Vec<u8>>) -> Vec<u8> {
        let mut rec = vec![6, 23, 37, 37, 1, 13];
        rec.extend_from_slice(b"tablegen_metadatagen_metadata\x02");
        let mut cell = vi(rec.len() as u64);
        cell.push(1);
        cell.extend(rec);
        let mut p1 = leaf(true, &cell, 1);
        p1[..16].copy_from_slice(b"SQLite format 3\0");
        p1[16..18].copy_from_slice(&(PS as u16).to_be_bytes());
        p1[56..60].copy_from_slice(&[0, 0, 0, 1]);
        let mut out = p1;
        for p in pages {
            out.extend(p);
        }
        out
    }

    /// Leaf cell declaring `total` (> 477) payload bytes with the local part SQLite keeps for
    /// 512 byte pages (same formula as `leaf_payload`) and an overflow pointer to page `next`.
    fn overflow_cell(total: u64, next: u32) -> Vec<u8> {
        let (max_local, min_local) = (PS as u64 - 35, (PS as u64 - 12) * 32 / 255 - 23);
        assert!(total > max_local);
        let k = min_local + (total - min_local) % (PS as u64 - 4);
        let local = if k <= max_local { k } else { min_local };
        let mut c = vi(total);
        c.push(1);
        c.extend(vec![0u8; local as usize]);
        c.extend(next.to_be_bytes());
        c
    }

    #[test]
    fn fixture_databases_read() {
        let rows = read_bytes(&fixture(1)).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].0, (200, 20, 50));
    }

    /// Leaf cell with `n` payload bytes kept locally (n <= 477).
    fn local_cell(n: usize) -> Vec<u8> {
        let mut c = vi(n as u64);
        c.push(1);
        c.extend(vec![0u8; n]);
        c
    }

    fn looping_chain() -> Vec<u8> {
        let mut chain = vec![0u8; PS];
        chain[..4].copy_from_slice(&3u32.to_be_bytes());
        chain
    }

    #[test]
    fn many_cells_sharing_one_payload_exceed_the_payload_budget() {
        // 100 pointers to one 400 byte cell: 40,000 bytes from a 1,024 byte file. Few pages
        // and few rows, so only the payload budget can refuse it.
        let bytes = db(vec![leaf(false, &local_cell(400), 100)]);
        assert!(read_bytes(&bytes).is_none());
    }

    #[test]
    fn cyclic_overflow_chain_exceeds_the_page_budget() {
        // 1,500 declared bytes (within the 1,536 byte file) but the chain is a self-loop that
        // would need 3 overflow pages with only 2 left: only the page budget refuses it.
        let bytes = db(vec![
            leaf(false, &overflow_cell(1500, 3), 1),
            looping_chain(),
        ]);
        assert!(read_bytes(&bytes).is_none());
    }

    fn interior(children: &[u32]) -> Vec<u8> {
        let (last, cells) = children.split_last().unwrap();
        let mut p = vec![0u8; PS];
        p[0] = 0x05;
        p[3..5].copy_from_slice(&(cells.len() as u16).to_be_bytes());
        p[8..12].copy_from_slice(&last.to_be_bytes());
        let mut at = PS;
        for (i, c) in cells.iter().enumerate() {
            at -= 5;
            p[at..at + 4].copy_from_slice(&c.to_be_bytes());
            p[at + 4] = 1;
            p[12 + 2 * i..14 + 2 * i].copy_from_slice(&(at as u16).to_be_bytes());
        }
        p
    }

    #[test]
    fn row_cap_refuses_more_than_max_rows() {
        // 401 leaves x 250 pointers = 100,250 rows of 1 byte each: pages and payload stay far
        // inside their budgets, so only the row cap refuses it.
        let leaves = 401u32;
        let mids = 6u32;
        let first_leaf = 3 + mids;
        let mut pages = vec![]; // page 2: top interior, pages 3..: mids, then leaves
        pages.push(interior(&(3..3 + mids).collect::<Vec<_>>()));
        for m in 0..mids {
            let from = first_leaf + m * 70;
            let to = (from + 70).min(first_leaf + leaves);
            pages.push(interior(&(from..to).collect::<Vec<_>>()));
        }
        for _ in 0..leaves {
            pages.push(leaf(false, &local_cell(1), 250));
        }
        assert!(read_bytes(&db(pages)).is_none());
    }

    #[test]
    fn huge_declared_size_is_refused() {
        let bytes = db(vec![
            leaf(false, &overflow_cell(1 << 40, 3), 1),
            vec![0; PS],
        ]);
        assert!(read_bytes(&bytes).is_none());
    }

    #[test]
    fn cyclic_interior_pages_are_refused() {
        let mut interior = vec![0u8; PS];
        interior[0] = 0x05;
        interior[8..12].copy_from_slice(&2u32.to_be_bytes()); // right child is itself
        assert!(read_bytes(&db(vec![interior])).is_none());
    }

    #[test]
    fn child_page_out_of_range_is_refused() {
        let mut interior = vec![0u8; PS];
        interior[0] = 0x05;
        interior[8..12].copy_from_slice(&0u32.to_be_bytes());
        assert!(read_bytes(&db(vec![interior.clone()])).is_none());
        interior[8..12].copy_from_slice(&900u32.to_be_bytes());
        assert!(read_bytes(&db(vec![interior])).is_none());
    }

    #[test]
    fn truncated_files_never_panic() {
        for n in [1, 3, 5, 8] {
            let full = fixture(n);
            for len in (0..full.len()).step_by(97).chain([full.len() - 1]) {
                let _ = read_bytes(&full[..len]);
            }
            for page in 0..=full.len() / 1024 {
                let _ = read_bytes(&full[..page * 1024]);
            }
        }
    }

    #[test]
    fn corrupted_bytes_never_panic() {
        for n in [1, 2, 3] {
            let full = fixture(n);
            let step = if n == 3 { 17 } else { 3 };
            for at in (0..full.len()).step_by(step) {
                for v in [0x00, 0xff] {
                    let mut b = full.clone();
                    b[at] = v;
                    let _ = read_bytes(&b);
                }
            }
        }
    }

    #[test]
    fn bad_header_fields_are_refused() {
        let good = fixture(1);
        let mut b = good.clone();
        b[16..18].copy_from_slice(&3u16.to_be_bytes()); // page size not a power of two
        assert!(read_bytes(&b).is_none());
        let mut b = good;
        b[20] = 255; // reserved bytes: must not panic whatever the walk makes of it
        let _ = read_bytes(&b);
    }

    #[test]
    fn requests_match_rows_by_usage() {
        // a request with no row never takes a model from a neighbour
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.db");
        std::fs::write(&p, fixture(6)).unwrap();
        assert_eq!(
            resolve_models(&p, &[(900, 90, 0), (1, 1, 1)]),
            ["model-f", "unknown"]
        );
        assert_eq!(resolve_models(&p, &[(2, 2, 2)]), ["unknown"]);
    }

    #[test]
    fn matching_keeps_row_order() {
        let p = tempfile::tempdir().unwrap();
        let p = p.path().join("x.db");
        std::fs::write(&p, fixture(1)).unwrap();
        let rows = read_gen_metadata(&p).unwrap();
        let (a, b, c) = (rows[0].0, rows[1].0, rows[2].0);
        let name = |reqs: &[Usage]| resolve_models(&p, reqs);
        let in_order = name(&[a, b, c]);
        assert!(in_order.iter().all(|m| m != UNKNOWN_MODEL));
        // a request earlier than the cursor has no row left; later ones still match
        let out_of_order = name(&[b, a, c]);
        assert_eq!(out_of_order[1], UNKNOWN_MODEL);
        assert_eq!(out_of_order[0], in_order[1]);
        assert_eq!(out_of_order[2], in_order[2]);
        // a repeated usage consumes one row each time, never the same row twice
        assert_eq!(name(&[b, b])[1], UNKNOWN_MODEL);
    }
}
