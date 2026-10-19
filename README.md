# Check Dependency Inheritance in Cargo Workspaces

A Cargo subcommand that detects and fixes workspace dependency inheritance issues in Cargo workspaces. It finds crates that specify dependency versions directly instead of using [`workspace = true`](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#inheriting-a-dependency-from-a-workspace), flags version mismatches, suggests candidates for workspace promotion, and can fix most of the reported problems.

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

The suggested entry is the entry that `--fix` writes. It keeps `features`, `optional`, and `public`, and it adds `default-features = true` when the member needs it. When the member cannot inherit the workspace entry as it is, the error gives the reason, not an entry:

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

A dependency appears in multiple crates but isn't declared in `[workspace.dependencies]`. This check helps you identify dependencies that should be centralized.

```
warning: `serde_yaml` appears in 3 crates but is not in [workspace.dependencies]
  --> crates/config/Cargo.toml
  --> crates/node/Cargo.toml
  --> crates/cli/Cargo.toml
  hint: consider adding `serde_yaml = "0.9"` to [workspace.dependencies]
```

The hint adds `default-features = false` when a crate sets it, as `--fix` does.

By default, this is reported as a **warning** and does not cause the tool to exit with a non-zero code. Use `--promotion-failure` to treat promotion candidates as errors.

### How entries are compared

- The members are the crates that cargo reports for the workspace, including the root package and path dependencies inside the workspace root. The tool runs `cargo metadata --no-deps` to get them, so the workspace must load in cargo. It runs the cargo in `CARGO`, which cargo sets when you run `cargo workspace-inheritance-check`, or else `cargo` on `PATH`. An empty `CARGO` counts as unset. The tool resolves a relative `CARGO` path from the current directory, and looks up a bare name, such as `cargo-1.88`, on `PATH`.
- Cargo inherits a dependency by its key, so a member entry is compared with the `[workspace.dependencies]` entry that has the same key. Both must name the same package (`package = "..."`) from the same source: the same registry, path, or git repository. A path or git entry must also name the same `registry` or `registry-index`, because `cargo publish` writes that registry for the dependency. A git or `registry-index` URL in plain form matches as cargo compares sources. A plain URL has only letters, digits, `.`, `_`, `~`, `-`, and `%`, with no query, no `file` scheme, and no `.` or empty path segment. It ignores the case of its scheme, and of an `http`, `https`, `ws`, `wss`, or `ftp` host. It also ignores a default port, one trailing `/`, and a final `.git`. A `github.com` URL also ignores the case of its path, and its scheme when the scheme has no `+`. Any other URL matches only the same text. `registry = "crates-io"` is crates.io. Otherwise the member cannot inherit that entry, and the tool reports nothing for it.
- A version requirement with only numbers and an optional `^`, such as `"1"` or `"^1.0"`, is compared by the versions it matches. So `"1"` and `"1.0"` are the same requirement, and `"0"` and `"0.0"` are not. Any other requirement is compared as written.
- A path or git dependency without a version is not checked.
- A promotion candidate is a dependency from crates.io or from a named `registry`, and it is not renamed. A `registry-index`, path, or git dependency is not a promotion candidate. Each crate counts once, even when it lists the dependency in several sections.

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

| Flag <br/>_________________________ | Description | Default |
|-------------------|-------------|---------|
| `--path <PATH>` | Path to the workspace root, or to a directory inside the workspace. From a directory inside it, the tool checks the whole workspace, and `--fix` fixes the whole workspace. | `.` |
| `--promotion-threshold <N>` | Minimum number of crates a dependency must appear in before it is flagged as a promotion candidate. For example, `--promotion-threshold 3` means a dependency must appear in at least 3 crates to trigger a warning. | `2` |
| `--promotion-failure` | Promote promotion candidate warnings to errors, causing the tool to exit with code 1 when candidates are found. Useful in CI when you want to enforce that all shared dependencies are declared in `[workspace.dependencies]`. | `false` |
| `--format <FORMAT>` | Output format: `human` or `json` | `human` |
| `--no-fail` | Exit with code 0 even when errors are found. Useful when you want to see the report without failing a CI pipeline. | `false` |
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
- When the member sets `default-features = false` and the workspace entry enables default features, the tool skips the entry. Cargo ignores the member value in edition 2021, and older cargo releases reject it in edition 2024.
- A promoted entry gets `default-features = false` when any member sets it. The other members then get `default-features = true`.
- When the member gives a version and the workspace entry gives none, the tool skips the entry, because inheriting would drop the version requirement.
- When the workspace entry has a `default_features` key and the member uses edition 2024 or later, the tool skips the entry. Cargo rejects that key in such a member, also in a workspace entry that the member inherits. To let the tool fix the entry, rename the key to `default-features`, or remove it when the entry also has `default-features`.
- When an inline-table entry has a comment between its braces, the tool skips the entry, or the promotion that would rewrite it. Only TOML 1.1 allows such a comment. The rewrite puts the entry on one line, so the comment would be lost. The tool also skips a promotion into an inline `[workspace]` or `dependencies` table with such a comment.
- In the `[dependencies.serde]` form, the comments of a removed key, such as `version`, go to the `workspace = true` line.
- The tool skips a promotion when the members use different versions, or when `[workspace.dependencies]` already has the key.

Each skip prints a `skipped:` line with the reason. After the fixes, the tool checks the workspace again and prints the report in the selected `--format`.

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | No errors found (warnings are OK unless `--promotion-failure` is set) |
| `1` | Errors found (or promotion warnings promoted to errors via `--promotion-failure`), or the tool could not read the workspace or write a fix |
| `2` | Invalid command-line arguments |

With `--fix`, the exit code reflects the errors that remain after the fixes. `--no-fail` makes the exit code `0` when errors are found. It does not change the exit code when the tool itself fails.

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

`dependency` matches the dependency key or the package name. `member` is a path from the workspace root, and can use `..` for a crate outside the root, such as `member = "../shared"`. An absolute path works too. It is a directory, which covers the crates below it, or a manifest path. So `member = "."` covers every crate, and `member = "Cargo.toml"` covers only the root package. The `[[workspace.metadata.inheritance-check.ignore]]` array-of-tables form works too. An invalid rule, such as one without a string `dependency`, with an unknown key, or with an empty `member`, is an error.

## JSON Output

Use `--format json` for machine-readable output, useful for integrating with other tools or custom CI scripts:

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
| `path` | Path to the workspace root, or to a directory inside it | `.` |
| `promotion-threshold` | Min crate count for promotion warning | `2` |
| `promotion-failure` | Treat promotion candidates as errors | `false` |
| `format` | Output format: `human` or `json` | `human` |
| `no-fail` | Exit 0 even when errors are found | `false` |
| `fix` | Automatically fix reported problems | `false` |
| `version` | Version to install, or `path` to build the crate in the action's own directory. With `path`, the binary matches the action ref. | `latest` |

### Without the action

```yaml
- run: cargo install cargo-workspace-inheritance-check
- run: cargo workspace-inheritance-check
```

## Alternate Registry Support

Dependencies that use [alternate registries](https://doc.rust-lang.org/cargo/reference/registries.html#using-an-alternate-registry) are handled correctly:

- A member dep like `serde = { version = "1.0", registry = "my-registry" }` will **not** match a workspace dep `serde = "1.0"` (default crates.io registry) — they are from different sources
- Promotion candidates are grouped by name **and** registry — deps from different registries are never grouped together
- `--fix` strips `registry` from member deps when converting to `{ workspace = true }` (the workspace dep owns the registry, just like it owns the version)
- `--fix` carries `registry` into promoted workspace deps:

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
- `[dev_dependencies]` and `[build_dependencies]`, the spellings that cargo accepts before edition 2024. Cargo ignores such a table when the hyphen spelling is also present, and so does the tool.
