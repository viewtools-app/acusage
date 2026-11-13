//! Text tables and the JSON envelope. Pure: output is returned as a string.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Local, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::app::{Body, Output};
use crate::blocks::{BlockRow, BlocksReport};
use crate::model::{AccountSource, CostKind, LimitWindow};
use crate::pricing::PriceSource;
use crate::report::{LimitRow, Totals, UsageReport, sum_totals};

#[derive(Clone)]
pub struct RenderOpts {
    pub json: bool,
    /// Show times in UTC instead of local time.
    pub utc: bool,
    /// Hide the money column (and its footnotes) in tables.
    pub no_cost: bool,
    pub color: bool,
    /// Compact columns even when the terminal is wide enough for the full table.
    pub compact: bool,
    pub full: bool,
    /// Terminal width in columns; `None` means no limit (not a TTY).
    pub width: Option<usize>,
}

const LIMITS_NOTE: &str =
    "Claude Code does not store limit data locally; Claude limits are not shown.";
const BAR_CELLS: usize = 20;

pub fn render(out: &Output, o: &RenderOpts) -> String {
    if o.json { json(out) } else { text(out, o) }
}

// ---------------------------------------------------------------- JSON

#[derive(Serialize)]
struct Envelope {
    schema: u32,
    report: &'static str,
    generated_at: DateTime<Utc>,
    rows: Value,
    totals: Value,
    unpriced_models: Value,
    local_models: Value,
    price_sources: Value,
}

fn json(out: &Output) -> String {
    let (rows, totals, unpriced, local, sources) = match &out.body {
        Body::Usage(r) => (
            val(&r.rows),
            val(&r.totals),
            val(&r.unpriced_models),
            val(&r.local_models),
            val(&r.price_sources),
        ),
        Body::Session(r) => (
            val(&r.rows),
            val(&r.totals),
            val(&r.unpriced_models),
            val(&r.local_models),
            val(&r.price_sources),
        ),
        Body::Blocks(r) => (
            val(&r.rows),
            val(&r.totals),
            val(&r.unpriced_models),
            val(&r.local_models),
            val(&r.price_sources),
        ),
        Body::Accounts(r) => (
            val(&r.rows),
            val(&r.totals),
            val(&r.unpriced_models),
            val(&r.local_models),
            val(&r.price_sources),
        ),
        // Limits carry no tokens or prices: totals is null, unpriced_models empty.
        Body::Limits(r) => (
            val(r),
            Value::Null,
            Value::Array(vec![]),
            Value::Array(vec![]),
            Value::Object(Default::default()),
        ),
    };
    let env = Envelope {
        schema: 1,
        report: out.name,
        generated_at: out.generated_at,
        rows,
        totals,
        unpriced_models: unpriced,
        local_models: local,
        price_sources: sources,
    };
    let mut s = serde_json::to_string_pretty(&env).expect("envelope serializes");
    s.push('\n');
    s
}

fn val<T: Serialize + ?Sized>(x: &T) -> Value {
    serde_json::to_value(x).expect("report serializes")
}

// ---------------------------------------------------------------- tables

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Plain,
    Bold,
    Blue,
    Cyan,
    Yellow,
    YellowBold,
    Green,
    Red,
    Dim,
}

impl Style {
    fn wrap(self, text: &str, color: bool) -> String {
        let code = match self {
            Style::Plain => return text.to_string(),
            _ if !color => return text.to_string(),
            Style::Bold => "1",
            Style::Blue => "34",
            Style::Cyan => "36",
            Style::Yellow => "33",
            Style::YellowBold => "1;33",
            Style::Green => "32",
            Style::Red => "31",
            Style::Dim => "2",
        };
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}

/// A table cell; `text` may hold several lines (`\n`).
#[derive(Clone)]
struct Cell {
    text: String,
    style: Style,
}

fn cell(text: impl Into<String>, style: Style) -> Cell {
    Cell {
        text: text.into(),
        style,
    }
}

fn plain(text: impl Into<String>) -> Cell {
    cell(text, Style::Plain)
}

const ACCOUNT_CAP: usize = 24;
const ACCOUNT_CAP_FULL: usize = 48;
const MODELS_CAP: usize = 32;
const PROJECT_CAP: usize = 20;

#[derive(Clone)]
struct Table {
    header: Vec<&'static str>,
    /// Right-aligned columns (numbers).
    right: Vec<bool>,
    /// Maximum width per column; longer text is cut with `…`.
    caps: Vec<Option<usize>>,
    /// Columns that may be narrowed further on a small terminal: `(column, minimum width)`, first
    /// one is narrowed first.
    shrink: Vec<(usize, usize)>,
    /// Columns cut in the middle (`user…ple.com`) instead of at the end.
    middle: Vec<bool>,
    rows: Vec<Vec<Cell>>,
    /// Indices of `rows` drawn without a separator above them (sub-rows of the row before).
    glued: Vec<usize>,
    /// Indices of `rows` drawn as one cell across the table (headings); only the first cell is used.
    spans: Vec<usize>,
    /// Final row, set apart by a separator.
    total: Option<Vec<Cell>>,
}

impl Table {
    fn new(header: Vec<&'static str>, right: Vec<bool>) -> Self {
        let n = header.len();
        Table {
            header,
            right,
            caps: vec![None; n],
            shrink: vec![],
            middle: vec![false; n],
            rows: vec![],
            glued: vec![],
            spans: vec![],
            total: None,
        }
    }

    fn cap(mut self, col: usize, max: usize, min: usize) -> Self {
        self.caps[col] = Some(max);
        self.shrink.push((col, min));
        self
    }

    /// Cut this column in the middle when it is too long.
    fn mid(mut self, col: usize) -> Self {
        self.middle[col] = true;
        self
    }

    /// Remove a column by header name (rows, total, caps and shrink entries included).
    fn drop_col(&mut self, name: &str) {
        let Some(i) = self.header.iter().position(|h| *h == name) else {
            return;
        };
        self.header.remove(i);
        self.right.remove(i);
        self.caps.remove(i);
        self.middle.remove(i);
        self.shrink.retain(|(c, _)| *c != i);
        for s in &mut self.shrink {
            if s.0 > i {
                s.0 -= 1;
            }
        }
        for r in self.rows.iter_mut().chain(self.total.as_mut()) {
            if i < r.len() {
                r.remove(i);
            }
        }
    }

    fn widths(&self) -> Vec<usize> {
        let mut w: Vec<usize> = self.header.iter().map(|h| h.chars().count()).collect();
        for (k, r) in self.rows.iter().enumerate() {
            if self.spans.contains(&k) {
                continue;
            }
            for (i, c) in r.iter().enumerate() {
                let longest = c.text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
                w[i] = w[i].max(longest);
            }
        }
        for (i, c) in self.total.iter().flatten().enumerate() {
            let longest = c.text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
            w[i] = w[i].max(longest);
        }
        for (i, cap) in self.caps.iter().enumerate() {
            if let Some(c) = cap {
                w[i] = w[i].min(*c);
            }
        }
        w
    }

    /// Printed width of every line: cells plus one space each side plus borders.
    fn width(&self) -> usize {
        self.widths().iter().map(|w| w + 3).sum::<usize>() + 1
    }

    /// Narrow the shrinkable columns, in order, until the table fits `target` (or they hit their
    /// minimum).
    fn shrink_to(&mut self, target: usize) {
        if self.width() > target
            && let Some(i) = self.header.iter().position(|h| *h == "Agent")
        {
            // "Claude Code" is only shortened to "Claude" when the table does not fit.
            for r in self.rows.iter_mut().chain(self.total.as_mut()) {
                if let Some(c) = r.get_mut(i) {
                    c.text = c.text.replace("Claude Code", "Claude");
                }
            }
        }
        for (col, min) in self.shrink.clone() {
            let excess = self.width().saturating_sub(target);
            if excess == 0 {
                return;
            }
            let cur = self.widths()[col];
            self.caps[col] = Some(cur.saturating_sub(excess).max(min.min(cur)));
        }
    }

