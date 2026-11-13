# Contributing

acusage is read-only and offline. Every change keeps that: no network, no file writes (tests may use temp dirs), no credential reads.

## Setup and checks

Needs stable Rust with edition 2024. CI runs the same checks:

```sh
cargo run -- daily
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test -q
```

## Snapshots

`tests/snapshots.rs` compares output with `tests/snapshots/`. After an intended output change, run `UPDATE_SNAPSHOTS=1 cargo test -q` and review the diff.

## Fixtures

Fixtures in `tests/fixtures/` are synthetic: fake UUIDs, `user@example.com`, `/Users/user/...`. Never commit a real session log, even redacted.

## New log formats

Add a module under `src/sources/` with a synthetic fixture that includes malformed lines. A parser must:

- skip unknown record types and fields, never fail on them (formats change between agent versions);
- take account labels only from non-secret account fields, never from credential files;
- count each billable request once, even when streamed or resumed sessions repeat records.

## Prices

- Built-in table: `src/pricing.rs`.
- LiteLLM snapshot: `data/litellm-prices.json`. Refresh with `python3 scripts/update-litellm-prices.py --download` (Python 3, standard library), or pass a local LiteLLM JSON file. The snapshot records the sha256 of its input. Review the diff and run `cargo test -q`.

## Pull requests

- [Conventional Commits](https://www.conventionalcommits.org/): `type(scope): subject`.
- One topic per PR, against `main`.
- Update `docs/` and `CHANGELOG.md` when behavior changes.

By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
