//! Reads a Cargo workspace: its members, `[workspace.dependencies]`, the ignore rules, and the
//! dependency entries that each member declares.

use std::collections::BTreeMap;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use toml_edit::{DocumentMut, Item, TableLike, Value};

/// A parsed workspace.
#[derive(Debug)]
pub struct WorkspaceInfo {
    /// The workspace root directory, as cargo reports it.
    pub root_path: PathBuf,
    /// The entries of `[workspace.dependencies]`, by key.
    pub workspace_deps: BTreeMap<String, DepSpec>,
    /// The member entries without `workspace = true` that no ignore rule covers, grouped by member
    /// in manifest path order. Path and git entries that give no version are left out.
    pub member_deps: Vec<MemberDep>,
}

/// Exemption declared in `[workspace.metadata.inheritance-check]`.
///
/// `dependency` matches the dependency key or the package name. `member` is relative to the
/// workspace root. It is a crate directory (e.g. `crates/bar`), which also covers the crates
/// below it, or a manifest path (e.g. `Cargo.toml` for the root package). `.` and `None` cover
/// every member.
struct IgnoreRule {
    dependency: String,
    member: Option<String>,
}

impl IgnoreRule {
    fn matches(&self, dep: &MemberDep) -> bool {
        if self.dependency != dep.key && self.dependency != dep.spec.package {
            return false;
        }
        let scope = self.member.as_deref().unwrap_or(".").replace('\\', "/");
        let scope = scope.trim_start_matches("./").trim_end_matches('/');
        if scope.is_empty() || scope == "." {
            return true;
        }
        dep.member
            .strip_prefix(scope)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

/// A dependency entry that a member declares instead of inheriting it.
#[derive(Debug, Clone)]
pub struct MemberDep {
    pub manifest: PathBuf,
    /// The manifest path, relative to the workspace root when the member is inside it, with
    /// `/` separators on every OS.
    pub member: String,
    pub table: DepTable,
    pub key: String,
    pub spec: DepSpec,
}

/// A dependency table in a manifest, such as `[dependencies]` or
/// `[target.'cfg(windows)'.dev-dependencies]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepTable {
    /// The key under `[target]`, such as `cfg(windows)`, or `None` for a top-level table.
    pub target: Option<String>,
    /// The table name, such as `dev-dependencies`.
    pub kind: &'static str,
}

/// What a dependency entry declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepSpec {
    /// The `package` value, or the entry's key when it has none.
    pub package: String,
    pub source: DepSource,
    /// The version requirement, or `None` when the entry gives none.
    pub version: Option<String>,
    /// The `default-features` value, which is `true` when the entry does not set it.
    pub default_features: bool,
}

/// Where a dependency comes from.
///
/// The `registry` of a path or git dependency is the registry that `cargo publish` writes for
/// it, so two entries that differ only there are different dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepSource {
    /// A dependency from a registry.
    Registry(Registry),
    /// A local path, resolved against the directory of the manifest that declares it.
    Path { path: PathBuf, registry: Registry },
    /// A git repository and the `branch`, `tag`, or `rev` that the entry sets. The URL has no
    /// trailing `/` or `.git`, because cargo ignores both when it compares sources.
    Git {
        url: String,
        branch: Option<String>,
        tag: Option<String>,
        rev: Option<String>,
        registry: Registry,
    },
}

/// The registry that an entry names with `registry` or `registry-index`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registry {
    /// crates.io, when the entry names no registry or names `crates-io`.
    CratesIo,
    /// An alternate registry by name, from `registry`.
    Named(String),
    /// A registry by index URL, from `registry-index`.
    Index(String),
}

/// Dependency table names. Cargo accepts the underscore spellings before edition 2024.
const DEP_KINDS: [&str; 5] = [
    "dependencies",
    "dev-dependencies",
    "build-dependencies",
    "dev_dependencies",
    "build_dependencies",
];

pub(crate) fn read_manifest(path: &Path) -> Result<DocumentMut, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    content
        .parse()
        .map_err(|e| format!("Failed to parse {}: {e}", path.display()))
}

