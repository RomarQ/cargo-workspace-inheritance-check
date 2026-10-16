//! Compares each member's own dependency entries with `[workspace.dependencies]`, and decides
//! what `--fix` does about each problem.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::fix::{Edit, Fix, Promotion};
use crate::workspace::{DepSource, DepSpec, MemberDep, WorkspaceInfo, relative_manifest};

/// A reported problem, with the fix that `--fix` applies or the reason it applies none.
#[derive(Debug)]
pub struct Finding {
    pub diagnostic: Diagnostic,
    pub(crate) fix: Result<Fix, String>,
}

/// An entry that could move to `[workspace.dependencies]`: the member's relative manifest
/// path, its manifest path, and the entry.
type Usage<'a> = (String, &'a Path, &'a MemberDep);

/// Check every member entry that does not inherit from the workspace.
///
/// An entry inherits by its key, as in cargo. So it is reported against the workspace entry
/// with the same key, when both name the same package from the same source. A registry
/// dependency that is not renamed and whose key is not in the workspace is a promotion
/// candidate when at least `promotion_threshold` crates declare it with the same registry.
/// Entries that an ignore rule covers are skipped.
pub fn run_checks(workspace: &WorkspaceInfo, promotion_threshold: usize) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut candidates: BTreeMap<(&str, Option<&str>), Vec<Usage>> = BTreeMap::new();

    for member in &workspace.members {
        let member_rel = relative_manifest(&workspace.root_path, &member.manifest_path);
        for dep in &member.dependencies {
            let ignored = workspace
                .ignore_rules
                .iter()
                .any(|r| r.matches(&dep.key, &dep.spec.package, &member_rel));
            if ignored {
                continue;
            }
            match workspace.workspace_deps.get(&dep.key) {
                Some(ws) if ws.package == dep.spec.package && ws.source == dep.spec.source => {
                    findings.push(not_inherited(dep, ws, &member.manifest_path, &member_rel));
                }
                // The workspace uses this key for another package or source, so the entry
                // can neither inherit it nor be promoted under it.
                Some(_) => {}
                None => {
                    if let DepSource::Registry(registry) = &dep.spec.source
                        && dep.spec.package == dep.key
                    {
                        candidates
                            .entry((&dep.key, registry.as_deref()))
                            .or_default()
                            .push((member_rel.clone(), &member.manifest_path, dep));
                    }
                }
            }
        }
    }

    let root_manifest = workspace.root_path.join("Cargo.toml");
    for ((key, registry), usages) in candidates {
        let mut members: Vec<&str> = usages.iter().map(|(m, ..)| m.as_str()).collect();
        // A member's usages are next to each other, because members are checked in turn.
        members.dedup();
        if members.len() < promotion_threshold {
            continue;
        }
        let diagnostic = Diagnostic {
            severity: Severity::Warning,
            dependency: key.to_string(),
            kind: DiagnosticKind::PromotionCandidate {
                count: members.len(),
                members: members.iter().map(|m| m.to_string()).collect(),
                suggested_version: most_common_version(&usages),
                suggested_registry: registry.map(String::from),
            },
        };
        let fix = promotion_fix(key, registry, &usages, &members, &root_manifest);
        findings.push(Finding { diagnostic, fix });
    }

    findings
}

fn not_inherited(dep: &MemberDep, ws: &DepSpec, manifest: &Path, member_rel: &str) -> Finding {
    let key = &dep.key;
    let member = member_rel.to_string();
    let (kind, description) = match (&dep.spec.version, &ws.version) {
        (Some(from), Some(to)) if from != to => (
            DiagnosticKind::VersionMismatch {
                version: Some(from.clone()),
                member,
                workspace_version: Some(to.clone()),
            },
            format!("`{key}` in {member_rel} changed from {from} to {to} (workspace version)"),
        ),
        (version, workspace_version) => (
            DiagnosticKind::NotInherited {
                version: version.clone(),
                member,
                workspace_version: workspace_version.clone(),
            },
            format!("`{key}` in {member_rel} now uses workspace inheritance"),
        ),
    };
    let fix = if !dep.spec.default_features && ws.default_features {
        Err(format!(
            "`{key}` in {member_rel} sets `default-features = false`, so the workspace entry \
             must set it too"
        ))
    } else if let (Some(version), None) = (&dep.spec.version, &ws.version) {
        Err(format!(
            "`{key}` in {member_rel} requires version {version}, but the workspace entry has \
             no version"
        ))
    } else {
        Ok(Fix {
            promotion: None,
            edits: vec![edit(manifest, dep, ws.default_features)],
            description,
        })
    };
    let diagnostic = Diagnostic {
        severity: Severity::Error,
        dependency: key.clone(),
        kind,
    };
    Finding { diagnostic, fix }
}

