//! Reads a Cargo workspace: its members, `[workspace.dependencies]`, the ignore rules, and the
//! dependency entries that each member declares.

use std::collections::BTreeMap;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use toml_edit::{DocumentMut, Item, TableLike};

/// A parsed workspace.
#[derive(Debug)]
pub struct WorkspaceInfo {
    /// The workspace root directory, as cargo reports it.
    pub root_path: PathBuf,
    /// The entries of `[workspace.dependencies]`, by key.
    pub workspace_deps: BTreeMap<String, DepSpec>,
    /// The members, sorted by manifest path.
    pub members: Vec<MemberCrate>,
    pub ignore_rules: Vec<IgnoreRule>,
}

/// Exemption declared in `[workspace.metadata.inheritance-check]`.
///
/// `dependency` matches the dependency key or the package name. `member` is the crate
/// directory relative to the workspace root (e.g. `crates/bar`); when `None`, the rule applies
/// to every member.
#[derive(Debug, Clone)]
pub struct IgnoreRule {
    pub dependency: String,
    pub member: Option<String>,
}

impl IgnoreRule {
    /// Whether the rule covers the entry `key` for `package` in the member whose manifest is
    /// at `member_manifest`, a path such as `crates/bar/Cargo.toml` from
    /// [`relative_manifest`].
    pub fn matches(&self, key: &str, package: &str, member_manifest: &str) -> bool {
        if self.dependency != key && self.dependency != package {
            return false;
        }
        let Some(scope) = &self.member else {
            return true;
        };
        let scope = scope.replace('\\', "/");
        let scope = scope.trim_start_matches("./").trim_end_matches('/');
        member_manifest
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// A member crate and the dependency entries it declares itself.
#[derive(Debug)]
pub struct MemberCrate {
    pub manifest_path: PathBuf,
    /// The entries that do not use `workspace = true`.
    pub dependencies: Vec<MemberDep>,
}

/// A dependency entry that a member declares instead of inheriting it.
#[derive(Debug)]
pub struct MemberDep {
    pub table: DepTable,
    /// The entry's key, which is also the key that cargo inherits by.
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
    pub version: Option<String>,
    /// The `default-features` value, which is `true` when the entry does not set it.
    pub default_features: bool,
}

/// Where a dependency comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepSource {
    /// A registry, where `None` is crates.io.
    Registry(Option<String>),
    /// A local path, resolved against the directory of the manifest that declares it.
    Path(PathBuf),
    Git {
        url: String,
        branch: Option<String>,
        tag: Option<String>,
        rev: Option<String>,
    },
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

/// `manifest` relative to `root`, with `/` separators, so that the output is the same on
/// every OS.
pub fn relative_manifest(root: &Path, manifest: &Path) -> String {
    let relative = manifest.strip_prefix(root).unwrap_or(manifest);
    relative.to_string_lossy().replace(MAIN_SEPARATOR, "/")
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
                let table = scope.get(kind)?.as_table_like()?;
                let target = target.map(String::from);
                Some((DepTable { target, kind }, table))
            })
        })
}

/// The dependency table `table` in `doc`, if it exists.
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

    let workspace_deps = parse_workspace_deps(workspace_table, path);
    let ignore_rules = parse_ignore_rules(workspace_table)?;

    let metadata = cargo_metadata(path)?;
    let mut manifest_paths: Vec<PathBuf> = metadata
        .packages
        .into_iter()
        .map(|p| p.manifest_path)
        .collect();
    manifest_paths.sort();
    let members = manifest_paths
        .into_iter()
        .map(parse_member)
        .collect::<Result<_, _>>()?;

    Ok(WorkspaceInfo {
        root_path: metadata.workspace_root,
        workspace_deps,
        members,
        ignore_rules,
    })
}

/// The parts of `cargo metadata --no-deps` output that the check reads. With `--no-deps`,
/// `packages` holds exactly the workspace members.
#[derive(Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    packages: Vec<MetadataPackage>,
}

#[derive(Deserialize)]
struct MetadataPackage {
    manifest_path: PathBuf,
}

