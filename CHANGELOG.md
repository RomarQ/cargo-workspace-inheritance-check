# Changelog

All notable changes to this project will be documented in this file.

## [1.1.2] - 2026-03-18

### Fixed

- `--fix` now strips `default-features` from member entries when converting to `{ workspace = true }`, since it must be set at the workspace level to have any effect
- `--fix` now checks target-specific dependency sections (e.g. `[target.'cfg(windows)'.dependencies]`) when determining whether to set `default-features = false` on promoted workspace dependencies

## [1.1.1] - 2026-03-18

### Fixed

- `--fix` now propagates `default-features = false` to `[workspace.dependencies]` when promoting a dependency that any member uses with `default-features = false`. Without this, Cargo silently ignores the member-level setting (pre-2024 edition) or raises a hard error (2024 edition).

## [1.1.0] - 2026-03-18

### What's Changed

- `--fix` flag to automatically fix all reported problems:
  - **Not inherited / version mismatch**: replaces explicit versions with `{ workspace = true }` in member crates
  - **Promotion candidates**: adds the dependency to `[workspace.dependencies]` and updates all member crates
  - Preserves other dependency attributes (e.g. `features`, `optional`)
  - Handles all dependency sections including target-specific dependencies
- `fix` input for the GitHub Action

## [1.0.0] - 2026-03-17

**Full Changelog**: https://github.com/RomarQ/cargo-workspace-inheritance-check/compare/v0.2.0...v1.0.0

### Added

- GitHub Action for CI integration (`uses: RomarQ/cargo-workspace-inheritance-check@v1`)
- CI workflow with smoke tests

## [0.2.0] - 2026-03-17

**Full Changelog**: https://github.com/RomarQ/cargo-workspace-inheritance-check/compare/v0.1.0...v0.2.0

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
