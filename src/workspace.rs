use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;

pub struct WorkspaceInfo {
    pub root_path: PathBuf,
    pub workspace_deps: BTreeMap<String, WorkspaceDep>,
    pub members: Vec<MemberCrate>,
}

pub struct WorkspaceDep {
    pub version: Option<String>,
}

pub struct MemberCrate {
    pub manifest_path: PathBuf,
    pub dependencies: Vec<MemberDep>,
}

pub struct MemberDep {
    pub name: String,
    pub package: Option<String>,
    pub version: Option<String>,
    pub workspace: bool,
}

pub(crate) const DEP_SECTIONS: [&str; 3] =
    ["dependencies", "dev-dependencies", "build-dependencies"];

pub fn parse_workspace(path: &Path) -> Result<WorkspaceInfo, String> {
    let root_toml_path = path.join("Cargo.toml");
    let content = std::fs::read_to_string(&root_toml_path)
        .map_err(|e| format!("Failed to read {}: {e}", root_toml_path.display()))?;
    let doc: DocumentMut = content
        .parse()
        .map_err(|e| format!("Failed to parse {}: {e}", root_toml_path.display()))?;

    let workspace_table = doc
        .get("workspace")
        .and_then(|v| v.as_table())
        .ok_or_else(|| format!("No [workspace] section in {}", root_toml_path.display()))?;

    let workspace_deps = parse_workspace_deps(workspace_table);

    let member_patterns: Vec<String> = workspace_table
        .get("members")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let exclude_patterns: Vec<String> = workspace_table
        .get("exclude")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let member_paths = expand_members(path, &member_patterns, &exclude_patterns)?;

    let mut members = Vec::new();
    for member_path in member_paths {
        let manifest_path = member_path.join("Cargo.toml");
        match parse_member(&manifest_path) {
            Ok(member) => members.push(member),
            Err(e) => eprintln!("warning: skipping {}: {e}", manifest_path.display()),
        }
    }

    Ok(WorkspaceInfo {
        root_path: path.to_path_buf(),
        workspace_deps,
        members,
    })
}

fn parse_workspace_deps(workspace_table: &toml_edit::Table) -> BTreeMap<String, WorkspaceDep> {
    let mut deps = BTreeMap::new();
    let Some(dep_table) = workspace_table
        .get("dependencies")
        .and_then(|v| v.as_table())
    else {
        return deps;
    };
    for (name, item) in dep_table {
        let version = item
            .as_str()
            .map(String::from)
            .or_else(|| {
                item.as_table()
                    .and_then(|t| t.get("version"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| {
                item.as_inline_table()
                    .and_then(|t| t.get("version"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });
        deps.insert(name.to_string(), WorkspaceDep { version });
    }
    deps
}

fn expand_members(
    root: &Path,
    patterns: &[String],
    excludes: &[String],
) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    for pattern in patterns {
        let full_pattern = root.join(pattern).to_string_lossy().to_string();
        let matches = glob::glob(&full_pattern)
            .map_err(|e| format!("Invalid glob pattern '{pattern}': {e}"))?;
        for entry in matches {
            let entry = entry.map_err(|e| format!("Glob error: {e}"))?;
            if entry.join("Cargo.toml").exists() {
                paths.push(entry);
            }
        }
    }
    if !excludes.is_empty() {
        let mut excluded_paths = Vec::new();
        for pattern in excludes {
            let full_pattern = root.join(pattern).to_string_lossy().to_string();
            if let Ok(matches) = glob::glob(&full_pattern) {
                for entry in matches.flatten() {
                    excluded_paths.push(entry);
                }
            }
        }
        paths.retain(|p| !excluded_paths.iter().any(|ex| p.starts_with(ex)));
    }
    paths.sort();
    Ok(paths)
}

fn parse_member(manifest_path: &Path) -> Result<MemberCrate, String> {
    let content = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("Failed to read {}: {e}", manifest_path.display()))?;
    let doc: DocumentMut = content
        .parse()
        .map_err(|e| format!("Failed to parse {}: {e}", manifest_path.display()))?;

    let mut dependencies = Vec::new();

    for section in &DEP_SECTIONS {
        if let Some(table) = doc.get(*section).and_then(|v| v.as_table()) {
            parse_dep_table(table, &mut dependencies);
        }
    }

    // Also check target-specific deps
    if let Some(target_table) = doc.get("target").and_then(|v| v.as_table()) {
        for (_target_cfg, target_value) in target_table {
            if let Some(target_tbl) = target_value.as_table() {
                for section in &DEP_SECTIONS {
                    if let Some(dep_table) = target_tbl.get(*section).and_then(|v| v.as_table()) {
                        parse_dep_table(dep_table, &mut dependencies);
                    }
                }
            }
        }
    }

    Ok(MemberCrate {
        manifest_path: manifest_path.to_path_buf(),
        dependencies,
    })
}

fn parse_dep_table(table: &toml_edit::Table, deps: &mut Vec<MemberDep>) {
    for (key, item) in table {
        let (version, workspace, package) = if let Some(s) = item.as_str() {
            (Some(s.to_string()), false, None)
        } else if let Some(t) = item.as_table().map(|t| t as &dyn toml_edit::TableLike)
            .or_else(|| item.as_inline_table().map(|t| t as &dyn toml_edit::TableLike))
        {
            let version = t.get("version").and_then(|v| v.as_str()).map(String::from);
            let workspace = t
                .get("workspace")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let package = t.get("package").and_then(|v| v.as_str()).map(String::from);

            // Skip pure path/git deps with no version
            if !workspace && version.is_none() && (t.contains_key("path") || t.contains_key("git"))
            {
                continue;
            }

            (version, workspace, package)
        } else {
            continue;
        };

        deps.push(MemberDep {
            name: key.to_string(),
            package,
            version,
            workspace,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn test_parse_valid_workspace() {
        let ws = parse_workspace(&fixture("valid_workspace")).unwrap();
        assert_eq!(ws.workspace_deps.len(), 1);
        assert!(ws.workspace_deps.contains_key("serde"));
        assert_eq!(ws.members.len(), 2);
    }

    #[test]
    fn test_workspace_exclude() {
        let ws = parse_workspace(&fixture("with_exclude")).unwrap();
        assert_eq!(ws.members.len(), 1);
        assert!(
            ws.members[0]
                .manifest_path
                .to_str()
                .unwrap()
                .contains("included")
        );
    }

    #[test]
    fn test_member_deps_parsed() {
        let ws = parse_workspace(&fixture("valid_workspace")).unwrap();
        for member in &ws.members {
            assert!(!member.dependencies.is_empty());
            assert!(member.dependencies[0].workspace);
        }
    }

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
}
