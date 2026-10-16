//! Writes the fixes that [`crate::check::run_checks`] plans.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, InlineTable, Item, TableLike, Value};

use crate::check::Finding;
use crate::workspace::{DepTable, dep_table_mut, read_manifest, write_manifest};

/// Keys that cargo reads on an entry with `workspace = true`. It ignores any other key with an
/// "unused manifest key" warning, and `default-features` has its own rule in [`Edit`].
const INHERITABLE_KEYS: [&str; 3] = ["features", "optional", "public"];

/// The change that `--fix` makes for one finding.
#[derive(Debug)]
pub struct Fix {
    /// An entry to add to `[workspace.dependencies]` before the edits. When the root manifest
    /// already has that key, the whole fix is skipped.
    pub promotion: Option<Promotion>,
    /// Member entries to rewrite to `{ workspace = true }`.
    pub edits: Vec<Edit>,
    /// What the fix does, for the `fixed:` line.
    pub description: String,
}

/// An entry to add to `[workspace.dependencies]`.
#[derive(Debug)]
pub struct Promotion {
    pub root_manifest: PathBuf,
    pub key: String,
    pub version: String,
    pub registry: Option<String>,
    pub default_features: bool,
}

impl Promotion {
    /// The value written to `[workspace.dependencies]`.
    pub fn value(&self) -> Value {
        if self.default_features && self.registry.is_none() {
            return self.version.as_str().into();
        }
        let mut table = InlineTable::new();
        table.insert("version", self.version.as_str().into());
        if !self.default_features {
            table.insert("default-features", false.into());
        }
        if let Some(registry) = &self.registry {
            table.insert("registry", registry.as_str().into());
        }
        Value::InlineTable(table)
    }
}

/// Rewrites one member entry to `{ workspace = true }`.
#[derive(Debug)]
pub struct Edit {
    pub manifest: PathBuf,
    pub table: DepTable,
    pub key: String,
    /// Write `default-features = true`, because the entry uses default features and the
    /// workspace entry disables them. Otherwise any `default-features` key is removed.
    pub enable_default_features: bool,
}

/// What [`apply_fixes`] did.
#[derive(Debug)]
pub struct FixSummary {
    /// A `fixed:` line for each finding that was fixed.
    pub fixed: Vec<String>,
    /// A `skipped:` line with the reason for each finding that was left as it is.
    pub skipped: Vec<String>,
    pub files_modified: usize,
}

/// Apply the fix of every finding.
///
/// Promotions go into the root manifest first. A promotion whose key the root already has is
/// skipped, and its members keep their entries. Then each member manifest is read, edited, and
/// written once.
///
/// # Errors
///
/// Returns an error when a manifest cannot be read, parsed, or written, or when it no longer
/// has an entry that a fix names. Files written before the error keep their changes.
pub fn apply_fixes(findings: &[Finding]) -> Result<FixSummary, String> {
    let mut fixed = Vec::new();
    let mut skipped = Vec::new();
    let mut root: Option<(&Path, DocumentMut)> = None;
    let mut root_changed = false;
    let mut edits: BTreeMap<&Path, Vec<&Edit>> = BTreeMap::new();

    for finding in findings {
        let fix = match &finding.fix {
            Ok(fix) => fix,
            Err(reason) => {
                skipped.push(format!("skipped: {reason}"));
                continue;
            }
        };
        if let Some(promotion) = &fix.promotion {
            if root.is_none() {
                let path = promotion.root_manifest.as_path();
                root = Some((path, read_manifest(path)?));
            }
            let (_, doc) = root.as_mut().expect("the root manifest is read above");
            if !insert_workspace_dep(doc, promotion)? {
                skipped.push(format!(
                    "skipped: `{}` is not promoted, because [workspace.dependencies] already has it",
                    promotion.key,
                ));
                continue;
            }
            root_changed = true;
        }
        for edit in &fix.edits {
            edits.entry(edit.manifest.as_path()).or_default().push(edit);
        }
        fixed.push(format!("fixed: {}", fix.description));
    }

    let mut modified_files = BTreeSet::new();
    if let Some((path, doc)) = &root
        && root_changed
    {
        write_manifest(path, doc)?;
        modified_files.insert(*path);
    }
    // The root manifest can also be a member, so it is read again after the promotions.
    for (manifest, file_edits) in edits {
        let mut doc = read_manifest(manifest)?;
        for edit in file_edits {
            let rewritten = dep_table_mut(&mut doc, &edit.table).is_some_and(|table| {
                rewrite_dep_entry(table, &edit.key, edit.enable_default_features)
            });
            if !rewritten {
                return Err(format!(
                    "`{}` is no longer in {}. Run the check again.",
                    edit.key,
                    manifest.display(),
                ));
            }
        }
        write_manifest(manifest, &doc)?;
        modified_files.insert(manifest);
    }

    Ok(FixSummary {
        fixed,
        skipped,
        files_modified: modified_files.len(),
    })
}

