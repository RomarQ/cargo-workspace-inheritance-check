use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, InlineTable, Item, Value};

use crate::diagnostic::{format_dep_value, Diagnostic, DiagnosticKind};
use crate::workspace::{
    for_each_dep_table, for_each_dep_table_mut, item_as_table_like, read_manifest, write_manifest,
};

/// Keys that are owned by the workspace dep and must be stripped from members.
const STRIPPED_KEYS: &[&str] = &["version", "default-features", "registry"];

pub struct FixSummary {
    pub fixes_applied: usize,
    pub files_modified: usize,
    pub actions: Vec<String>,
}

struct Promotion<'a> {
    dep_name: &'a str,
    version: &'a str,
    registry: Option<&'a str>,
    default_features: bool,
}

pub fn apply_fixes(
    workspace_root: &Path,
    diagnostics: &[Diagnostic],
) -> Result<FixSummary, String> {
    // Collect all work first, then batch file I/O.
    // member_fixes: path -> list of dep names to rewrite
    let mut member_fixes: BTreeMap<PathBuf, Vec<&str>> = BTreeMap::new();
    let mut promotions: Vec<Promotion> = Vec::new();
    let mut actions = Vec::new();
    let mut fixes_applied = 0;

    for diag in diagnostics {
        match &diag.kind {
            DiagnosticKind::NotInherited { member, .. }
            | DiagnosticKind::VersionMismatch { member, .. } => {
                let full_path = workspace_root.join(member);
                member_fixes
                    .entry(full_path)
                    .or_default()
                    .push(&diag.dependency);
                fixes_applied += 1;
                let msg = match &diag.kind {
                    DiagnosticKind::NotInherited { .. } => format!(
                        "fixed: `{}` in {} now uses workspace inheritance",
                        diag.dependency, member,
                    ),
                    DiagnosticKind::VersionMismatch {
                        version,
                        workspace_version,
                        ..
                    } => format!(
                        "fixed: `{}` in {} changed from {} to {} (workspace version)",
                        diag.dependency,
                        member,
                        version.as_deref().unwrap_or("?"),
                        workspace_version.as_deref().unwrap_or("?"),
                    ),
                    _ => unreachable!(),
                };
                actions.push(msg);
            }
            DiagnosticKind::PromotionCandidate {
                members,
                suggested_version,
                suggested_registry,
                ..
            } => {
                let Some(version) = suggested_version.as_deref() else {
                    continue;
                };
                let default_features = !any_member_disables_default_features(
                    workspace_root,
                    members,
                    &diag.dependency,
                );
                promotions.push(Promotion {
                    dep_name: &diag.dependency,
                    version,
                    registry: suggested_registry.as_deref(),
                    default_features,
                });
                for member_path in members {
                    member_fixes
                        .entry(workspace_root.join(member_path))
                        .or_default()
                        .push(&diag.dependency);
                }
                fixes_applied += 1;
                let member_list = members.join(", ");
                let dep_value = format_dep_value(version, suggested_registry.as_deref());
                actions.push(format!(
                    "fixed: `{dep} = {dep_value}` added to [workspace.dependencies], updated: {member_list}",
                    dep = diag.dependency,
                ));
            }
        }
    }

    // Apply all promotions to root Cargo.toml in a single read/write.
    let mut modified_files = BTreeSet::new();
    if !promotions.is_empty() {
        let root_toml = workspace_root.join("Cargo.toml");
        let mut doc = read_manifest(&root_toml)?;
        for promo in &promotions {
            insert_workspace_dep(&mut doc, promo)?;
        }
        write_manifest(&root_toml, &doc)?;
        modified_files.insert(root_toml);
    }

    // Apply all member dep rewrites, one read/write per file.
    for (manifest_path, dep_names) in &member_fixes {
        let mut doc = read_manifest(manifest_path)?;
        let mut modified = false;
        for_each_dep_table_mut(&mut doc, |table| {
            for dep_name in dep_names {
                if let Some(key) = find_dep_key(table, dep_name) {
                    if rewrite_dep_entry(table, &key) {
                        modified = true;
                    }
                }
            }
        });
        if modified {
            write_manifest(manifest_path, &doc)?;
            modified_files.insert(manifest_path.clone());
        }
    }

    Ok(FixSummary {
        fixes_applied,
        files_modified: modified_files.len(),
        actions,
    })
}