/// Promote only a version that every usage already declares, so `--fix` changes no member's
/// version requirement.
fn promotion_fix(
    key: &str,
    registry: Option<&str>,
    usages: &[Usage],
    members: &[&str],
    root_manifest: &Path,
) -> Result<Fix, String> {
    let versions: BTreeSet<Option<&str>> = usages
        .iter()
        .map(|(.., dep)| dep.spec.version.as_deref())
        .collect();
    let version = match versions.into_iter().collect::<Vec<_>>()[..] {
        [Some(version)] => version,
        [None] => {
            return Err(format!(
                "`{key}` is not promoted, because no member gives a version"
            ));
        }
        _ => {
            let listed: Vec<String> = usages
                .iter()
                .map(|(m, _, dep)| format!("{m} ({})", dep.spec.version.as_deref().unwrap_or("*")))
                .collect();
            return Err(format!(
                "`{key}` is not promoted, because members use different versions: {}",
                listed.join(", "),
            ));
        }
    };
    // Cargo ignores or rejects a member's `default-features = false` unless the workspace
    // entry also sets it, so one such member decides the workspace entry.
    let default_features = usages.iter().all(|(.., dep)| dep.spec.default_features);
    let promotion = Promotion {
        root_manifest: root_manifest.to_path_buf(),
        key: key.to_string(),
        version: version.to_string(),
        registry: registry.map(String::from),
        default_features,
    };
    let description = format!(
        "`{key} = {}` added to [workspace.dependencies], updated: {}",
        promotion.value(),
        members.join(", "),
    );
    Ok(Fix {
        promotion: Some(promotion),
        edits: usages
            .iter()
            .map(|(_, manifest, dep)| edit(manifest, dep, default_features))
            .collect(),
        description,
    })
}

fn edit(manifest: &Path, dep: &MemberDep, ws_default_features: bool) -> Edit {
    Edit {
        manifest: manifest.to_path_buf(),
        table: dep.table.clone(),
        key: dep.key.clone(),
        enable_default_features: dep.spec.default_features && !ws_default_features,
    }
}