pub(crate) fn write_manifest(path: &Path, doc: &DocumentMut) -> Result<(), String> {
    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

/// Every dependency table in a document, top-level and per target.
fn dep_tables(doc: &DocumentMut) -> impl Iterator<Item = (DepTable, &dyn TableLike)> {
    let targets = doc
        .get("target")
        .and_then(Item::as_table_like)
        .into_iter()
        .flat_map(|targets| targets.iter())
        .filter_map(|(target, item)| Some((Some(target), item.as_table_like()?)));
    std::iter::once((None, doc.as_table() as &dyn TableLike))
        .chain(targets)
        .flat_map(|(target, scope)| {
            DEP_KINDS.into_iter().filter_map(move |kind| {
                // Cargo ignores an underscore table when its hyphen spelling is in the same scope.
                if kind.contains('_') && scope.contains_key(&kind.replace('_', "-")) {
                    return None;
                }
                let table = scope.get(kind)?.as_table_like()?;
                let target = target.map(String::from);
                Some((DepTable { target, kind }, table))
            })
        })
}

pub(crate) fn dep_table_mut<'a>(
    doc: &'a mut DocumentMut,
    table: &DepTable,
) -> Option<&'a mut dyn TableLike> {
    let scope: &mut dyn TableLike = match &table.target {
        None => doc.as_table_mut(),
        Some(target) => doc
            .get_mut("target")?
            .as_table_like_mut()?
            .get_mut(target)?
            .as_table_like_mut()?,
    };
    scope.get_mut(table.kind)?.as_table_like_mut()
}

/// Parse the workspace whose root manifest is `path/Cargo.toml`.
///
/// Cargo decides the member set: this runs `cargo metadata --no-deps` in `path`, with the
/// cargo from the `CARGO` environment variable that cargo sets for subcommands, or else the
/// `cargo` on `PATH`.
///
/// # Errors
///
/// Returns an error when the root manifest has no `[workspace]` table, when `cargo metadata`
/// fails, when a manifest cannot be read or parsed, or when an ignore rule is invalid.
pub fn parse_workspace(path: &Path) -> Result<WorkspaceInfo, String> {
    let root_toml_path = path.join("Cargo.toml");
    let doc = read_manifest(&root_toml_path)?;

    let workspace_table = doc
        .get("workspace")
        .and_then(Item::as_table_like)
        .ok_or_else(|| format!("No [workspace] section in {}", root_toml_path.display()))?;

    let workspace_deps = workspace_table
        .get("dependencies")
        .and_then(Item::as_table_like)
        .into_iter()
        .flat_map(|deps| deps.iter())
        .filter_map(|(key, item)| Some((key.to_string(), parse_spec(key, item, path)?)))
        .collect();
    let ignore_rules = parse_ignore_rules(workspace_table)?;

    let mut metadata = cargo_metadata(path)?;
    metadata
        .packages
        .sort_by(|a, b| a.manifest_path.cmp(&b.manifest_path));
    let mut member_deps = Vec::new();
    for MetadataPackage { manifest_path } in metadata.packages {
        member_deps.extend(parse_member(&metadata.workspace_root, manifest_path)?);
    }
    member_deps.retain(|dep| !ignore_rules.iter().any(|rule| rule.matches(dep)));

    Ok(WorkspaceInfo {
        root_path: metadata.workspace_root,
        workspace_deps,
        member_deps,
    })
}

/// With `--no-deps`, `packages` holds exactly the workspace members.
#[derive(Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    packages: Vec<MetadataPackage>,
}

#[derive(Deserialize)]
struct MetadataPackage {
    manifest_path: PathBuf,
}