    fn render(&self, color: bool) -> String {
        let w = self.widths();
        // `above` / `below`: is the neighbouring row a heading that spans all columns?
        let border = |l: &str, m: &str, r: &str, above: Option<bool>, below: Option<bool>| {
            let joint = match (above, below) {
                (Some(true), Some(true)) => "─",
                (Some(true), _) if m == "┼" => "┬",
                (_, Some(true)) if m == "┼" => "┴",
                (Some(true), _) => "─",
                _ => m,
            };
            let parts: Vec<String> = w.iter().map(|&x| "─".repeat(x + 2)).collect();
            format!("{l}{}{r}\n", parts.join(joint))
        };
        let cut = |text: &str, width: usize, mid: bool| {
            if mid {
                trunc_mid(text, width)
            } else {
                trunc(text, width)
            }
        };
        let lines = |cells: &[(&str, Style)]| -> String {
            let height = cells
                .iter()
                .map(|(t, _)| t.lines().count())
                .max()
                .unwrap_or(1)
                .max(1);
            let mut s = String::new();
            for k in 0..height {
                let parts: Vec<String> = cells
                    .iter()
                    .enumerate()
                    .map(|(i, (text, style))| {
                        let t = cut(text.lines().nth(k).unwrap_or(""), w[i], self.middle[i]);
                        let pad = " ".repeat(w[i] - t.chars().count());
                        let padded = if self.right[i] {
                            format!("{pad}{t}")
                        } else {
                            format!("{t}{pad}")
                        };
                        // Colour the text only, so padding stays plain.
                        let colored = if t.is_empty() {
                            padded
                        } else {
                            padded.replacen(&t, &style.wrap(&t, color), 1)
                        };
                        format!(" {colored} ")
                    })
                    .collect();
                s.push_str(&format!("│{}│\n", parts.join("│")));
            }
            s
        };
        let spanned = |c: &Cell| -> String {
            // Interior: every column and its padding, plus the joints between them.
            let inner = w.iter().map(|x| x + 2).sum::<usize>() + w.len() - 1 - 2;
            let t = trunc(&c.text, inner);
            let pad = " ".repeat(inner - t.chars().count());
            format!("│ {}{pad} │\n", c.style.wrap(&t, color))
        };
        let as_cells = |r: &'_ [Cell]| -> String {
            let cs: Vec<(&str, Style)> = r.iter().map(|c| (c.text.as_str(), c.style)).collect();
            lines(&cs)
        };
        let head: Vec<(&str, Style)> = self.header.iter().map(|h| (*h, Style::Blue)).collect();
        let mut s = border("┌", "┬", "┐", None, Some(false));
        s.push_str(&lines(&head));
        let mut above = false;
        for (i, r) in self.rows.iter().enumerate() {
            let span = self.spans.contains(&i);
            if !self.glued.contains(&i) {
                s.push_str(&border("├", "┼", "┤", Some(above), Some(span)));
            }
            s.push_str(&if span { spanned(&r[0]) } else { as_cells(r) });
            above = span;
        }
        if let Some(t) = &self.total {
            s.push_str(&border("├", "┼", "┤", Some(above), Some(false)));
            s.push_str(&as_cells(t));
            above = false;
        }
        s.push_str(&border("└", "┴", "┘", Some(above), None));
        s
    }
}

/// Rounded title box: bold title, blank line, dim labels with plain values, then the domain
/// right-aligned. As wide as its longest line (the label rows are already wrapped to the
/// terminal width), whatever the table width.
fn title_box(title: &str, rows: &[(&str, String)], domain: &str, color: bool) -> String {
    let label_w = rows
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0);
    let value_w = rows
        .iter()
        .flat_map(|(_, v)| v.lines())
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0);
    let content = title
        .chars()
        .count()
        .max(label_w + 2 + value_w)
        .max(domain.chars().count());
    let inner = content + 2;
    let line = |text: &str, styled: String| {
        let pad = inner - 1 - text.chars().count();
        format!("│ {styled}{}│\n", " ".repeat(pad))
    };
    let bar = "─".repeat(inner);
    let mut s = format!("╭{bar}╮\n");
    s.push_str(&line(title, Style::Bold.wrap(title, color)));
    s.push_str(&line("", String::new()));
    for (label, value) in rows {
        for (i, v) in value.lines().enumerate() {
            let l = if i == 0 { *label } else { "" };
            let text = format!("{l:<label_w$}  {v}");
            let styled = format!("{}  {v}", Style::Dim.wrap(&format!("{l:<label_w$}"), color));
            s.push_str(&line(&text, styled));
        }
    }
    let pad = inner - 1 - domain.chars().count();
    s.push_str(&format!(
        "│{}{} │\n",
        " ".repeat(pad),
        Style::Dim.wrap(domain, color)
    ));
    s.push_str(&format!("╰{bar}╯\n"));
    s
}

pub fn thousands(n: u64) -> String {
    let d = n.to_string();
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Cut to `max` chars, ending in `…` when something was dropped.
/// A trailing `†` mark survives the cut, after the `…`.
fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let (body, mark) = match s.strip_suffix(LITELLM_MARK) {
        Some(b) if max >= 2 => (b, Some(LITELLM_MARK)),
        _ => (s, None),
    };
    let keep = max.saturating_sub(1 + usize::from(mark.is_some()));
    let mut t: String = body.chars().take(keep).collect();
    t.push('…');
    t.extend(mark);
    t
}

/// `claude-opus-5-5` -> `opus-5-5`; a trailing `-YYYYMMDD` is dropped too.
fn short_model(m: &str) -> String {
    let s = m.strip_prefix("claude-").unwrap_or(m);
    match s.rsplit_once('-') {
        Some((head, tail))
            if !head.is_empty() && tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head.to_string()
        }
        _ => s.to_string(),
    }
}

/// Distinct short model names, in order.
fn short_names(m: &[String]) -> Vec<String> {
    let mut names: Vec<String> = vec![];
    for x in m {
        let s = short_model(x);
        if !names.contains(&s) {
            names.push(s);
        }
    }
    names
}

/// Marks a model priced from the LiteLLM snapshot (third-party, not checked).
const LITELLM_MARK: char = '†';

/// Ids to mark: the LiteLLM-priced ones, none when money is hidden (no prices on screen).
fn lit_marks(sources: &BTreeMap<String, PriceSource>, m: Money) -> BTreeSet<String> {
    if m == Money::Hidden {
        return BTreeSet::new();
    }
    sources
        .iter()
        .filter(|(_, s)| **s == PriceSource::Litellm)
        .map(|(id, _)| id.clone())
        .collect()
}

/// Distinct short names, each with the mark when any id behind it is in `lit`.
fn marked_names(m: &[String], lit: &BTreeSet<String>) -> Vec<String> {
    short_names(m)
        .into_iter()
        .map(|n| {
            if m.iter().any(|id| lit.contains(id) && short_model(id) == n) {
                format!("{n}{LITELLM_MARK}")
            } else {
                n
            }
        })
        .collect()
}

