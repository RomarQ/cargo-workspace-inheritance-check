use std::collections::{BTreeMap, HashMap};

use crate::diagnostic::{CheckKind, Diagnostic, Severity};
use crate::workspace::WorkspaceInfo;

pub fn run_checks(workspace: &WorkspaceInfo, promotion_threshold: usize) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    // Checks 1 & 2: not-inherited and version-mismatch
    for member in &workspace.members {
        let member_rel = member
            .manifest_path
            .strip_prefix(&workspace.root_path)
            .unwrap_or(&member.manifest_path)
            .to_string_lossy()
            .to_string();

        for dep in &member.dependencies {
            if dep.workspace {
                continue;
            }

            let lookup_name = dep.package.as_deref().unwrap_or(&dep.name);

            if let Some(ws_dep) = workspace.workspace_deps.get(lookup_name) {
                if let (Some(dep_ver), Some(ws_ver)) = (&dep.version, &ws_dep.version) {
                    if dep_ver == ws_ver {
                        diagnostics.push(Diagnostic {
                            severity: Severity::Error,
                            check: CheckKind::NotInherited,
                            dependency: lookup_name.to_string(),
                            version: Some(dep_ver.clone()),
                            member: Some(member_rel.clone()),
                            workspace_version: Some(ws_ver.clone()),
                            count: None,
                            members: None,
                            suggested_version: None,
                        });
                    } else {
                        diagnostics.push(Diagnostic {
                            severity: Severity::Error,
                            check: CheckKind::VersionMismatch,
                            dependency: lookup_name.to_string(),
                            version: Some(dep_ver.clone()),
                            member: Some(member_rel.clone()),
                            workspace_version: Some(ws_ver.clone()),
                            count: None,
                            members: None,
                            suggested_version: None,
                        });
                    }
                } else {
                    // Workspace dep has no version but member should still inherit
                    diagnostics.push(Diagnostic {
                        severity: Severity::Error,
                        check: CheckKind::NotInherited,
                        dependency: lookup_name.to_string(),
                        version: dep.version.clone(),
                        member: Some(member_rel.clone()),
                        workspace_version: None,
                        count: None,
                        members: None,
                        suggested_version: None,
                    });
                }
            }
        }
    }

    // Check 3: promotion candidates
    let mut dep_usage: BTreeMap<String, Vec<(String, Option<String>)>> = BTreeMap::new();
    for member in &workspace.members {
        let member_rel = member
            .manifest_path
            .strip_prefix(&workspace.root_path)
            .unwrap_or(&member.manifest_path)
            .to_string_lossy()
            .to_string();

        for dep in &member.dependencies {
            if dep.workspace {
                continue;
            }
            let lookup_name = dep.package.as_deref().unwrap_or(&dep.name);
            if workspace.workspace_deps.contains_key(lookup_name) {
                continue;
            }
            dep_usage
                .entry(lookup_name.to_string())
                .or_default()
                .push((member_rel.clone(), dep.version.clone()));
        }
    }

    for (dep_name, usages) in &dep_usage {
        if usages.len() >= promotion_threshold {
            let mut version_counts: HashMap<&str, usize> = HashMap::new();
            for (_, ver) in usages {
                if let Some(v) = ver.as_deref() {
                    *version_counts.entry(v).or_default() += 1;
                }
            }
            let suggested_version = version_counts
                .into_iter()
                .max_by(|(v1, c1), (v2, c2)| c1.cmp(c2).then_with(|| v1.cmp(v2)))
                .map(|(v, _)| v.to_string());

            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                check: CheckKind::PromotionCandidate,
                dependency: dep_name.clone(),
                version: None,
                member: None,
                workspace_version: None,
                count: Some(usages.len()),
                members: Some(usages.iter().map(|(m, _)| m.clone()).collect()),
                suggested_version,
            });
        }
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::parse_workspace;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn test_valid_workspace_no_diagnostics() {
        let ws = parse_workspace(&fixture("valid_workspace")).unwrap();
        let diags = run_checks(&ws, 2);
        assert!(diags.is_empty());
    }

    #[test]
    fn test_not_inherited() {
        let ws = parse_workspace(&fixture("not_inherited")).unwrap();
        let diags = run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].dependency, "serde");
        assert!(matches!(diags[0].check, CheckKind::NotInherited));
    }

    #[test]
    fn test_version_mismatch() {
        let ws = parse_workspace(&fixture("version_mismatch")).unwrap();
        let diags = run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].dependency, "rand");
        assert!(matches!(diags[0].check, CheckKind::VersionMismatch));
        assert_eq!(diags[0].version.as_deref(), Some("0.7"));
        assert_eq!(diags[0].workspace_version.as_deref(), Some("0.8"));
    }

    #[test]
    fn test_promotion_candidate() {
        let ws = parse_workspace(&fixture("promotion_candidate")).unwrap();
        let diags = run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].dependency, "serde_yaml");
        assert!(matches!(diags[0].check, CheckKind::PromotionCandidate));
        assert_eq!(diags[0].count, Some(2));
    }

    #[test]
    fn test_promotion_threshold_filters() {
        let ws = parse_workspace(&fixture("promotion_candidate")).unwrap();
        let diags = run_checks(&ws, 3);
        assert!(
            diags.is_empty(),
            "threshold 3 should filter out serde_yaml appearing in 2 crates"
        );
    }

    #[test]
    fn test_exclude_not_checked() {
        let ws = parse_workspace(&fixture("with_exclude")).unwrap();
        let diags = run_checks(&ws, 2);
        assert!(
            diags.is_empty(),
            "excluded member should not generate diagnostics"
        );
    }

    #[test]
    fn test_target_specific_not_inherited() {
        let ws = parse_workspace(&fixture("target_deps")).unwrap();
        let diags = run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].dependency, "winapi");
        assert!(matches!(diags[0].check, CheckKind::NotInherited));
    }
}
