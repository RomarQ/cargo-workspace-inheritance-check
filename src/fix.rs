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
pub(crate) struct Fix {
    /// An entry to add to `[workspace.dependencies]` before the edits. When the root manifest
    /// already has that key, the whole fix is skipped.
    pub promotion: Option<Promotion>,
    /// Member entries to rewrite to `{ workspace = true }`.
    pub edits: Vec<Edit>,
    /// What the fix does, for the `fixed:` line.
    pub description: String,
}

/// An entry to add to `[workspace.dependencies]`. Every promotion of one run names the same
/// root manifest.
#[derive(Debug)]
pub(crate) struct Promotion {
    pub root_manifest: PathBuf,
    pub key: String,
    pub version: String,
    pub registry: Option<String>,
    /// `false` writes `default-features = false` on the entry.
    pub default_features: bool,
}

impl Promotion {
    /// The value written to `[workspace.dependencies]`.
    pub fn value(&self) -> Value {
        let registry = self.registry.as_deref();
        dep_value(&self.version, registry, self.default_features)
    }
}

/// A `[workspace.dependencies]` value: the bare version, or an inline table with the other keys.
pub(crate) fn dep_value(version: &str, registry: Option<&str>, default_features: bool) -> Value {
    if default_features && registry.is_none() {
        return version.into();
    }
    let mut table = InlineTable::new();
    table.insert("version", version.into());
    if !default_features {
        table.insert("default-features", false.into());
    }
    if let Some(registry) = registry {
        table.insert("registry", registry.into());
    }
    Value::InlineTable(table)
}

/// Rewrites one member entry to `{ workspace = true }`.
#[derive(Debug)]
pub(crate) struct Edit {
    pub manifest: PathBuf,
    pub table: DepTable,
    pub key: String,
    /// Write `default-features = true`, because the entry uses default features and the
    /// workspace entry disables them. Otherwise any `default-features` key is removed.
    pub enable_default_features: bool,
}