/// One `- name` line per distinct short model name.
fn models_cell(m: &[String], lit: &BTreeSet<String>) -> String {
    marked_names(m, lit)
        .iter()
        .map(|n| format!("- {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Models cell of a row with `-b`: the name for one model, `N models` for more (listed below).
fn breakdown_models_cell(m: &[String], lit: &BTreeSet<String>) -> String {
    let names = marked_names(m, lit);
    if names.len() == 1 {
        names[0].clone()
    } else {
        format!("{} models", names.len())
    }
}

/// Colour on when forced, or on a terminal; `--no-color` and `NO_COLOR` always win.
pub fn use_color(force: bool, no_color: bool, no_color_env: bool, tty: bool) -> bool {
    !no_color && !no_color_env && (force || tty)
}

/// `████░░░░` of `BAR_CELLS` cells for a percentage.
fn bar(pct: f64) -> String {
    let filled = ((pct / 100.0 * BAR_CELLS as f64).round().max(0.0) as usize).min(BAR_CELLS);
    format!("{}{}", "█".repeat(filled), "░".repeat(BAR_CELLS - filled))
}

/// What the money column holds in one table.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Money {
    Hidden,
    /// Every row is billed spend.
    Billed,
    /// No row is billed: API-equivalent value, for comparison only.
    ApiValue,
    /// Both kinds: API-equivalent values are marked with `~`.
    Mixed,
}

fn money_mode(kinds: impl Iterator<Item = CostKind>, o: &RenderOpts) -> Money {
    if o.no_cost {
        return Money::Hidden;
    }
    let (mut billed, mut equiv) = (false, false);
    for k in kinds {
        match k {
            CostKind::Billed => billed = true,
            CostKind::ApiEquivalent => equiv = true,
        }
    }
    match (billed, equiv) {
        (true, true) => Money::Mixed,
        (_, true) => Money::ApiValue,
        _ => Money::Billed,
    }
}

impl Money {
    fn head(self) -> Option<&'static str> {
        match self {
            Money::Hidden => None,
            Money::Billed => Some("Cost (USD)"),
            Money::ApiValue => Some("API Value (USD)"),
            Money::Mixed => Some("Cost / API Value (USD)"),
        }
    }
}

/// Kind of a group of rows: billed only when every row is.
fn merged(kinds: impl Iterator<Item = CostKind>) -> CostKind {
    if kinds.into_iter().all(|k| k == CostKind::Billed) {
        CostKind::Billed
    } else {
        CostKind::ApiEquivalent
    }
}

/// `$1,234.56`, `*` after it when some usage is unpriced, `~` in front for API-equivalent values in a mixed table, `-` without a price.
fn cost(t: &Totals, tilde: bool) -> String {
    let Some(c) = t.cost_usd else {
        return "-".into();
    };
    let cents = (c * 100.0).round() as u64;
    format!(
        "{}${}.{:02}{}",
        if tilde { "~" } else { "" },
        thousands(cents / 100),
        cents % 100,
        if t.partial_cost { "*" } else { "" }
    )
}

/// Which token columns to show.
#[derive(Clone, Copy)]
struct Spec {
    compact: bool,
    reasoning: bool,
    money: Money,
}

fn tok_head(s: Spec) -> Vec<&'static str> {
    if s.compact {
        vec!["Input", "Output"]
    } else if s.reasoning {
        vec![
            "Input",
            "Output",
            "Reasoning",
            "Cache Create",
            "Cache Read",
            "Total Tokens",
        ]
    } else {
        vec![
            "Input",
            "Output",
            "Cache Create",
            "Cache Read",
            "Total Tokens",
        ]
    }
}

fn tok_cells(t: &Totals, s: Spec, style: Style) -> Vec<Cell> {
    let n = |v: u64| cell(thousands(v), style);
    if s.compact {
        vec![n(t.input_tokens), n(t.output_tokens)]
    } else if s.reasoning {
        vec![
            n(t.input_tokens),
            n(t.output_tokens),
            n(t.reasoning_tokens),
            n(t.cache_write_tokens),
            n(t.cache_read_tokens),
            n(t.total_tokens),
        ]
    } else {
        vec![
            n(t.input_tokens),
            n(t.output_tokens),
            n(t.cache_write_tokens),
            n(t.cache_read_tokens),
            n(t.total_tokens),
        ]
    }
}

/// Header and alignment: left lead columns, right-aligned tokens and money, optional left tail.
fn head_row(lead: &[&'static str], s: Spec, tail: Option<&'static str>) -> Table {
    let mut h = lead.to_vec();
    let mut right = vec![false; h.len()];
    h.extend(tok_head(s));
    h.extend(s.money.head());
    right.resize(h.len(), true);
    if let Some(t) = tail {
        h.push(t);
        right.push(false);
    }
    Table::new(h, right)
}

/// Lead cells + token cells + money cell + optional tail cell.
fn data_row(
    mut lead: Vec<Cell>,
    t: &Totals,
    s: Spec,
    kind: CostKind,
    style: Style,
    tail: Option<Cell>,
) -> Vec<Cell> {
    lead.extend(tok_cells(t, s, style));
    if s.money != Money::Hidden {
        let tilde = s.money == Money::Mixed && kind == CostKind::ApiEquivalent;
        lead.push(cell(cost(t, tilde), style));
    }
    lead.extend(tail);
    lead
}

/// Cut to `max` chars keeping both ends, `user…ple.com`.
fn trunc_mid(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".repeat(max);
    }
    let keep = max - 1;
    let (head, tail) = (keep.div_ceil(2), keep / 2);
    let h: String = s.chars().take(head).collect();
    let t: String = s.chars().skip(n - tail).collect();
    format!("{h}…{t}")
}

/// `user@example.com (Acme Inc)` -> `user (Acme)`; aliases and ids stay as they are.
fn short_label(label: &str) -> String {
    let (addr, org) = match label.split_once(" (") {
        Some((a, o)) if o.ends_with(')') => (a, Some(&o[..o.len() - 1])),
        _ => (label, None),
    };
    let Some((local, _)) = addr.split_once('@') else {
        return label.to_string();
    };
    // A personal org is named after the email: it adds nothing.
    let org = org
        .and_then(|o| o.split_whitespace().next())
        .filter(|w| !w.contains('@'));
    match org {
        Some(w) => format!("{local} ({})", trunc(w, 10)),
        None => local.to_string(),
    }
}

/// The label as shown in tables: short unless `--full`.
fn disp(label: &str, o: &RenderOpts) -> String {
    if o.full {
        label.to_string()
    } else {
        short_label(label)
    }
}

fn account_cell(label: &str, o: &RenderOpts) -> Cell {
    cell(
        disp(label, o),
        if label == "unknown" {
            Style::Dim
        } else {
            Style::Plain
        },
    )
}

/// Every account label of a report, for the collision check.
fn labels_of(out: &Output) -> Vec<&str> {
    match &out.body {
        Body::Usage(r) => r.rows.iter().map(|x| x.account.label.as_str()).collect(),
        Body::Session(r) => r.rows.iter().map(|x| x.account.label.as_str()).collect(),
        Body::Blocks(r) => r.rows.iter().map(|x| x.account.label.as_str()).collect(),
        Body::Limits(r) => r.iter().map(|x| x.account.label.as_str()).collect(),
        Body::Accounts(_) => vec![],
    }
}

/// A note when two different accounts end up with the same short label.
fn collision_note(out: &Output, o: &RenderOpts) -> Option<String> {
    let mut seen: std::collections::BTreeMap<String, std::collections::BTreeSet<&str>> =
        Default::default();
    for l in labels_of(out) {
        seen.entry(disp(l, o)).or_default().insert(l);
    }
    let dup: Vec<&String> = seen
        .iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(k, _)| k)
        .collect();
    if dup.is_empty() {
        return None;
    }
    let names: Vec<&str> = dup.iter().map(|s| s.as_str()).collect();
    Some(format!(
        "{} look alike in this table; run acusage accounts for the full labels or set [aliases] in {}\n",
        names.join(", "),
        out.config_path
    ))
}

fn agent_name(tool: &str) -> String {
    let mut c = tool.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn day(ts: DateTime<Utc>, utc: bool) -> String {
    if utc {
        ts.format("%Y-%m-%d").to_string()
    } else {
        ts.with_timezone(&Local).format("%Y-%m-%d").to_string()
    }
}

fn stamp(ts: DateTime<Utc>, utc: bool) -> String {
    if utc {
        ts.format("%Y-%m-%d %H:%M UTC").to_string()
    } else {
        ts.with_timezone(&Local)
            .format("%Y-%m-%d %H:%M %:z")
            .to_string()
    }
}

/// How to build one table at a given compactness, and what to drop when it is still too wide.
struct Plan<'a> {
    /// Lines above the table (text, style).
    heading: Vec<(String, Style)>,
    build: Box<dyn Fn(bool) -> Table + 'a>,
    /// Columns dropped, in order, before switching to the compact layout.
    pre: &'static [&'static str],
    /// Columns dropped, in order, after it.
    post: &'static [&'static str],
    /// Has a compact layout at all.
    compactable: bool,
}

