//! Pure aggregation: records + labels + prices + options -> report structs.
//! No I/O and no rendering (see `render`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Datelike, Duration, FixedOffset, Local, NaiveDate, Utc};
use serde::Serialize;

use crate::cli::{Order, Tool};
use crate::model::{
    AccountRef, AccountSource, Billing, CostKind, LimitSnapshot, LimitWindow, UsageRecord,
};
use crate::pricing::{PriceSource, PriceTable};
use crate::sources::claude::AccountLabel;

/// Time zone used for bucketing and for `--since/--until`.
#[derive(Debug, Clone, Copy)]
pub enum Zone {
    Local,
    Utc,
    /// Fixed offset (tests, and a stable alternative to `Local`).
    Fixed(FixedOffset),
}

impl Zone {
    pub fn date(self, ts: DateTime<Utc>) -> NaiveDate {
        match self {
            Zone::Local => ts.with_timezone(&Local).date_naive(),
            Zone::Utc => ts.date_naive(),
            Zone::Fixed(o) => ts.with_timezone(&o).date_naive(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Day,
    Week,
    Month,
}

impl Bucket {
    /// Day `YYYY-MM-DD`; week: its Monday as `YYYY-MM-DD`; month `YYYY-MM`.
    pub fn key(self, d: NaiveDate) -> String {
        match self {
            Bucket::Day => d.format("%Y-%m-%d").to_string(),
            Bucket::Week => {
                let monday = d - Duration::days(i64::from(d.weekday().num_days_from_monday()));
                monday.format("%Y-%m-%d").to_string()
            }
            Bucket::Month => d.format("%Y-%m").to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub zone: Zone,
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
    pub accounts: Vec<String>,
    pub tool: Option<Tool>,
    /// Fill `Row::breakdown` with one entry per model.
    pub breakdown: bool,
    pub order: Order,
    /// Keep only the most recent N buckets (day, week or month).
    pub last: Option<usize>,
    /// Group usage reports by project first.
    pub instances: bool,
    /// Keep only records whose project path contains this text (case-insensitive).
    pub project: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            zone: Zone::Utc,
            since: None,
            until: None,
            accounts: Vec::new(),
            tool: None,
            breakdown: false,
            order: Order::Asc,
            last: None,
            instances: false,
            project: None,
        }
    }
}

// ---------------------------------------------------------------- labels

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelSource {
    Alias,
    Email,
    Id,
    Unknown,
}

/// Resolves display labels: alias (id, then email) > email (+ org name) > id prefix.
pub struct Labeler<'a> {
    labels: &'a HashMap<String, AccountLabel>,
    aliases: &'a HashMap<String, String>,
    /// `(tool, id)` of accounts that reported a plan type (Codex subscription evidence).
    plans: HashSet<(String, String)>,
}

impl<'a> Labeler<'a> {
    pub fn new(
        labels: &'a HashMap<String, AccountLabel>,
        aliases: &'a HashMap<String, String>,
    ) -> Self {
        Labeler {
            labels,
            aliases,
            plans: HashSet::new(),
        }
    }

    /// Record the accounts whose limit snapshots carry a `plan_type`.
    pub fn with_plans(mut self, snaps: &[LimitSnapshot]) -> Self {
        self.plans = snaps
            .iter()
            .filter(|s| s.plan_type.is_some())
            .map(|s| (s.account.tool.clone(), s.account.id.clone()))
            .collect();
        self
    }

    /// Billing kind of an account; the rule is documented on [`Billing`].
    pub fn billing(&self, a: &AccountRef) -> Billing {
        if a.source == AccountSource::Unknown {
            return Billing::Unknown;
        }
        if a.tool == "codex" && !self.plans.contains(&(a.tool.clone(), a.id.clone())) {
            return Billing::Unknown;
        }
        Billing::Subscription
    }

    pub fn email(&self, a: &AccountRef) -> Option<&str> {
        self.labels.get(&a.id)?.email.as_deref()
    }

    pub fn label(&self, a: &AccountRef) -> (String, LabelSource) {
        if a.source == AccountSource::Unknown {
            return ("unknown".into(), LabelSource::Unknown);
        }
        if let Some(l) = self.aliases.get(&a.id) {
            return (l.clone(), LabelSource::Alias);
        }
        let info = self.labels.get(&a.id);
        let email = info.and_then(|i| i.email.as_deref());
        if let Some(e) = email {
            let hit = self
                .aliases
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(e))
                .map(|(_, v)| v);
            if let Some(l) = hit {
                return (l.clone(), LabelSource::Alias);
            }
            // The org name belongs to the profile's current org: use it only for that org.
            let org = info
                .filter(|i| a.org_id.is_none() || i.org_uuid == a.org_id)
                .and_then(|i| i.org_name.as_deref())
                .filter(|_| a.tool == "claude");
            let text = match org {
                Some(o) => format!("{e} ({o})"),
                None => e.to_string(),
            };
            return (text, LabelSource::Email);
        }
        (a.id.chars().take(8).collect(), LabelSource::Id)
    }

    /// `--account` match: label or email (case-insensitive) or id prefix.
    pub(crate) fn matches(&self, a: &AccountRef, wanted: &str) -> bool {
        let w = wanted.to_lowercase();
        self.label(a).0.to_lowercase() == w
            || self.email(a).is_some_and(|e| e.to_lowercase() == w)
            || (!w.is_empty() && a.id.to_lowercase().starts_with(&w))
    }
}

// ---------------------------------------------------------------- output types

#[derive(Debug, Clone, Serialize)]
pub struct AccountInfo {
    pub tool: String,
    pub id: String,
    pub org_id: Option<String>,
    pub label: String,
    pub source: AccountSource,
    pub billing: Billing,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Totals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Codex reasoning tokens: a part of `output_tokens`, display only.
    pub reasoning_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_tokens: u64,
    /// Sum of the priced records; `None` when none of them has a price.
    pub cost_usd: Option<f64>,
    /// Some records were not priced, so `cost_usd` is a lower bound.
    pub partial_cost: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub bucket: String,
    pub account: AccountInfo,
    pub cost_kind: CostKind,
    /// Short project name; only with `--instances`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Sorted unique models.
    pub models: Vec<String>,
    /// The same models, most expensive first (display only).
    #[serde(skip)]
    pub models_ranked: Vec<String>,
    #[serde(flatten)]
    pub totals: Totals,
    /// Per-model numbers; filled only with `Options::breakdown`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub breakdown: Vec<ModelRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRow {
    pub model: String,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize)]
