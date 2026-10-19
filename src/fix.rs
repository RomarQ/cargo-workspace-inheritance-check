//! Writes the fixes that [`crate::check::run_checks`] plans.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use toml_edit::{Decor, DocumentMut, InlineTable, Item, RawString, TableLike, Value};

use crate::check::Finding;
use crate::workspace::{Manifest, MemberDep, dep_table_mut, read_manifest, write_manifest};

/// Keys that cargo reads on an entry with `workspace = true`. It ignores any other key with an
/// "unused manifest key" warning, and `default-features` has its own rule in [`Edit`].
const INHERITABLE_KEYS: [&str; 3] = ["features", "optional", "public"];

/// The change that `--fix` makes for one finding.
#[derive(Debug)]
pub(crate) struct Fix {
    pub promotion: Option<Promotion>,
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
    pub value: Value,
}

/// Rewrites one member entry to `{ workspace = true }`.
#[derive(Debug)]
pub(crate) struct Edit {
    pub dep: MemberDep,
    /// Write `default-features = true`, because the entry uses default features and the
    /// workspace entry disables them. Otherwise any `default-features` key is removed.
    pub enable_default_features: bool,
}

impl Edit {
    /// The entry that the edit writes, as one inline line for a message. The edit itself keeps
    /// the `[dependencies.serde]` form of an entry, and the lines and comments of an array.
    pub(crate) fn suggestion(&self) -> String {
        let mut entry = inherited_entry(&self.dep.entry, self.enable_default_features);
        for (_, value) in entry.iter_mut() {
            if let Some(array) = value.as_array_mut() {
                array.fmt();
            }
        }
        entry.to_string()
    }
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
    let mut root: Option<(&PathBuf, Manifest)> = None;
    let mut edits: BTreeMap<&PathBuf, Vec<&Edit>> = BTreeMap::new();

    for finding in findings {
        let fix = match &finding.fix {
            Ok(fix) => fix,
            Err(reason) => {
                summary.skipped.push(format!("skipped: {reason}"));
                continue;
            }
        };
        if let Some(promotion) = &fix.promotion {
            let path = &promotion.root_manifest;
            let (_, manifest) = match &mut root {
                Some(loaded) => loaded,
                None => root.insert((path, read_manifest(path)?)),
            };
            if !insert_workspace_dep(&mut manifest.doc, promotion)? {
                summary.skipped.push(format!(
                    "skipped: `{}` is not promoted, because [workspace.dependencies] already has it",
                    promotion.key,
                ));
                continue;
            }
        }
        for edit in &fix.edits {
            edits.entry(&edit.dep.manifest).or_default().push(edit);
        }
        summary.fixed.push(format!("fixed: {}", fix.description));
    }