impl<'a> Plan<'a> {
    fn new(
        build: impl Fn(bool) -> Table + 'a,
        pre: &'static [&'static str],
        post: &'static [&'static str],
    ) -> Self {
        Plan {
            heading: vec![],
            build: Box::new(build),
            pre,
            post,
            compactable: true,
        }
    }
}

/// One fit routine for every view. Order: narrow the text columns (`…`), drop the least useful
/// columns, then the compact layout (and its own drops). `--full` keeps everything. Returns the
/// table and whether the compact layout was chosen automatically.
fn fit(p: &Plan, o: &RenderOpts) -> (Table, bool) {
    let Some(width) = o.width.filter(|_| !o.full) else {
        return ((p.build)(o.compact), false);
    };
    let mut stages: Vec<(bool, Vec<&str>)> = vec![];
    let all: Vec<&str> = p.pre.iter().chain(p.post).copied().collect();
    if !o.compact {
        for k in 0..=p.pre.len() {
            stages.push((false, p.pre[..k].to_vec()));
        }
    }
    if p.compactable || o.compact {
        for k in 0..=all.len() {
            stages.push((true, all[..k].to_vec()));
        }
    } else {
        for k in p.pre.len() + 1..=all.len() {
            stages.push((false, all[..k].to_vec()));
        }
    }
    let mut last = None;
    for (compact, drops) in stages {
        let mut t = (p.build)(compact);
        for d in &drops {
            t.drop_col(d);
        }
        t.shrink_to(width);
        let ok = t.width() <= width;
        last = Some((t, compact && p.compactable && !o.compact));
        if ok {
            break;
        }
    }
    last.expect("at least one stage")
}

/// Wrap every line of `s` at word boundaries to `width`; continuation lines are indented by two.
fn wrap(s: &str, width: Option<usize>) -> String {
    let Some(width) = width.map(|w| w.max(30)) else {
        return s.to_string();
    };
    let mut out = String::new();
    for line in s.lines() {
        let mut cur = String::new();
        let mut first = true;
        for word in line.split(' ') {
            let room = if first { width } else { width - 2 };
            if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > room {
                out.push_str(&cur);
                out.push('\n');
                cur = format!("  {word}");
                first = false;
            } else if cur.is_empty() {
                cur = word.to_string();
            } else {
                cur.push(' ');
                cur.push_str(word);
            }
        }
        out.push_str(&cur);
        out.push('\n');
    }
    out
}

/// Title box + tables + notes. Each plan is one table (with its heading); `notes` come first,
/// the compact-layout note next, `tail` (e.g. the default window) last.
fn view(
    out: &Output,
    accounts: String,
    tools: &[&str],
    o: &RenderOpts,
    plans: Vec<Plan>,
    notes: String,
    tail: &str,
) -> String {
    let mut s = String::new();
    let mut compacted = false;
    let mut body = String::new();
    for (i, p) in plans.iter().enumerate() {
        let (t, c) = fit(p, o);
        compacted |= c;
        if i > 0 {
            body.push('\n');
        }
        for (line, style) in &p.heading {
            body.push_str(&style.wrap(line, o.color));
            body.push('\n');
        }
        body.push_str(&t.render(o.color));
    }
    s.push_str(&title_box(
        &format!("ACUsage · {}", title_of(out)),
        &box_rows(out, &accounts, tools, compacted, o),
        crate::HOMEPAGE.trim_start_matches("https://"),
        o.color,
    ));
    s.push_str(&body);
    let mut n = notes;
    if compacted && let Some(w) = o.width {
        n.push_str(&format!(
            "compact layout (terminal is {w} columns); use --full or a wider terminal\n"
        ));
    }
    n.push_str(tail);
    s + &wrap(&n, o.width)
}

fn notes(
    out: &Output,
    (unpriced, local, sources): (&[String], &[String], &BTreeMap<String, PriceSource>),
    m: Money,
    o: &RenderOpts,
) -> String {
    let mut s = String::new();
    if m != Money::Hidden {
        let names = short_names(unpriced);
        if !names.is_empty() {
            s.push_str(&format!(
                "* excludes unpriced usage: {}; add prices under [prices] in {}\n",
                names.join(", "),
                out.config_path
            ));
        }
        if !local.is_empty() {
            s.push_str(&format!(
                "local models counted at $0: {}\n",
                short_names(local).join(", ")
            ));
        }
        if !lit_marks(sources, m).is_empty() {
            s.push_str(&format!(
                "{LITELLM_MARK} priced from LiteLLM snapshot (third-party, not checked)\n"
            ));
        }
        if m == Money::ApiValue {
            s.push_str("API Value = what this usage would cost at API prices; subscriptions are not billed per token.\n");
        }
        if m == Money::Mixed {
            s.push_str("~ marks API Value, what this usage would cost at API prices; subscriptions are not billed per token.\n");
        }
    }
    if let Some(c) = collision_note(out, o) {
        s.push_str(&c);
    }
    if m != Money::Hidden && has_unknown(out) {
        s.push_str(&format!(
            "unknown = usage whose log has no account record{}; see README#accounts\n",
            unknown_span(out)
        ));
    }
    s
}

fn unknown_span(out: &Output) -> String {
    match &out.unknown {
        Some(u) => format!(
            " ({} sessions, {}..{})",
            u.sessions,
            day(u.first, true),
            day(u.last, true)
        ),
        None => String::new(),
    }
}

fn tools_of<'a>(it: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut v: Vec<&str> = it.collect();
    v.sort();
    v.dedup();
    v
}

fn title_of(out: &Output) -> &'static str {
    match out.name {
        "weekly" => "Weekly Token Usage Report",
        "monthly" => "Monthly Token Usage Report",
        "session" => "Session Report",
        "blocks" => "Billing Blocks",
        "accounts" => "Accounts",
        "limits" => "Rate Limits",
        _ => "Daily Token Usage Report",
    }
}

/// `3 + unknown`; usage without an account record is not counted as an account.
fn count_accounts<'a>(it: impl Iterator<Item = &'a crate::report::AccountInfo>) -> String {
    let mut named: Vec<_> = vec![];
    let mut unknown = false;
    for a in it {
        if a.source == AccountSource::Unknown {
            unknown = true;
        } else {
            named.push((&a.tool, &a.id, &a.org_id));
        }
    }
    named.sort();
    named.dedup();
    match (named.len(), unknown) {
        (0, _) => "unknown".into(),
        (n, false) => n.to_string(),
        (n, true) => format!("{n} + unknown"),
    }
}

fn product_name(tool: &str) -> String {
    match tool {
        "claude" => "Claude Code".into(),
        "codex" => "Codex".into(),
        "antigravity" => "Antigravity".into(),
        "gemini" => "Gemini CLI".into(),
        other => agent_name(other),
    }
}

fn date_text(d: chrono::NaiveDate) -> String {
    d.format("%-d %b %Y").to_string()
}

/// Join `parts` with ", " into lines of at most `max` characters (one line when `None`).
fn join_parts(parts: &[String], max: Option<usize>) -> Vec<String> {
    let mut lines: Vec<String> = vec![];
    for p in parts {
        match lines.last_mut() {
            Some(l) if max.is_none_or(|m| l.chars().count() + 2 + p.chars().count() <= m) => {
                l.push_str(", ");
                l.push_str(p);
            }
            _ => lines.push(p.clone()),
        }
    }
    lines
}

/// The Period value: dates, time zone and the window option.
fn period_text(out: &Output, o: &RenderOpts) -> String {
    let q = &out.query;
    let today = out.generated_at.date_naive();
    let (first, last) = q.period.unwrap_or((today, today));
    let (a, b) = (q.since.unwrap_or(first), q.until.unwrap_or(last));
    let mut t = if a == b {
        date_text(a)
    } else {
        format!("{} to {}", date_text(a), date_text(b))
    };
    t.push_str(&if o.utc {
        " (UTC)".to_string()
    } else {
        format!(
            " (UTC{})",
            out.generated_at.with_timezone(&Local).format("%:z")
        )
    });
    if matches!(out.name, "daily" | "weekly" | "monthly") {
        let unit = match out.name {
            "weekly" => "week",
            "monthly" => "month",
            _ => "day",
        };
        if let Some(n) = q.last {
            t.push_str(&format!(
                ", last {n} {unit}{}",
                if n == 1 { "" } else { "s" }
            ));
        } else if out.default_window {
            t.push_str(", last 30 days");
        }
    }
    t
}

