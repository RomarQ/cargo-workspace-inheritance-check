# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Changed

- The member set comes from `cargo metadata --no-deps`, so it follows cargo's rules. It includes the root package and the path dependencies inside the workspace root, and it applies `exclude` as cargo does. A workspace that cargo cannot load, or a member manifest that does not parse, is now an error.
- The tool needs cargo at run time. It runs the cargo in `CARGO`, which cargo sets for a subcommand, or else `cargo` on `PATH`. An empty `CARGO` counts as unset.
- A member outside the workspace root is named from the root, such as `../shared/Cargo.toml`, also when `members` gives its absolute path.
- A member entry is compared with the workspace entry that has the same key, because cargo inherits by key. Both must name the same package from the same source (registry, `registry-index`, path, or git). A path or git entry must also name the same `registry` or `registry-index`. `registry = "crates-io"` is crates.io. For a renamed entry, the JSON `dependency` value is the key.
- Renamed, `registry-index`, path, and git dependencies are not promotion candidates. A promotion counts each crate once, and it gives no suggested version when two versions tie.
- `--fix` checks the workspace again after the fixes, prints that report in the selected `--format`, and exits 1 when errors remain, unless `--no-fail` is set.
- An invalid ignore rule, including one with an unknown key or an empty `member`, is an error. A rule's `dependency` matches the key or the package name. `member` accepts a leading `./`, `.` covers every crate, and a manifest path such as `Cargo.toml` covers only that crate.
- An ignore rule's `member` is a path from the workspace root, and an absolute path works too. So `member = ".."` covers every crate below the parent directory of the root, including the crates inside the root. In 1.3.0 it covered every crate that the tool named with a leading `../`, and no crate inside the root. To keep that scope, write one rule for each directory outside the root, such as `../shared`.
- `--promotion-threshold 0` is rejected.
- toml_edit 0.25 reads TOML 1.1 manifests. The minimum supported Rust version is 1.88.

### Fixed

- `--fix` keeps what cargo resolves for each member, except the version on a mismatch and the features of the workspace entry. It removes every key that cargo ignores on an inherited entry, such as `path`, `git`, `package`, and `registry`, and keeps `features`, `optional`, and `public`.
- `--fix` writes `default-features = true` where the member uses default features and the workspace entry disables them. The not-inherited error also shows this key. It skips an entry that disables them when the workspace entry does not, and an entry with a version when the workspace entry has none.
- `--fix` skips a promotion whose members use different versions, or whose key `[workspace.dependencies]` already has. Before, members could move to another version or another registry.
- `--fix` reads the `default_features` spelling, keeps the comment after a rewritten entry, and reports a fix only after it is written.
- Inline tables are read at every level: dependency tables, target entries, `[workspace]` and its dependencies, and metadata. `--fix` also promotes into an inline `[workspace]`. `[dev_dependencies]` and `[build_dependencies]` are checked, unless the hyphen spelling is also present, because cargo then ignores them.
- `--path ./x` gives member paths relative to the workspace root.
- The promotion warning for one crate says `appears in 1 crate`.
- The GitHub Action passes its inputs through the environment, so a path with a space works and an input cannot run shell code. Each input is one argument, so an input can no longer add other flags. It installs with `--locked`.

### Added

- `--version`.

## [1.3.0] - 2026-04-21

**Full Changelog**: https://github.com/RomarQ/cargo-workspace-inheritance-check/compare/v1.2.0...v1.3.0

### Added

- Ignore rules via `[workspace.metadata.inheritance-check]` in the root `Cargo.toml`:

  ```toml
  [workspace.metadata.inheritance-check]
  ignore = [
    { dependency = "rand", member = "crates/bar" }, # skip in a specific crate
    { dependency = "openssl" },                     # skip everywhere
  ]
  ```

  Ignored `(dependency, member)` pairs are suppressed from both reporting and `--fix`.
  Without `member`, the rule applies to every crate and also drops the dependency
  from promotion-candidate grouping. Both inline-array and
  `[[workspace.metadata.inheritance-check.ignore]]` array-of-tables syntax are
  accepted.

## [1.2.0] - 2026-03-19

**Full Changelog**: https://github.com/RomarQ/cargo-workspace-inheritance-check/compare/v1.1.2...v1.2.0

## What's Changed
* Simplify codebase: extract shared helpers and reduce duplication (https://github.com/RomarQ/cargo-workspace-inheritance-check/pull/13, https://github.com/RomarQ/cargo-workspace-inheritance-check/pull/17)
* refactor: simplify Diagnostic to enum, extract for_each_dep_table_mut (https://github.com/RomarQ/cargo-workspace-inheritance-check/pull/14)
* feat: alternate registry support (https://github.com/RomarQ/cargo-workspace-inheritance-check/pull/15)

### Added

- Alternate registry support: dependencies using `registry = "my-registry"` are now handled correctly
  - A member dep only matches a workspace dep if both use the same registry (or both use the default crates.io)
  - Promotion candidates are grouped by `(name, registry)` pair — deps from different registries are not grouped together
  - `--fix` strips `registry` from member deps when converting to `{ workspace = true }` (the workspace dep owns the registry)
  - `--fix` carries `registry` into promoted workspace deps

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