/// Find the TOML key for a dependency, handling package renames.
fn find_dep_key(table: &dyn toml_edit::TableLike, dep_name: &str) -> Option<String> {
    if table.contains_key(dep_name) {
        return Some(dep_name.to_string());
    }
    // Scan for renamed packages: `alias = { package = "dep_name", ... }`
    for (key, item) in table.iter() {
        let package_name = item_as_table_like(item)
            .and_then(|t| t.get("package"))
            .and_then(|v| v.as_str());
        if package_name == Some(dep_name) {
            return Some(key.to_string());
        }
    }
    None
}

/// Rewrite a dependency entry to use `{ workspace = true }`.
///
/// Strips `version`, `default-features`, and `registry` (which must be set at
/// the workspace level). Preserves other keys like `features` and `optional`.
fn rewrite_dep_entry(table: &mut dyn toml_edit::TableLike, key: &str) -> bool {
    let Some(item) = table.get_mut(key) else {
        return false;
    };

    // Handle dotted-key table style: [dependencies.foo]
    if let Some(dep_table) = item.as_table_mut() {
        if dep_table.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
            return false;
        }
        for k in STRIPPED_KEYS {
            dep_table.remove(k);
        }
        dep_table.insert("workspace", toml_edit::value(true));
        return true;
    }

    // Handle inline styles
    match item.as_value() {
        Some(Value::String(_)) => {
            // `serde = "1.0"` -> `serde = { workspace = true }`
            let mut inline = InlineTable::new();
            inline.insert("workspace", Value::from(true));
            *item = Item::Value(Value::InlineTable(inline));
            true
        }
        Some(Value::InlineTable(existing)) => {
            if existing.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
                return false;
            }
            let mut rebuilt = InlineTable::new();
            rebuilt.insert("workspace", Value::from(true));
            for (k, v) in existing.iter() {
                if k != "workspace" && !STRIPPED_KEYS.contains(&k) {
                    rebuilt.insert(k, v.clone());
                }
            }
            *item = Item::Value(Value::InlineTable(rebuilt));
            true
        }
        _ => false,
    }
}

/// Check if any member usage of a dependency sets `default-features = false`.
///
/// Cargo ignores `default-features = false` in members unless the workspace dep
/// also sets it. In the 2024 edition this mismatch is a hard error. So if any
/// member needs `default-features = false`, the workspace dep must have it too.
fn any_member_disables_default_features(
    workspace_root: &Path,
    members: &[String],
    dep_name: &str,
) -> bool {
    for member_path in members {
        let full_path = workspace_root.join(member_path);
        let Ok(doc) = read_manifest(&full_path) else {
            continue;
        };

        let mut found = false;
        for_each_dep_table(&doc, |table| {
            if has_default_features_false(table, dep_name) {
                found = true;
            }
        });
        if found {
            return true;
        }
    }

    false
}

fn has_default_features_false(table: &dyn toml_edit::TableLike, dep_name: &str) -> bool {
    find_dep_key(table, dep_name)
        .and_then(|key| table.get(&key))
        .and_then(item_as_table_like)
        .and_then(|t| t.get("default-features"))
        .and_then(|v| v.as_bool())
        == Some(false)
}

