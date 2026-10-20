# Check Dependency Inheritance in Cargo Workspaces

A Cargo subcommand that finds the crates of a workspace that specify a dependency version directly instead of using [`workspace = true`](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#inheriting-a-dependency-from-a-workspace). It also flags version mismatches, suggests dependencies to add to `[workspace.dependencies]`, and can fix most of the problems that it reports.

## Why?

Cargo workspaces support [dependency inheritance](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#inheriting-a-dependency-from-a-workspace). You declare a shared dependency once in the root `Cargo.toml` under `[workspace.dependencies]`, and member crates use it with `{ workspace = true }`. Then all members use the same version, and each dependency is declared in one place.

**Cargo has no built-in lint** that finds a crate that specifies a version directly instead of inheriting it from the workspace. [cargo#15578](https://github.com/rust-lang/cargo/issues/15578) and [rust-clippy#10306](https://github.com/rust-lang/rust-clippy/issues/10306) request one.

## What It Checks

### 1. Workspace dependency not inherited (error)

A crate declares a dependency with an explicit version that already exists in `[workspace.dependencies]`. It should use `{ workspace = true }` instead.

```
error: `serde = "1.0"` in crates/foo/Cargo.toml should use `serde = { workspace = true }`
```

The suggested entry is the entry that `--fix` writes. It keeps `features`, `optional`, and `public`, and it adds `default-features = true` when the member needs it. When the member cannot inherit the workspace entry as it is, a `hint:` line gives the reason instead of an entry:

```
error: `core = "0.1"` in crates/foo/Cargo.toml should inherit from [workspace.dependencies]
  hint: it requires version 0.1, but the workspace entry has no version
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
  --> crates/cli/Cargo.toml
  --> crates/config/Cargo.toml
  --> crates/node/Cargo.toml
  hint: consider adding `serde_yaml = "0.9"` to [workspace.dependencies]
```

The hint adds `default-features = false` when a crate sets it, as `--fix` does.

By default, this is reported as a **warning** and does not cause the tool to exit with a non-zero code. Use `--promotion-failure` to treat promotion candidates as errors.

### How entries are compared

- The members are the crates that `cargo metadata` reports for the workspace, including the root package and path dependencies inside the workspace root. So the tool needs cargo, and the workspace must load in cargo.
- Cargo inherits a dependency by its key, so a member entry is compared with the `[workspace.dependencies]` entry that has the same key. Both must name the same package (`package = "..."`) from the same source: the same registry, path, or git repository and reference (`branch`, `tag`, or `rev`). Otherwise the member cannot inherit that entry, and the tool reports nothing for it.
- A version requirement with only numbers and an optional `^`, such as `"1"` or `"^1.0"`, is compared by the versions it matches. So `"1"` and `"1.0"` are the same requirement, and `"0"` and `"0.0"` are not. Any other requirement is compared as written.
- A member's path or git dependency without a version is not checked.
- A promotion candidate is a dependency from crates.io or from a named `registry`, and it is not renamed. A path or git dependency is not a promotion candidate. Each crate counts once, even when it lists the dependency in several sections.

## Installation

The tool needs Rust 1.88 or later.

```bash
cargo install cargo-workspace-inheritance-check
```

## Usage

### Default behavior

Run it in the workspace root, or pass `--path`. Without flags, it checks all workspace members against `[workspace.dependencies]` in the root `Cargo.toml`. It reports errors for dependencies not using inheritance ([checks 1 and 2](#what-it-checks)) and warnings for promotion candidates ([check 3](#3-candidate-for-workspace-promotion-warning)). The tool exits with code 1 if any **errors** are found, and 0 if there are only warnings or no issues.

```bash
cargo workspace-inheritance-check
```

### Options

| Flag <br/>_________________________ | Description | Default |
|-------------------|-------------|---------|
| `--path <PATH>` | Path to the workspace root | `.` |
| `--promotion-threshold <N>` | Minimum number of crates a dependency must appear in before it is flagged as a promotion candidate. | `2` |
| `--promotion-failure` | Report promotion candidates as errors, so the tool exits with code 1 when it finds one. | `false` |
| `--format <FORMAT>` | Output format: `human` or `json` | `human` |
| `--no-fail` | Exit with code 0 even when errors are found. | `false` |
| `--fix` | Fix the reported problems, then check again and report the problems that remain. For not-inherited and version-mismatch errors, rewrites the entry to `{ workspace = true }`. For promotion candidates, adds the dependency to `[workspace.dependencies]` and updates the member crates. See [What `--fix` changes](#what---fix-changes). | `false` |

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

# Fix the reported problems, then report the ones that remain
cargo workspace-inheritance-check --fix
```

## What `--fix` Changes

`--fix` rewrites a member entry only when cargo then resolves the same dependency, with two exceptions. On a version mismatch, the member moves to the workspace version. The member also gets the `features` of the workspace entry, because cargo adds them to an inherited entry.

- The member entry keeps `features`, `optional`, and `public`. The tool removes the keys that the workspace entry owns, such as `version`, `registry`, `path`, `git`, and `package`.
- When the workspace entry sets `default-features = false` and the member uses default features, the tool writes `default-features = true` on the member.
- When the member sets `default-features = false` and the workspace entry enables default features, the tool skips the entry. Cargo ignores the member value in edition 2021, and cargo before 1.99 rejects it in edition 2024.
- When the workspace entry uses the `default_features` spelling and the member uses edition 2024 or later, the tool skips the entry, because cargo rejects that spelling there.
- A promoted entry gets `default-features = false` when any member sets it. The other members then get `default-features = true`.
- When the member gives a version and the workspace entry gives none, the tool skips the entry, because inheriting would drop the version requirement.
- The tool skips a promotion when the members use different versions, or when `[workspace.dependencies]` already has the key.

Each skip prints a `skipped:` line with the reason.

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | No errors found (warnings are OK unless `--promotion-failure` is set) |
| `1` | Errors found (or promotion warnings promoted to errors via `--promotion-failure`), or the tool could not read the workspace or write a fix |
| `2` | Invalid command-line arguments |

With `--fix`, the exit code reflects the errors that remain after the fixes. `--no-fail` makes the exit code `0` when errors are found. It does not change the exit code when the tool itself fails.

## Ignore Rules

To skip a dependency, add an ignore rule to the workspace root `Cargo.toml`:

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

`dependency` matches the dependency key or the package name. `member` is a path from the workspace root, and can use `..` for a crate outside the root, such as `member = "../shared"`. An absolute path works too. It is a directory, which covers the crates below it, or a manifest path. So `member = "."` covers every crate, and `member = "Cargo.toml"` covers only the root package. The `[[workspace.metadata.inheritance-check.ignore]]` array-of-tables form works too. An invalid rule, such as one without a string `dependency`, with an unknown key, or with an empty `member`, is an error.

## JSON Output

`--format json` prints the report as JSON:

```json
{
  "diagnostics": [
    {
      "severity": "error",
      "dependency": "serde",
      "check": "not-inherited",
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

`check` is `not-inherited`, `version-mismatch`, or `promotion-candidate`.

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

The action builds the tool from its own ref, so `@v1.4.0` runs 1.4.0, and `@v1` runs the newest 1.x release. It uses the job's Rust toolchain, which must be 1.88 or later. Tags before `v1.4.0` install the newest release from crates.io instead.

All inputs are optional:

| Input | Description | Default |
|-------|-------------|---------|
| `path` | Path to the workspace root | `.` |
| `promotion-threshold` | Min crate count for promotion warning | `2` |
| `promotion-failure` | Treat promotion candidates as errors | `false` |
| `format` | Output format: `human` or `json` | `human` |
| `no-fail` | Exit 0 even when errors are found | `false` |
| `fix` | Automatically fix reported problems | `false` |

### Without the action

```yaml
- run: cargo install cargo-workspace-inheritance-check
- run: cargo workspace-inheritance-check
```

## Alternate Registry Support

For dependencies from [alternate registries](https://doc.rust-lang.org/cargo/reference/registries.html#using-an-alternate-registry):

- Promotion candidates are grouped by name **and** registry, so dependencies from different registries are never grouped together.
- The promotion hint names the registry, and `--fix` writes that entry to `[workspace.dependencies]`:

```
warning: `my-crate` appears in 2 crates but is not in [workspace.dependencies]
  --> crates/one/Cargo.toml
  --> crates/two/Cargo.toml
  hint: consider adding `my-crate = { version = "1.0", registry = "my-registry" }` to [workspace.dependencies]
```

## Dependency Sections Checked

All dependency sections in member crates are checked:

- `[dependencies]`
- `[dev-dependencies]`
- `[build-dependencies]`
- `[target.'...'.dependencies]` (and dev/build variants)
- `[dev_dependencies]` and `[build_dependencies]`, the spellings that cargo accepts before edition 2024.