/// Ask cargo for the member set, so it follows cargo's rules: the root package, path
/// dependencies inside the root, and `exclude`.
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
        .and_then(Item::as_table_like)
        .and_then(|t| t.get("inheritance-check"))
        .and_then(Item::as_table_like)
        .and_then(|t| t.get("ignore"))
    else {
        return Ok(Vec::new());
    };
    let invalid = || {
        "Invalid `ignore` in [workspace.metadata.inheritance-check]: expected an array of \
         `{ dependency = \"...\", member = \"...\" }` entries, where `member` is optional"
            .to_string()
    };

    // `ignore = [{ .. }]` holds inline tables; `[[...ignore]]` is an array of tables.
    let entries: Vec<&dyn TableLike> = if let Some(array) = ignore.as_array() {
        array
            .iter()
            .map(|v| v.as_inline_table().map(|t| t as &dyn TableLike))
            .collect::<Option<_>>()
            .ok_or_else(invalid)?
    } else {
        let tables = ignore.as_array_of_tables().ok_or_else(invalid)?;
        tables.iter().map(|t| t as &dyn TableLike).collect()
    };
    entries
        .into_iter()
        .map(|t| rule_from_table_like(t).ok_or_else(invalid))
        .collect()
}

fn rule_from_table_like(table: &dyn TableLike) -> Option<IgnoreRule> {
    let dependency = table.get("dependency")?.as_str()?.to_string();
    // A `member` that is not a string makes the rule invalid, so it never widens to every crate.
    let member = match table.get("member") {
        None => None,
        Some(member) => Some(member.as_str()?.to_string()),
    };
    Some(IgnoreRule { dependency, member })
}

fn parse_workspace_deps(workspace: &dyn TableLike, root: &Path) -> BTreeMap<String, DepSpec> {
    workspace
        .get("dependencies")
        .and_then(Item::as_table_like)
        .into_iter()
        .flat_map(|deps| deps.iter())
        .filter_map(|(key, item)| Some((key.to_string(), parse_spec(key, item, root)?)))
        .collect()
}

fn parse_member(manifest_path: PathBuf) -> Result<MemberCrate, String> {
    let doc = read_manifest(&manifest_path)?;
    let manifest_dir = manifest_path
        .parent()
        .expect("a manifest path ends in Cargo.toml");

    let dependencies = dep_tables(&doc)
        .flat_map(|(table, entries)| {
            entries.iter().filter_map(move |(key, item)| {
                let spec = parse_spec(key, item, manifest_dir)?;
                // A path or git dependency without a version has no version to compare.
                if spec.version.is_none() && !matches!(spec.source, DepSource::Registry(_)) {
                    return None;
                }
                let key = key.to_string();
                let table = table.clone();
                Some(MemberDep { table, key, spec })
            })
        })
        .collect();

    Ok(MemberCrate {
        manifest_path,
        dependencies,
    })
}

/// Parse a dependency entry declared in the manifest in `manifest_dir`. Returns `None` for an
/// entry with `workspace = true`, and for a value that is neither a string nor a table.
fn parse_spec(key: &str, item: &Item, manifest_dir: &Path) -> Option<DepSpec> {
    if let Some(version) = item.as_str() {
        return Some(DepSpec {
            package: key.to_string(),
            source: DepSource::Registry(None),
            version: Some(version.to_string()),
            default_features: true,
        });
    }
    let t = item.as_table_like()?;
    if t.get("workspace").and_then(Item::as_bool) == Some(true) {
        return None;
    }
    let source = if let Some(path) = str_field(t, "path") {
        let path = manifest_dir.join(path);
        DepSource::Path(std::fs::canonicalize(&path).unwrap_or(path))
    } else if let Some(url) = str_field(t, "git") {
        DepSource::Git {
            url,
            branch: str_field(t, "branch"),
            tag: str_field(t, "tag"),
            rev: str_field(t, "rev"),
        }
    } else {
        DepSource::Registry(str_field(t, "registry"))
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
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let result = parse_workspace(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_ignore_rules_array_of_tables() {
        let tmp = temp_workspace(
            "\n[[workspace.metadata.inheritance-check.ignore]]\n\
             dependency = \"rand\"\nmember = \"crates/bar\"",
            &[("bar", "")],
        );
        let ws = parse_workspace(tmp.path()).unwrap();
        let rules: Vec<_> = ws
            .ignore_rules
            .iter()
            .map(|r| (r.dependency.as_str(), r.member.as_deref()))
            .collect();
        assert_eq!(rules, [("rand", Some("crates/bar"))]);
    }

    #[test]
    fn test_invalid_ignore_rule_is_an_error() {
        let tmp = temp_workspace(
            "\n[workspace.metadata.inheritance-check]\n\
             ignore = [{ dependency = \"rand\", member = [\"crates/bar\"] }]",
            &[("bar", "")],
        );
        let err = parse_workspace(tmp.path()).unwrap_err();
        assert!(err.contains("Invalid `ignore`"), "{err}");
    }
}
