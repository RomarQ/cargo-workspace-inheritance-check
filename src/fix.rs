use std::collections::BTreeSet;
use std::path::Path;

use toml_edit::{DocumentMut, InlineTable, Item, Value};

use crate::diagnostic::{CheckKind, Diagnostic};

pub struct FixSummary {
    pub fixes_applied: usize,
    pub files_modified: usize,
    pub actions: Vec<String>,
}

pub fn apply_fixes(
    workspace_root: &Path,
    diagnostics: &[Diagnostic],
) -> Result<FixSummary, String> {
    let mut modified_files = BTreeSet::new();
    let mut fixes_applied = 0;
    let mut actions = Vec::new();

    for diag in diagnostics {
        match &diag.check {
            CheckKind::NotInherited => {
                let member = diag.member.as_deref().unwrap_or("?");
                let full_path = workspace_root.join(member);
                if fix_member_dep(&full_path, &diag.dependency)? {
                    modified_files.insert(full_path);
                    fixes_applied += 1;
                    actions.push(format!(
                        "fixed: `{}` in {} now uses workspace inheritance",
                        diag.dependency, member,
                    ));
                }
            }
            CheckKind::VersionMismatch => {
                let member = diag.member.as_deref().unwrap_or("?");
                let dep_ver = diag.version.as_deref().unwrap_or("?");
                let ws_ver = diag.workspace_version.as_deref().unwrap_or("?");
                let full_path = workspace_root.join(member);
                if fix_member_dep(&full_path, &diag.dependency)? {
                    modified_files.insert(full_path);
                    fixes_applied += 1;
                    actions.push(format!(
                        "fixed: `{}` in {} changed from {} to {} (workspace version)",
                        diag.dependency, member, dep_ver, ws_ver,
                    ));
                }
            }
            CheckKind::PromotionCandidate => {
                let Some(version) = &diag.suggested_version else {
                    continue;
                };
                let root_toml = workspace_root.join("Cargo.toml");
                add_workspace_dep(&root_toml, &diag.dependency, version)?;
                modified_files.insert(root_toml);

                if let Some(members) = &diag.members {
                    for member_path in members {
                        let full_path = workspace_root.join(member_path);
                        fix_member_dep(&full_path, &diag.dependency)?;
                        modified_files.insert(full_path);
                    }
                }
                fixes_applied += 1;
                let member_list = diag
                    .members
                    .as_ref()
                    .map(|m| m.join(", "))
                    .unwrap_or_default();
                actions.push(format!(
                    "fixed: `{} = \"{}\"` added to [workspace.dependencies], updated: {member_list}",
                    diag.dependency, version,
                ));
            }
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
        let package_name = item
            .as_inline_table()
            .and_then(|t| t.get("package"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                item.as_table()
                    .and_then(|t| t.get("package"))
                    .and_then(|v| v.as_str())
            });
        if package_name == Some(dep_name) {
            return Some(key.to_string());
        }
    }
    None
}

fn fix_member_dep(manifest_path: &Path, dep_name: &str) -> Result<bool, String> {
    let content = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("Failed to read {}: {e}", manifest_path.display()))?;
    let mut doc: DocumentMut = content
        .parse()
        .map_err(|e| format!("Failed to parse {}: {e}", manifest_path.display()))?;

    let mut modified = false;
    let sections = ["dependencies", "dev-dependencies", "build-dependencies"];

    for section in &sections {
        if let Some(table) = doc.get_mut(section).and_then(|v| v.as_table_like_mut())
            && let Some(key) = find_dep_key(table, dep_name)
            && rewrite_dep_entry(table, &key)
        {
            modified = true;
        }
    }

    // Target-specific dependencies
    if let Some(target_item) = doc.get_mut("target")
        && let Some(target_table) = target_item.as_table_mut()
    {
        let target_keys: Vec<String> = target_table.iter().map(|(k, _)| k.to_string()).collect();
        for target_key in target_keys {
            for section in &sections {
                if let Some(dep_table) = target_table
                    .get_mut(&target_key)
                    .and_then(|v| v.as_table_like_mut())
                    && let Some(dep_tbl) = dep_table
                        .get_mut(section)
                        .and_then(|v| v.as_table_like_mut())
                    && let Some(key) = find_dep_key(dep_tbl, dep_name)
                    && rewrite_dep_entry(dep_tbl, &key)
                {
                    modified = true;
                }
            }
        }
    }

    if modified {
        std::fs::write(manifest_path, doc.to_string())
            .map_err(|e| format!("Failed to write {}: {e}", manifest_path.display()))?;
    }

    Ok(modified)
}