/// What [`apply_fixes`] did.
#[derive(Debug, Default)]
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
    let mut summary = FixSummary::default();
    let mut root: Option<(&Path, DocumentMut)> = None;
    let mut root_changed = false;
    let mut edits: BTreeMap<&Path, Vec<&Edit>> = BTreeMap::new();

    for finding in findings {
        let fix = match &finding.fix {
            Ok(fix) => fix,
            Err(reason) => {
                summary.skipped.push(format!("skipped: {reason}"));
                continue;
            }
        };
        if let Some(promotion) = &fix.promotion {
            let path = promotion.root_manifest.as_path();
            let (_, doc) = match &mut root {
                Some(loaded) => loaded,
                None => root.insert((path, read_manifest(path)?)),
            };
            if !insert_workspace_dep(doc, promotion)? {
                summary.skipped.push(format!(
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
        summary.fixed.push(format!("fixed: {}", fix.description));
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
                    "`{}` in {} changed after the check",
                    edit.key,
                    manifest.display(),
                ));
            }
        }
        write_manifest(manifest, &doc)?;
        modified_files.insert(manifest);
    }
    summary.files_modified = modified_files.len();
    Ok(summary)
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
    let no_workspace = "No [workspace] section in the root Cargo.toml";
    let workspace = doc.get_mut("workspace").ok_or(no_workspace)?;
    let inline_workspace = workspace.is_inline_table();
    // An inline `workspace = { .. }` can hold only inline values.
    let empty = if inline_workspace {
        Item::Value(InlineTable::new().into())
    } else {
        toml_edit::table()
    };
    let workspace = workspace.as_table_like_mut().ok_or(no_workspace)?;
    let deps = workspace.entry("dependencies").or_insert(empty);
    let inline_deps = deps.is_inline_table();
    let entries = deps
        .as_table_like_mut()
        .ok_or("[workspace.dependencies] is not a table")?;
    if entries.contains_key(&promotion.key) {
        return Ok(false);
    }
    entries.insert(&promotion.key, Item::Value(promotion.value()));
    // An inline table keeps the spacing of its old last value, so respace it. A standard table
    // is left alone, because its `fmt` also drops comments.
    if inline_deps {
        entries.fmt();
    }
    if inline_workspace {
        workspace.fmt();
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::run_checks;
    use crate::workspace::parse_workspace;
    use crate::{add_root_package, temp_workspace};

    fn read_file(tmp: &tempfile::TempDir, relative: &str) -> String {
        std::fs::read_to_string(tmp.path().join(relative)).unwrap()
    }

    /// The text after the `[dependencies]` header of the manifest `relative`.
    fn deps_section(tmp: &tempfile::TempDir, relative: &str) -> String {
        let manifest = read_file(tmp, relative);
        let (_, deps) = manifest.split_once("[dependencies]\n").unwrap();
        deps.to_string()
    }

    /// Check and fix the workspace, then check it again. Returns the summary, and the
    /// findings of the second check.
    fn fix(tmp: &tempfile::TempDir) -> (FixSummary, Vec<Finding>) {
        let findings = run_checks(&parse_workspace(tmp.path()).unwrap(), 2);
        let summary = apply_fixes(&findings).unwrap();
        let after = run_checks(&parse_workspace(tmp.path()).unwrap(), 2);
        (summary, after)
    }

    #[test]
    fn test_fix_keeps_only_inheritable_keys_and_comments() {
        let tmp = temp_workspace(
            "core = { path = \"crates/core\", version = \"0.1\" }\n\
             serde1 = { package = \"serde\", version = \"1.0\" }\n\
             my-crate = { version = \"1.0\", registry = \"my-registry\" }\n\
             log = { version = \"0.4\", default-features = false }\n\
             rand = \"0.8\"\ntokio = { version = \"1\", default-features = false }",
            &[
                ("core", "\n[features]\nx = []"),
                (
                    "a",
                    "rand = \"0.7\" # pinned\n\
                     core = { version = \"0.1\", path = \"../core\", features = [\"x\"], optional = true }\n\
                     serde1 = { package = \"serde\", version = \"1.0\", default-features = true }\n\
                     my-crate = { version = \"1.0\", registry = \"my-registry\", features = [\"y\"] }\n\
                     log = { version = \"0.4\", default-features = false }\n\n\
                     [dependencies.tokio]\nversion = \"1\"\nfeatures = [\"full\"]",
                ),
            ],
        );
        let (summary, after) = fix(&tmp);
        assert_eq!(
            summary.fixed[..2],
            [
                "fixed: `rand` in crates/a/Cargo.toml changed from 0.7 to 0.8 (workspace version)",
                "fixed: `core` in crates/a/Cargo.toml now uses workspace inheritance",
            ]
        );
        assert_eq!(
            deps_section(&tmp, "crates/a/Cargo.toml"),
            "rand = { workspace = true } # pinned\n\
             core = { workspace = true, features = [\"x\"], optional = true }\n\
             serde1 = { workspace = true }\n\
             my-crate = { workspace = true, features = [\"y\"] }\n\
             log = { workspace = true }\n\n\
             [dependencies.tokio]\nfeatures = [\"full\"]\nworkspace = true\ndefault-features = true\n"
        );
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_promotion_keeps_each_members_default_features_and_registry() {
        let registry_dep = "my-crate = { version = \"1.0\", registry = \"my-registry\" }";
        let tmp = temp_workspace(
            "",
            &[
                ("one", &format!("ed25519-dalek = \"2.1\"\n{registry_dep}")),
                (
                    "two",
                    &format!(
                        "ed25519-dalek = {{ version = \"2.1\", default_features = false }}\n\n\
                         [target.'cfg(windows)'.dependencies]\n{registry_dep}"
                    ),
                ),
            ],
        );
        let (summary, after) = fix(&tmp);
        assert_eq!(
            summary.fixed[1],
            "fixed: `my-crate = { version = \"1.0\", registry = \"my-registry\" }` added to \
             [workspace.dependencies], updated: crates/one/Cargo.toml, crates/two/Cargo.toml"
        );
        assert_eq!(summary.files_modified, 3);
        assert_eq!(
            read_file(&tmp, "Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\n\
             ed25519-dalek = { version = \"2.1\", default-features = false }\n\
             my-crate = { version = \"1.0\", registry = \"my-registry\" }\n\n"
        );
        assert_eq!(
            deps_section(&tmp, "crates/one/Cargo.toml"),
            "ed25519-dalek = { workspace = true, default-features = true }\n\
             my-crate = { workspace = true }\n"
        );
        assert_eq!(
            deps_section(&tmp, "crates/two/Cargo.toml"),
            "ed25519-dalek = { workspace = true }\n\n\
             [target.'cfg(windows)'.dependencies]\nmy-crate = { workspace = true }\n"
        );
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_skips_a_fix_that_changes_what_cargo_resolves() {
        let tmp = temp_workspace(
            "serde = \"1.0\"\ncore = { path = \"crates/core\" }",
            &[
                ("core", ""),
                (
                    "a",
                    "serde = { version = \"1.0\", default-features = false }\n\
                     core = { path = \"../core\", version = \"0.1\" }\nrand = \"0.7\"",
                ),
                ("b", "rand = \"0.8\""),
            ],
        );
        let (summary, _) = fix(&tmp);
        assert_eq!(
            summary.skipped,
            [
                "skipped: `serde` in crates/a/Cargo.toml sets `default-features = false`, so the \
                 workspace entry must set it too",
                "skipped: `core` in crates/a/Cargo.toml requires version 0.1, but the workspace \
                 entry has no version",
                "skipped: `rand` is not promoted, because members use different versions: \
                 crates/a/Cargo.toml (0.7), crates/b/Cargo.toml (0.8)",
            ]
        );
        assert_eq!(summary.files_modified, 0);
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
        assert_eq!(summary.files_modified, 3);
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_promotion_into_a_root_that_is_also_a_member() {
        let tmp = temp_workspace("", &[("a", "anyhow = \"1.0\"")]);
        add_root_package(&tmp, "anyhow = \"1.0\"");
        let (_, after) = fix(&tmp);
        assert_eq!(
            deps_section(&tmp, "Cargo.toml"),
            "anyhow = { workspace = true }\n\n[workspace]\nmembers = [\"crates/*\"]\n\n\
             [workspace.dependencies]\nanyhow = \"1.0\"\n\n"
        );
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn test_fix_promotion_into_an_inline_workspace_table() {
        let tmp = temp_workspace("", &[("a", "anyhow = \"1.0\""), ("b", "anyhow = \"1.0\"")]);
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "workspace = { members = [\"crates/*\"] }\n",
        )
        .unwrap();
        let (_, after) = fix(&tmp);
        assert_eq!(
            read_file(&tmp, "Cargo.toml"),
            "workspace = { members = [\"crates/*\"], dependencies = { anyhow = \"1.0\" } }\n"
        );
        assert!(after.is_empty(), "{after:?}");
    }
}
