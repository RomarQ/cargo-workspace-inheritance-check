# Check Dependency Inheritance in Cargo Workspaces

A Cargo subcommand that enforces dependency inheritance hygiene in Cargo workspaces. It detects crates that specify dependency versions directly instead of using [`workspace = true`](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#inheriting-a-dependency-from-a-workspace), flags version mismatches, and suggests candidates for workspace promotion.

## Why?

Cargo workspaces support [dependency inheritance](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#inheriting-a-dependency-from-a-workspace),you declare shared dependency versions once in the root `Cargo.toml` under `[workspace.dependencies]`, and member crates reference them with `{ workspace = true }`. This prevents version drift and duplicated dependency specs across workspace members.

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

A dependency appears in multiple crates but isn't declared in `[workspace.dependencies]`.

```
warning: `serde_yaml` appears in 3 crates but is not in [workspace.dependencies]
  --> crates/config/Cargo.toml
  --> crates/node/Cargo.toml
  --> crates/cli/Cargo.toml
  hint: consider adding `serde_yaml = "0.9"` to [workspace.dependencies]
```

## Installation

```bash
cargo install cargo-workspace-inheritance-check
```

## Usage

Run from your workspace root:

```bash
# Check workspace dependency inheritance
cargo workspace-inheritance-check

# Specify a different workspace root
cargo workspace-inheritance-check --path /path/to/workspace

# Set promotion threshold (default: 2)
cargo workspace-inheritance-check --promotion-threshold 3

# Treat promotion candidates as errors
cargo workspace-inheritance-check --promotion-failure

# Output as JSON
cargo workspace-inheritance-check --format json

# Don't fail on errors (always exit 0)
cargo workspace-inheritance-check --no-fail
```

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `--path <PATH>` | Path to workspace root | `.` |
| `--promotion-threshold <N>` | Min crate count before suggesting workspace promotion | `2` |
| `--promotion-failure` | Treat promotion candidates as errors | `false` |
| `--format <FORMAT>` | Output format: `human`, `json` | `human` |
| `--no-fail` | Exit 0 even when errors are found | `false` |

## Ignore Rules

You can skip specific dependencies from being checked by adding ignore rules to your workspace root `Cargo.toml`:

```toml
[workspace.metadata.inheritance-check]
ignore = [
  # Ignore rand in a specific crate
  { dependency = "rand", member = "crates/bar" },
  # Ignore openssl in all crates
  { dependency = "openssl" },
]
```

## JSON Output

Use `--format json` for machine-readable output, useful for integrating with other tools:

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
- name: Install cargo-workspace-inheritance-check
  run: cargo install cargo-workspace-inheritance-check

- name: Check workspace dependency inheritance
  run: cargo workspace-inheritance-check
```

## Dependency Sections Checked

All dependency sections in member crates are checked:

- `[dependencies]`
- `[dev-dependencies]`
- `[build-dependencies]`
- `[target.'...'.dependencies]` (and dev/build variants)
