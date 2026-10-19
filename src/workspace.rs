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
    /// in manifest path order. Path and git entries that give no version are left out, and so are
    /// the entries whose key `[workspace.dependencies]` has in a form that cargo cannot inherit,
    /// such as `{ workspace = true }`.
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
    /// A git repository and the `branch`, `tag`, or `rev` that the entry sets. The URL is in
    /// the form that cargo compares sources by.
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
    /// A registry by index URL, from `registry-index`, in the form that cargo compares.
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

/// A parsed manifest, and the line ending that its file uses.
pub(crate) struct Manifest {
    pub doc: DocumentMut,
    /// The first line of the file ends in `\r\n`.
    crlf: bool,
}

pub(crate) fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let doc = content
        .parse()
        .map_err(|e| format!("Failed to parse {}: {e}", path.display()))?;
    let crlf = content
        .find('\n')
        .is_some_and(|end| content[..end].ends_with('\r'));
    Ok(Manifest { doc, crlf })
}

/// Write `manifest` to `path` with the line ending that its file used.
pub(crate) fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut text = manifest.doc.to_string();
    if manifest.crlf {
        // toml_edit writes `\n`, but a multi-line string keeps the `\r\n` that it read.
        text = text.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    std::fs::write(path, text).map_err(|e| format!("Failed to write {}: {e}", path.display()))
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
    let doc = read_manifest(&root_toml_path)?.doc;

    let workspace_table = doc
        .get("workspace")
        .and_then(Item::as_table_like)
        .ok_or_else(|| format!("No [workspace] section in {}", root_toml_path.display()))?;

    let workspace_entries = workspace_table
        .get("dependencies")
        .and_then(Item::as_table_like);
    let workspace_deps: BTreeMap<String, DepSpec> = workspace_entries
        .into_iter()
        .flat_map(|deps| deps.iter())
        .filter_map(|(key, item)| Some((key.to_string(), parse_spec(key, item, path)?)))
        .collect();
    let unusable_key = |key: &str| {
        !workspace_deps.contains_key(key) && workspace_entries.is_some_and(|e| e.contains_key(key))
    };
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
    member_deps.retain(|dep| {
        !unusable_key(&dep.key) && !ignore_rules.iter().any(|rule| rule.matches(dep, root))
    });

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
    // Cargo runs in the root, so resolve a path from here first. A bare name, with no
    // separator, stays a lookup on PATH.
    if cargo.to_string_lossy().contains(std::path::is_separator) {
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let root = root.display();
        return Err(match stderr.trim_end() {
            "" => format!("cargo metadata failed for {root} with {}", output.status),
            stderr => format!("cargo metadata failed for {root}:\n{stderr}"),
        });
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
    let doc = read_manifest(&manifest)?.doc;
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
        (Some(index), _) => Registry::Index(canonical_url(&index)),
        (None, Some(name)) if name != "crates-io" => Registry::Named(name),
        (None, _) => Registry::CratesIo,
    };
    let source = if let Some(path) = str_field(t, "path") {
        let path = manifest_dir.join(path);
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        DepSource::Path { path, registry }
    } else if let Some(url) = str_field(t, "git") {
        DepSource::Git {
            url: canonical_url(&url),
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

/// `url` in the form that cargo compares sources by, so two URLs are the same source when their
/// forms are equal. As cargo's `CanonicalUrl` on a parsed URL, it lowercases the scheme, and the
/// host of an `http` or `https` URL, and drops their default port. It strips one trailing `/`.
/// Then, unless the scheme has a `+`, a `github.com` URL becomes `https` with a lowercase path,
/// and a final `.git` is stripped. A URL without `://` is left as it is.
fn canonical_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let mut scheme = scheme.to_ascii_lowercase();
    let (authority, rest) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let (path, suffix) = rest.split_at(rest.find(['?', '#']).unwrap_or(rest.len()));
    let (userinfo, mut host) = match authority.rsplit_once('@') {
        Some((userinfo, host)) => (Some(userinfo), host.to_string()),
        None => (None, authority.to_string()),
    };
    // Only an `http` or `https` URL has a host that the `url` crate parses, so only its case
    // and default port go.
    let special = |scheme: &str, host: &mut String| {
        let default_port = match scheme {
            "http" => ":80",
            "https" => ":443",
            _ => return,
        };
        host.make_ascii_lowercase();
        if let Some(name) = host.strip_suffix(default_port) {
            *host = name.to_string();
        }
    };
    special(&scheme, &mut host);
    let mut path = path.strip_suffix('/').unwrap_or(path).to_string();
    if !scheme.contains('+') {
        if host.split(':').next() == Some("github.com") {
            scheme = "https".to_string();
            special(&scheme, &mut host);
            path.make_ascii_lowercase();
        }
        if let Some(repo) = path.strip_suffix(".git") {
            path = repo.to_string();
        }
    }
    let userinfo = userinfo.map(|u| format!("{u}@")).unwrap_or_default();
    format!("{scheme}://{userinfo}{host}{path}{suffix}")
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
    fn test_urls_compare_as_cargo_compares_sources() {
        let same = [
            (
                "https://github.com/Foo/Bar",
                "https://github.com/foo/bar.git/",
            ),
            (
                "HTTPS://GitHub.com:443/Foo/Bar",
                "https://github.com/foo/bar",
            ),
            ("git://github.com/Foo/Bar", "https://github.com/foo/bar"),
            (
                "https://GitLab.com/Foo/Bar.git",
                "https://gitlab.com/Foo/Bar/",
            ),
            (
                "sparse+https://example.com/index/",
                "sparse+https://example.com/index",
            ),
        ];
        let different = [
            ("https://gitlab.com/Foo/Bar", "https://gitlab.com/foo/bar"),
            ("git://GitHub.com/Foo/Bar", "https://github.com/foo/bar"),
            (
                "git+https://github.com/Foo/Bar",
                "https://github.com/foo/bar",
            ),
            (
                "https://gitlab.com/foo/bar.GIT",
                "https://gitlab.com/foo/bar",
            ),
            ("http://example.com:8080/x", "http://example.com/x"),
        ];
        for (a, b) in same {
            assert_eq!(canonical_url(a), canonical_url(b), "{a} and {b}");
        }
        for (a, b) in different {
            assert_ne!(canonical_url(a), canonical_url(b), "{a} and {b}");
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
