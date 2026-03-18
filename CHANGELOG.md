# Changelog

All notable changes to this project will be documented in this file.

## [1.1.0] - 2026-03-18

## What's Changed
* feature(fix-arg): Add --fix argument by @RomarQ in https://github.com/RomarQ/cargo-workspace-inheritance-check/pull/7

## New Contributors
* @RomarQ made their first contribution in https://github.com/RomarQ/cargo-workspace-inheritance-check/pull/7

**Full Changelog**: https://github.com/RomarQ/cargo-workspace-inheritance-check/compare/v1.0.0...v1.1.0


## [Unreleased]

### Added

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
