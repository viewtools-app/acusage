# Usage

```sh
acusage [OPTIONS] [COMMAND]
```

Run `acusage --help` or `acusage <command> --help` for the same list in your terminal.

## Commands

| Command | Shows |
|---|---|
| `daily` | Usage per day (the default) |
| `weekly` | Usage per ISO week, Monday start |
| `monthly` | Usage per month |
| `session` | One row per session |
| `blocks` | 5-hour billing blocks per account |
| `accounts` | Every account seen |
| `limits` | Codex rate-limit windows |

A bare `acusage` shows the last 30 days of `daily`.

## Options

Filters:

| Option | Effect |
|---|---|
| `-s, --since <DATE>` | First day, inclusive (`YYYY-MM-DD` or `YYYYMMDD`) |
| `-u, --until <DATE>` | Last day, inclusive |
| `--last <N>` | Only the most recent N days, weeks or months |
| `--account <LABEL>` | Label, email or id prefix; repeatable; `unknown` selects unattributed usage |
| `--tool <TOOL>` | `claude`, `codex`, `antigravity` or `gemini` |
| `-p, --project <TEXT>` | Projects whose path contains TEXT, case-insensitive (`daily`, `weekly`, `monthly`) |

Layout and output:

| Option | Effect |
|---|---|
| `-i, --instances` | Group by project first, with a subtotal each (`daily`, `weekly`, `monthly`) |
| `-b, --breakdown` | One sub-row per model under rows that used two or more |
| `--compact` | Only Input and Output columns |
| `--full` | Never shrink or drop columns; full session ids |
| `-o, --order <asc\|desc>` | Bucket order, default `asc` |
| `--utc` | Bucket in UTC instead of local time |
| `-j, --json` | JSON output, see [JSON](json.md) |
| `--no-cost` | Hide the money column |
| `--color`, `--no-color` | Force color on or off |
| `--verbose` | Counts of skipped files and lines and of account sources, on stderr; never log content |

`--since` and `--until` do not apply to `limits`, which shows the latest snapshot per account.

## Log locations

| Agent | Default | Override |
|---|---|---|
| Claude Code | `~/.claude` plus every `~/.claude-*` profile | `--claude-dir`, `$CLAUDE_CONFIG_DIR` (comma separated) |
| Codex | `~/.codex` (`sessions/`, `archived_sessions/`) | `--codex-dir`, `$CODEX_HOME` |
| Antigravity | `~/.gemini/antigravity-cli` (`brain/*/.system_generated/logs/transcript.jsonl`) | `--antigravity-dir` |
| Gemini CLI | `~/.gemini` (`tmp/<project>/chats/session-*.json` and `.jsonl` only) | `--gemini-dir` |

All `--*-dir` options are repeatable. If `$CLAUDE_CONFIG_DIR` points at `~/.claude`, it is still the default dir: labels come from `~/.claude.json` and there is no profile fallback.

## Reading a table

A sample table is in the [README](../README.md). Tables end with notes like these:

```
* excludes unpriced usage: opus-4-1, gpt-meta; add prices under [prices] in ~/.config/acusage/config.toml
† priced from LiteLLM snapshot (third-party, not checked)
API Value = what this usage would cost at API prices; subscriptions are not billed per token.
unknown = usage whose log has no account record (2 sessions, 2026-11-02..2026-11-02); see README#accounts
```

- First column: `Date`, `Week` or `Month`. Last row: `Total`.
- A bucket with several rows starts with an `All` row; its sub-rows are `- Claude Code`, `- Codex`, ...
- The Agent column is hidden when only one agent is present.
- Codex tables add `Reasoning`, which is part of Output (display only).
- Money header: `Cost (USD)`, `API Value (USD)`, or `Cost / API Value (USD)` when both appear (API values then start with `~`).
- `$X*`: some usage in the cell is unpriced, so the amount is a lower bound. `-`: nothing priced.
- `†` after a model: priced from the LiteLLM snapshot. See [Configuration](configuration.md#prices).
- Accounts show the alias, else the email local part plus a short org name (`user (Example)`). Long labels are cut in the middle. A Note warns when two labels collide.
- Models and breakdown sub-rows are sorted by cost, highest first. With `--breakdown` the Models cell reads `N models` and each model gets a `└─ model` line.
- Projects are the last path segment of the working directory; `(no project)` when unknown. With `-i`, projects are sorted by cost.

## Width and color

- Width comes from `$COLUMNS` or `stty size`. Piped output has no width limit.
- When the table is too wide: long cells shrink with `…`, then the compact layout is used (with a Note), then the least useful columns are dropped.
- `--full` disables all of that; `--compact` forces the compact layout.
- Color only on a terminal. `--color` forces it when piped; `--no-color` and `NO_COLOR` always disable it.

## blocks

```sh
acusage blocks --active          # the running block, with projections
acusage blocks -t max --recent   # last 3 days, as % of each account's busiest block
```

| Option | Effect |
|---|---|
| `-a, --active` | Only the active block |
| `-r, --recent` | Blocks started in the last 3 days, plus the active one |
| `-t, --token-limit <N\|max>` | Show % of a token limit; `max` = busiest earlier block of each account |
| `-n, --session-length <HOURS>` | Block length, default 5 |

`blocks` takes the filter and layout options above except `-i` and `-p`.

- One section per account, headed `<label> (Claude Code)` or `<label> (Codex)`.
- Idle stretches show as `idle Xh Ym`.
- The active block shows `PROJECTED` with the burn rate in `tok/min`, and `REMAINING` with `-t`.
- Codex blocks are estimates from the logs. For the real windows use `acusage limits`.
- Codex usage without an account is left out unless you pass `--account unknown`; it is then labeled `mixed accounts`. Claude usage without an account keeps its own `unknown (Claude Code)` section.

How blocks are built: [How it works](how-it-works.md#blocks).

## session

- One row per session: an 8-character id (full with `--full` and in JSON) and the last activity as `YYYY-MM-DD HH:MM`.

## limits

```
$ acusage limits --utc
┌──────────┬──────┬────────┬─────────────────────────────┬──────────┬──────────────────────┐
│ Account  │ Plan │ Window │ Used                        │ Resets   │ Observed             │
├──────────┼──────┼────────┼─────────────────────────────┼──────────┼──────────────────────┤
│ 00000000 │ plus │ 7d     │  40.0% ████████░░░░░░░░░░░░ │ in 3d 5h │ 2026-11-02 09:03 UTC │
├──────────┼──────┼────────┼─────────────────────────────┼──────────┼──────────────────────┤
│ 00000000 │ pro  │ 5h     │   7.0%                      │ expired  │ 2026-11-02 11:03 UTC │
└──────────┴──────┴────────┴─────────────────────────────┴──────────┴──────────────────────┘
```

- The latest Codex 5-hour and 7-day windows per account, from the Codex logs.
- A window whose reset time has passed shows `expired` and sorts last.
- Claude Code stores no limit data locally, so Claude limits are not shown.

## Exit codes

| Code | When |
|---|---|
| 0 | Success |
| 1 | Error, for example an unknown `--account` (the known labels are listed) or an invalid config file |
| 2 | Contradictory filters, for example `--since` after `--until` |

## Uninstall

- npm: `npm uninstall -g acusage`
- Shell installer: delete `~/.cargo/bin/acusage`
- Cargo: `cargo uninstall acusage`