pub struct Subtotal {
    pub bucket: String,
    /// Only with `--instances`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    pub rows: Vec<Row>,
    /// One per bucket that has several accounts.
    pub subtotals: Vec<Subtotal>,
    pub totals: Totals,
    pub unpriced_models: Vec<String>,
    /// Local models counted at $0 (not unpriced).
    pub local_models: Vec<String>,
    /// Source of every priced model in the report (JSON `price_sources`).
    pub price_sources: BTreeMap<String, PriceSource>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub session_id: Option<String>,
    pub last_activity: DateTime<Utc>,
    pub account: AccountInfo,
    pub cost_kind: CostKind,
    /// Last path segment of the working directory.
    pub project: Option<String>,
    pub models: Vec<String>,
    /// The same models, most expensive first (display only).
    #[serde(skip)]
    pub models_ranked: Vec<String>,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionReport {
    pub rows: Vec<SessionRow>,
    pub totals: Totals,
    pub unpriced_models: Vec<String>,
    /// Local models counted at $0 (not unpriced).
    pub local_models: Vec<String>,
    /// Source of every priced model in the report (JSON `price_sources`).
    pub price_sources: BTreeMap<String, PriceSource>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountRow {
    pub account: AccountInfo,
    pub cost_kind: CostKind,
    pub label_source: LabelSource,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountsReport {
    pub rows: Vec<AccountRow>,
    pub totals: Totals,
    pub unpriced_models: Vec<String>,
    /// Local models counted at $0 (not unpriced).
    pub local_models: Vec<String>,
    /// Source of every priced model in the report (JSON `price_sources`).
    pub price_sources: BTreeMap<String, PriceSource>,
}

/// One limits snapshot with a resolved account label.
#[derive(Debug, Clone, Serialize)]
pub struct LimitRow {
    pub account: AccountInfo,
    pub plan_type: Option<String>,
    pub primary: Option<LimitWindow>,
    pub secondary: Option<LimitWindow>,
    pub observed_at: DateTime<Utc>,
}

/// Snapshots that pass the tool and account filters (dates do not apply).
pub fn limits_report(snaps: &[LimitSnapshot], labeler: &Labeler, o: &Options) -> Vec<LimitRow> {
    snaps
        .iter()
        .filter(|s| o.tool.is_none_or(|t| s.tool == tool_name(t)))
        .filter(|s| {
            o.accounts.is_empty() || o.accounts.iter().any(|w| labeler.matches(&s.account, w))
        })
        .map(|s| LimitRow {
            account: info(&s.account, labeler),
            plan_type: s.plan_type.clone(),
            primary: s.primary.clone(),
            secondary: s.secondary.clone(),
            observed_at: s.observed_at,
        })
        .collect()
}

// ---------------------------------------------------------------- accumulation

#[derive(Default)]
pub(crate) struct Acc {
    input: u64,
    output: u64,
    reasoning: u64,
    cache_write: u64,
    cache_read: u64,
    cost: f64,
    priced: u64,
    unpriced: u64,
    pub(crate) models: BTreeSet<String>,
    model_cost: BTreeMap<String, f64>,
}

impl Acc {
    pub(crate) fn add(&mut self, r: &UsageRecord, prices: &PriceTable) {
        self.input = self.input.saturating_add(r.input);
        self.output = self.output.saturating_add(r.output);
        self.reasoning = self.reasoning.saturating_add(r.reasoning);
        self.cache_write = self
            .cache_write
            .saturating_add(r.cache_write_5m.saturating_add(r.cache_write_1h));
        self.cache_read = self.cache_read.saturating_add(r.cache_read);
        match prices.cost(r) {
            Some(c) => {
                self.cost += c;
                self.priced += 1;
                *self.model_cost.entry(r.model.clone()).or_insert(0.0) += c;
            }
            None => self.unpriced += 1,
        }
        self.models.insert(r.model.clone());
    }

    /// Models by cost descending (unpriced last), then by name.
    pub(crate) fn ranked(&self) -> Vec<String> {
        let mut v: Vec<&String> = self.models.iter().collect();
        v.sort_by(|a, b| {
            let c = |m: &String| self.model_cost.get(m).copied().unwrap_or(-1.0);
            c(b).total_cmp(&c(a)).then_with(|| a.cmp(b))
        });
        v.into_iter().cloned().collect()
    }

    pub(crate) fn totals(&self) -> Totals {
        Totals {
            input_tokens: self.input,
            output_tokens: self.output,
            reasoning_tokens: self.reasoning,
            cache_write_tokens: self.cache_write,
            cache_read_tokens: self.cache_read,
            total_tokens: self
                .input
                .saturating_add(self.output)
                .saturating_add(self.cache_write)
                .saturating_add(self.cache_read),
            cost_usd: (self.priced > 0).then_some(self.cost),
            partial_cost: self.priced > 0 && self.unpriced > 0,
        }
    }

    fn merge(&mut self, o: &Acc) {
        self.input = self.input.saturating_add(o.input);
        self.output = self.output.saturating_add(o.output);
        self.reasoning = self.reasoning.saturating_add(o.reasoning);
        self.cache_write = self.cache_write.saturating_add(o.cache_write);
        self.cache_read = self.cache_read.saturating_add(o.cache_read);
        self.cost += o.cost;
        self.priced += o.priced;
        self.unpriced += o.unpriced;
    }
}

/// A group shows the strongest attribution seen in it: `owner` beats `profile`.
pub(crate) fn prefer_owner(cur: &mut AccountRef, new: &AccountRef) {
    if new.source == AccountSource::Owner && cur.source != AccountSource::Owner {
        cur.source = AccountSource::Owner;
    }
}

pub(crate) type AccountKey = (String, String, Option<String>);

pub(crate) fn account_key(a: &AccountRef) -> AccountKey {
    (a.tool.clone(), a.id.clone(), a.org_id.clone())
}

pub(crate) fn info(a: &AccountRef, labeler: &Labeler) -> AccountInfo {
    AccountInfo {
        tool: a.tool.clone(),
        id: a.id.clone(),
        org_id: a.org_id.clone(),
        label: labeler.label(a).0,
        source: a.source,
        billing: labeler.billing(a),
    }
}

pub fn tool_name(t: Tool) -> &'static str {
    match t {
        Tool::Claude => "claude",
        Tool::Codex => "codex",
        Tool::Antigravity => "antigravity",
        Tool::Gemini => "gemini",
    }
}

/// Records that pass the tool, date and account filters.
pub(crate) fn filter<'a>(
    records: &'a [UsageRecord],
    labeler: &Labeler,
    o: &Options,
) -> Vec<&'a UsageRecord> {
    records
        .iter()
        .filter(|r| o.tool.is_none_or(|t| r.tool == tool_name(t)))
        .filter(|r| {
            let d = o.zone.date(r.ts);
            o.since.is_none_or(|s| d >= s) && o.until.is_none_or(|u| d <= u)
        })
        .filter(|r| {
            o.project.as_ref().is_none_or(|p| {
                r.project
                    .as_ref()
                    .is_some_and(|x| x.to_lowercase().contains(&p.to_lowercase()))
            })
        })
        .filter(|r| {
            o.accounts.is_empty() || o.accounts.iter().any(|w| labeler.matches(&r.account, w))
        })
        .collect()
}

