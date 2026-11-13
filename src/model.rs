use chrono::{DateTime, Utc};
use serde::Serialize;

/// Where an account attribution came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountSource {
    Owner,
    Profile,
    Unknown,
}

/// How an account pays for usage.
///
/// Rule (best effort): neither tool writes an auth-mode field into its local logs, so this is
/// inferred. Claude: an account attributed by a session-owner record or a profile `oauthAccount`
/// is `Subscription`; an unattributed one is `Unknown`. Codex: an account whose rate-limit
/// snapshots carry a `plan_type` is `Subscription`, otherwise `Unknown`. `Api` is never inferred
/// without positive evidence, so nothing produces it today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    Subscription,
    Api,
    Unknown,
}

/// Meaning of a cost figure: real spend or an API-equivalent value for comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CostKind {
    Billed,
    ApiEquivalent,
}

impl Billing {
    /// Only `Api` is billed; subscription and unknown values are API-equivalent.
    pub fn cost_kind(self) -> CostKind {
        match self {
            Billing::Api => CostKind::Billed,
            _ => CostKind::ApiEquivalent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct AccountRef {
    pub tool: String,
    pub id: String,
    pub org_id: Option<String>,
    pub source: AccountSource,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageRecord {
    pub ts: DateTime<Utc>,
    pub tool: String,
    pub account: AccountRef,
    pub model: String,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub input: u64,
    pub output: u64,
    /// Codex reasoning tokens, a part of `output` (display only); 0 for Claude.
    pub reasoning: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    /// `(message.id, requestId)` for Claude; `None` means count once.
    pub dedup_key: Option<String>,
}

/// One rate-limit window as reported by the tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LimitWindow {
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    /// Unix seconds.
    pub resets_at: Option<i64>,
}

/// Latest rate-limit state seen for an account.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LimitSnapshot {
    pub tool: String,
    pub account: AccountRef,
    pub primary: Option<LimitWindow>,
    pub secondary: Option<LimitWindow>,
    pub plan_type: Option<String>,
    pub observed_at: DateTime<Utc>,
}
