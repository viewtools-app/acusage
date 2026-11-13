//! Billing blocks per account (the `blocks` report). Pure: records in, rows out.
//!
//! A block starts at the first record's timestamp floored to the UTC hour and lasts
//! `session_hours`. A record at or after the block end starts a new block. (A gap of at least
//! `session_hours` since the previous record always lands at or after the end, so it needs no
//! rule of its own.) Gap rows are added between two blocks of one account when the idle time
//! between their last and first record is at least `session_hours`.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::cli::{Order, TokenLimit};
use crate::model::{AccountRef, AccountSource, CostKind, UsageRecord};
use crate::pricing::{PriceSource, PriceTable};
use crate::report::{
    Acc, AccountInfo, AccountKey, Labeler, Options, Totals, account_key, filter, info,
    prefer_owner, sum_totals,
};

#[derive(Debug, Clone)]
pub struct BlockOpts {
    /// Only the active block.
    pub active: bool,
    /// Only blocks that started within the last 3 days, plus the active one.
    pub recent: bool,
    pub token_limit: Option<TokenLimit>,
    pub session_hours: u32,
    pub now: DateTime<Utc>,
}

impl BlockOpts {
    pub fn new(now: DateTime<Utc>) -> Self {
        BlockOpts {
            active: false,
            recent: false,
            token_limit: None,
            session_hours: 5,
            now,
        }
    }
}

/// Where an active block is heading at its current pace.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Projection {
    pub burn_rate_tokens_per_min: f64,
    pub projected_tokens: u64,
    pub projected_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockRow {
    /// Block start (hour floor), or for a gap row the last activity before it.
    pub start: DateTime<Utc>,
    /// Block end (start + session length), or for a gap row the next activity.
    pub end: DateTime<Utc>,
    /// Last record of the block; `None` for a gap row.
    pub actual_end: Option<DateTime<Utc>>,
    pub active: bool,
    pub gap: bool,
    pub account: AccountInfo,
    pub cost_kind: CostKind,
    pub models: Vec<String>,
    #[serde(skip)]
    pub models_ranked: Vec<String>,
    #[serde(flatten)]
    pub totals: Totals,
    /// The limit this block is measured against (`--token-limit`); never on a gap row.
    pub token_limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection: Option<Projection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlocksReport {
    pub rows: Vec<BlockRow>,
    pub totals: Totals,
    pub unpriced_models: Vec<String>,
    /// Local models counted at $0 (not unpriced).
    pub local_models: Vec<String>,
    /// Source of every priced model in the report (JSON `price_sources`).
    pub price_sources: BTreeMap<String, PriceSource>,
    /// Session length the blocks were cut with (display only).
    #[serde(skip)]
    pub session_hours: u32,
    /// The limit is `--token-limit max` (display only).
    #[serde(skip)]
    pub limit_is_max: bool,
}

struct Block<'a> {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    recs: Vec<&'a UsageRecord>,
}

impl Block<'_> {
    fn first(&self) -> DateTime<Utc> {
        self.recs[0].ts
    }
    fn last(&self) -> DateTime<Utc> {
        self.recs[self.recs.len() - 1].ts
    }
}

fn floor_hour(ts: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(ts.timestamp().div_euclid(3600) * 3600, 0).expect("in range")
}

/// Split time-sorted records of one account into blocks.
fn split<'a>(recs: &[&'a UsageRecord], hours: u32) -> Vec<Block<'a>> {
    let len = Duration::hours(i64::from(hours));
    let mut out: Vec<Block<'a>> = vec![];
    for &r in recs {
        match out.last_mut() {
            Some(b) if r.ts < b.end => b.recs.push(r),
            _ => {
                let start = floor_hour(r.ts);
                out.push(Block {
                    start,
                    end: start + len,
                    recs: vec![r],
                });
            }
        }
    }
    out
}

/// Open (now before the end) and used within the last session length.
fn is_active(b: &Block, now: DateTime<Utc>, hours: u32) -> bool {
    b.first() <= now && now < b.end && now - b.last() < Duration::hours(i64::from(hours))
}

/// Burn rate over the span from the first to the last record, carried to the block end.
fn projection(b: &Block, total: &Totals, now: DateTime<Utc>) -> Option<Projection> {
    let span = (b.last() - b.first()).num_seconds() as f64 / 60.0;
    if span <= 0.0 {
        return None;
    }
    let left = ((b.end - now).num_seconds().max(0)) as f64 / 60.0;
    let rate = total.total_tokens as f64 / span;
    Some(Projection {
        burn_rate_tokens_per_min: rate,
        projected_tokens: total
            .total_tokens
            .saturating_add((rate * left).round() as u64),
        projected_cost_usd: total.cost_usd.map(|c| c + c / span * left),
    })
}