fn cargo_metadata(root: &Path) -> Result<Metadata, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    // Run from the root so cargo reads the workspace's own `.cargo/config.toml`.
    let output = Command::new(cargo)
        .args(["metadata", "--no-deps", "--offline", "--color", "never"])
        .args(["--format-version", "1", "--manifest-path", "Cargo.toml"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("Failed to run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed for {}:\n{}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim_end(),
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to read cargo metadata output: {e}"))
}

fn parse_ignore_rules(workspace: &dyn TableLike) -> Result<Vec<IgnoreRule>, String> {
    let Some(ignore) = workspace
        .get("metadata")
        .and_then(|metadata| metadata.get("inheritance-check"))
        .and_then(|config| config.get("ignore"))
    else {
        return Ok(Vec::new());
    };
    let rules = match ignore {
        Item::Value(Value::Array(array)) => array
            .iter()
            .map(|v| rule_from_table_like(v.as_inline_table()?))
            .collect(),
        Item::ArrayOfTables(tables) => tables.iter().map(|t| rule_from_table_like(t)).collect(),
        _ => None,
    };
    rules.ok_or_else(|| {
        "Invalid `ignore` in [workspace.metadata.inheritance-check]: expected an array of \
         `{ dependency = \"...\", member = \"...\" }` entries, where `member` is optional"
            .to_string()
    })
}

fn rule_from_table_like(table: &dyn TableLike) -> Option<IgnoreRule> {
    // An unknown key, such as a misspelled `member`, or a value that is not a string makes the
    // rule invalid, so a mistake never widens it to every crate.
    if !table
        .iter()
        .all(|(key, value)| matches!(key, "dependency" | "member") && value.is_str())
    {
        return None;
    }
    Some(IgnoreRule {
        dependency: str_field(table, "dependency")?,
        member: str_field(table, "member"),
    })
}

fn parse_member(root: &Path, manifest: PathBuf) -> Result<Vec<MemberDep>, String> {
    let doc = read_manifest(&manifest)?;
    let manifest_dir = manifest
        .parent()
        .expect("a manifest path ends in Cargo.toml");
    let relative = manifest.strip_prefix(root).unwrap_or(&manifest);
    let member = relative.to_string_lossy().replace(MAIN_SEPARATOR, "/");
    let mut deps = Vec::new();
    for (table, entries) in dep_tables(&doc) {
        for (key, item) in entries.iter() {
            let Some(spec) = parse_spec(key, item, manifest_dir) else {
                continue;
            };
            // A path or git dependency without a version has no version to compare.
            if spec.version.is_none() && !matches!(spec.source, DepSource::Registry(_)) {
                continue;
            }
            deps.push(MemberDep {
                manifest: manifest.clone(),
                member: member.clone(),
                table: table.clone(),
                key: key.to_string(),
                spec,
            });
        }
    }
    Ok(deps)
}

/// Parse a dependency entry declared in the manifest in `manifest_dir`. Returns `None` for an
/// entry with `workspace = true`, and for a value that is neither a string nor a table.
fn parse_spec(key: &str, item: &Item, manifest_dir: &Path) -> Option<DepSpec> {
    if let Some(version) = item.as_str() {
        return Some(DepSpec {
            package: key.to_string(),
            source: DepSource::Registry(Registry::CratesIo),
            version: Some(version.to_string()),
            default_features: true,
        });
    }
    let t = item.as_table_like()?;
    if t.get("workspace").and_then(Item::as_bool) == Some(true) {
        return None;
    }
    let registry = match (str_field(t, "registry-index"), str_field(t, "registry")) {
        (Some(index), _) => Registry::Index(index),
        (None, Some(name)) if name != "crates-io" => Registry::Named(name),
        (None, _) => Registry::CratesIo,
    };
    let source = if let Some(path) = str_field(t, "path") {
        let path = manifest_dir.join(path);
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        DepSource::Path { path, registry }
    } else if let Some(url) = str_field(t, "git") {
        let url = url.strip_suffix('/').unwrap_or(&url);
        DepSource::Git {
            url: url.strip_suffix(".git").unwrap_or(url).to_string(),
            branch: str_field(t, "branch"),
            tag: str_field(t, "tag"),
            rev: str_field(t, "rev"),
            registry,
        }
    } else {
        DepSource::Registry(registry)
    };
    let default_features = t
        .get("default-features")
        .or_else(|| t.get("default_features"))
        .and_then(Item::as_bool);
    Some(DepSpec {
        package: str_field(t, "package").unwrap_or_else(|| key.to_string()),
        source,
        version: str_field(t, "version"),
        default_features: default_features.unwrap_or(true),
    })
}

fn str_field(table: &dyn TableLike, key: &str) -> Option<String> {
    table.get(key)?.as_str().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temp_workspace;

    #[test]
    fn test_no_workspace_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let err = parse_workspace(dir.path()).unwrap_err();
        assert!(err.starts_with("No [workspace] section in "), "{err}");
    }

    #[test]
    fn test_invalid_ignore_rule_is_an_error() {
        let rules = [
            "{ dependency = \"rand\", member = [\"crates/bar\"] }",
            "{ dependency = \"rand\", members = \"crates/bar\" }",
        ];
        for rule in rules {
            let tmp = temp_workspace(
                &format!("\n[workspace.metadata.inheritance-check]\nignore = [{rule}]"),
                &[("bar", "")],
            );
            let err = parse_workspace(tmp.path()).unwrap_err();
            assert!(err.contains("Invalid `ignore`"), "{rule}: {err}");
        }
    }
}
