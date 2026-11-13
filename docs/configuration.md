# Configuration

acusage needs no config. To rename accounts or set prices, create `~/.config/acusage/config.toml` (or point `$ACUSAGE_CONFIG` at a file). A missing file means defaults; an invalid file is an error that names the path.

```toml
# Labels. The key is the account id or email, as in `acusage accounts --json` (rows[].account.id).
[aliases]
"00000000-0000-0000-0000-000000000000" = "Work"
"user@example.com" = "Personal"

# Prices in USD per 1,000,000 tokens. All five fields are required.
[prices."my-model-1"]
input = 1.00
output = 5.00
cache_write_5m = 1.25
cache_write_1h = 2.00
cache_read = 0.10
```

For OpenAI models, which have no cache write price, set both `cache_write_*` fields to `0`.

## Prices

All prices are offline and embedded in the binary. A model takes the first source that knows it, even if a later one has an exact id:

| Order | Source | `price_sources` value | Mark |
|---|---|---|---|
| 1 | Your `[prices]` | `config` | none |
| 2 | Built-in table: current Anthropic, OpenAI and some Gemini models (`src/pricing.rs`) | `builtin` | none |
| 3 | [LiteLLM](https://github.com/BerriAI/litellm) snapshot: Anthropic, OpenAI, Gemini, Qwen | `litellm` | `†` |

- Each source also tries the id without a trailing `-YYYYMMDD`, thinking level (`-high`, ...) or variant (`-thinking`, `-tiered`, `-thinking-high`).
- A `-tiered` model costs as its base model; long-context tiers are not modeled.
- LiteLLM entries without cache prices: Anthropic cache write is 1.25x input (5 min) and 2x (1 h), cache read 0.1x; other providers write `0`, read at input price. Entries priced 0/0 are left out.
- `†` follows the model name and the notes say `priced from LiteLLM snapshot (third-party, not checked)`. If several ids share one shortened line, it is marked if any is LiteLLM-priced. The note stays in the compact layout; `--no-cost` hides both.
- A model in no source is unpriced: its money cell gets `*` and the notes list it.

Local models cost $0 and are not unpriced. An id is local when it has exactly one `:` and a non-empty tag that is not all digits (`qwen3:4b`, `llama3:latest`). Bedrock ids, `ft:` fine-tunes and `arn:` ids are not local. The notes list them as `local models counted at $0: ...`; a `[prices]` entry wins.

## Accounts

Each row belongs to one account. Its label is the alias, else the email, else the id.

| Source | How it is found |
|---|---|
| `owner` | The log names the account: a Claude `session-owner` record or `creator_account_id` in a Codex `session_meta`. Strongest. |
| `profile` | The log lives in a Claude config dir whose `.claude.json` has an `oauthAccount`. |
| `unknown` | Neither: old logs, removed or never signed-in config dirs, and all Antigravity and Gemini CLI logs (their credentials live in files acusage never opens). |

- `unknown` usage is shown but not counted: the title reads `Accounts  N + unknown`. Its cost kind is `api_equivalent`, as the billing mode is unknown.
- `acusage --verbose` prints, per agent, how many records came from each source.
- Aliases only rename; they never move usage between accounts.