/// Rewrite the entry `key` of `table` to `{ workspace = true }`, and keep only the keys that
/// cargo reads on an inherited entry.
fn rewrite_dep_entry(table: &mut dyn TableLike, key: &str, enable_default_features: bool) -> bool {
    let Some(item) = table.get_mut(key) else {
        return false;
    };

    // `[dependencies.serde]` form: edit the table in place, so its comments stay.
    if let Some(dep) = item.as_table_mut() {
        dep.retain(|k, _| INHERITABLE_KEYS.contains(&k));
        dep.insert("workspace", toml_edit::value(true));
        if enable_default_features {
            dep.insert("default-features", toml_edit::value(true));
        }
        return true;
    }

    let Some(value) = item.as_value_mut() else {
        return false;
    };
    let mut inherited = InlineTable::new();
    inherited.insert("workspace", true.into());
    if let Some(existing) = value.as_inline_table() {
        for (k, v) in existing.iter() {
            if INHERITABLE_KEYS.contains(&k) {
                inherited.insert(k, v.clone());
            }
        }
    }
    if enable_default_features {
        inherited.insert("default-features", true.into());
    }
    inherited.fmt();
    // toml_edit keeps the comment after an entry in the value's decor.
    *inherited.decor_mut() = value.decor().clone();
    *value = Value::InlineTable(inherited);
    true
}