/// The View value (empty when the view is the default one).
fn view_parts(out: &Output, compacted: bool) -> Vec<String> {
    let q = &out.query;
    let usage = matches!(out.name, "daily" | "weekly" | "monthly");
    let mut v = vec![];
    if q.breakdown && usage {
        v.push("by model".to_string());
    }
    if q.instances && usage {
        v.push("by project".to_string());
    }
    if compacted {
        v.push("compact".to_string());
    }
    v
}

/// The Filter value (empty when no filter was given).
fn filter_parts(out: &Output) -> Vec<String> {
    let q = &out.query;
    let mut v = vec![];
    if let Some(t) = q.tool {
        v.push(format!("tool: {}", product_name(t)));
    }
    if !q.accounts.is_empty() {
        v.push(format!("account: {}", q.accounts.join(", ")));
    }
    if let Some(p) = &q.project {
        v.push(format!("project: {p}"));
    }
    v
}

/// The label rows of the title box.
fn box_rows(
    out: &Output,
    accounts: &str,
    tools: &[&str],
    compacted: bool,
    o: &RenderOpts,
) -> Vec<(&'static str, String)> {
    // Two columns of padding, two of borders, then the widest label and its gap.
    let max = o.width.map(|w| w.max(30).saturating_sub(4 + 10 + 2));
    let mut names: Vec<String> = tools.iter().map(|t| product_name(t)).collect();
    names.sort();
    names.dedup();
    let joined = |v: Vec<String>| join_parts(&v, max).join("\n");
    let mut rows = vec![
        ("Period", period_text(out, o)),
        ("Accounts", accounts.to_string()),
        ("Agents", joined(names)),
    ];
    for (label, v) in [
        ("View", view_parts(out, compacted)),
        ("Filter", filter_parts(out)),
    ] {
        if !v.is_empty() {
            rows.push((label, joined(v)));
        }
    }
    rows
}

fn has_unknown(out: &Output) -> bool {
    let is = |a: &crate::report::AccountInfo| a.source == AccountSource::Unknown;
    match &out.body {
        Body::Usage(r) => r.rows.iter().any(|x| is(&x.account)),
        Body::Session(r) => r.rows.iter().any(|x| is(&x.account)),
        Body::Blocks(r) => r.rows.iter().any(|x| is(&x.account)),
        Body::Accounts(r) => r.rows.iter().any(|x| is(&x.account)),
        Body::Limits(_) => false,
    }
}

/// Models most expensive first when the report ranked them, else in name order.
fn ranked<'a>(models: &'a [String], by_cost: &'a [String]) -> &'a [String] {
    if by_cost.is_empty() { models } else { by_cost }
}

fn text(out: &Output, o: &RenderOpts) -> String {
    let tail = if out.default_window {
        "showing last 30 days; use -s/--since for more\n"
    } else {
        ""
    };
    match &out.body {
        Body::Usage(r) => {
            if r.rows.is_empty() {
                return "No usage found.\n".into();
            }
            let m = money_mode(r.rows.iter().map(|x| x.cost_kind), o);
            let tools = tools_of(r.rows.iter().map(|x| x.account.tool.as_str()));
            let reasoning = tools == ["codex"];
            let hide = tools.len() <= 1;
            let first = match out.name {
                "weekly" => "Week",
                "monthly" => "Month",
                _ => "Date",
            };
            let build = move |compact: bool| {
                let s = Spec {
                    compact,
                    reasoning,
                    money: m,
                };
                let mut t = usage_table(r, s, first, o);
                if hide {
                    t.drop_col("Agent");
                }
                t
            };
            let p = Plan::new(build, &[], &["Models", "Agent"]);
            view(
                out,
                count_accounts(r.rows.iter().map(|x| &x.account)),
                &tools,
                o,
                vec![p],
                notes(
                    out,
                    (&r.unpriced_models, &r.local_models, &r.price_sources),
                    m,
                    o,
                ),
                tail,
            )
        }
        Body::Session(r) => {
            if r.rows.is_empty() {
                return "No usage found.\n".into();
            }
            let m = money_mode(r.rows.iter().map(|x| x.cost_kind), o);
            let tools = tools_of(r.rows.iter().map(|x| x.account.tool.as_str()));
            let reasoning = tools == ["codex"];
            let hide = tools.len() <= 1;
            let acap = if o.full {
                ACCOUNT_CAP_FULL
            } else {
                ACCOUNT_CAP
            };
            let build = move |compact: bool| {
                let s = Spec {
                    compact,
                    reasoning,
                    money: m,
                };
                let lit = lit_marks(&r.price_sources, m);
                let mut t = head_row(
                    &["Session", "Agent", "Account", "Project", "Models"],
                    s,
                    Some("Last Activity"),
                )
                .cap(2, acap, 8)
                .mid(2)
                .cap(4, MODELS_CAP, 10)
                .cap(3, PROJECT_CAP, 8);
                for row in &r.rows {
                    let id = row.session_id.as_deref().map_or("-".to_string(), |s| {
                        if o.full {
                            s.to_string()
                        } else {
                            s.chars().take(8).collect()
                        }
                    });
                    let lead = vec![
                        plain(id),
                        plain(product_name(&row.account.tool)),
                        account_cell(&row.account.label, o),
                        plain(row.project.clone().unwrap_or_else(|| "-".into())),
                        plain(models_cell(ranked(&row.models, &row.models_ranked), &lit)),
                    ];
                    t.rows.push(data_row(
                        lead,
                        &row.totals,
                        s,
                        row.cost_kind,
                        Style::Plain,
                        Some(cell(clock(row.last_activity, o.utc), Style::Cyan)),
                    ));
                }
                let kind = merged(r.rows.iter().map(|x| x.cost_kind));
                let mut lead = vec![cell("Total", Style::YellowBold)];
                lead.resize_with(5, || plain(""));
                t.total = Some(data_row(
                    lead,
                    &r.totals,
                    s,
                    kind,
                    Style::YellowBold,
                    Some(plain("")),
                ));
                if hide {
                    t.drop_col("Agent");
                }
                t
            };
            let p = Plan::new(build, &[], &["Models", "Project", "Agent"]);
            view(
                out,
                count_accounts(r.rows.iter().map(|x| &x.account)),
                &tools,
                o,
                vec![p],
                notes(
                    out,
                    (&r.unpriced_models, &r.local_models, &r.price_sources),
                    m,
                    o,
                ),
                tail,
            )
        }
        Body::Accounts(r) => {
            if r.rows.is_empty() {
                return "No usage found.\n".into();
            }
            let m = money_mode(r.rows.iter().map(|x| x.cost_kind), o);
            let tools = tools_of(r.rows.iter().map(|x| x.account.tool.as_str()));
            let reasoning = tools == ["codex"];
            let hide = tools.len() <= 1;
            // This view is where the full labels live.
            let of = RenderOpts {
                full: true,
                ..o.clone()
            };
            let build = move |compact: bool| {
                let s = Spec {
                    compact,
                    reasoning,
                    money: m,
                };
                let mut t = head_row(
                    &["Account", "Agent", "Billing", "First Seen", "Last Seen"],
                    s,
                    None,
                )
                .cap(0, ACCOUNT_CAP_FULL, 8)
                .mid(0);
                for row in &r.rows {
                    let lead = vec![
                        account_cell(&row.account.label, &of),
                        plain(product_name(&row.account.tool)),
                        plain(format!("{:?}", row.account.billing).to_lowercase()),
                        cell(day(row.first_seen, of.utc), Style::Cyan),
                        cell(day(row.last_seen, of.utc), Style::Cyan),
                    ];
                    t.rows.push(data_row(
                        lead,
                        &row.totals,
                        s,
                        row.cost_kind,
                        Style::Plain,
                        None,
                    ));
                }
                let kind = merged(r.rows.iter().map(|x| x.cost_kind));
                let mut lead = vec![cell("Total", Style::YellowBold)];
                lead.resize_with(5, || plain(""));
                t.total = Some(data_row(lead, &r.totals, s, kind, Style::YellowBold, None));
                if hide {
                    t.drop_col("Agent");
                }
                t
            };
            let p = Plan::new(build, &[], &["Billing", "First Seen", "Last Seen", "Agent"]);
            view(
                out,
                count_accounts(r.rows.iter().map(|x| &x.account)),
                &tools,
                o,
                vec![p],
                notes(
                    out,
                    (&r.unpriced_models, &r.local_models, &r.price_sources),
                    m,
                    o,
                ),
                tail,
            )
        }
        Body::Blocks(r) => blocks(out, r, out.generated_at, o, tail),
        Body::Limits(rows) => limits(out, rows, out.generated_at, o),
    }
}