/// The version that most crates declare, or `None` when no crate gives a version or several
/// versions share the highest count.
fn most_common_version(usages: &[Usage]) -> Option<String> {
    let declared: BTreeSet<(&str, &str)> = usages
        .iter()
        .filter_map(|(member, _, dep)| Some((member.as_str(), dep.spec.version.as_deref()?)))
        .collect();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, version) in declared {
        *counts.entry(version).or_default() += 1;
    }
    let highest = *counts.values().max()?;
    let mut top = counts.into_iter().filter(|&(_, count)| count == highest);
    let (version, _) = top.next()?;
    top.next().is_none().then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::parse_workspace;
    use crate::{add_root_package, fixture, temp_workspace};

    fn check(root: &Path, promotion_threshold: usize) -> Vec<Diagnostic> {
        let ws = parse_workspace(root).unwrap();
        run_checks(&ws, promotion_threshold)
            .into_iter()
            .map(|f| f.diagnostic)
            .collect()
    }

    /// The (dependency, member) of each error, and (dependency, "promotion") of each
    /// promotion candidate.
    fn summarize(diags: &[Diagnostic]) -> Vec<(&str, &str)> {
        diags
            .iter()
            .map(|d| match &d.kind {
                DiagnosticKind::NotInherited { member, .. }
                | DiagnosticKind::VersionMismatch { member, .. } => {
                    (d.dependency.as_str(), member.as_str())
                }
                DiagnosticKind::PromotionCandidate { .. } => (d.dependency.as_str(), "promotion"),
            })
            .collect()
    }

    #[test]
    fn test_valid_workspace_no_diagnostics() {
        assert!(check(&fixture("valid_workspace"), 2).is_empty());
    }

    #[test]
    fn test_not_inherited() {
        let diags = check(&fixture("not_inherited"), 2);
        assert_eq!(summarize(&diags), [("serde", "crates/foo/Cargo.toml")]);
        assert!(matches!(diags[0].kind, DiagnosticKind::NotInherited { .. }));
    }

    #[test]
    fn test_version_mismatch() {
        let diags = check(&fixture("version_mismatch"), 2);
        let [
            Diagnostic {
                dependency,
                kind:
                    DiagnosticKind::VersionMismatch {
                        version,
                        workspace_version,
                        ..
                    },
                ..
            },
        ] = &diags[..]
        else {
            panic!("{diags:?}");
        };
        assert_eq!(dependency, "rand");
        assert_eq!(version.as_deref(), Some("0.7"));
        assert_eq!(workspace_version.as_deref(), Some("0.8"));
    }

    #[test]
    fn test_promotion_candidate() {
        let diags = check(&fixture("promotion_candidate"), 2);
        let [
            Diagnostic {
                dependency,
                kind: DiagnosticKind::PromotionCandidate { count, .. },
                ..
            },
        ] = &diags[..]
        else {
            panic!("{diags:?}");
        };
        assert_eq!(dependency, "serde_yaml");
        assert_eq!(*count, 2);
    }

    #[test]
    fn test_target_specific_not_inherited() {
        let diags = check(&fixture("target_deps"), 2);
        assert_eq!(summarize(&diags), [("winapi", "crates/plat/Cargo.toml")]);
    }

    #[test]
    fn test_registry_mismatch_neither_matched_nor_promoted() {
        let diags = check(&fixture("registry_mismatch"), 1);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn test_promotion_candidate_with_registry() {
        let diags = check(&fixture("registry_promotion"), 2);
        let [
            Diagnostic {
                kind:
                    DiagnosticKind::PromotionCandidate {
                        suggested_registry, ..
                    },
                ..
            },
        ] = &diags[..]
        else {
            panic!("{diags:?}");
        };
        assert_eq!(suggested_registry.as_deref(), Some("my-registry"));
    }

    #[test]
    fn test_different_registries_not_grouped_for_promotion() {
        let diags = check(&fixture("registry_different"), 2);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn test_same_registry_flagged_as_not_inherited() {
        let diags = check(&fixture("registry_not_inherited"), 2);
        assert_eq!(summarize(&diags), [("my-crate", "crates/app/Cargo.toml")]);
    }

    #[test]
    fn test_root_package_is_checked() {
        let tmp = temp_workspace("serde = \"1.0\"", &[("a", "")]);
        add_root_package(&tmp, "serde = \"1.0\"");
        let diags = check(tmp.path(), 2);
        assert_eq!(summarize(&diags), [("serde", "Cargo.toml")]);
    }

    #[test]
    fn test_other_sources_are_neither_matched_nor_promoted() {
        let tmp = temp_workspace(
            "core = { path = \"crates/core\", version = \"0.1\" }\nlocal = \"0.1\"\nidx = \"0.1\"",
            &[
                ("core", ""),
                ("local", ""),
                ("other", ""),
                (
                    "a",
                    "core = { path = \"../core\", version = \"0.1\" }\n\
                     local = { path = \"../local\", version = \"0.1\" }\n\
                     other = { path = \"../other\", version = \"0.1\" }\n\
                     idx = { version = \"0.1\", registry-index = \"sparse+https://example.invalid/index/\" }",
                ),
                (
                    "b",
                    "local = { path = \"../local\", version = \"0.1\" }\n\
                     other = { path = \"../other\", version = \"0.1\" }",
                ),
            ],
        );
        let diags = check(tmp.path(), 2);
        assert_eq!(summarize(&diags), [("core", "crates/a/Cargo.toml")]);
    }

    #[test]
    fn test_equivalent_sources_match() {
        let tmp = temp_workspace(
            "foo = { git = \"https://example.invalid/foo\", version = \"0.1\" }\nbar = \"1\"",
            &[(
                "a",
                "foo = { git = \"https://example.invalid/foo.git/\", version = \"0.1\" }\n\
                 bar = { version = \"1\", registry = \"crates-io\" }",
            )],
        );
        let diags = check(tmp.path(), 2);
        let a = "crates/a/Cargo.toml";
        assert_eq!(summarize(&diags), [("foo", a), ("bar", a)]);
    }

    #[test]
    fn test_renamed_deps_follow_the_key() {
        let tmp = temp_workspace(
            "serde1 = { package = \"serde\", version = \"1.0\" }\n\
             md5 = { package = \"md-5\", version = \"0.10\" }\nrand = \"0.8\"",
            &[
                (
                    "a",
                    "serde1 = { package = \"serde\", version = \"1.0\" }\nmd5 = \"0.7\"\n\
                     rand_old = { package = \"rand\", version = \"0.7\" }",
                ),
                ("b", "rand_old = { package = \"rand\", version = \"0.7\" }"),
            ],
        );
        let diags = check(tmp.path(), 2);
        assert_eq!(summarize(&diags), [("serde1", "crates/a/Cargo.toml")]);
    }

    #[test]
    fn test_ignore_rules() {
        let tmp = temp_workspace(
            "rand = \"0.8\"\n\n[workspace.metadata.inheritance-check]\nignore = [\n\
             { dependency = \"rand\", member = \"crates/bar\" },\n\
             { dependency = \"rand\", member = \"./crates/baz/\" },\n\
             { dependency = \"openssl\" },\n]",
            &[
                ("bar", "rand = \"0.7\"\nopenssl = \"0.10\""),
                ("barista", "rand = \"0.7\"\nopenssl = \"0.10\""),
                ("baz", "rand = \"0.7\""),
            ],
        );
        let diags = check(tmp.path(), 2);
        assert_eq!(summarize(&diags), [("rand", "crates/barista/Cargo.toml")]);
    }

    #[test]
    fn test_ignore_rule_scopes_for_the_root() {
        let tmp = temp_workspace(
            "serde = \"1.0\"\nrand = \"0.8\"\n\n[workspace.metadata.inheritance-check]\nignore = [\n\
             { dependency = \"serde\", member = \"Cargo.toml\" },\n\
             { dependency = \"rand\", member = \".\" },\n]",
            &[("a", "serde = \"1.0\"\nrand = \"0.8\"")],
        );
        add_root_package(&tmp, "serde = \"1.0\"\nrand = \"0.8\"");
        let diags = check(tmp.path(), 2);
        assert_eq!(summarize(&diags), [("serde", "crates/a/Cargo.toml")]);
    }

    #[test]
    fn test_promotion_counts_crates_not_sections() {
        let tmp = temp_workspace(
            "",
            &[("a", "tokio = \"1\"\n\n[dev-dependencies]\ntokio = \"1\"")],
        );
        let diags = check(tmp.path(), 2);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn test_suggested_version_is_most_common_and_none_on_a_tie() {
        let tmp = temp_workspace(
            "",
            &[
                ("a", "rand = \"0.9\"\nitertools = \"0.10\""),
                (
                    "b",
                    "rand = \"0.9\"\nitertools = \"0.9\"\n\n[dev-dependencies]\nitertools = \"0.9\"",
                ),
                ("c", "rand = \"0.8\""),
            ],
        );
        let diags = check(tmp.path(), 2);
        let suggested: Vec<_> = diags
            .iter()
            .map(|d| match &d.kind {
                DiagnosticKind::PromotionCandidate {
                    suggested_version, ..
                } => (d.dependency.as_str(), suggested_version.as_deref()),
                _ => panic!("{diags:?}"),
            })
            .collect();
        assert_eq!(suggested, [("itertools", None), ("rand", Some("0.9"))]);
    }

    #[test]
    fn test_dev_and_build_dependencies_checked() {
        let tmp = temp_workspace(
            "serde = \"1.0\"\ncc = \"1.0\"",
            &[(
                "app",
                "\n[dev-dependencies]\nserde = \"1.0\"\n\n[build_dependencies]\ncc = \"1.0\"",
            )],
        );
        let diags = check(tmp.path(), 2);
        let mut found = summarize(&diags);
        found.sort();
        let app = "crates/app/Cargo.toml";
        assert_eq!(found, [("cc", app), ("serde", app)]);
    }

    #[test]
    fn test_inline_tables_are_read() {
        let tmp = temp_workspace(
            "serde = \"1.0\"",
            &[
                (
                    "a",
                    "\n[target.'cfg(unix)']\ndependencies = { serde = \"1.0\" }",
                ),
                (
                    "b",
                    "\n[target]\n\"cfg(unix)\" = { dependencies = { serde = \"1.0\" } }",
                ),
            ],
        );
        let diags = check(tmp.path(), 2);
        assert_eq!(
            summarize(&diags),
            [
                ("serde", "crates/a/Cargo.toml"),
                ("serde", "crates/b/Cargo.toml")
            ]
        );
    }
}