/// Insert `promotion` into the root document. Returns `false` when the key already exists.
fn insert_workspace_dep(doc: &mut DocumentMut, promotion: &Promotion) -> Result<bool, String> {
    let workspace = doc
        .get_mut("workspace")
        .and_then(Item::as_table_mut)
        .ok_or("No [workspace] table in the root Cargo.toml")?;
    let deps = workspace
        .entry("dependencies")
        .or_insert(toml_edit::table())
        .as_table_like_mut()
        .ok_or("[workspace.dependencies] is not a table")?;
    if deps.contains_key(&promotion.key) {
        return Ok(false);
    }
    deps.insert(&promotion.key, Item::Value(promotion.value()));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::run_checks;
    use crate::workspace::parse_workspace;
    use crate::{fixture, temp_workspace};

    fn copy_fixture(name: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        copy_dir_recursive(&fixture(name), tmp.path());
        tmp
    }

    fn copy_dir_recursive(src: &Path, dst: &Path) {
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if src_path.is_dir() {
                std::fs::create_dir_all(&dst_path).unwrap();
                copy_dir_recursive(&src_path, &dst_path);
            } else {
                std::fs::copy(&src_path, &dst_path).unwrap();
            }
        }
    }

    fn read_file(tmp: &tempfile::TempDir, relative: &str) -> String {
        std::fs::read_to_string(tmp.path().join(relative)).unwrap()
    }

    /// Check and fix the workspace, then check it again. Returns the summary, and the
    /// findings of the second check.
    fn fix(tmp: &tempfile::TempDir) -> (FixSummary, Vec<Finding>) {
        let findings = run_checks(&parse_workspace(tmp.path()).unwrap(), 2);
        let summary = apply_fixes(&findings).unwrap();
        let after = run_checks(&parse_workspace(tmp.path()).unwrap(), 2);
        (summary, after)
    }

    /// The keys of the entry at `path` in the manifest `relative`, in order.
    fn entry_keys(tmp: &tempfile::TempDir, relative: &str, path: &[&str]) -> Vec<String> {
        let doc: DocumentMut = read_file(tmp, relative).parse().unwrap();
        let item = path.iter().fold(doc.as_item(), |item, key| &item[key]);
        let table = item
            .as_table_like()
            .unwrap_or_else(|| panic!("{path:?}: {item}"));
        table.iter().map(|(k, _)| k.to_string()).collect()
    }

    #[test]
    fn test_fix_not_inherited() {
        let tmp = copy_fixture("not_inherited");
        let (summary, after) = fix(&tmp);
        assert_eq!(summary.fixed.len(), 1);
        let keys = entry_keys(&tmp, "crates/foo/Cargo.toml", &["dependencies", "serde"]);
        assert_eq!(keys, ["workspace"]);
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_version_mismatch() {
        let tmp = copy_fixture("version_mismatch");
        let (summary, after) = fix(&tmp);
        assert_eq!(
            summary.fixed,
            ["fixed: `rand` in crates/bar/Cargo.toml changed from 0.7 to 0.8 (workspace version)"]
        );
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_promotion_candidate() {
        let tmp = copy_fixture("promotion_candidate");
        let (summary, after) = fix(&tmp);
        assert_eq!(
            summary.fixed,
            [
                "fixed: `serde_yaml = \"0.9\"` added to [workspace.dependencies], updated: \
                 crates/one/Cargo.toml, crates/two/Cargo.toml"
            ]
        );
        assert_eq!(summary.files_modified, 3);
        assert!(read_file(&tmp, "Cargo.toml").contains("serde_yaml = \"0.9\""));
        for name in ["one", "two"] {
            let manifest = format!("crates/{name}/Cargo.toml");
            let keys = entry_keys(&tmp, &manifest, &["dependencies", "serde_yaml"]);
            assert_eq!(keys, ["workspace"]);
        }
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_keeps_only_inheritable_keys() {
        let tmp = temp_workspace(
            "core = { path = \"crates/core\", version = \"0.1\" }\n\
             serde1 = { package = \"serde\", version = \"1.0\" }\n\
             my-crate = { version = \"1.0\", registry = \"my-registry\" }",
            &[
                ("core", "\n[features]\nx = []"),
                (
                    "app",
                    "core = { version = \"0.1\", path = \"../core\", features = [\"x\"], optional = true }\n\
                     serde1 = { package = \"serde\", version = \"1.0\", default-features = true }\n\
                     my-crate = { version = \"1.0\", registry = \"my-registry\", features = [\"y\"] }",
                ),
            ],
        );
        let (summary, after) = fix(&tmp);
        assert_eq!(summary.fixed.len(), 3);
        let app = "crates/app/Cargo.toml";
        let keys = |key| entry_keys(&tmp, app, &["dependencies", key]);
        assert_eq!(keys("core"), ["workspace", "features", "optional"]);
        assert_eq!(keys("serde1"), ["workspace"]);
        assert_eq!(keys("my-crate"), ["workspace", "features"]);
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_dotted_table() {
        let tmp = temp_workspace(
            "serde = \"1.0\"",
            &[(
                "app",
                "\n[dependencies.serde]\nversion = \"1.0\"\nfeatures = [\"derive\"]",
            )],
        );
        let (_, after) = fix(&tmp);
        let keys = entry_keys(&tmp, "crates/app/Cargo.toml", &["dependencies", "serde"]);
        assert_eq!(keys, ["features", "workspace"]);
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_keeps_trailing_comment() {
        let tmp = temp_workspace("serde = \"1.0\"", &[("app", "serde = \"1.0\" # pinned")]);
        fix(&tmp);
        let content = read_file(&tmp, "crates/app/Cargo.toml");
        assert!(
            content.contains("serde = { workspace = true } # pinned\n"),
            "{content}"
        );
    }

    #[test]
    fn test_fix_promotion_keeps_each_members_default_features() {
        let tmp = temp_workspace(
            "",
            &[
                (
                    "one",
                    "ed25519-dalek = { version = \"2.1\", default_features = false }",
                ),
                ("two", "ed25519-dalek = \"2.1\""),
            ],
        );
        let (_, after) = fix(&tmp);
        let root = read_file(&tmp, "Cargo.toml");
        assert!(
            root.contains("ed25519-dalek = { version = \"2.1\", default-features = false }"),
            "{root}"
        );
        let entry = ["dependencies", "ed25519-dalek"];
        assert_eq!(
            entry_keys(&tmp, "crates/one/Cargo.toml", &entry),
            ["workspace"]
        );
        let two = read_file(&tmp, "crates/two/Cargo.toml");
        assert!(
            two.contains("ed25519-dalek = { workspace = true, default-features = true }"),
            "{two}"
        );
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_skips_entry_that_disables_default_features() {
        let tmp = temp_workspace(
            "serde = \"1.0\"",
            &[(
                "app",
                "serde = { version = \"1.0\", default-features = false }",
            )],
        );
        let before = read_file(&tmp, "crates/app/Cargo.toml");
        let (summary, after) = fix(&tmp);
        assert!(summary.fixed.is_empty());
        assert_eq!(summary.skipped.len(), 1);
        assert!(summary.skipped[0].starts_with("skipped: `serde` in crates/app/Cargo.toml"));
        assert_eq!(read_file(&tmp, "crates/app/Cargo.toml"), before);
        assert_eq!(after.len(), 1);
    }

    #[test]
    fn test_fix_skips_promotion_with_different_versions() {
        let tmp = temp_workspace("", &[("a", "rand = \"0.7\""), ("b", "rand = \"0.8\"")]);
        let root = read_file(&tmp, "Cargo.toml");
        let (summary, _) = fix(&tmp);
        assert_eq!(
            summary.skipped,
            [
                "skipped: `rand` is not promoted, because members use different versions: \
                 crates/a/Cargo.toml (0.7), crates/b/Cargo.toml (0.8)"
            ]
        );
        assert_eq!(summary.files_modified, 0);
        assert_eq!(read_file(&tmp, "Cargo.toml"), root);
    }

    #[test]
    fn test_fix_skips_promotion_when_the_key_exists() {
        let registry_dep = "my-crate = { version = \"1.0\", registry = \"my-registry\" }";
        let tmp = temp_workspace(
            "",
            &[
                ("a", registry_dep),
                ("b", registry_dep),
                ("c", "my-crate = \"1.0\""),
                ("d", "my-crate = \"1.0\""),
            ],
        );
        let (summary, after) = fix(&tmp);
        assert_eq!(summary.fixed.len(), 1);
        assert_eq!(
            summary.skipped,
            [
                "skipped: `my-crate` is not promoted, because [workspace.dependencies] already has it"
            ]
        );
        let entry = ["dependencies", "my-crate"];
        assert_eq!(
            entry_keys(&tmp, "crates/a/Cargo.toml", &entry),
            ["version", "registry"]
        );
        assert_eq!(
            entry_keys(&tmp, "crates/c/Cargo.toml", &entry),
            ["workspace"]
        );
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_promotion_into_a_root_that_is_also_a_member() {
        let tmp = temp_workspace("", &[("a", "anyhow = \"1.0\"")]);
        let root = format!(
            "[package]\nname = \"root\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nanyhow = \"1.0\"\n\n{}",
            read_file(&tmp, "Cargo.toml"),
        );
        std::fs::write(tmp.path().join("Cargo.toml"), root).unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
        let (_, after) = fix(&tmp);
        let root = read_file(&tmp, "Cargo.toml");
        assert!(root.contains("anyhow = \"1.0\""), "{root}");
        assert_eq!(
            entry_keys(&tmp, "Cargo.toml", &["dependencies", "anyhow"]),
            ["workspace"]
        );
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_target_specific_not_inherited() {
        let tmp = copy_fixture("target_deps");
        let (_, after) = fix(&tmp);
        let entry = ["target", "cfg(windows)", "dependencies", "winapi"];
        assert_eq!(
            entry_keys(&tmp, "crates/plat/Cargo.toml", &entry),
            ["workspace"]
        );
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_promotion_carries_registry() {
        let tmp = copy_fixture("registry_promotion");
        let (_, after) = fix(&tmp);
        let root = read_file(&tmp, "Cargo.toml");
        assert!(
            root.contains("my-crate = { version = \"1.0\", registry = \"my-registry\" }"),
            "{root}"
        );
        for name in ["one", "two"] {
            let manifest = format!("crates/{name}/Cargo.toml");
            let keys = entry_keys(&tmp, &manifest, &["dependencies", "my-crate"]);
            assert_eq!(keys, ["workspace"]);
        }
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_promotion_with_target_specific_default_features() {
        let tmp = copy_fixture("target_deps_promotion");
        let (_, after) = fix(&tmp);
        let root = read_file(&tmp, "Cargo.toml");
        assert!(
            root.contains("winapi = { version = \"0.3\", default-features = false }"),
            "{root}"
        );
        for name in ["one", "two"] {
            let manifest = format!("crates/{name}/Cargo.toml");
            let entry = ["target", "cfg(windows)", "dependencies", "winapi"];
            assert_eq!(entry_keys(&tmp, &manifest, &entry), ["workspace"]);
        }
        assert!(after.is_empty(), "{after:?}");
    }
}