/// Date, Agent, Account, Models and the numbers; with `-i` a heading and a subtotal per project.
fn usage_table(r: &UsageReport, s: Spec, first: &'static str, o: &RenderOpts) -> Table {
    let acap = if o.full {
        ACCOUNT_CAP_FULL
    } else {
        ACCOUNT_CAP
    };
    let lit = lit_marks(&r.price_sources, s.money);
    let mut t = head_row(&[first, "Agent", "Account", "Models"], s, None)
        .cap(2, acap, 8)
        .mid(2)
        .cap(3, MODELS_CAP, 10);
    let by_project = r.rows.iter().any(|x| x.project.is_some());
    let project_of = |x: &crate::report::Row| x.project.clone().unwrap_or("(no project)".into());
    let projects = {
        let mut p: Vec<String> = r.rows.iter().map(project_of).collect();
        p.dedup();
        p.len()
    };
    // Close the project that started at `from`: a subtotal row when there are several.
    let subtotal = |t: &mut Table, from: usize, to: usize| {
        if projects < 2 || from >= to {
            return;
        }
        let g = &r.rows[from..to];
        let sum = sum_totals(g.iter().map(|x| &x.totals));
        let kind = merged(g.iter().map(|x| x.cost_kind));
        let lead = vec![
            cell("Subtotal", Style::Yellow),
            plain(""),
            plain(""),
            plain(""),
        ];
        t.rows
            .push(data_row(lead, &sum, s, kind, Style::Yellow, None));
    };
    let mut i = 0;
    let mut project: Option<String> = None;
    let mut start = 0;
    while i < r.rows.len() {
        let j = r.rows[i..]
            .iter()
            .position(|x| x.bucket != r.rows[i].bucket || x.project != r.rows[i].project)
            .map_or(r.rows.len(), |p| i + p);
        let group = &r.rows[i..j];
        if by_project && project.as_deref() != Some(&project_of(&group[0])) {
            if project.is_some() {
                subtotal(&mut t, start, i);
            }
            start = i;
            let p = project_of(&group[0]);
            t.spans.push(t.rows.len());
            t.rows
                .push(vec![cell(format!("Project: {p}"), Style::Cyan)]);
            project = Some(p);
        }
        let multi = group.len() >= 2;
        let date = |first: bool| {
            if first {
                cell(group[0].bucket.clone(), Style::Cyan)
            } else {
                plain("")
            }
        };
        if multi {
            let sum = sum_totals(group.iter().map(|x| &x.totals));
            let kind = merged(group.iter().map(|x| x.cost_kind));
            let lead = vec![date(true), plain("All"), plain(""), plain("")];
            t.rows
                .push(data_row(lead, &sum, s, kind, Style::Plain, None));
        }
        for (k, row) in group.iter().enumerate() {
            let agent = product_name(&row.account.tool);
            let names = ranked(&row.models, &row.models_ranked);
            let subs = !row.breakdown.is_empty() && short_names(names).len() >= 2;
            let models = if row.breakdown.is_empty() {
                models_cell(names, &lit)
            } else {
                breakdown_models_cell(names, &lit)
            };
            let lead = vec![
                date(!multi && k == 0),
                plain(if multi { format!("- {agent}") } else { agent }),
                account_cell(&row.account.label, o),
                plain(models),
            ];
            t.rows.push(data_row(
                lead,
                &row.totals,
                s,
                row.cost_kind,
                Style::Plain,
                None,
            ));
            let mut subrows: Vec<&crate::report::ModelRow> = row.breakdown.iter().collect();
            subrows.sort_by(|a, b| {
                b.totals
                    .cost_usd
                    .unwrap_or(0.0)
                    .total_cmp(&a.totals.cost_usd.unwrap_or(0.0))
                    .then(b.totals.total_tokens.cmp(&a.totals.total_tokens))
            });
            for m in subrows.into_iter().filter(|_| subs) {
                let lead = vec![
                    plain(""),
                    plain(""),
                    plain(""),
                    cell(
                        format!(
                            "└─ {}",
                            marked_names(std::slice::from_ref(&m.model), &lit)[0]
                        ),
                        Style::Dim,
                    ),
                ];
                t.glued.push(t.rows.len());
                t.rows.push(data_row(
                    lead,
                    &m.totals,
                    s,
                    row.cost_kind,
                    Style::Dim,
                    None,
                ));
            }
        }
        i = j;
    }
    if project.is_some() {
        subtotal(&mut t, start, r.rows.len());
    }
    let kind = merged(r.rows.iter().map(|x| x.cost_kind));
    let lead = vec![
        cell("Total", Style::YellowBold),
        plain(""),
        plain(""),
        plain(""),
    ];
    t.total = Some(data_row(lead, &r.totals, s, kind, Style::YellowBold, None));
    t
}

// ---------------------------------------------------------------- blocks

/// `3h 12m`, or `55m` under an hour.
fn hm(d: chrono::Duration) -> String {
    let mins = d.num_minutes().max(0);
    if mins >= 60 {
        format!("{}h {}m", mins / 60, mins % 60)
    } else {
        format!("{mins}m")
    }
}

fn clock(ts: DateTime<Utc>, utc: bool) -> String {
    if utc {
        ts.format("%Y-%m-%d %H:%M").to_string()
    } else {
        ts.with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    }
}

fn pct_cell(tokens: u64, limit: u64) -> Cell {
    if limit == 0 {
        return plain("-");
    }
    let p = tokens as f64 / limit as f64 * 100.0;
    let style = if p > 100.0 {
        Style::Red
    } else if p > 80.0 {
        Style::Yellow
    } else {
        Style::Plain
    };
    cell(format!("{p:.1}%"), style)
}

/// Token cells of the blocks table: the compact form keeps Total Tokens (the projections need it).
fn block_tokens(t: &Totals, s: Spec, style: Style) -> Vec<Cell> {
    if s.compact {
        let n = |v: u64| cell(thousands(v), style);
        vec![n(t.input_tokens), n(t.output_tokens), n(t.total_tokens)]
    } else {
        tok_cells(t, s, style)
    }
}

