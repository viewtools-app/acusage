# JSON output

`acusage <command> --json` prints one object. `schema` is `1`. Within schema 1, fields are only added, never renamed, removed or retyped; a breaking change bumps `schema`.

| Field | Type | Notes |
|---|---|---|
| `schema` | number | `1` |
| `report` | string | `daily`, `weekly`, `monthly`, `blocks`, `session`, `accounts`, `limits` |
| `generated_at` | string | UTC, RFC 3339 |
| `rows` | array | Shape depends on `report`, see below |
| `totals` | object or null | Totals fields; `null` for `limits` |
| `unpriced_models` | string[] | Empty for `limits` |
| `local_models` | string[] | Local models counted at $0; empty for `limits` |
| `price_sources` | object | Priced model id to `config`, `builtin`, `litellm` or `local`; `{}` for `limits` |
| `config_path` | string | Optional; the config file looked up |
| `unknown` | object or null | Optional; counts of usage whose log has no account |
| `attribution` | array | Optional; record counts per account source, per agent (with `--verbose`) |

## Rows

| Report | Fields (plus totals fields unless noted) |
|---|---|
| `daily`, `weekly`, `monthly` | `bucket` (`YYYY-MM-DD` day or week Monday, `YYYY-MM` month), `account`, `cost_kind`, `models`; `project` with `--instances`; `breakdown` with `--breakdown` |
| `blocks` | `start`, `end`, `actual_end` (or null), `active`, `gap`, `account`, `cost_kind`, `models`, `token_limit` (or null), `projection` (or null) |
| `session` | `session_id` (or null), `last_activity`, `account`, `cost_kind`, `project` (or null), `models` |
| `accounts` | `account`, `cost_kind`, `label_source` (`alias`, `email`, `id`, `unknown`), `first_seen`, `last_seen` |
| `limits` | `account`, `plan_type` (or null), `primary`, `secondary` (window or null), `observed_at`; no totals |

- `models`: sorted strings. `project`: last path segment; `(unknown)` in usage rows, null in `session`.
- `breakdown`: per-model objects (`model` plus totals fields), highest cost first.
- `projection`: `burn_rate_tokens_per_min`, `projected_tokens`, `projected_cost_usd`.
- Gap rows (`gap: true`) have zero totals. Limits window: `used_percent`, `window_minutes` (or null), `resets_at` (Unix seconds or null).
- `cost_kind`: `billed` or `api_equivalent`. Timestamps are strings. Daily subtotals are table-only.

## Shared objects

| Totals field | Meaning |
|---|---|
| `input_tokens`, `output_tokens`, `cache_write_tokens`, `cache_read_tokens`, `total_tokens` | Numbers |
| `reasoning_tokens` | Part of `output_tokens`. Codex and Gemini only; 0 for Claude and Antigravity |
| `cost_usd` | Sum of priced usage; null if nothing is priced |
| `partial_cost` | true if some usage is unpriced, so `cost_usd` is a lower bound (`$X*` in tables) |

| `account` field | Values |
|---|---|
| `tool` | `claude`, `codex`, `antigravity`, `gemini` |
| `id`, `label` | string |
| `org_id` | string or null |
| `source` | `owner`, `profile`, `unknown` ([details](configuration.md#accounts)) |
| `billing` | `subscription`, `api`, `unknown` |
