//! Reads a Cargo workspace: its members, `[workspace.dependencies]`, the ignore rules, and the
//! dependency entries that each member declares.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Component, MAIN_SEPARATOR, Path, PathBuf};
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
/// `dependency` matches the dependency key or the package name. `member` is a path from the
/// workspace root, and can use `..` for a member outside it. An absolute path works too. It is a
/// directory (e.g. `crates/bar`), which covers the crates below it, or a manifest path (e.g.
/// `Cargo.toml` for the root package). `.` and `None` cover every member, also one outside the
/// root.
struct IgnoreRule {
    dependency: String,
    member: Option<String>,
}

impl IgnoreRule {
    fn matches(&self, dep: &MemberDep, root: &Path) -> bool {
        if self.dependency != dep.key && self.dependency != dep.spec.package {
            return false;
        }
        let Some(member) = &self.member else {
            return true;
        };
        let scope = normalize(&root.join(member.replace('\\', "/")));
        scope == root || dep.manifest.starts_with(scope)
    }
}

/// A dependency entry that a member declares instead of inheriting it.
#[derive(Debug, Clone)]
pub struct MemberDep {
    pub manifest: PathBuf,
    /// The manifest path relative to the workspace root, such as `../outside/Cargo.toml` for a
    /// member outside it, with `/` separators on every OS. A member on another Windows drive
    /// keeps its absolute path.
    pub member: String,
    /// The member's edition, such as `2021`, as cargo reports it. Editions are years, so they
    /// compare in order as text.
    pub edition: String,
    pub table: DepTable,
    pub key: String,
    pub spec: DepSpec,
    /// The entry as the manifest writes it.
    pub(crate) entry: Item,
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
    /// `true` when the entry has a `default_features` key. Cargo rejects that spelling in a
    /// package on edition 2024 or later, also on an entry that the package inherits.
    pub underscore_default_features: bool,
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
/// cargo from the `CARGO` environment variable that cargo sets for subcommands when it is not
/// empty, or else the `cargo` on `PATH`.
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
    for package in metadata.packages {
        member_deps.extend(parse_member(&metadata.workspace_root, package)?);
    }
    let root = &metadata.workspace_root;
    member_deps.retain(|dep| !ignore_rules.iter().any(|rule| rule.matches(dep, root)));

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
    edition: String,
}

fn cargo_metadata(root: &Path) -> Result<Metadata, String> {
    let mut cargo = std::env::var_os("CARGO")
        .filter(|cargo| !cargo.is_empty())
        .map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    // Cargo runs in the root, so a relative path must be resolved from here first. A bare name
    // stays bare, for the lookup on PATH.
    if cargo.components().count() > 1 {
        cargo = std::path::absolute(&cargo)
            .map_err(|e| format!("Failed to resolve CARGO `{}`: {e}", cargo.display()))?;
    }
    // Run from the root so cargo reads the workspace's own `.cargo/config.toml`.
    let output = Command::new(&cargo)
        .args(["metadata", "--no-deps", "--offline", "--color", "never"])
        .args(["--format-version", "1", "--manifest-path", "Cargo.toml"])
        .current_dir(root)
        .output()
        .map_err(|e| {
            let cargo = cargo.display();
            let mut message = format!("Failed to run cargo metadata with `{cargo}`: {e}");
            if e.kind() == ErrorKind::NotFound {
                message.push_str(
                    ". The tool needs cargo: set CARGO to its path, or unset CARGO and put cargo \
                     on PATH",
                );
            }
            message
        })?;
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
         `{ dependency = \"...\", member = \"...\" }` entries, where `member` is optional and \
         not empty"
            .to_string()
    })
}

fn rule_from_table_like(table: &dyn TableLike) -> Option<IgnoreRule> {
    // An unknown key, a value that is not a string, or an empty `member` makes the rule invalid.
    // So a mistake, such as a misspelled `member`, never widens the rule to every crate.
    if !table
        .iter()
        .all(|(key, value)| matches!(key, "dependency" | "member") && value.is_str())
    {
        return None;
    }
    let member = str_field(table, "member");
    if member.as_deref() == Some("") {
        return None;
    }
    Some(IgnoreRule {
        dependency: str_field(table, "dependency")?,
        member,
    })
}

fn parse_member(root: &Path, package: MetadataPackage) -> Result<Vec<MemberDep>, String> {
    let manifest = package.manifest_path;
    let doc = read_manifest(&manifest)?;
    let manifest_dir = manifest
        .parent()
        .expect("a manifest path ends in Cargo.toml");
    let member = relative_to(&manifest, root)
        .to_string_lossy()
        .replace(MAIN_SEPARATOR, "/");
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
                edition: package.edition.clone(),
                table: table.clone(),
                key: key.to_string(),
                spec,
                entry: item.clone(),
            });
        }
    }
    Ok(deps)
}

/// `path` from `base`, with a `..` for each part of `base` that is not above `path`. Both must be
/// normalized, as cargo reports them. `path` stays absolute when the two share no prefix, such as
/// another drive on Windows.
fn relative_to(path: &Path, base: &Path) -> PathBuf {
    let shared = path
        .components()
        .zip(base.components())
        .take_while(|(a, b)| a == b)
        .count();
    if shared == 0 {
        return path.to_path_buf();
    }
    let up = base.components().count() - shared;
    std::iter::repeat_n(Component::ParentDir, up)
        .chain(path.components().skip(shared))
        .collect()
}

/// Resolve `.` and `..` in `path` without reading the file system, as cargo does for the paths
/// it reports.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            part => normalized.push(part),
        }
    }
    normalized
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
            underscore_default_features: false,
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
        underscore_default_features: t.contains_key("default_features"),
    })
}

fn str_field(table: &dyn TableLike, key: &str) -> Option<String> {
    table.get(key)?.as_str().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::fixture;

    #[test]
    fn test_no_workspace_section() {
        let err = parse_workspace(&fixture("no_workspace")).unwrap_err();
        assert!(err.starts_with("No [workspace] section in "), "{err}");
    }

    #[test]
    fn test_invalid_ignore_rule_is_an_error() {
        for name in [
            "ignore_member_not_a_string",
            "ignore_member_empty",
            "ignore_unknown_key",
        ] {
            let err = parse_workspace(&fixture(name)).unwrap_err();
            assert!(err.contains("Invalid `ignore`"), "{name}: {err}");
        }
    }

    #[test]
    fn test_members_outside_the_root_are_named_and_ignored_from_the_root() {
        let ws = parse_workspace(&fixture("outside_member/a/ws")).unwrap();
        let deps: Vec<_> = ws
            .member_deps
            .iter()
            .map(|d| (d.key.as_str(), d.member.as_str()))
            .collect();
        assert_eq!(
            deps,
            [
                ("serde", "../outside/Cargo.toml"),
                ("serde", "crates/a/Cargo.toml"),
                ("log", "../../far/Cargo.toml"),
                ("serde", "../../far/Cargo.toml"),
            ]
        );
    }
}