/// One table for the blocks of one account.
fn block_table(
    group: &[&BlockRow],
    lit: &BTreeSet<String>,
    s: Spec,
    now: DateTime<Utc>,
    o: &RenderOpts,
) -> Table {
    let m = s.money;
    let limited = group.iter().any(|x| x.token_limit.is_some());
    let mut head = vec!["Block Start", "Duration/Status", "Models"];
    let ntok = block_tokens(&Totals::default(), s, Style::Plain).len();
    head.extend(if s.compact {
        vec!["Input", "Output", "Total Tokens"]
    } else {
        tok_head(s)
    });
    if limited {
        head.push("%");
    }
    head.extend(m.head());
    if s.compact {
        for h in head.iter_mut() {
            if *h == "API Value (USD)" {
                *h = "API Value";
            }
        }
    }
    let mut right = vec![false; 3];
    right.resize(head.len(), true);
    let mut t = Table::new(head, right).cap(2, MODELS_CAP, 10);
    // Numbers after the three lead cells: tokens, optional %, optional money.
    let tail = |tokens: Vec<Cell>, pct: Option<Cell>, money: Option<Cell>| {
        let mut v = tokens;
        if limited {
            v.push(pct.unwrap_or_else(|| plain("")));
        }
        if m != Money::Hidden {
            v.push(money.unwrap_or_else(|| plain("")));
        }
        v
    };
    // A row that only fills Total Tokens (the last token cell).
    let total_only = |v: u64| {
        let mut c: Vec<Cell> = (0..ntok).map(|_| plain("")).collect();
        *c.last_mut().expect("token columns") = plain(thousands(v));
        c
    };
    for row in group {
        if row.gap {
            let secs = (row.end - row.start).num_seconds();
            t.spans.push(t.rows.len());
            t.rows.push(vec![cell(
                format!("idle {}", hm(chrono::Duration::seconds(secs))),
                Style::Dim,
            )]);
            continue;
        }
        let tilde = m == Money::Mixed && row.cost_kind == CostKind::ApiEquivalent;
        let status = if row.active {
            cell(format!("ACTIVE\n{} left", hm(row.end - now)), Style::Green)
        } else {
            let last = row.actual_end.unwrap_or(row.start);
            plain(hm(last - row.start))
        };
        let mut v = vec![
            cell(clock(row.start, o.utc), Style::Cyan),
            status,
            plain(models_cell(ranked(&row.models, &row.models_ranked), lit)),
        ];
        v.extend(tail(
            block_tokens(&row.totals, s, Style::Plain),
            row.token_limit
                .map(|l| pct_cell(row.totals.total_tokens, l)),
            Some(plain(cost(&row.totals, tilde))),
        ));
        t.rows.push(v);
        if !row.active {
            continue;
        }
        // Active block: where the limit and the burn rate lead.
        let extra = |what: String, tokens: u64, pct: Option<Cell>, money: Option<Cell>| {
            let mut v = vec![plain(""), cell(what, Style::Yellow), plain("")];
            v.extend(tail(total_only(tokens), pct, money));
            v
        };
        if let Some(l) = row.token_limit {
            let left = l.saturating_sub(row.totals.total_tokens);
            t.rows.push(extra(
                "REMAINING".into(),
                left,
                Some(pct_cell(left, l)),
                None,
            ));
        }
        if let Some(p) = &row.projection {
            let proj = Totals {
                cost_usd: p.projected_cost_usd,
                ..Totals::default()
            };
            t.rows.push(extra(
                format!(
                    "PROJECTED\n{} tok/min",
                    thousands(p.burn_rate_tokens_per_min.round() as u64)
                ),
                p.projected_tokens,
                row.token_limit.map(|l| pct_cell(p.projected_tokens, l)),
                Some(plain(cost(&proj, tilde))),
            ));
        }
    }
    let real: Vec<&&BlockRow> = group.iter().filter(|x| !x.gap).collect();
    let sum = sum_totals(real.iter().map(|x| &x.totals));
    let kind = merged(real.iter().map(|x| x.cost_kind));
    let tilde = m == Money::Mixed && kind == CostKind::ApiEquivalent;
    let mut v = vec![cell("Total", Style::YellowBold), plain(""), plain("")];
    v.extend(tail(
        block_tokens(&sum, s, Style::YellowBold),
        None,
        Some(cell(cost(&sum, tilde), Style::YellowBold)),
    ));
    t.total = Some(v);
    t
}

fn blocks(
    out: &Output,
    r: &BlocksReport,
    now: DateTime<Utc>,
    o: &RenderOpts,
    tail: &str,
) -> String {
    if r.rows.is_empty() {
        return "No usage found.\n".into();
    }
    let m = money_mode(r.rows.iter().filter(|x| !x.gap).map(|x| x.cost_kind), o);
    let tools = tools_of(r.rows.iter().map(|x| x.account.tool.as_str()));
    // One run of blocks per account: one section each.
    let mut groups: Vec<Vec<&BlockRow>> = vec![];
    for row in &r.rows {
        let same = groups.last().and_then(|g| g.first()).is_some_and(|f| {
            f.account.tool == row.account.tool && f.account.label == row.account.label
        });
        if same {
            groups.last_mut().expect("group").push(row);
        } else {
            groups.push(vec![row]);
        }
    }
    let lit = &lit_marks(&r.price_sources, m);
    let plans: Vec<Plan> = groups
        .into_iter()
        .map(|group| {
            let first = &group[0].account;
            let reasoning = first.tool == "codex";
            let mut heading = vec![(
                format!("{} ({})", disp(&first.label, o), product_name(&first.tool)),
                Style::Cyan,
            )];
            if first.tool == "codex" {
                heading.push((
                    format!(
                        "estimated {}h blocks; real Codex windows: acusage limits",
                        r.session_hours
                    ),
                    Style::Dim,
                ));
            }
            if let Some(l) = group.iter().find_map(|x| x.token_limit) {
                let how = if r.limit_is_max {
                    " (max of earlier blocks)"
                } else {
                    ""
                };
                heading.push((format!("limit {} tokens{how}", thousands(l)), Style::Dim));
            }
            let mut p = Plan::new(
                move |compact: bool| {
                    let s = Spec {
                        compact,
                        reasoning,
                        money: m,
                    };
                    block_table(&group, lit, s, now, o)
                },
                &[],
                &["Models", "Input", "Output"],
            );
            p.heading = heading;
            p
        })
        .collect();
    let mut n = notes(
        out,
        (&r.unpriced_models, &r.local_models, &r.price_sources),
        m,
        o,
    );
    n.push_str(&format!(
        "Blocks start on the hour of the first activity ({} time shown).\n",
        if o.utc { "UTC" } else { "local" }
    ));
    view(
        out,
        count_accounts(r.rows.iter().map(|x| &x.account)),
        &tools,
        o,
        plans,
        n,
        tail,
    )
}

// ---------------------------------------------------------------- limits

fn window_name(minutes: Option<u64>) -> String {
    match minutes {
        None => "-".into(),
        Some(m) if m > 0 && m % 1440 == 0 => format!("{}d", m / 1440),
        Some(m) if m > 0 && m % 60 == 0 => format!("{}h", m / 60),
        Some(m) => format!("{m}m"),
    }
}

/// `in 2h 13m`.
fn relative(secs: i64) -> String {
    let abs = secs.unsigned_abs();
    let (d, h, m) = (abs / 86400, abs % 86400 / 3600, abs % 3600 / 60);
    let span = if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    };
    if secs >= 0 {
        format!("in {span}")
    } else {
        format!("{span} ago")
    }
}

/// Reset cell and whether the window already expired.
fn resets(w: &LimitWindow, now: DateTime<Utc>) -> (Cell, bool) {
    match w.resets_at.and_then(|s| DateTime::from_timestamp(s, 0)) {
        Some(ts) if ts < now => (cell("expired", Style::Dim), true),
        Some(ts) => (plain(relative((ts - now).num_seconds())), false),
        None => (plain("-"), false),
    }
}

