# Check Dependency Inheritance in Cargo Workspaces

A Cargo subcommand that detects and fixes workspace dependency inheritance issues in Cargo workspaces. It finds crates that specify dependency versions directly instead of using [`workspace = true`](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#inheriting-a-dependency-from-a-workspace), flags version mismatches, suggests candidates for workspace promotion, and can automatically fix all reported problems.

## Why?

Cargo workspaces support [dependency inheritance](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#inheriting-a-dependency-from-a-workspace), you declare shared dependency versions once in the root `Cargo.toml` under `[workspace.dependencies]`, and member crates reference them with `{ workspace = true }`. This prevents version drift and duplicated dependency specs across workspace members.

However, **Cargo has no built-in lint** for detecting when a crate specifies a version directly instead of inheriting from the workspace. [Clippy issue #10306](https://github.com/rust-lang/rust-clippy/issues/10306) tracks this as a feature request but remains unimplemented.

`cargo-workspace-inheritance-check` fills this gap.

## What It Checks

### 1. Workspace dependency not inherited (error)

A crate declares a dependency with an explicit version that already exists in `[workspace.dependencies]`. It should use `{ workspace = true }` instead.

```
error: `serde = "1.0"` in crates/foo/Cargo.toml should use `serde = { workspace = true }`
```

### 2. Version mismatch (error)

A crate declares a dependency with a different version than what's in `[workspace.dependencies]`.

```
error: `rand = "0.7"` in crates/bar/Cargo.toml has a different version than workspace `rand = "0.8"`
```

### 3. Candidate for workspace promotion (warning)

A dependency appears in multiple crates but isn't declared in `[workspace.dependencies]`. This check helps you identify dependencies that should be centralized.

```
warning: `serde_yaml` appears in 3 crates but is not in [workspace.dependencies]
  --> crates/config/Cargo.toml
  --> crates/node/Cargo.toml
  --> crates/cli/Cargo.toml
  hint: consider adding `serde_yaml = "0.9"` to [workspace.dependencies]
```

By default, this is reported as a **warning** and does not cause the tool to exit with a non-zero code. Use `--promotion-failure` to treat promotion candidates as errors.

## Installation

```bash
cargo install cargo-workspace-inheritance-check
```

## Usage

### Default behavior

Running without any flags checks all workspace members against `[workspace.dependencies]` in the root `Cargo.toml`. It reports errors for dependencies not using inheritance ([checks 1 and 2](#what-it-checks)) and warnings for promotion candidates ([check 3](#3-candidate-for-workspace-promotion-warning)). The tool exits with code 1 if any **errors** are found, and 0 if there are only warnings or no issues.

```bash
cargo workspace-inheritance-check
```

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `--path <PATH>` | Path to the workspace root | `.` |
| `--promotion-threshold <N>` | Minimum number of crates a dependency must appear in before it is flagged as a promotion candidate. For example, `--promotion-threshold 3` means a dependency must appear in at least 3 crates to trigger a warning. | `2` |
| `--promotion-failure` | Promote promotion candidate warnings to errors, causing the tool to exit with code 1 when candidates are found. Useful in CI when you want to enforce that all shared dependencies are declared in `[workspace.dependencies]`. | `false` |
| `--format <FORMAT>` | Output format: `human` or `json` | `human` |
| `--no-fail` | Always exit with code 0, regardless of errors found. Useful when you want to see the report without failing a CI pipeline. | `false` |
| `--fix` | Automatically fix all reported problems. For not-inherited and version-mismatch errors, replaces explicit versions with `{ workspace = true }`. For promotion candidates, adds the dependency to `[workspace.dependencies]` and updates all member crates. Other dependency attributes (e.g. `features`, `optional`) are preserved. If any member sets `default-features = false`, the workspace dependency will too (required for correctness — Cargo ignores member-level `default-features = false` unless the workspace dependency also sets it, and the 2024 edition makes this a hard error). | `false` |

### Examples

```bash
# Check a workspace at a specific path
cargo workspace-inheritance-check --path /path/to/workspace

# Only flag dependencies used in 3+ crates as promotion candidates
cargo workspace-inheritance-check --promotion-threshold 3

# Fail CI if any dependency could be promoted to workspace level
cargo workspace-inheritance-check --promotion-failure

# Get machine-readable output
cargo workspace-inheritance-check --format json

# Report issues without failing
cargo workspace-inheritance-check --no-fail

# Automatically fix all reported problems
cargo workspace-inheritance-check --fix
```

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | No errors found (warnings are OK unless `--promotion-failure` is set) |
| `1` | Errors found (or promotion warnings promoted to errors via `--promotion-failure`) |

`--no-fail` overrides the exit code to always be `0`.

## Ignore Rules

You can skip specific dependencies from being checked by adding ignore rules to your workspace root `Cargo.toml`:

```toml
[workspace.metadata.inheritance-check]
ignore = [
  # Ignore rand in a specific crate (e.g., it intentionally uses a different version)
  { dependency = "rand", member = "crates/bar" },
  # Ignore openssl in all crates
  { dependency = "openssl" },
]
```

Ignored dependencies are skipped in both reporting and fixing.

## JSON Output

Use `--format json` for machine-readable output, useful for integrating with other tools or custom CI scripts:

```json
{
  "diagnostics": [
    {
      "severity": "error",
      "check": "not-inherited",
      "dependency": "serde",
      "version": "1.0",
      "member": "crates/foo/Cargo.toml",
      "workspace_version": "1.0"
    }
  ],
  "summary": {
    "errors": 1,
    "warnings": 0
  }
}
```

## CI Integration

### GitHub Actions

```yaml
- uses: RomarQ/cargo-workspace-inheritance-check@v1
```

With options:

```yaml
- uses: RomarQ/cargo-workspace-inheritance-check@v1
  with:
    promotion-failure: true
    promotion-threshold: 3
```

All inputs are optional:

| Input | Description | Default |
|-------|-------------|---------|
| `path` | Path to the workspace root | `.` |
| `promotion-threshold` | Min crate count for promotion warning | `2` |
| `promotion-failure` | Treat promotion candidates as errors | `false` |
| `format` | Output format: `human` or `json` | `human` |
| `no-fail` | Exit 0 even when errors are found | `false` |
| `fix` | Automatically fix reported problems | `false` |
| `version` | Version to install | `latest` |

### Without the action

```yaml
- run: cargo install cargo-workspace-inheritance-check
- run: cargo workspace-inheritance-check
```

## Dependency Sections Checked

All dependency sections in member crates are checked:

- `[dependencies]`
- `[dev-dependencies]`
- `[build-dependencies]`
- `[target.'...'.dependencies]` (and dev/build variants)
