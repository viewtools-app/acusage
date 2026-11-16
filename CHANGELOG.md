# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning: [SemVer](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0] - 2026-11-16

### Changed
- Renamed from acusage to atkusage (Agent Token usage): the binary, crate, npm package, config directory (`~/.config/atkusage/`), `$ATKUSAGE_CONFIG` and the report title. The old config path and `$ACUSAGE_CONFIG` still work as fallbacks.

### Added
- README demo GIFs for every command, recorded from synthetic logs with `scripts/demo/record.sh` (needs [vhs](https://github.com/charmbracelet/vhs)).

## [0.5.0] - 2026-11-13

First public release.

### Added
- Reports from Claude Code, Codex, Antigravity and Gemini CLI logs: `daily`, `weekly`, `monthly`, `session`, `blocks`, `accounts`, `limits`.
- Per-account attribution with config labels.
- Filters by project, date, account and tool; per-model breakdown; compact and full layouts.
- `--json` output, schema 1.
- Offline price estimates (built-in table, LiteLLM snapshot, config overrides).
- Binaries for macOS and Linux, a shell installer and an npm package.

[Unreleased]: https://github.com/viewtools-app/atkusage/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/viewtools-app/atkusage/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/viewtools-app/atkusage/releases/tag/v0.5.0