fn rewrite_dep_entry(table: &mut dyn toml_edit::TableLike, key: &str) -> bool {
    let Some(item) = table.get_mut(key) else {
        return false;
    };

    // Handle dotted-key table style: [dependencies.foo]
    if let Some(dep_table) = item.as_table_mut() {
        if dep_table.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
            return false;
        }
        dep_table.remove("version");
        dep_table.insert("workspace", toml_edit::value(true));
        return true;
    }

    // Handle inline styles
    match item.as_value() {
        Some(Value::String(_)) => {
            // `serde = "1.0"` → `serde = { workspace = true }`
            let mut inline = InlineTable::new();
            inline.insert("workspace", Value::from(true));
            *item = Item::Value(Value::InlineTable(inline));
            true
        }
        Some(Value::InlineTable(existing)) => {
            if existing.get("workspace").and_then(|v| v.as_bool()) == Some(true) {
                return false;
            }
            // Rebuild with workspace = true first, preserving other keys except version
            let mut rebuilt = InlineTable::new();
            rebuilt.insert("workspace", Value::from(true));
            for (k, v) in existing.iter() {
                if k != "version" && k != "workspace" {
                    rebuilt.insert(k, v.clone());
                }
            }
            *item = Item::Value(Value::InlineTable(rebuilt));
            true
        }
        _ => false,
    }
}

fn add_workspace_dep(root_toml_path: &Path, dep_name: &str, version: &str) -> Result<(), String> {
    let content = std::fs::read_to_string(root_toml_path)
        .map_err(|e| format!("Failed to read {}: {e}", root_toml_path.display()))?;
    let mut doc: DocumentMut = content
        .parse()
        .map_err(|e| format!("Failed to parse {}: {e}", root_toml_path.display()))?;

    let workspace = doc
        .get_mut("workspace")
        .and_then(|v| v.as_table_mut())
        .ok_or_else(|| format!("No [workspace] in {}", root_toml_path.display()))?;

    if !workspace.contains_key("dependencies") {
        workspace.insert("dependencies", toml_edit::table());
    }

    let ws_deps = workspace
        .get_mut("dependencies")
        .and_then(|v| v.as_table_mut())
        .ok_or("Failed to access [workspace.dependencies]")?;

    if !ws_deps.contains_key(dep_name) {
        ws_deps.insert(dep_name, toml_edit::value(version));
    }

    std::fs::write(root_toml_path, doc.to_string())
        .map_err(|e| format!("Failed to write {}: {e}", root_toml_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check;
    use crate::workspace::parse_workspace;
    use std::path::PathBuf;

    fn copy_fixture(name: &str) -> tempfile::TempDir {
        let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let tmp = tempfile::tempdir().unwrap();
        copy_dir_recursive(&fixture_dir, tmp.path());
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

    #[test]
    fn test_fix_not_inherited() {
        let tmp = copy_fixture("not_inherited");
        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);

        let summary = apply_fixes(tmp.path(), &diags).unwrap();
        assert_eq!(summary.fixes_applied, 1);

        // Verify the file was fixed
        let content = std::fs::read_to_string(tmp.path().join("crates/foo/Cargo.toml")).unwrap();
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

        let content = std::fs::read_to_string(tmp.path().join("crates/bar/Cargo.toml")).unwrap();
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
        assert!(matches!(diags[0].check, CheckKind::PromotionCandidate));

        let summary = apply_fixes(tmp.path(), &diags).unwrap();
        assert_eq!(summary.fixes_applied, 1);

        // Verify root Cargo.toml has the new workspace dep
        let root_content = std::fs::read_to_string(tmp.path().join("Cargo.toml")).unwrap();
        assert!(root_content.contains("serde_yaml"));

        // Verify members use workspace = true
        let one_content =
            std::fs::read_to_string(tmp.path().join("crates/one/Cargo.toml")).unwrap();
        assert!(one_content.contains("workspace = true"));
        assert!(!one_content.contains("serde_yaml = \"0.9\""));

        let two_content =
            std::fs::read_to_string(tmp.path().join("crates/two/Cargo.toml")).unwrap();
        assert!(two_content.contains("workspace = true"));

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
        let tmp = tempfile::tempdir().unwrap();
        // Create a workspace with a dep that has features
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nserde = \"1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("crates/app/src")).unwrap();
        std::fs::write(
            tmp.path().join("crates/app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = { version = \"1.0\", features = [\"derive\"] }\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("crates/app/src/lib.rs"), "").unwrap();

        let ws = parse_workspace(tmp.path()).unwrap();
        let diags = check::run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);

        apply_fixes(tmp.path(), &diags).unwrap();

        let content = std::fs::read_to_string(tmp.path().join("crates/app/Cargo.toml")).unwrap();
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
}
