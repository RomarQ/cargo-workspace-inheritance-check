use std::collections::{BTreeMap, HashMap};

use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::workspace::WorkspaceInfo;

/// Key: (dep_name, registry). Value: list of (member_path, version).
type DepUsageMap = BTreeMap<(String, Option<String>), Vec<(String, Option<String>)>>;

pub fn run_checks(workspace: &WorkspaceInfo, promotion_threshold: usize) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut dep_usage: DepUsageMap = BTreeMap::new();

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

            if let Some(ws_dep) = workspace.workspace_deps.get(lookup_name)
                && dep.registry == ws_dep.registry
            {
                let kind = match (&dep.version, &ws_dep.version) {
                    (Some(dv), Some(wv)) if dv != wv => DiagnosticKind::VersionMismatch {
                        version: dep.version.clone(),
                        member: member_rel.clone(),
                        workspace_version: ws_dep.version.clone(),
                    },
                    _ => DiagnosticKind::NotInherited {
                        version: dep.version.clone(),
                        member: member_rel.clone(),
                        workspace_version: ws_dep.version.clone(),
                    },
                };
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    dependency: lookup_name.to_string(),
                    kind,
                });
            } else {
                dep_usage
                    .entry((lookup_name.to_string(), dep.registry.clone()))
                    .or_default()
                    .push((member_rel.clone(), dep.version.clone()));
            }
        }
    }

    for ((dep_name, registry), usages) in &dep_usage {
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
                dependency: dep_name.clone(),
                kind: DiagnosticKind::PromotionCandidate {
                    count: usages.len(),
                    members: usages.iter().map(|(m, _)| m.clone()).collect(),
                    suggested_version,
                    suggested_registry: registry.clone(),
                },
            });
        }
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use crate::workspace::parse_workspace;

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
        assert!(matches!(diags[0].kind, DiagnosticKind::NotInherited { .. }));
    }

    #[test]
    fn test_version_mismatch() {
        let ws = parse_workspace(&fixture("version_mismatch")).unwrap();
        let diags = run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].dependency, "rand");
        assert!(matches!(
            diags[0].kind,
            DiagnosticKind::VersionMismatch { .. }
        ));
        if let DiagnosticKind::VersionMismatch {
            version,
            workspace_version,
            ..
        } = &diags[0].kind
        {
            assert_eq!(version.as_deref(), Some("0.7"));
            assert_eq!(workspace_version.as_deref(), Some("0.8"));
        }
    }

    #[test]
    fn test_promotion_candidate() {
        let ws = parse_workspace(&fixture("promotion_candidate")).unwrap();
        let diags = run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].dependency, "serde_yaml");
        assert!(matches!(
            diags[0].kind,
            DiagnosticKind::PromotionCandidate { .. }
        ));
        if let DiagnosticKind::PromotionCandidate { count, .. } = &diags[0].kind {
            assert_eq!(*count, 2);
        }
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
        assert!(matches!(diags[0].kind, DiagnosticKind::NotInherited { .. }));
    }

    #[test]
    fn test_registry_mismatch_not_flagged_as_not_inherited() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("crates/app/src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n\
             [workspace.dependencies]\n\
             serde = \"1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("crates/app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n\
             serde = { version = \"1.0\", registry = \"my-registry\" }\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("crates/app/src/lib.rs"), "").unwrap();

        let ws = parse_workspace(dir.path()).unwrap();
        let diags = run_checks(&ws, 2);
        assert!(
            diags.is_empty(),
            "dep from different registry should not match workspace dep: {diags:?}"
        );
    }

    #[test]
    fn test_promotion_candidate_with_registry() {
        let dir = tempfile::tempdir().unwrap();
        for name in &["one", "two"] {
            std::fs::create_dir_all(dir.path().join(format!("crates/{name}/src"))).unwrap();
            std::fs::write(
                dir.path().join(format!("crates/{name}/Cargo.toml")),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                     [dependencies]\n\
                     my-crate = {{ version = \"1.0\", registry = \"my-registry\" }}\n"
                ),
            )
            .unwrap();
            std::fs::write(dir.path().join(format!("crates/{name}/src/lib.rs")), "").unwrap();
        }
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\n",
        )
        .unwrap();

        let ws = parse_workspace(dir.path()).unwrap();
        let diags = run_checks(&ws, 2);
        assert_eq!(diags.len(), 1);
        assert!(matches!(
            diags[0].kind,
            DiagnosticKind::PromotionCandidate { .. }
        ));
        if let DiagnosticKind::PromotionCandidate {
            suggested_registry, ..
        } = &diags[0].kind
        {
            assert_eq!(suggested_registry.as_deref(), Some("my-registry"));
        }
    }

    #[test]
    fn test_different_registries_not_grouped_for_promotion() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("crates/one/src")).unwrap();
        std::fs::write(
            dir.path().join("crates/one/Cargo.toml"),
            "[package]\nname = \"one\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n\
             my-crate = { version = \"1.0\", registry = \"registry-a\" }\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("crates/one/src/lib.rs"), "").unwrap();

        std::fs::create_dir_all(dir.path().join("crates/two/src")).unwrap();
        std::fs::write(
            dir.path().join("crates/two/Cargo.toml"),
            "[package]\nname = \"two\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n\
             my-crate = { version = \"1.0\", registry = \"registry-b\" }\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("crates/two/src/lib.rs"), "").unwrap();

        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\n",
        )
        .unwrap();

        let ws = parse_workspace(dir.path()).unwrap();
        let diags = run_checks(&ws, 2);
        assert!(
            diags.is_empty(),
            "deps from different registries should not be grouped: {diags:?}"
        );
    }

    #[test]
    fn test_same_registry_flagged_as_not_inherited() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("crates/app/src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n\
             [workspace.dependencies]\n\
             my-crate = { version = \"1.0\", registry = \"my-registry\" }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("crates/app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n\
             my-crate = { version = \"1.0\", registry = \"my-registry\" }\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("crates/app/src/lib.rs"), "").unwrap();

        let ws = parse_workspace(dir.path()).unwrap();
        let diags = run_checks(&ws, 2);
        assert_eq!(diags.len(), 1, "same registry should match: {diags:?}");
        assert!(matches!(diags[0].kind, DiagnosticKind::NotInherited { .. }));
    }
}