fn grand(accs: impl Iterator<Item = impl std::borrow::Borrow<Acc>>) -> Totals {
    let mut g = Acc::default();
    for a in accs {
        g.merge(a.borrow());
    }
    g.totals()
}

/// Cost descending (no cost last), then label for a stable order.
pub(crate) fn cost_desc(a: &Totals, b: &Totals) -> std::cmp::Ordering {
    let ca = a.cost_usd.unwrap_or(-1.0);
    let cb = b.cost_usd.unwrap_or(-1.0);
    cb.total_cmp(&ca)
}

// ---------------------------------------------------------------- reports

pub(crate) const UNKNOWN_PROJECT: &str = "(unknown)";

/// Short display names for project paths: the last segment, or as many trailing segments as it
/// takes to tell two paths apart.
pub(crate) fn short_project_names(paths: &BTreeSet<String>) -> HashMap<String, String> {
    let segs: Vec<(&String, Vec<&str>)> = paths
        .iter()
        .map(|p| (p, p.split(['/', '\\']).filter(|s| !s.is_empty()).collect()))
        .collect();
    let tail = |v: &[&str], n: usize| v[v.len().saturating_sub(n)..].join("/");
    segs.iter()
        .map(|(path, v)| {
            let mut n = 1;
            while n < v.len()
                && segs
                    .iter()
                    .any(|(o, ov)| o != path && tail(ov, n) == tail(v, n))
            {
                n += 1;
            }
            let name = if v.is_empty() {
                (*path).clone()
            } else {
                tail(v, n)
            };
            ((*path).clone(), name)
        })
        .collect()
}

