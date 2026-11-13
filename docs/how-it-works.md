# How it works

## Counting

- Each billable request is counted once. Claude: per `(message.id, requestId)` across all files. Codex: per `response_id`, with a fallback for logs that only have token-count events. Gemini CLI: per message id.
- Unknown record types, unknown fields and malformed lines are skipped, never fatal.
- Money for subscription accounts is labeled API value, not cost.
- Prices are never fetched at run time, so they lag new models until the table or snapshot is updated. Sources and marks: [Configuration](configuration.md#prices).
- Long-context price tiers (OpenAI above 272K input tokens, Haiku 5.5 above 100K) are not modeled.

## Gemini CLI

Built from the documented log format; not yet verified on real logs.

- Each `type: "gemini"` message with a `tokens` object is one request.
- `Input` = input minus cached plus tool tokens. Cache read = cached. `Output` includes thinking tokens, also shown as reasoning.
- No account record, so usage is `unknown`. The project is not shown.
- Built-in prices: Standard paid tier text prices for gemini-3.8-flash, 3.6-flash, 3.5-flash-lite and 3.1-flash-lite. Thinking suffixes (`-high`, `-medium`, `-low`, `-minimal`) price as the base id and show in full.
- Flash 3.8 and 3.6 use the prices in force until 2026-12-31. From 2027-01-01 they are 1.50 input, 7.50 output, 0.15 cached per million tokens; override under `[prices]` until the table is updated.
- Cache storage per hour is not modeled.

## Antigravity

- Each transcript record is one request, not cumulative. `Input` is uncached input; cache read is separate; `Output` includes reasoning, so money does too.
- The model comes from `conversations/<id>.db`, read by a minimal read-only SQLite reader. The write-ahead log is ignored.
- Requests match database rows by `(input, output, cache read)`, in order; all-zero rows match only all-zero requests.
- A row with only a model code takes the id seen with that code in the same database, else the lowercased code.
- A request with no matching row gets model `unknown`, never a guess.
- Limits: databases over 64 MiB are skipped. Malformed ones are bounded by file size and 100,000 rows.
- Priced if the model id is in any price source; otherwise unpriced until you add `[prices]`.

## Blocks

A block starts at the UTC hour of an account's first record and lasts `--session-length` hours (default 5). A record at or after the end opens a new block. A gap row is added when an account is idle for at least one block length.

## Performance

About 100 ms (± 5) mean and 66 MiB peak memory for `daily --json` over 480 MB of Claude Code logs, 90 MB of Codex logs and 60 MB of Antigravity data (MacBook, 2026-11-08, [hyperfine](https://github.com/sharkdp/hyperfine) `-N --warmup 3 --runs 20`). Measure your own:

```sh
hyperfine -N --warmup 3 'acusage daily --json'
```
