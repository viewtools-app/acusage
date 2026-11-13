# AGENTS.md

Notes for AI coding agents working on acusage. Humans: see [CONTRIBUTING.md](CONTRIBUTING.md).

acusage is a Rust CLI that reads local Claude Code, Codex, Antigravity and Gemini CLI logs and
reports token usage and estimated cost per account.

## Commands
- Build: `cargo build`
- Run: `cargo run -- daily`
- Test: `cargo test -q` (one test: `cargo test -q <name>`)
- Lint: `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
- Snapshots: after an intended output change, `UPDATE_SNAPSHOTS=1 cargo test -q`, then review the diff

## Rules
- Read-only: never write, lock, rename or delete any file, and never open a network connection.
- Never read credential files (tokens, auth files, keychains).
- Unknown log records and fields are skipped, never fatal.
- Fixtures are synthetic: fake UUIDs, `user@example.com`, `/Users/user/...`. Never paste a real log.
- Do not weaken or delete tests to make a change pass.
- No new dependencies without a clear reason in the PR.
- When behavior or output changes, update `docs/` and `CHANGELOG.md` in the same PR.
- Conventional Commits (`feat:`, `fix:`, `docs:` ...). No AI attribution lines.