/// Insert a promoted dependency into the in-memory root document.
fn insert_workspace_dep(doc: &mut DocumentMut, promo: &Promotion) -> Result<(), String> {
    let workspace = doc
        .get_mut("workspace")
        .and_then(|v| v.as_table_mut())
        .ok_or("No [workspace] in root Cargo.toml")?;

    if !workspace.contains_key("dependencies") {
        workspace.insert("dependencies", toml_edit::table());
    }

    let ws_deps = workspace
        .get_mut("dependencies")
        .and_then(|v| v.as_table_mut())
        .ok_or("Failed to access [workspace.dependencies]")?;

    if !ws_deps.contains_key(promo.dep_name) {
        if promo.default_features && promo.registry.is_none() {
            ws_deps.insert(promo.dep_name, toml_edit::value(promo.version));
        } else {
            let mut table = InlineTable::new();
            table.insert("version", Value::from(promo.version));
            if !promo.default_features {
                table.insert("default-features", Value::from(false));
            }
            if let Some(reg) = promo.registry {
                table.insert("registry", Value::from(reg));
            }
            ws_deps.insert(promo.dep_name, Item::Value(Value::InlineTable(table)));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check;
    use crate::workspace::parse_workspace;

    fn copy_fixture(name: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        copy_dir_recursive(&crate::fixture(name), tmp.path());
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

    /// Create a temp workspace with a root Cargo.toml and member crates.
    /// Each member is a tuple of (name, dependencies_toml_fragment).
    fn temp_workspace(workspace_deps: &str, members: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            format!(
                "[workspace]\nmembers = [\"crates/*\"]\n\n\
                 [workspace.dependencies]\n{workspace_deps}\n"
            ),
        )
        .unwrap();

        for (name, deps) in members {
            std::fs::create_dir_all(tmp.path().join(format!("crates/{name}/src"))).unwrap();
            std::fs::write(
                tmp.path().join(format!("crates/{name}/Cargo.toml")),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                     [dependencies]\n{deps}\n"
                ),
            )
            .unwrap();
            std::fs::write(tmp.path().join(format!("crates/{name}/src/lib.rs")), "").unwrap();
        }

        tmp
    }

    fn read_file(tmp: &tempfile::TempDir, relative: &str) -> String {
        std::fs::read_to_string(tmp.path().join(relative)).unwrap()
    }

    #[test]
    fn test_fix_not_inherited() {
        let tmp = copy_fixture("not_inherited");
        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);

        let summary = apply_fixes(tmp.path(), &diags).unwrap();
        assert_eq!(summary.fixes_applied, 1);

        let content = read_file(&tmp, "crates/foo/Cargo.toml");
        assert!(content.contains("workspace = true"));
        assert!(!content.contains("serde = \"1.0\""));

        // Re-run checks: should be clean
        let ws2 = parse_workspace(tmp.path()).unwrap();
        let diags2 = check::run_checks(&ws2, 2);
        assert!(
            diags2.is_empty(),
            "expected no diagnostics after fix, got: {diags2:?}"
        );
    }

    #[test]
    fn test_fix_version_mismatch() {
        let tmp = copy_fixture("version_mismatch");
        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);

        let summary = apply_fixes(tmp.path(), &diags).unwrap();
        assert_eq!(summary.fixes_applied, 1);

        let content = read_file(&tmp, "crates/bar/Cargo.toml");
        assert!(content.contains("workspace = true"));
        assert!(!content.contains("rand = \"0.7\""));

        let ws2 = parse_workspace(tmp.path()).unwrap();
        let diags2 = check::run_checks(&ws2, 2);
        assert!(
            diags2.is_empty(),
            "expected no diagnostics after fix, got: {diags2:?}"
        );
    }

    #[test]
    fn test_fix_promotion_candidate() {
        let tmp = copy_fixture("promotion_candidate");
        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);
        assert!(matches!(
            diags[0].kind,
            DiagnosticKind::PromotionCandidate { .. }
        ));

        let summary = apply_fixes(tmp.path(), &diags).unwrap();
        assert_eq!(summary.fixes_applied, 1);

        let root_content = read_file(&tmp, "Cargo.toml");
        assert!(root_content.contains("serde_yaml"));

        for name in &["one", "two"] {
            let content = read_file(&tmp, &format!("crates/{name}/Cargo.toml"));
            assert!(content.contains("workspace = true"));
            assert!(!content.contains("serde_yaml = \"0.9\""));
        }

        // Re-run checks: should be clean
        let ws2 = parse_workspace(tmp.path()).unwrap();
        let diags2 = check::run_checks(&ws2, 2);
        assert!(
            diags2.is_empty(),
            "expected no diagnostics after fix, got: {diags2:?}"
        );
    }

    #[test]
    fn test_fix_preserves_features() {
        let tmp = temp_workspace(
            "serde = \"1.0\"",
            &[(
                "app",
                "serde = { version = \"1.0\", features = [\"derive\"] }",
            )],
        );

        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);

        apply_fixes(tmp.path(), &diags).unwrap();

        let content = read_file(&tmp, "crates/app/Cargo.toml");
        assert!(content.contains("workspace = true"));
        assert!(content.contains("features"));
        assert!(!content.contains("serde = { version"));
    }

    #[test]
    fn test_fix_valid_workspace_no_changes() {
        let tmp = copy_fixture("valid_workspace");
        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert!(diags.is_empty());

        let summary = apply_fixes(tmp.path(), &diags).unwrap();
        assert_eq!(summary.fixes_applied, 0);
    }

    #[test]
    fn test_fix_promotion_sets_default_features_false_when_any_member_disables() {
        let tmp = temp_workspace(
            "serde = \"1.0\"",
            &[
                (
                    "one",
                    "serde = { workspace = true }\n\
                     ed25519-dalek = { version = \"2.1\", default-features = false }",
                ),
                (
                    "two",
                    "serde = { workspace = true }\n\
                     ed25519-dalek = \"2.1\"",
                ),
            ],
        );

        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);

        apply_fixes(tmp.path(), &diags).unwrap();

        // Workspace dep must have default-features = false because at least one
        // member needs it. Without it, member `default-features = false` is
        // silently ignored (pre-2024) or a hard error (2024 edition).
        let root = read_file(&tmp, "Cargo.toml");
        assert!(
            root.contains("default-features = false"),
            "workspace dep should have default-features = false when any member disables it, got:\n{root}"
        );

        // Member that had default-features = false should have it stripped
        let one = read_file(&tmp, "crates/one/Cargo.toml");
        assert!(
            !one.contains("default-features"),
            "member one should not have default-features after fix, got:\n{one}"
        );
    }

    #[test]
    fn test_fix_target_specific_not_inherited() {
        let tmp = copy_fixture("target_deps");
        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);

        let summary = apply_fixes(tmp.path(), &diags).unwrap();
        assert_eq!(summary.fixes_applied, 1);

        let content = read_file(&tmp, "crates/plat/Cargo.toml");
        assert!(
            content.contains("workspace = true"),
            "target-specific dep should use workspace inheritance, got:\n{content}"
        );
        assert!(
            !content.contains("winapi = \"0.3\""),
            "explicit version should be removed, got:\n{content}"
        );

        // Re-run checks: should be clean
        let ws2 = parse_workspace(tmp.path()).unwrap();
        let diags2 = check::run_checks(&ws2, 2);
        assert!(
            diags2.is_empty(),
            "expected no diagnostics after fix, got: {diags2:?}"
        );
    }

    #[test]
    fn test_fix_strips_registry_from_member() {
        let tmp = copy_fixture("registry_not_inherited");
        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);

        apply_fixes(tmp.path(), &diags).unwrap();

        let content = read_file(&tmp, "crates/app/Cargo.toml");
        assert!(content.contains("workspace = true"));
        assert!(
            !content.contains("registry"),
            "registry should be stripped from member dep, got:\n{content}"
        );
    }

    #[test]
    fn test_fix_promotion_carries_registry() {
        let tmp = copy_fixture("registry_promotion");
        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);

        apply_fixes(tmp.path(), &diags).unwrap();

        let root = read_file(&tmp, "Cargo.toml");
        assert!(
            root.contains("registry = \"my-registry\""),
            "promoted workspace dep should include registry, got:\n{root}"
        );

        // Members should NOT have registry (workspace owns it)
        for name in &["one", "two"] {
            let content = read_file(&tmp, &format!("crates/{name}/Cargo.toml"));
            assert!(content.contains("workspace = true"));
            assert!(
                !content.contains("registry"),
                "member {name} should not have registry after fix, got:\n{content}"
            );
        }
    }

    #[test]
    fn test_fix_promotion_with_target_specific_default_features() {
        let tmp = copy_fixture("target_deps_promotion");
        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);
        assert!(matches!(
            diags[0].kind,
            DiagnosticKind::PromotionCandidate { .. }
        ));

        apply_fixes(tmp.path(), &diags).unwrap();

        let root = read_file(&tmp, "Cargo.toml");
        assert!(
            root.contains("default-features = false"),
            "workspace dep should have default-features = false from target deps, got:\n{root}"
        );

        for name in &["one", "two"] {
            let member = read_file(&tmp, &format!("crates/{name}/Cargo.toml"));
            assert!(
                !member.contains("default-features"),
                "member {name} should not have default-features after fix, got:\n{member}"
            );
        }
    }
}
