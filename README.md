# acusage

Know which account spent your AI coding tokens.

acusage (AI coding usage) reads the local logs of Claude Code, Codex, Antigravity and Gemini CLI and reports tokens and estimated cost per account, agent, model, day and session. Offline, read-only, one Rust binary.

[![CI](https://github.com/viewtools-app/acusage/actions/workflows/ci.yml/badge.svg)](https://github.com/viewtools-app/acusage/actions/workflows/ci.yml) [![npm](https://img.shields.io/npm/v/acusage)](https://www.npmjs.com/package/acusage) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

```
$ acusage daily --compact          # illustrative numbers
╭──────────────────────────────────────────╮
│ ACUsage · Daily Token Usage Report       │
│                                          │
│ Period    9 Nov 2026                     │
│ Accounts  2                              │
│ Agents    Claude Code, Codex             │
│                    acusage.viewtools.app │
╰──────────────────────────────────────────╯
┌────────────┬───────────────┬────────────────┬───────────────┬───────────┬─────────┬─────────────────┐
│ Date       │ Agent         │ Account        │ Models        │ Input     │ Output  │ API Value (USD) │
├────────────┼───────────────┼────────────────┼───────────────┼───────────┼─────────┼─────────────────┤
│ 2026-11-09 │ All           │                │               │ 1,912,407 │ 402,118 │          $48.10 │
├────────────┼───────────────┼────────────────┼───────────────┼───────────┼─────────┼─────────────────┤
│            │ - Claude Code │ me (Personal)  │ - opus-5-5    │ 1,204,331 │ 251,092 │          $31.75 │
├────────────┼───────────────┼────────────────┼───────────────┼───────────┼─────────┼─────────────────┤
│            │ - Codex       │ me (Work)      │ - gpt-6.1-sol │   708,076 │ 151,026 │          $16.35 │
├────────────┼───────────────┼────────────────┼───────────────┼───────────┼─────────┼─────────────────┤
│ Total      │               │                │               │ 1,912,407 │ 402,118 │          $48.10 │
└────────────┴───────────────┴────────────────┴───────────────┴───────────┴─────────┴─────────────────┘
API Value = what this usage would cost at API prices; subscriptions are not billed per token.
```

## Install

```sh
npx acusage                # run without installing (Node 18+)
npm install -g acusage
curl -LsSf https://github.com/viewtools-app/acusage/releases/latest/download/acusage-installer.sh | sh
cargo install --git https://github.com/viewtools-app/acusage
```

Prebuilt for macOS (arm64, x86_64) and Linux (arm64, x86_64, glibc 2.34+). No Windows yet.

## Usage

| Command | Shows |
|---|---|
| `acusage` / `acusage daily` | Usage per day (bare `acusage`: last 30 days) |
| `acusage weekly` | Usage per ISO week, Monday start |
| `acusage monthly` | Usage per month |
| `acusage session` | One row per session |
| `acusage blocks` | 5-hour billing blocks per account |
| `acusage accounts` | Every account seen |
| `acusage limits` | Codex rate-limit windows |

```sh
acusage daily --since 2026-11-01 --breakdown   # per-model sub-rows
acusage blocks --active                        # current block with projections
acusage monthly --tool codex --json            # machine-readable
```

### Demos

The recordings below use synthetic logs (`scripts/demo/record.sh`); numbers are illustrative.

<details open>
<summary><code>acusage daily</code>: usage per day</summary>

![acusage daily --last 2](docs/demo/daily.gif)
</details>

<details>
<summary><code>acusage weekly</code>: usage per ISO week</summary>

![acusage weekly --last 2](docs/demo/weekly.gif)
</details>

<details>
<summary><code>acusage monthly</code>: usage per month</summary>

![acusage monthly](docs/demo/monthly.gif)
</details>

<details>
<summary><code>acusage session</code>: one row per session</summary>

![acusage session](docs/demo/session.gif)
</details>

<details>
<summary><code>acusage blocks</code>: 5-hour billing blocks</summary>

![acusage blocks --active --account Personal](docs/demo/blocks.gif)
</details>

<details>
<summary><code>acusage accounts</code>: every account seen</summary>

![acusage accounts](docs/demo/accounts.gif)
</details>

<details>
<summary><code>acusage limits</code>: Codex rate-limit windows</summary>

![acusage limits](docs/demo/limits.gif)
</details>

## Accounts

Each row belongs to one account, labeled by alias, else email, else id. The account comes from the log itself (`owner`) or from the Claude config dir the log lives in (`profile`).
Anything else is `unknown` (old logs, removed or signed-out config dirs, all Antigravity and Gemini CLI logs) and is left out of the title's `Accounts` count.
Rename an account with `[aliases]`; see [Configuration](docs/configuration.md#accounts).

## Privacy

- No network: the binary opens no sockets. The npm and shell installers fetch it from GitHub Releases once.
- No writes: it never creates, changes, locks or deletes a file.
- No credentials: from `.claude.json` it reads only the account fields used for labels.

## Docs

[Usage](docs/usage.md) · [Configuration](docs/configuration.md) · [JSON](docs/json.md) · [How it works](docs/how-it-works.md) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

## License

MIT ([LICENSE](LICENSE)). Third-party data and fonts: [NOTICE](NOTICE).
