//! Reads a Cargo workspace: its members, `[workspace.dependencies]`, the ignore rules, and the
//! dependency entries that each member declares.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Component, MAIN_SEPARATOR, Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use toml_edit::{Decor, DocumentMut, InlineTable, Item, RawString, TableLike, Value};

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
    /// `true` when a promotion would respace an inline table of the root manifest that has a
    /// comment between its braces, and so lose the comment.
    pub(crate) root_inline_comment: bool,
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
    /// Every line of the file ends in `\r\n`.
    crlf: bool,
}

pub(crate) fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let doc = content
        .parse()
        .map_err(|e| format!("Failed to parse {}: {e}", path.display()))?;
    let crlf = content.contains("\r\n") && !content.replace("\r\n", "").contains('\n');
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

/// Parse the workspace whose root is `path`, or the workspace that `path` is in.
///
/// When `path/Cargo.toml` has no `[workspace]` table, `cargo locate-project --workspace` finds
/// the root above `path`, as for cargo's own commands. Cargo decides the member set: this runs
/// `cargo metadata --no-deps` in the root. The cargo is the one in the `CARGO` environment
/// variable that cargo sets for subcommands, when it is not empty, or else `cargo` on `PATH`.
///
/// # Errors
///
/// Returns an error when no workspace root is found, or when cargo fails. Also returns an error
/// when a manifest cannot be read or parsed, or when an ignore rule is invalid.
pub fn parse_workspace(path: &Path) -> Result<WorkspaceInfo, String> {
    let (root_dir, doc) = workspace_manifest(path)?;
    let root_toml_path = root_dir.join("Cargo.toml");

    let workspace_table = doc
        .get("workspace")
        .and_then(Item::as_table_like)
        .ok_or_else(|| format!("No [workspace] section in {}", root_toml_path.display()))?;

    let workspace_deps = workspace_table
        .get("dependencies")
        .and_then(Item::as_table_like)
        .into_iter()
        .flat_map(|deps| deps.iter())
        .filter_map(|(key, item)| Some((key.to_string(), parse_spec(key, item, &root_dir)?)))
        .collect();
    let ignore_rules = parse_ignore_rules(workspace_table)?;

    let mut metadata = cargo_metadata(path, &root_dir)?;
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
        root_inline_comment: root_inline_comment(&doc),
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

/// The root of the workspace that `path` is, or is in, and its manifest.
fn workspace_manifest(path: &Path) -> Result<(PathBuf, DocumentMut), String> {
    let manifest = path.join("Cargo.toml");
    let read = match read_manifest(&manifest) {
        Ok(read) if read.doc.contains_key("workspace") => return Ok((path.into(), read.doc)),
        read => read.err(),
    };
    let args = ["--workspace", "--message-format", "plain"].map(OsStr::new);
    let stdout = run_cargo(path, "locate-project", &args).map_err(|e| {
        read.unwrap_or_else(|| {
            let manifest = manifest.display();
            format!("No [workspace] section in {manifest}, and cargo finds no workspace: {e}")
        })
    })?;
    let located = String::from_utf8(stdout)
        .map_err(|_| "cargo locate-project gave a path that is not UTF-8".to_string())?;
    let root = Path::new(located.trim_end())
        .parent()
        .expect("a manifest path ends in Cargo.toml");
    Ok((root.into(), read_manifest(&root.join("Cargo.toml"))?.doc))
}

/// The members of the workspace whose root is `root`. Cargo runs in `dir`, as for cargo's own
/// commands, so it reads each `.cargo/config.toml` from `dir` up.
fn cargo_metadata(dir: &Path, root: &Path) -> Result<Metadata, String> {
    let manifest = std::path::absolute(root.join("Cargo.toml"))
        .map_err(|e| format!("Failed to resolve {}: {e}", root.display()))?;
    let args = [
        "--no-deps",
        "--offline",
        "--format-version",
        "1",
        "--manifest-path",
    ];
    let args = [&args.map(OsStr::new)[..], &[manifest.as_os_str()]].concat();
    serde_json::from_slice(&run_cargo(dir, "metadata", &args)?)
        .map_err(|e| format!("Failed to read cargo metadata output: {e}"))
}

/// Run `cargo <subcommand> <args>` in `dir`, and return what it prints on stdout.
fn run_cargo(dir: &Path, subcommand: &str, args: &[&OsStr]) -> Result<Vec<u8>, String> {
    let mut cargo = std::env::var_os("CARGO")
        .filter(|cargo| !cargo.is_empty())
        .map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    // Cargo runs in `dir`, so resolve a relative path from here first. A bare name, with no
    // separator, stays a lookup on PATH.
    if cargo.to_string_lossy().contains(std::path::is_separator) {
        cargo = std::path::absolute(&cargo)
            .map_err(|e| format!("Failed to resolve CARGO `{}`: {e}", cargo.display()))?;
    }
    let output = Command::new(&cargo)
        .args(["--color", "never", subcommand])
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| {
            let cargo = cargo.display();
            let mut message = format!("Failed to run cargo {subcommand} with `{cargo}`: {e}");
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
        let dir = dir.display();
        return Err(match stderr.trim_end() {
            "" => format!("cargo {subcommand} failed for {dir} with {}", output.status),
            stderr => format!("cargo {subcommand} failed for {dir}:\n{stderr}"),
        });
    }
    Ok(output.stdout)
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
            if item.get("workspace").and_then(Item::as_bool) == Some(true) {
                continue;
            }
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

/// Parse a dependency entry declared in the manifest in `manifest_dir`. Returns `None` for a
/// value that is neither a string nor a table. A `workspace` key is ignored, as cargo ignores it
/// in `[workspace.dependencies]`.
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
/// forms are equal. A plain URL gets cargo's rules, as [`plain_canonical_url`] lists them. Any
/// other URL stays as it is. Equal text is always the same source, so such a URL can only miss a
/// match, never make a false one.
fn canonical_url(url: &str) -> String {
    plain_canonical_url(url).unwrap_or_else(|| url.to_string())
}

/// Cargo's form of a plain URL, from its `CanonicalUrl` on a URL that the `url` crate parsed.
/// `None` for a URL that is not plain.
///
/// A plain URL is `scheme://[user@]host[:port][/path]`, with ASCII letters, digits, and `.`, `_`,
/// `~`, `-` (and `%` in the path) only. It has no `file` scheme, no port with a leading zero, no
/// empty segment but a trailing `/`, and no `.` or `..` segment, also not as `%2e` or before a
/// `.git` that goes. Other URLs depend on
/// more of the `url` crate's parsing rules. Cargo's form of a plain URL is plain too, so it never
/// equals another URL as written.
///
/// The scheme is lowercase. A special scheme (`http`, `https`, `ws`, `wss`, or `ftp`) has a
/// lowercase host and drops its default port. One trailing `/` goes, unless the path is only `/`.
/// Then, unless the scheme has a `+`, a `github.com` URL becomes `https` with a lowercase path,
/// and a final `.git` goes. Cargo writes the shortened last segment back, so its `%` becomes
/// `%25`.
fn plain_canonical_url(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let mut scheme = scheme.to_ascii_lowercase();
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let (user, host_port) = match authority.split_once('@') {
        Some((user, host_port)) => (Some(user), host_port),
        None => (None, authority),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (host_port, None),
    };
    let plain = |text: &str, extra: &str| {
        !text.is_empty()
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    };
    let plain_segment = |segment: &str| {
        plain(segment, "._~-%")
            && !matches!(segment, "." | ".." | "..git" | "...git")
            && !segment.to_ascii_lowercase().contains("%2e")
    };
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    // Only the last segment can be empty, as in one trailing `/`.
    let plain_path = segments.iter().enumerate().all(|(i, segment)| {
        (segment.is_empty() && i + 1 == segments.len()) || plain_segment(segment)
    });
    let is_plain = scheme.starts_with(|c: char| c.is_ascii_lowercase())
        && plain(&scheme, "+.-")
        && scheme != "file"
        && user.is_none_or(|user| plain(user, "._~-"))
        && plain(host, ".-")
        && port.is_none_or(|port| !port.starts_with('0') && port.parse::<u16>().is_ok())
        && plain_path;
    if !is_plain {
        return None;
    }
    let mut host = host.to_string();
    let mut port = port;
    let mut path = path.to_string();
    let parse = |scheme: &str, host: &mut String, port: &mut Option<&str>, path: &mut String| {
        let default_port = match scheme {
            "http" | "ws" => "80",
            "https" | "wss" => "443",
            "ftp" => "21",
            _ => return,
        };
        host.make_ascii_lowercase();
        if *port == Some(default_port) {
            *port = None;
        }
        if path.is_empty() {
            path.push('/');
        }
    };
    parse(&scheme, &mut host, &mut port, &mut path);
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    if !scheme.contains('+') {
        if host == "github.com" {
            scheme = "https".to_string();
            parse(&scheme, &mut host, &mut port, &mut path);
            path.make_ascii_lowercase();
        }
        if let Some(repo) = path.strip_suffix(".git") {
            let (dir, last) = repo.rsplit_once('/').unwrap_or(("", repo));
            path = format!("{dir}/{}", last.replace('%', "%25"));
        }
    }
    let user = user.map(|user| format!("{user}@")).unwrap_or_default();
    let port = port.map(|port| format!(":{port}")).unwrap_or_default();
    Some(format!("{scheme}://{user}{host}{port}{path}"))
}

/// `true` when the `[workspace]` table of `doc`, or its `dependencies`, is an inline table with a
/// comment between its braces.
fn root_inline_comment(doc: &DocumentMut) -> bool {
    let workspace = doc.get("workspace");
    let dependencies = workspace.and_then(|workspace| workspace.get("dependencies"));
    workspace.is_some_and(has_inline_comment) || dependencies.is_some_and(has_inline_comment)
}

/// `true` when `entry` is an inline table with a comment between its braces, also in an inline
/// table inside it. Only TOML 1.1 allows such a comment, and a rewrite to one line loses it.
pub(crate) fn has_inline_comment(entry: &Item) -> bool {
    entry.as_inline_table().is_some_and(inline_table_comment)
}

fn inline_table_comment(table: &InlineTable) -> bool {
    let comment = |raw: Option<&RawString>| {
        raw.and_then(RawString::as_str)
            .is_some_and(|text| text.contains('#'))
    };
    let in_decor = |decor: &Decor| comment(decor.prefix()) || comment(decor.suffix());
    comment(Some(table.trailing()))
        || table.iter().any(|(name, value)| {
            let key = table.key(name).expect("an entry has a key");
            in_decor(key.leaf_decor())
                || in_decor(key.dotted_decor())
                || in_decor(value.decor())
                || value.as_inline_table().is_some_and(inline_table_comment)
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
    fn test_cargo_reads_the_config_of_the_path_inside_a_workspace() {
        let ws = parse_workspace(&fixture("member_config/crates/a")).unwrap();
        let deps: Vec<_> = ws.member_deps.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(deps, ["foo"]);
    }

    #[test]
    fn test_only_a_comment_between_inline_braces_blocks_the_rewrite() {
        let manifest = read_manifest(&fixture("inline_comments").join("Cargo.toml")).unwrap();
        let entries = manifest.doc["dependencies"].as_table_like().unwrap();
        let found: Vec<_> = entries
            .iter()
            .map(|(key, entry)| (key, has_inline_comment(entry)))
            .collect();
        assert_eq!(
            found,
            [
                ("before_key", true),
                ("after_comma", true),
                ("before_brace", true),
                ("dotted", true),
                ("nested", true),
                ("multi_line", false),
                ("in_array", false),
                ("after_entry", false),
            ]
        );
        assert!(root_inline_comment(&manifest.doc));
        let plain = read_manifest(&fixture("fix_inline_workspace_root_package").join("Cargo.toml"));
        assert!(!root_inline_comment(&plain.unwrap().doc));
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
            ("ws://Example.com:80/x", "ws://example.com/x"),
            ("ftp://github.com:21/Foo/Bar", "https://github.com/foo/bar"),
            ("https://example.com", "https://example.com/"),
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
            (
                "https://example.com/my%20repo",
                "https://example.com/my%20repo.git",
            ),
            ("sparse+https://example.com", "sparse+https://example.com/"),
            (
                "https://example.com\\Org\\Repo",
                "https://example.com/org/repo",
            ),
            (
                "https://example.com/repo?a/b",
                "https://example.com/repo?a\\b",
            ),
            ("file://C:/repo", "file://c:/repo"),
            ("http://github.com:080/repo", "ssh://github.com:080/repo"),
            (
                "https://example.com/org/...git",
                "https://example.com/org/..",
            ),
            ("https:///repo.git", "https:///repo"),
            ("https://github.com//repo.git", "https://github.com//repo"),
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