    let mut modified_files = BTreeSet::new();
    if let Some((path, manifest)) = &root {
        write_manifest(path, manifest)?;
        modified_files.insert(*path);
    }
    // The root manifest can also be a member, so it is read again after the promotions.
    for (manifest, file_edits) in edits {
        let mut parsed = read_manifest(manifest)?;
        for edit in file_edits {
            let rewritten = dep_table_mut(&mut parsed.doc, &edit.dep.table).is_some_and(|table| {
                rewrite_dep_entry(table, &edit.dep.key, edit.enable_default_features)
            });
            if !rewritten {
                return Err(format!(
                    "`{}` in {} changed after the check",
                    edit.dep.key,
                    manifest.display(),
                ));
            }
        }
        write_manifest(manifest, &parsed)?;
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

    let mut inherited = inherited_entry(item, enable_default_features);
    let Some(value) = item.as_value_mut() else {
        return false;
    };
    // toml_edit keeps the comment after an entry in the value's decor.
    *inherited.decor_mut() = value.decor().clone();
    *value = Value::InlineTable(inherited);
    true
}

/// The inline entry `{ workspace = true }`, with the keys of `entry` that cargo reads on an
/// inherited entry.
fn inherited_entry(entry: &Item, enable_default_features: bool) -> InlineTable {
    let mut inherited = InlineTable::new();
    inherited.insert("workspace", true.into());
    for (key, item) in entry.as_table_like().into_iter().flat_map(|t| t.iter()) {
        if INHERITABLE_KEYS.contains(&key)
            && let Some(value) = item.as_value()
        {
            inherited.insert(key, value.clone());
        }
    }
    if enable_default_features {
        inherited.insert("default-features", true.into());
    }
    inherited.fmt();
    inherited
}

/// `true` when `entry` is an inline table with a comment between its braces, which only TOML 1.1
/// allows. The rewrite to one line would lose the comment.
pub(crate) fn has_inline_comment(entry: &Item) -> bool {
    let Some(table) = entry.as_inline_table() else {
        return false;
    };
    let comment = |raw: Option<&RawString>| {
        raw.and_then(RawString::as_str)
            .is_some_and(|text| text.contains('#'))
    };
    let in_decor = |decor: &Decor| comment(decor.prefix()) || comment(decor.suffix());
    comment(Some(table.trailing()))
        || table.iter().any(|(name, value)| {
            let key = table.key(name).expect("an entry has a key");
            in_decor(key.leaf_decor()) || in_decor(key.dotted_decor()) || in_decor(value.decor())
        })
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
    entries.insert(&promotion.key, Item::Value(promotion.value.clone()));
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
    use std::path::Path;

    use crate::fixtures::{assert_fixed, fixture, fixture_copy};
    use crate::workspace::parse_workspace;

    /// Fix a copy of the fixture `name`, and assert that the copy matches `<name>.fixed`.
    fn fix(name: &str) -> FixSummary {
        let tmp = fixture_copy(name);
        let findings = run_checks(&parse_workspace(tmp.path()).unwrap(), 2);
        let summary = apply_fixes(&findings).unwrap();
        assert_fixed(tmp.path(), name);
        summary
    }

    #[test]
    fn test_fix_keeps_only_inheritable_keys_and_comments() {
        let summary = fix("fix_inheritable_keys");
        assert_eq!(
            summary.fixed[..2],
            [
                "fixed: `rand` in crates/a/Cargo.toml changed from 0.7 to 0.8 (workspace version)",
                "fixed: `core` in crates/a/Cargo.toml now uses workspace inheritance",
            ]
        );
    }

    #[test]
    fn test_fix_keeps_crlf_line_endings() {
        let tmp = fixture_copy("fix_inheritable_keys");
        let crlf = |path: &Path| std::fs::read_to_string(path).unwrap().replace('\n', "\r\n");
        for manifest in [
            "Cargo.toml",
            "crates/a/Cargo.toml",
            "crates/core/Cargo.toml",
        ] {
            let path = tmp.path().join(manifest);
            std::fs::write(&path, crlf(&path)).unwrap();
        }
        apply_fixes(&run_checks(&parse_workspace(tmp.path()).unwrap(), 2)).unwrap();
        let written = std::fs::read_to_string(tmp.path().join("crates/a/Cargo.toml")).unwrap();
        let fixed = fixture("fix_inheritable_keys.fixed").join("crates/a/Cargo.toml");
        assert_eq!(written, crlf(&fixed));
    }

    #[test]
    fn test_fix_promotion_keeps_each_members_default_features_and_registry() {
        let summary = fix("fix_promotion_default_features");
        assert_eq!(
            summary.fixed[1],
            "fixed: `my-crate = { version = \"1.0\", registry = \"my-registry\" }` added to \
             [workspace.dependencies], updated: crates/one/Cargo.toml, crates/two/Cargo.toml"
        );
        assert_eq!(summary.files_modified, 3);
    }

    #[test]
    fn test_fix_skips_a_fix_that_changes_what_cargo_resolves() {
        let summary = fix("fix_skips");
        assert_eq!(
            summary.skipped,
            [
                "skipped: `serde` in crates/a/Cargo.toml sets `default-features = false`, so the \
                 workspace entry must set it too",
                "skipped: `core` in crates/a/Cargo.toml requires version 0.1, but the workspace \
                 entry has no version",
                "skipped: `my-crate` is not promoted, because [workspace.dependencies] already has it",
                "skipped: `rand` is not promoted, because members use different versions: \
                 crates/a/Cargo.toml (0.7), crates/b/Cargo.toml (0.8)",
            ]
        );
        assert_eq!(summary.files_modified, 3);
    }

    #[test]
    fn test_fix_skips_a_workspace_spelling_that_the_member_edition_rejects() {
        let summary = fix("fix_underscore_default_features");
        let reason = "uses edition 2024, so the workspace entry must use `default-features`, not \
                      `default_features`";
        assert_eq!(
            summary.skipped,
            ["log", "itoa", "ryu"]
                .map(|key| format!("skipped: `{key}` in crates/b/Cargo.toml {reason}"))
        );
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
                ("multi_line", false),
                ("in_array", false),
                ("after_entry", false),
            ]
        );
    }

    #[test]
    fn test_fix_promotion_into_an_inline_workspace_of_a_root_package() {
        fix("fix_inline_workspace_root_package");
    }
}