pub fn blocks_report(
    records: &[UsageRecord],
    labeler: &Labeler,
    prices: &PriceTable,
    o: &Options,
    b: &BlockOpts,
) -> BlocksReport {
    let recs = filter(records, labeler, o);
    let mut by_account: BTreeMap<AccountKey, (AccountRef, Vec<&UsageRecord>)> = BTreeMap::new();
    for r in recs {
        let e = by_account
            .entry(account_key(&r.account))
            .or_insert_with(|| (r.account.clone(), vec![]));
        prefer_owner(&mut e.0, &r.account);
        e.1.push(r);
    }
    // Sections by account: gaps belong to one account, so interleaving accounts by time would
    // make them ambiguous.
    let mut groups: Vec<_> = by_account.into_values().collect();
    groups.sort_by_cached_key(|(a, _)| (a.tool.clone(), labeler.label(a).0, a.id.clone()));

    let cutoff = b.now - Duration::days(3);
    let mut rows = vec![];
    let mut kept: Vec<&UsageRecord> = vec![];
    // Codex logs often carry no account record: blocks of mixed, unknown accounts mean little, so
    // they are shown only when asked for with `--account unknown`.
    let wants_unknown = o.accounts.iter().any(|a| a.eq_ignore_ascii_case("unknown"));
    for (acct, mut rs) in groups {
        let unknown_codex = acct.tool == "codex" && acct.source == AccountSource::Unknown;
        if unknown_codex && !wants_unknown {
            continue;
        }
        rs.sort_by_key(|r| r.ts);
        let blocks = split(&rs, b.session_hours);
        let stats: Vec<(Totals, Vec<String>, Vec<String>)> = blocks
            .iter()
            .map(|bl| {
                let mut acc = Acc::default();
                for r in &bl.recs {
                    acc.add(r, prices);
                }
                (
                    acc.totals(),
                    acc.models.iter().cloned().collect(),
                    acc.ranked(),
                )
            })
            .collect();
        // The limit is measured before the recent/active filters, over every block seen.
        let limit = match b.token_limit {
            None => None,
            Some(TokenLimit::Tokens(n)) => Some(n),
            Some(TokenLimit::Max) => {
                // Earlier blocks only: the open block is what the limit is measured against.
                let earlier = |closed: bool| {
                    blocks
                        .iter()
                        .zip(&stats)
                        .filter(|(bl, _)| !closed || !is_active(bl, b.now, b.session_hours))
                        .map(|(_, (t, _, _))| t.total_tokens)
                        .max()
                };
                earlier(true).or_else(|| earlier(false))
            }
        };
        let mut info = info(&acct, labeler);
        if unknown_codex {
            info.label = "mixed accounts".into();
        }
        let cost_kind = labeler.billing(&acct).cost_kind();
        let mut prev_last: Option<DateTime<Utc>> = None;
        let gap_len = Duration::hours(i64::from(b.session_hours));
        let mut acct_rows = vec![];
        for (bl, (totals, models, ranked)) in blocks.iter().zip(stats) {
            let active = is_active(bl, b.now, b.session_hours);
            let keep = (!b.active || active) && (!b.recent || active || bl.start >= cutoff);
            if !keep {
                continue;
            }
            if let Some(last) = prev_last.filter(|l| bl.first() - *l >= gap_len) {
                acct_rows.push(BlockRow {
                    start: last,
                    end: bl.first(),
                    actual_end: None,
                    active: false,
                    gap: true,
                    account: info.clone(),
                    cost_kind,
                    models: vec![],
                    models_ranked: vec![],
                    totals: Totals::default(),
                    token_limit: None,
                    projection: None,
                });
            }
            prev_last = Some(bl.last());
            kept.extend(bl.recs.iter().copied());
            acct_rows.push(BlockRow {
                start: bl.start,
                end: bl.end,
                actual_end: Some(bl.last()),
                active,
                gap: false,
                account: info.clone(),
                cost_kind,
                models,
                models_ranked: ranked,
                projection: active.then(|| projection(bl, &totals, b.now)).flatten(),
                totals,
                token_limit: limit,
            });
        }
        if o.order == Order::Desc {
            acct_rows.reverse();
        }
        rows.extend(acct_rows);
    }
    let totals = sum_totals(rows.iter().filter(|r| !r.gap).map(|r| &r.totals));
    BlocksReport {
        rows,
        totals,
        unpriced_models: prices.unpriced(kept.iter().copied()),
        local_models: prices.local_models(kept.iter().copied()),
        price_sources: prices.price_sources(kept.iter().copied()),
        session_hours: b.session_hours,
        limit_is_max: matches!(b.token_limit, Some(TokenLimit::Max)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AccountSource;
    use chrono::TimeZone;
    use std::collections::HashMap;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, h, m, 0).unwrap()
    }

    fn acct(id: &str) -> AccountRef {
        AccountRef {
            tool: "claude".into(),
            id: id.into(),
            org_id: None,
            source: AccountSource::Profile,
        }
    }

    fn rec(id: &str, ts: DateTime<Utc>, tokens: u64) -> UsageRecord {
        UsageRecord {
            ts,
            tool: "claude".into(),
            account: acct(id),
            model: "claude-test-1".into(),
            session_id: None,
            project: None,
            input: tokens,
            output: 0,
            reasoning: 0,
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_read: 0,
            dedup_key: None,
        }
    }

    fn refs(v: &[UsageRecord]) -> Vec<&UsageRecord> {
        v.iter().collect()
    }

    fn report(recs: &[UsageRecord], b: &BlockOpts) -> BlocksReport {
        let (labels, aliases) = (HashMap::new(), HashMap::new());
        let l = Labeler::new(&labels, &aliases);
        let o = Options::default();
        blocks_report(recs, &l, &PriceTable::builtin(), &o, b)
    }

    fn opts(now: DateTime<Utc>) -> BlockOpts {
        BlockOpts::new(now)
    }

    #[test]
    fn block_starts_at_the_utc_hour_and_spans_the_session() {
        let v = [rec("a", at(10, 20), 1), rec("a", at(12, 59), 1)];
        let bl = split(&refs(&v), 5);
        assert_eq!(bl.len(), 1);
        assert_eq!(bl[0].start, at(10, 0));
        assert_eq!(bl[0].end, at(15, 0));
        assert_eq!(bl[0].recs.len(), 2);
    }

    #[test]
    fn record_at_the_exact_end_starts_a_new_block() {
        let v = [
            rec("a", at(10, 20), 1),
            rec("a", at(14, 59), 1),
            rec("a", at(15, 0), 1),
        ];
        let bl = split(&refs(&v), 5);
        assert_eq!(bl.len(), 2);
        assert_eq!(bl[0].recs.len(), 2);
        assert_eq!(bl[1].start, at(15, 0));
    }

    #[test]
    fn long_idle_gap_starts_a_new_block() {
        let v = [rec("a", at(10, 0), 1), rec("a", at(16, 30), 1)];
        let bl = split(&refs(&v), 5);
        assert_eq!(bl.len(), 2);
        assert_eq!(bl[1].start, at(16, 0));
    }

    #[test]
    fn session_length_changes_the_window() {
        let v = [rec("a", at(10, 0), 1), rec("a", at(12, 30), 1)];
        assert_eq!(split(&refs(&v), 5).len(), 1);
        assert_eq!(split(&refs(&v), 2).len(), 2);
    }

    #[test]
    fn accounts_are_split_separately() {
        let v = [
            rec("a", at(10, 0), 1),
            rec("b", at(10, 30), 2),
            rec("a", at(11, 0), 3),
        ];
        let r = report(&v, &opts(at(23, 0)));
        let blocks: Vec<_> = r.rows.iter().filter(|x| !x.gap).collect();
        assert_eq!(blocks.len(), 2);
        let totals: Vec<u64> = blocks.iter().map(|x| x.totals.total_tokens).collect();
        assert!(totals.contains(&4) && totals.contains(&2));
    }

    #[test]
    fn gap_row_only_when_idle_for_the_session_length() {
        // a: block 10:00, then 15:30 (idle 5h10m since 10:20 => gap), then 20:10 (idle 4h40m => none)
        let v = [
            rec("a", at(10, 20), 1),
            rec("a", at(15, 30), 1),
            rec("a", at(20, 10), 1),
        ];
        let r = report(&v, &opts(at(23, 0)));
        let kinds: Vec<bool> = r.rows.iter().map(|x| x.gap).collect();
        assert_eq!(kinds, [false, true, false, false]);
        let gap = &r.rows[1];
        assert_eq!(gap.start, at(10, 20));
        assert_eq!(gap.end, at(15, 30));
        assert!(gap.actual_end.is_none() && gap.token_limit.is_none());
        assert_eq!(gap.totals.total_tokens, 0);
    }

    #[test]
    fn active_needs_an_open_block_and_recent_activity() {
        let v = [rec("a", at(10, 20), 1), rec("a", at(12, 0), 1)];
        let bl = split(&refs(&v), 5);
        assert!(is_active(&bl[0], at(14, 59), 5));
        // block end reached
        assert!(!is_active(&bl[0], at(15, 0), 5));
        // last activity a full session ago, even if the block is still open (short block)
        let v2 = [rec("a", at(10, 0), 1)];
        let bl2 = split(&refs(&v2), 5);
        assert!(!is_active(&bl2[0], at(15, 0), 5));
        // a block whose first record is after now has not started
        assert!(!is_active(&bl[0], at(9, 0), 5));
    }

    #[test]
    fn active_filter_keeps_only_active_blocks() {
        let v = [
            rec("a", at(1, 0), 1),
            rec("a", at(10, 0), 5),
            rec("b", at(2, 0), 9),
        ];
        let mut b = opts(at(11, 0));
        b.active = true;
        let r = report(&v, &b);
        assert_eq!(r.rows.len(), 1);
        assert!(r.rows[0].active && !r.rows[0].gap);
        assert_eq!(r.rows[0].totals.total_tokens, 5);
        assert_eq!(r.totals.total_tokens, 5);
    }

    #[test]
    fn recent_keeps_last_three_days() {
        let day = |d: u32, h: u32| Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap();
        let v = [
            rec("a", day(1, 10), 1),
            rec("a", day(5, 10), 2),
            rec("a", day(8, 10), 4),
        ];
        let mut b = opts(day(8, 11));
        b.recent = true;
        let r = report(&v, &b);
        let blocks: Vec<u64> = r
            .rows
            .iter()
            .filter(|x| !x.gap)
            .map(|x| x.totals.total_tokens)
            .collect();
        assert_eq!(blocks, [4]);
        let all = report(&v, &opts(day(8, 11)));
        assert_eq!(all.rows.iter().filter(|x| !x.gap).count(), 3);
    }

    #[test]
    fn token_limit_max_is_per_account_over_all_blocks() {
        let v = [
            rec("a", at(1, 0), 100),
            rec("a", at(10, 0), 40),
            rec("b", at(1, 0), 7),
        ];
        let mut b = opts(at(12, 0));
        b.token_limit = Some(TokenLimit::Max);
        b.recent = true;
        let r = report(&v, &b);
        let a: Vec<_> = r
            .rows
            .iter()
            .filter(|x| !x.gap && x.account.id == "a")
            .collect();
        assert_eq!(a.len(), 2);
        assert!(a.iter().all(|x| x.token_limit == Some(100)));
        let bb = r.rows.iter().find(|x| x.account.id == "b").unwrap();
        assert_eq!(bb.token_limit, Some(7));
    }

    #[test]
    fn token_limit_max_ignores_the_recent_filter() {
        let day = |d: u32| Utc.with_ymd_and_hms(2026, 11, d, 10, 0, 0).unwrap();
        let v = [rec("a", day(1), 900), rec("a", day(8), 50)];
        let mut b = opts(day(8));
        b.token_limit = Some(TokenLimit::Max);
        b.recent = true;
        let r = report(&v, &b);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0].token_limit, Some(900));
    }

    #[test]
    fn fixed_token_limit_applies_to_every_block() {
        let v = [rec("a", at(1, 0), 10)];
        let mut b = opts(at(12, 0));
        b.token_limit = Some(TokenLimit::Tokens(500));
        let r = report(&v, &b);
        assert_eq!(r.rows[0].token_limit, Some(500));
        assert_eq!(report(&v, &opts(at(12, 0))).rows[0].token_limit, None);
    }

    #[test]
    fn projection_extends_the_burn_rate_to_the_block_end() {
        // 100 tokens over 10 minutes => 10/min; now 11:00, block ends 15:00 => 240 min left.
        let v = [rec("a", at(10, 0), 50), rec("a", at(10, 10), 50)];
        let r = report(&v, &opts(at(11, 0)));
        let p = r.rows[0]
            .projection
            .clone()
            .expect("active block has a projection");
        assert!((p.burn_rate_tokens_per_min - 10.0).abs() < 1e-9);
        assert_eq!(p.projected_tokens, 100 + 2400);
        assert_eq!(p.projected_cost_usd, None);
        // finished blocks carry none
        assert!(report(&v, &opts(at(20, 0))).rows[0].projection.is_none());
    }

    #[test]
    fn projection_needs_a_time_span() {
        let v = [rec("a", at(10, 0), 50)];
        let r = report(&v, &opts(at(11, 0)));
        assert!(r.rows[0].active);
        assert!(r.rows[0].projection.is_none());
    }

    #[test]
    fn order_desc_reverses_time_inside_an_account() {
        let v = [rec("a", at(1, 0), 1), rec("a", at(10, 0), 2)];
        let (labels, aliases) = (HashMap::new(), HashMap::new());
        let l = Labeler::new(&labels, &aliases);
        let o = Options {
            order: Order::Desc,
            ..Options::default()
        };
        let r = blocks_report(&v, &l, &PriceTable::builtin(), &o, &opts(at(12, 0)));
        let starts: Vec<_> = r.rows.iter().filter(|x| !x.gap).map(|x| x.start).collect();
        assert_eq!(starts, [at(10, 0), at(1, 0)]);
    }
}