fn limits(out: &Output, rows: &[LimitRow], now: DateTime<Utc>, o: &RenderOpts) -> String {
    let acap = if o.full {
        ACCOUNT_CAP_FULL
    } else {
        ACCOUNT_CAP
    };
    let build = move |_compact: bool| {
        let mut t = Table::new(
            vec!["Account", "Plan", "Window", "Used", "Resets", "Observed"],
            vec![false; 6],
        )
        .cap(0, acap, 8)
        .mid(0);
        let mut items: Vec<(bool, Vec<Cell>)> = vec![];
        for r in rows {
            for w in [&r.primary, &r.secondary].into_iter().flatten() {
                let (reset, expired) = resets(w, now);
                let style = if expired {
                    Style::Dim
                } else if w.used_percent >= 80.0 {
                    Style::Red
                } else if w.used_percent >= 50.0 {
                    Style::Yellow
                } else {
                    Style::Green
                };
                // An expired window has no live bar: its number is history.
                let used = if expired {
                    format!("{:>6}", format!("{:.1}%", w.used_percent))
                } else {
                    format!(
                        "{:>6} {}",
                        format!("{:.1}%", w.used_percent),
                        bar(w.used_percent)
                    )
                };
                let dim = |c: Cell| if expired { cell(c.text, Style::Dim) } else { c };
                items.push((
                    expired,
                    vec![
                        dim(account_cell(&r.account.label, o)),
                        dim(plain(r.plan_type.clone().unwrap_or_else(|| "-".into()))),
                        dim(plain(window_name(w.window_minutes))),
                        cell(used, style),
                        reset,
                        dim(plain(stamp(r.observed_at, o.utc))),
                    ],
                ));
            }
        }
        items.sort_by_key(|(expired, _)| *expired);
        t.rows = items.into_iter().map(|(_, row)| row).collect();
        t
    };
    let has_rows = rows
        .iter()
        .any(|r| r.primary.is_some() || r.secondary.is_some());
    let body = if has_rows {
        let tools = tools_of(rows.iter().map(|r| r.account.tool.as_str()));
        let mut p = Plan::new(build, &[], &["Observed", "Plan"]);
        p.compactable = false;
        let mut n = String::new();
        if let Some(c) = collision_note(out, o) {
            n.push_str(&c);
        }
        view(
            out,
            count_accounts(rows.iter().map(|r| &r.account)),
            &tools,
            o,
            vec![p],
            n,
            "",
        )
    } else {
        "No limit data found.\n".to_string()
    };
    format!("{body}\n{}", wrap(&format!("{LIMITS_NOTE}\n"), o.width))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_dedupes_dated_ids() {
        let ids = ["m:4b-20250101", "m:4b-20250202", "other:1b"].map(String::from);
        assert_eq!(short_names(&ids), ["m:4b", "other:1b"]);
    }

    #[test]
    fn partial_cost_gets_a_star() {
        let t = Totals {
            cost_usd: Some(12.5),
            partial_cost: true,
            ..Totals::default()
        };
        assert_eq!(cost(&t, false), "$12.50*");
        assert_eq!(cost(&t, true), "~$12.50*");
        let full = Totals {
            partial_cost: false,
            ..t.clone()
        };
        assert_eq!(cost(&full, false), "$12.50");
        assert_eq!(cost(&Totals::default(), false), "-");
    }

    #[test]
    fn model_names_are_shortened() {
        assert_eq!(short_model("claude-opus-5-5"), "opus-5-5");
        assert_eq!(short_model("claude-haiku-4-5-20251001"), "haiku-4-5");
        assert_eq!(short_model("gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn models_one_per_line_without_duplicates() {
        let m = [
            "claude-a-1".to_string(),
            "claude-a-1-20250101".into(),
            "b".into(),
        ];
        assert_eq!(models_cell(&m, &BTreeSet::new()), "- a-1\n- b");
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn multi_line_cells_and_total_row() {
        let mut t = Table::new(vec!["A", "N"], vec![false, true]);
        t.rows.push(vec![plain("- x\n- y"), plain("1")]);
        t.total = Some(vec![cell("Total", Style::Yellow), plain("1")]);
        let s = t.render(false);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 8);
        assert!(lines[4].contains("- y"));
        assert!(lines[5].starts_with('├'));
        assert!(lines.iter().all(|l| l.chars().count() == t.width()));
    }

    #[test]
    fn shrink_narrows_long_column_to_fit() {
        let mut t = Table::new(vec!["Account", "N"], vec![false, true]).cap(0, 40, 8);
        t.rows.push(vec![plain("a".repeat(40)), plain("1")]);
        let full = t.width();
        t.shrink_to(full - 10);
        assert_eq!(t.width(), full - 10);
        assert!(t.render(false).contains('…'));
    }

    #[test]
    fn breakdown_models_cell_counts_from_two() {
        let one = vec!["claude-a-1".to_string(), "claude-a-1".to_string()];
        assert_eq!(breakdown_models_cell(&one, &BTreeSet::new()), "a-1");
        let two = vec!["claude-a-1".to_string(), "b".to_string()];
        assert_eq!(breakdown_models_cell(&two, &BTreeSet::new()), "2 models");
    }

    #[test]
    fn no_color_beats_tty_and_force() {
        assert!(use_color(false, false, false, true));
        assert!(!use_color(false, false, false, false));
        assert!(use_color(true, false, false, false));
        assert!(!use_color(false, true, false, true));
        assert!(!use_color(false, false, true, true));
        assert!(!use_color(true, true, false, false));
    }

    #[test]
    fn breakdown_subrows_have_no_separator() {
        let mut t = Table::new(vec!["H"], vec![false]);
        t.rows.push(vec![plain("a")]);
        t.rows.push(vec![plain("b")]);
        t.glued.push(1);
        t.rows.push(vec![plain("c")]);
        assert_eq!(t.render(false).matches('├').count(), 2);
    }

    #[test]
    fn colour_wraps_text_only() {
        let mut t = Table::new(vec!["H"], vec![false]);
        t.rows.push(vec![cell("ab", Style::Cyan)]);
        let s = t.render(true);
        assert!(s.contains("\x1b[36mab\x1b[0m "));
        assert!(!t.render(false).contains('\x1b'));
    }

    #[test]
    fn box_parts_wrap_at_separators() {
        let p: Vec<String> = ["aaaa", "bbbb", "cccc"].map(String::from).to_vec();
        assert_eq!(join_parts(&p, None), vec!["aaaa, bbbb, cccc"]);
        assert_eq!(join_parts(&p, Some(10)), vec!["aaaa, bbbb", "cccc"]);
        assert!(join_parts(&[], Some(12)).is_empty());
    }

    #[test]
    fn product_names_and_dates() {
        assert_eq!(product_name("claude"), "Claude Code");
        assert_eq!(product_name("gemini"), "Gemini CLI");
        let d = chrono::NaiveDate::from_ymd_opt(2026, 11, 7).unwrap();
        assert_eq!(date_text(d), "7 Nov 2026");
    }

    #[test]
    fn title_box_is_left_aligned_and_styled() {
        let rows = vec![("Period", "x".to_string()), ("Agents", "a\nb".to_string())];
        let plain = title_box("ACUsage · T", &rows, "example.com", false);
        // Longest line is the title row "Agents  a" vs domain: 11 chars + 2 padding + 2 borders.
        assert!(plain.lines().all(|l| l.chars().count() == 15));
        assert!(!plain.contains('\x1b'));
        assert!(plain.contains("│ Period  x"));
        assert!(plain.contains("│ Agents  a"));
        assert!(plain.contains("│         b"));
        assert!(plain.contains("example.com │"));
        let col = title_box("ACUsage · T", &rows, "example.com", true);
        assert!(col.contains("\x1b[1mACUsage · T\x1b[0m"));
        assert!(col.contains("\x1b[2mPeriod\x1b[0m"));
        assert!(col.contains("\x1b[2mexample.com\x1b[0m"));
    }

    #[test]
    fn litellm_id_marks_its_line_and_collapsed_ids_mark_the_line() {
        let m = ["claude-x-20250101", "claude-x", "b"].map(String::from);
        let lit: BTreeSet<String> = ["claude-x-20250101".to_string()].into();
        assert_eq!(models_cell(&m, &lit), "- x\u{2020}\n- b");
        let one = ["b".to_string()];
        assert_eq!(breakdown_models_cell(&one, &lit), "b");
        let lit: BTreeSet<String> = ["b".to_string()].into();
        assert_eq!(breakdown_models_cell(&one, &lit), "b\u{2020}");
    }

    #[test]
    fn money_hidden_has_no_marks() {
        let src = BTreeMap::from([("a".to_string(), PriceSource::Litellm)]);
        assert!(lit_marks(&src, Money::Hidden).is_empty());
        assert_eq!(lit_marks(&src, Money::Billed).len(), 1);
    }

    #[test]
    fn trunc_keeps_the_mark_after_the_cut() {
        assert_eq!(
            trunc("gemini-3.8-flash\u{2020}", 8),
            "gemini\u{2026}\u{2020}"
        );
        assert_eq!(trunc("gemini-3.8-flash\u{2020}", 8).chars().count(), 8);
        assert_eq!(trunc("abc\u{2020}", 4), "abc\u{2020}");
        assert_eq!(trunc("abcdef", 4), "abc\u{2026}");
    }
}