pub fn usage_report(
    records: &[UsageRecord],
    labeler: &Labeler,
    prices: &PriceTable,
    bucket: Bucket,
    o: &Options,
) -> UsageReport {
    let mut recs = filter(records, labeler, o);
    if let Some(n) = o.last {
        let keys: BTreeSet<String> = recs.iter().map(|r| bucket.key(o.zone.date(r.ts))).collect();
        let keep: HashSet<String> = keys.into_iter().rev().take(n).collect();
        recs.retain(|r| keep.contains(&bucket.key(o.zone.date(r.ts))));
    }
    let names = short_project_names(&recs.iter().filter_map(|r| r.project.clone()).collect());
    let project_of = |r: &UsageRecord| {
        o.instances.then(|| {
            r.project
                .as_ref()
                .map_or(UNKNOWN_PROJECT.to_string(), |p| names[p].clone())
        })
    };
    // (project, bucket, account) -> (account, total, per-model)
    type Group = (AccountRef, Acc, BTreeMap<String, Acc>);
    let mut groups: BTreeMap<(Option<String>, String, AccountKey), Group> = BTreeMap::new();
    for r in &recs {
        let b = bucket.key(o.zone.date(r.ts));
        let e = groups
            .entry((project_of(r), b, account_key(&r.account)))
            .or_insert_with(|| (r.account.clone(), Acc::default(), BTreeMap::new()));
        prefer_owner(&mut e.0, &r.account);
        e.1.add(r, prices);
        if o.breakdown {
            e.2.entry(r.model.clone()).or_default().add(r, prices);
        }
    }

    let mut rows: Vec<Row> = groups
        .into_iter()
        .map(|((project, b, _), (a, acc, per_model))| Row {
            bucket: b,
            project,
            account: info(&a, labeler),
            cost_kind: labeler.billing(&a).cost_kind(),
            models: acc.models.iter().cloned().collect(),
            models_ranked: acc.ranked(),
            totals: acc.totals(),
            breakdown: per_model
                .into_iter()
                .map(|(model, m)| ModelRow {
                    model,
                    totals: m.totals(),
                })
                .collect(),
        })
        .collect();
    // Projects by total cost, most expensive first; the unknown project last.
    let mut project_cost: HashMap<Option<String>, f64> = HashMap::new();
    for r in &rows {
        *project_cost.entry(r.project.clone()).or_insert(0.0) += r.totals.cost_usd.unwrap_or(0.0);
    }
    rows.sort_by(|x, y| {
        let unknown = |p: &Option<String>| p.as_deref() == Some(UNKNOWN_PROJECT);
        let by_project = unknown(&x.project)
            .cmp(&unknown(&y.project))
            .then_with(|| project_cost[&y.project].total_cmp(&project_cost[&x.project]))
            .then_with(|| x.project.cmp(&y.project));
        let by_bucket = x.bucket.cmp(&y.bucket);
        let by_bucket = if o.order == Order::Desc {
            by_bucket.reverse()
        } else {
            by_bucket
        };
        by_project
            .then(by_bucket)
            .then_with(|| x.account.tool.cmp(&y.account.tool))
            .then_with(|| cost_desc(&x.totals, &y.totals))
            .then_with(|| x.account.label.cmp(&y.account.label))
            .then_with(|| x.models.cmp(&y.models))
    });
    for r in &mut rows {
        r.breakdown
            .sort_by(|x, y| cost_desc(&x.totals, &y.totals).then_with(|| x.model.cmp(&y.model)));
    }

    // Subtotals for buckets with several accounts, in row order.
    let mut subtotals = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let j = rows[i..]
            .iter()
            .position(|r| r.bucket != rows[i].bucket || r.project != rows[i].project)
            .map_or(rows.len(), |p| i + p);
        let accounts: BTreeSet<_> = rows[i..j]
            .iter()
            .map(|r| (&r.account.tool, &r.account.id, &r.account.org_id))
            .collect();
        if accounts.len() > 1 {
            let mut acc = Acc::default();
            for r in &rows[i..j] {
                acc.merge(&acc_of(&r.totals));
            }
            subtotals.push(Subtotal {
                bucket: rows[i].bucket.clone(),
                project: rows[i].project.clone(),
                totals: acc.totals(),
            });
        }
        i = j;
    }

    let totals = sum_totals(rows.iter().map(|r| &r.totals));
    UsageReport {
        rows,
        subtotals,
        totals,
        unpriced_models: prices.unpriced(recs.iter().copied()),
        local_models: prices.local_models(recs.iter().copied()),
        price_sources: prices.price_sources(recs.iter().copied()),
    }
}

/// Rebuild an accumulator from finished totals (cost counted as one priced record when present).
fn acc_of(t: &Totals) -> Acc {
    Acc {
        input: t.input_tokens,
        output: t.output_tokens,
        reasoning: t.reasoning_tokens,
        cache_write: t.cache_write_tokens,
        cache_read: t.cache_read_tokens,
        cost: t.cost_usd.unwrap_or(0.0),
        priced: u64::from(t.cost_usd.is_some()),
        unpriced: u64::from(t.partial_cost || t.cost_usd.is_none()),
        models: BTreeSet::new(),
        model_cost: BTreeMap::new(),
    }
}

pub fn sum_totals<'a>(ts: impl Iterator<Item = &'a Totals>) -> Totals {
    grand(ts.map(acc_of))
}

fn last_segment(cwd: &str) -> Option<String> {
    cwd.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .map(String::from)
}

pub fn session_report(
    records: &[UsageRecord],
    labeler: &Labeler,
    prices: &PriceTable,
    o: &Options,
) -> SessionReport {
    struct S {
        account: AccountRef,
        last: DateTime<Utc>,
        project: Option<String>,
        acc: Acc,
    }
    let recs = filter(records, labeler, o);
    let mut groups: BTreeMap<(String, Option<String>, AccountKey), S> = BTreeMap::new();
    for r in &recs {
        let s = groups
            .entry((
                r.tool.clone(),
                r.session_id.clone(),
                account_key(&r.account),
            ))
            .or_insert_with(|| S {
                account: r.account.clone(),
                last: r.ts,
                project: None,
                acc: Acc::default(),
            });
        prefer_owner(&mut s.account, &r.account);
        // Project of the latest record that knows one.
        if r.project.is_some() && (s.project.is_none() || r.ts >= s.last) {
            s.project = r.project.clone();
        }
        s.last = s.last.max(r.ts);
        s.acc.add(r, prices);
    }
    let mut rows: Vec<SessionRow> = groups
        .into_iter()
        .map(|((_, sid, _), s)| SessionRow {
            session_id: sid,
            last_activity: s.last,
            account: info(&s.account, labeler),
            cost_kind: labeler.billing(&s.account).cost_kind(),
            project: s.project.as_deref().and_then(last_segment),
            models: s.acc.models.iter().cloned().collect(),
            models_ranked: s.acc.ranked(),
            totals: s.acc.totals(),
        })
        .collect();
    rows.sort_by(|x, y| {
        let t = x.last_activity.cmp(&y.last_activity);
        let t = if o.order == Order::Desc {
            t.reverse()
        } else {
            t
        };
        t.then_with(|| cost_desc(&x.totals, &y.totals))
            .then_with(|| x.session_id.cmp(&y.session_id))
    });
    let totals = sum_totals(rows.iter().map(|r| &r.totals));
    SessionReport {
        rows,
        totals,
        unpriced_models: prices.unpriced(recs.iter().copied()),
        local_models: prices.local_models(recs.iter().copied()),
        price_sources: prices.price_sources(recs.iter().copied()),
    }
}

