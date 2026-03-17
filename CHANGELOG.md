# Changelog


## [0.2.0] - 2026-03-17

- Initial release
All notable changes to this project will be documented in this file.

## [0.1.1] - 2026-03-17

### Changed

- Use library crate in main.rs instead of re-declaring modules
- Switch to `BTreeMap` for deterministic diagnostic ordering
- Support glob patterns in `workspace.exclude`
- Deterministic tie-breaking for promotion version suggestion
- Validate `--format` flag with `ValueEnum` to reject invalid values
- Restrict `_subcommand` positional to expected value only

## [0.1.0] - 2026-03-17

### Added

- Initial release
- Check 1: Workspace dependency not inherited (error)
- Check 2: Version mismatch (error)
- Check 3: Candidate for workspace promotion (warning)
- Human-readable and JSON output formats
- `--path`, `--promotion-threshold`, `--promotion-failure`, `--format`, `--no-fail` flags
- Support for `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, and target-specific dependencies
- Support for `workspace.exclude` with glob patterns
- Support for renamed dependencies (`package` field)