pub fn accounts_report(
    records: &[UsageRecord],
    labeler: &Labeler,
    prices: &PriceTable,
    o: &Options,
) -> AccountsReport {
    let recs = filter(records, labeler, o);
    let mut groups: BTreeMap<AccountKey, (AccountRef, DateTime<Utc>, DateTime<Utc>, Acc)> =
        BTreeMap::new();
    for r in &recs {
        let e = groups
            .entry(account_key(&r.account))
            .or_insert_with(|| (r.account.clone(), r.ts, r.ts, Acc::default()));
        prefer_owner(&mut e.0, &r.account);
        e.1 = e.1.min(r.ts);
        e.2 = e.2.max(r.ts);
        e.3.add(r, prices);
    }
    let mut rows: Vec<AccountRow> = groups
        .into_values()
        .map(|(a, first, last, acc)| AccountRow {
            account: info(&a, labeler),
            cost_kind: labeler.billing(&a).cost_kind(),
            label_source: labeler.label(&a).1,
            first_seen: first,
            last_seen: last,
            totals: acc.totals(),
        })
        .collect();
    rows.sort_by(|x, y| {
        x.account
            .tool
            .cmp(&y.account.tool)
            .then_with(|| cost_desc(&x.totals, &y.totals))
            .then_with(|| x.account.label.cmp(&y.account.label))
    });
    let totals = sum_totals(rows.iter().map(|r| &r.totals));
    AccountsReport {
        rows,
        totals,
        unpriced_models: prices.unpriced(recs.iter().copied()),
        local_models: prices.local_models(recs.iter().copied()),
        price_sources: prices.price_sources(recs.iter().copied()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::Price;
    use chrono::TimeZone;

    const A1: &str = "00000000-0000-4000-8000-000000000001";
    const A2: &str = "00000000-0000-4000-8000-000000000002";

    fn acct(tool: &str, id: &str, src: AccountSource) -> AccountRef {
        AccountRef {
            tool: tool.into(),
            id: id.into(),
            org_id: None,
            source: src,
        }
    }

    fn rec(ts: &str, a: &AccountRef, model: &str, input: u64, output: u64) -> UsageRecord {
        UsageRecord {
            ts: ts.parse().unwrap(),
            tool: a.tool.clone(),
            account: a.clone(),
            model: model.into(),
            session_id: Some("s1".into()),
            project: Some("/Users/user/proj".into()),
            input,
            output,
            reasoning: 0,
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_read: 0,
            dedup_key: None,
        }
    }

    fn prices() -> PriceTable {
        PriceTable::builtin().with_overrides(HashMap::from([(
            "m".to_string(),
            Price {
                input: 1.0,
                output: 2.0,
                cache_write_5m: 0.0,
                cache_write_1h: 0.0,
                cache_read: 0.0,
            },
        )]))
    }

    fn label(email: Option<&str>, org: Option<&str>) -> AccountLabel {
        AccountLabel {
            email: email.map(String::from),
            display_name: None,
            org_uuid: None,
            org_name: org.map(String::from),
        }
    }

    fn empty() -> (HashMap<String, AccountLabel>, HashMap<String, String>) {
        (HashMap::new(), HashMap::new())
    }

    #[test]
    fn huge_token_counts_saturate() {
        let a = acct("claude", A1, AccountSource::Owner);
        let mut r = rec("2026-11-01T00:00:00Z", &a, "m", u64::MAX, u64::MAX);
        r.cache_write_5m = u64::MAX;
        r.cache_write_1h = u64::MAX;
        r.cache_read = u64::MAX;
        let mut acc = Acc::default();
        acc.add(&r, &prices());
        acc.add(&r, &prices());
        let mut g = Acc::default();
        g.merge(&acc);
        g.merge(&acc);
        let t = g.totals();
        assert_eq!(t.total_tokens, u64::MAX);
        assert_eq!(t.cache_write_tokens, u64::MAX);
    }

    #[test]
    fn label_priority() {
        let a = acct("claude", A1, AccountSource::Owner);
        let mut labels = HashMap::from([(
            A1.to_string(),
            label(Some("user@example.com"), Some("Acme")),
        )]);
        let mut aliases = HashMap::new();
        let l = |labels: &_, aliases: &_| Labeler::new(labels, aliases).label(&a);
        assert_eq!(
            l(&labels, &aliases),
            ("user@example.com (Acme)".into(), LabelSource::Email)
        );
        aliases.insert("USER@example.com".to_string(), "by-email".to_string());
        assert_eq!(
            l(&labels, &aliases),
            ("by-email".into(), LabelSource::Alias)
        );
        aliases.insert(A1.to_string(), "by-id".to_string());
        assert_eq!(l(&labels, &aliases), ("by-id".into(), LabelSource::Alias));
        labels.clear();
        aliases.clear();
        assert_eq!(l(&labels, &aliases), ("00000000".into(), LabelSource::Id));
        let u = acct("claude", "unknown", AccountSource::Unknown);
        assert_eq!(
            Labeler::new(&labels, &aliases).label(&u),
            ("unknown".into(), LabelSource::Unknown)
        );
    }

    #[test]
    fn org_name_only_for_matching_org() {
        let mut a = acct("claude", A1, AccountSource::Owner);
        let mut lab = label(Some("user@example.com"), Some("Acme"));
        lab.org_uuid = Some("org1".into());
        let labels = HashMap::from([(A1.to_string(), lab)]);
        let aliases = HashMap::new();
        a.org_id = Some("org2".into());
        assert_eq!(
            Labeler::new(&labels, &aliases).label(&a).0,
            "user@example.com"
        );
        a.org_id = Some("org1".into());
        assert_eq!(
            Labeler::new(&labels, &aliases).label(&a).0,
            "user@example.com (Acme)"
        );
    }

    #[test]
    fn timezone_edge_near_midnight() {
        let (labels, aliases) = empty();
        let lb = Labeler::new(&labels, &aliases);
        let a = acct("codex", A1, AccountSource::Owner);
        // 2026-11-07 23:30 UTC is 2026-11-08 06:30 at +07:00.
        let recs = vec![rec("2026-11-07T23:30:00Z", &a, "m", 10, 10)];
        let utc = Options {
            zone: Zone::Utc,
            ..Options::default()
        };
        let plus7 = Options {
            zone: Zone::Fixed(FixedOffset::east_opt(7 * 3600).unwrap()),
            ..Options::default()
        };
        let r = usage_report(&recs, &lb, &prices(), Bucket::Day, &utc);
        assert_eq!(r.rows[0].bucket, "2026-11-07");
        let r = usage_report(&recs, &lb, &prices(), Bucket::Day, &plus7);
        assert_eq!(r.rows[0].bucket, "2026-11-08");
        // --until is inclusive in the same zone: 11-07 excludes it at +07:00.
        let o = Options {
            until: NaiveDate::from_ymd_opt(2026, 11, 7),
            ..plus7.clone()
        };
        assert!(
            usage_report(&recs, &lb, &prices(), Bucket::Day, &o)
                .rows
                .is_empty()
        );
        let o = Options {
            since: NaiveDate::from_ymd_opt(2026, 11, 8),
            ..plus7
        };
        assert_eq!(
            usage_report(&recs, &lb, &prices(), Bucket::Day, &o)
                .rows
                .len(),
            1
        );
    }

    #[test]
    fn week_and_month_buckets() {
        let (labels, aliases) = empty();
        let lb = Labeler::new(&labels, &aliases);
        let a = acct("codex", A1, AccountSource::Owner);
        // Sunday 2026-11-08 and Monday 2026-11-09; Sunday belongs to week of 2026-11-02.
        let recs = vec![
            rec("2026-11-08T12:00:00Z", &a, "m", 1, 1),
            rec("2026-11-09T12:00:00Z", &a, "m", 1, 1),
            rec("2026-12-01T00:00:00Z", &a, "m", 1, 1),
        ];
        let o = Options::default();
        let w = usage_report(&recs, &lb, &prices(), Bucket::Week, &o);
        let keys: Vec<_> = w.rows.iter().map(|r| r.bucket.as_str()).collect();
        assert_eq!(keys, ["2026-11-02", "2026-11-09", "2026-11-30"]);
        let m = usage_report(&recs, &lb, &prices(), Bucket::Month, &o);
        let keys: Vec<_> = m.rows.iter().map(|r| r.bucket.as_str()).collect();
        assert_eq!(keys, ["2026-11", "2026-12"]);
        let d = Options {
            order: Order::Desc,
            ..Options::default()
        };
        let m = usage_report(&recs, &lb, &prices(), Bucket::Month, &d);
        assert_eq!(m.rows[0].bucket, "2026-12");
    }

    #[test]
    fn rows_subtotals_models_and_sort() {
        let (labels, aliases) = empty();
        let lb = Labeler::new(&labels, &aliases);
        let a1 = acct("codex", A1, AccountSource::Owner);
        let a2 = acct("codex", A2, AccountSource::Owner);
        let recs = vec![
            rec("2026-11-07T01:00:00Z", &a1, "m", 1_000_000, 0), // $1
            rec(
                "2026-11-07T02:00:00Z",
                &a1,
                "claude-sonnet-5-5",
                1_000_000,
                0,
            ), // $2
            rec("2026-11-07T03:00:00Z", &a2, "m", 4_000_000, 0), // $4
            rec("2026-11-08T03:00:00Z", &a2, "m", 1, 0),
        ];
        let r = usage_report(&recs, &lb, &prices(), Bucket::Day, &Options::default());
        assert_eq!(r.rows.len(), 3);
        // Within a bucket: cost descending, so a2 ($4) before a1 ($3).
        assert_eq!(r.rows[0].bucket, "2026-11-07");
        assert_eq!(r.rows[0].totals.cost_usd, Some(4.0));
        assert_eq!(r.rows[1].totals.cost_usd, Some(3.0));
        assert_eq!(r.rows[1].models, ["claude-sonnet-5-5", "m"]);
        assert_eq!(r.subtotals.len(), 1);
        assert_eq!(r.subtotals[0].bucket, "2026-11-07");
        assert_eq!(r.subtotals[0].totals.input_tokens, 6_000_000);
        assert_eq!(r.totals.input_tokens, 6_000_001);
        let by = Options {
            breakdown: true,
            ..Options::default()
        };
        let r = usage_report(&recs, &lb, &prices(), Bucket::Day, &by);
        assert_eq!(r.rows.len(), 3);
        // Breakdown lists every model of a row; the model rows add up to the row.
        assert_eq!(r.rows[1].breakdown.len(), 2);
        let sum: u64 = r.rows[1]
            .breakdown
            .iter()
            .map(|m| m.totals.input_tokens)
            .sum();
        assert_eq!(sum, r.rows[1].totals.input_tokens);
        // Without the flag nothing is listed.
        let r = usage_report(&recs, &lb, &prices(), Bucket::Day, &Options::default());
        assert!(r.rows.iter().all(|x| x.breakdown.is_empty()));
    }

    #[test]
    fn partial_and_missing_cost() {
        let (labels, aliases) = empty();
        let lb = Labeler::new(&labels, &aliases);
        let a = acct("codex", A1, AccountSource::Owner);
        let recs = vec![
            rec("2026-11-07T01:00:00Z", &a, "m", 1_000_000, 0),
            rec("2026-11-07T02:00:00Z", &a, "mystery", 5, 5),
            rec("2026-11-08T02:00:00Z", &a, "mystery", 5, 5),
        ];
        let r = usage_report(&recs, &lb, &prices(), Bucket::Day, &Options::default());
        assert_eq!(r.rows[0].totals.cost_usd, Some(1.0));
        assert!(r.rows[0].totals.partial_cost);
        assert_eq!(r.rows[1].totals.cost_usd, None);
        assert!(!r.rows[1].totals.partial_cost);
        assert_eq!(r.totals.cost_usd, Some(1.0));
        assert!(r.totals.partial_cost);
        assert_eq!(r.unpriced_models, ["mystery"]);
    }

    #[test]
    fn filters_by_tool_and_account() {
        let labels = HashMap::from([(A1.to_string(), label(Some("user@example.com"), None))]);
        let aliases = HashMap::from([(A2.to_string(), "Home".to_string())]);
        let lb = Labeler::new(&labels, &aliases);
        let c = acct("claude", A1, AccountSource::Owner);
        let x = acct("codex", A2, AccountSource::Owner);
        let recs = vec![
            rec("2026-11-07T01:00:00Z", &c, "m", 1, 1),
            rec("2026-11-07T01:00:00Z", &x, "m", 1, 1),
        ];
        let n = |o: &Options| {
            usage_report(&recs, &lb, &prices(), Bucket::Day, o)
                .rows
                .len()
        };
        let acc = |w: &str| Options {
            accounts: vec![w.into()],
            ..Options::default()
        };
        assert_eq!(n(&Options::default()), 2);
        assert_eq!(
            n(&Options {
                tool: Some(Tool::Codex),
                ..Options::default()
            }),
            1
        );
        assert_eq!(n(&acc("home")), 1); // label, case-insensitive
        assert_eq!(n(&acc("USER@example.com")), 1); // email
        assert_eq!(n(&acc("00000000-0000-4000-8000-00000000000")), 2); // id prefix
        assert_eq!(n(&acc("0000000000000")), 0);
        assert_eq!(
            n(&Options {
                accounts: vec!["home".into(), "user@example.com".into()],
                ..Options::default()
            }),
            2
        );
    }

    #[test]
    fn session_report_rows() {
        let (labels, aliases) = empty();
        let lb = Labeler::new(&labels, &aliases);
        let a = acct("codex", A1, AccountSource::Owner);
        let mut r1 = rec("2026-11-07T01:00:00Z", &a, "m", 10, 0);
        let mut r2 = rec("2026-11-07T05:00:00Z", &a, "m", 10, 0);
        r1.project = None;
        r2.project = Some("/Users/user/work/proj/".into());
        let mut r3 = rec("2026-11-06T05:00:00Z", &a, "m", 1, 0);
        r3.session_id = Some("s0".into());
        let rep = session_report(&[r1, r2, r3], &lb, &prices(), &Options::default());
        assert_eq!(rep.rows.len(), 2);
        assert_eq!(rep.rows[0].session_id.as_deref(), Some("s0"));
        let s1 = &rep.rows[1];
        assert_eq!(s1.project.as_deref(), Some("proj"));
        assert_eq!(s1.totals.input_tokens, 20);
        assert_eq!(
            s1.last_activity,
            Utc.with_ymd_and_hms(2026, 11, 7, 5, 0, 0).unwrap()
        );
        assert_eq!(rep.totals.input_tokens, 21);
    }

    #[test]
    fn accounts_report_rows() {
        let labels = HashMap::from([(A1.to_string(), label(Some("user@example.com"), None))]);
        let aliases = HashMap::new();
        let lb = Labeler::new(&labels, &aliases);
        let c = acct("claude", A1, AccountSource::Owner);
        let u = acct("claude", "unknown", AccountSource::Unknown);
        let recs = vec![
            rec("2026-11-07T01:00:00Z", &c, "m", 1_000_000, 0),
            rec("2026-11-09T01:00:00Z", &c, "m", 0, 1),
            rec("2026-11-08T01:00:00Z", &u, "m", 1, 0),
        ];
        let rep = accounts_report(&recs, &lb, &prices(), &Options::default());
        assert_eq!(rep.rows.len(), 2);
        let first = &rep.rows[0];
        assert_eq!(first.account.label, "user@example.com");
        assert_eq!(first.label_source, LabelSource::Email);
        assert_eq!(first.first_seen.to_rfc3339(), "2026-11-07T01:00:00+00:00");
        assert_eq!(first.last_seen.to_rfc3339(), "2026-11-09T01:00:00+00:00");
        assert_eq!(rep.rows[1].account.label, "unknown");
    }

    #[test]
    fn billing_rule_never_infers_api() {
        let (labels, aliases) = empty();
        let snap = |tool: &str, id: &str, plan: Option<&str>| LimitSnapshot {
            tool: tool.into(),
            account: acct(tool, id, AccountSource::Owner),
            primary: None,
            secondary: None,
            plan_type: plan.map(String::from),
            observed_at: "2026-11-01T00:00:00Z".parse().unwrap(),
        };
        let lb = Labeler::new(&labels, &aliases)
            .with_plans(&[snap("codex", A1, Some("plus")), snap("codex", A2, None)]);
        let b = |t: &str, id: &str, src| lb.billing(&acct(t, id, src));
        assert_eq!(b("claude", A1, AccountSource::Owner), Billing::Subscription);
        assert_eq!(
            b("claude", A1, AccountSource::Profile),
            Billing::Subscription
        );
        assert_eq!(
            b("claude", "unknown", AccountSource::Unknown),
            Billing::Unknown
        );
        assert_eq!(b("codex", A1, AccountSource::Owner), Billing::Subscription);
        assert_eq!(b("codex", A2, AccountSource::Owner), Billing::Unknown);
        assert_eq!(b("codex", A1, AccountSource::Unknown), Billing::Unknown);
        assert_eq!(Billing::Subscription.cost_kind(), CostKind::ApiEquivalent);
        assert_eq!(Billing::Unknown.cost_kind(), CostKind::ApiEquivalent);
        assert_eq!(Billing::Api.cost_kind(), CostKind::Billed);
    }

    #[test]
    fn owner_attribution_wins_in_a_merged_group() {
        let profile = acct("claude", A1, AccountSource::Profile);
        let owner = acct("claude", A1, AccountSource::Owner);
        let recs = vec![
            rec("2026-11-01T00:00:00Z", &profile, "m", 1, 1),
            rec("2026-11-01T01:00:00Z", &owner, "m", 1, 1),
        ];
        let (labels, aliases) = empty();
        let lb = Labeler::new(&labels, &aliases);
        let r = accounts_report(&recs, &lb, &prices(), &Options::default());
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0].account.source, AccountSource::Owner);
    }

    fn prec(ts: &str, a: &AccountRef, project: Option<&str>, tokens: u64) -> UsageRecord {
        let mut r = rec(ts, a, "claude-test-1", tokens, 0);
        r.project = project.map(String::from);
        r
    }

    #[test]
    fn project_names_use_the_last_segment_until_ambiguous() {
        let set: BTreeSet<String> = [
            "/Users/user/work/api",
            "/Users/user/play/api",
            "/Users/user/solo",
            "C:\\Users\\user\\win",
        ]
        .map(String::from)
        .into_iter()
        .collect();
        let n = short_project_names(&set);
        assert_eq!(n["/Users/user/work/api"], "work/api");
        assert_eq!(n["/Users/user/play/api"], "play/api");
        assert_eq!(n["/Users/user/solo"], "solo");
        assert_eq!(n["C:\\Users\\user\\win"], "win");
    }

    #[test]
    fn instances_group_by_project_first_with_unknown_last() {
        let a = acct("claude", A1, AccountSource::Profile);
        let recs = vec![
            prec("2026-11-01T10:00:00Z", &a, Some("/Users/user/zeta"), 1),
            prec("2026-11-02T10:00:00Z", &a, Some("/Users/user/alpha"), 2),
            prec("2026-11-01T11:00:00Z", &a, None, 4),
            prec("2026-11-02T11:00:00Z", &a, Some("/Users/user/zeta"), 8),
        ];
        let (labels, aliases) = empty();
        let l = Labeler::new(&labels, &aliases);
        let o = Options {
            instances: true,
            ..Options::default()
        };
        let r = usage_report(&recs, &l, &prices(), Bucket::Day, &o);
        let seen: Vec<(String, String, u64)> = r
            .rows
            .iter()
            .map(|x| {
                (
                    x.project.clone().unwrap(),
                    x.bucket.clone(),
                    x.totals.total_tokens,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("alpha".into(), "2026-11-02".into(), 2),
                ("zeta".into(), "2026-11-01".into(), 1),
                ("zeta".into(), "2026-11-02".into(), 8),
                ("(unknown)".into(), "2026-11-01".into(), 4),
            ]
        );
        // without --instances rows carry no project and merge
        let plain = usage_report(&recs, &l, &prices(), Bucket::Day, &Options::default());
        assert!(plain.rows.iter().all(|x| x.project.is_none()));
        assert_eq!(plain.rows.len(), 2);
    }

    #[test]
    fn project_filter_is_a_case_insensitive_substring() {
        let a = acct("claude", A1, AccountSource::Profile);
        let recs = vec![
            prec("2026-11-01T10:00:00Z", &a, Some("/Users/user/Alpha-app"), 1),
            prec("2026-11-01T10:00:00Z", &a, Some("/Users/user/beta"), 2),
            prec("2026-11-01T10:00:00Z", &a, None, 4),
        ];
        let (labels, aliases) = empty();
        let l = Labeler::new(&labels, &aliases);
        let o = Options {
            project: Some("alpha".into()),
            ..Options::default()
        };
        let r = usage_report(&recs, &l, &prices(), Bucket::Day, &o);
        assert_eq!(r.totals.total_tokens, 1);
    }
}
