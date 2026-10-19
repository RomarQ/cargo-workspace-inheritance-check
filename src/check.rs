//! Compares each member's own dependency entries with `[workspace.dependencies]`, and decides
//! what `--fix` does about each problem.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use toml_edit::{InlineTable, Value};

use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::fix::{Edit, Fix, Promotion};
use crate::workspace::{DepSource, DepSpec, MemberDep, Registry, WorkspaceInfo};

/// A reported problem, with the fix that `--fix` applies or the reason it applies none.
#[derive(Debug)]
pub struct Finding {
    pub diagnostic: Diagnostic,
    pub(crate) fix: Result<Fix, String>,
}

/// Check every member entry that does not inherit from the workspace.
///
/// An entry inherits by its key, as in cargo. So it is reported against the workspace entry with
/// the same key, when both name the same package from the same source. A promotion candidate is an
/// entry from crates.io or a named registry, with no rename and a key that the workspace does not
/// have. At least `promotion_threshold` crates must declare it with the same registry.
pub fn run_checks(workspace: &WorkspaceInfo, promotion_threshold: usize) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut candidates: BTreeMap<(&str, Option<&str>), Vec<&MemberDep>> = BTreeMap::new();

    for dep in &workspace.member_deps {
        match workspace.workspace_deps.get(&dep.key) {
            Some(ws) if ws.package == dep.spec.package && ws.source == dep.spec.source => {
                findings.push(not_inherited(dep, ws));
            }
            None if dep.spec.package == dep.key => {
                let registry = match &dep.spec.source {
                    DepSource::Registry(Registry::CratesIo) => None,
                    DepSource::Registry(Registry::Named(name)) => Some(name.as_str()),
                    _ => continue,
                };
                candidates
                    .entry((&dep.key, registry))
                    .or_default()
                    .push(dep);
            }
            _ => {}
        }
    }

    let root_manifest = workspace.root_path.join("Cargo.toml");
    for ((key, registry), usages) in candidates {
        let mut members: Vec<&str> = usages.iter().map(|dep| dep.member.as_str()).collect();
        // Dedup works because `member_deps` groups the entries by member.
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

fn not_inherited(dep: &MemberDep, ws: &DepSpec) -> Finding {
    let key = &dep.key;
    let member = &dep.member;
    let rewrite = edit(dep, ws.default_features);
    let blocker = blocker(dep, ws);
    let (kind, description) = match (&dep.spec.version, &ws.version) {
        (Some(from), Some(to)) if from != to => (
            DiagnosticKind::VersionMismatch {
                version: from.clone(),
                member: member.clone(),
                workspace_version: to.clone(),
            },
            format!("`{key}` in {member} changed from {from} to {to} (workspace version)"),
        ),
        (version, workspace_version) => (
            DiagnosticKind::NotInherited {
                version: version.clone(),
                member: member.clone(),
                workspace_version: workspace_version.clone(),
                suggestion: match &blocker {
                    Some(blocker) => Err(blocker.clone()),
                    None => Ok(rewrite.entry().to_string()),
                },
            },
            format!("`{key}` in {member} now uses workspace inheritance"),
        ),
    };
    let fix = match blocker {
        Some(blocker) => Err(format!("`{key}` in {member} {blocker}")),
        None => Ok(Fix {
            promotion: None,
            edits: vec![rewrite],
            description,
        }),
    };
    let diagnostic = Diagnostic {
        severity: Severity::Error,
        dependency: key.clone(),
        kind,
    };
    Finding { diagnostic, fix }
}

/// Why `dep` cannot inherit `ws` as it is, as a clause after the entry's name: inheriting would
/// change what cargo resolves, or cargo would reject the inherited entry.
fn blocker(dep: &MemberDep, ws: &DepSpec) -> Option<String> {
    if !dep.spec.default_features && ws.default_features {
        Some("sets `default-features = false`, so the workspace entry must set it too".into())
    } else if let (Some(version), None) = (&dep.spec.version, &ws.version) {
        Some(format!(
            "requires version {version}, but the workspace entry has no version"
        ))
    } else if ws.underscore_default_features && dep.edition.as_str() >= "2024" {
        Some(format!(
            "uses edition {}, so the workspace entry must use `default-features`, not \
             `default_features`",
            dep.edition,
        ))
    } else {
        None
    }
}

/// Promote only a version that every usage already declares, so `--fix` changes no member's
/// version requirement.
fn promotion_fix(
    key: &str,
    registry: Option<&str>,
    usages: &[&MemberDep],
    members: &[&str],
    root_manifest: &Path,
) -> Result<Fix, String> {
    let first = &usages[0].spec.version;
    if usages.iter().any(|dep| &dep.spec.version != first) {
        let listed: Vec<String> = usages
            .iter()
            .map(|dep| {
                let version = dep.spec.version.as_deref().unwrap_or("*");
                format!("{} ({version})", dep.member)
            })
            .collect();
        return Err(format!(
            "`{key}` is not promoted, because members use different versions: {}",
            listed.join(", "),
        ));
    }
    let Some(version) = first else {
        return Err(format!(
            "`{key}` is not promoted, because no member gives a version"
        ));
    };
    // Cargo ignores or rejects a member's `default-features = false` unless the workspace
    // entry also sets it, so one such member decides the workspace entry.
    let default_features = usages.iter().all(|dep| dep.spec.default_features);
    let value = dep_value(version, registry, default_features);
    let description = format!(
        "`{key} = {value}` added to [workspace.dependencies], updated: {}",
        members.join(", "),
    );
    Ok(Fix {
        promotion: Some(Promotion {
            root_manifest: root_manifest.to_path_buf(),
            key: key.to_string(),
            value,
        }),
        edits: usages
            .iter()
            .map(|dep| edit(dep, default_features))
            .collect(),
        description,
    })
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

fn edit(dep: &MemberDep, ws_default_features: bool) -> Edit {
    Edit {
        dep: dep.clone(),
        enable_default_features: dep.spec.default_features && !ws_default_features,
    }
}

fn most_common_version(usages: &[&MemberDep]) -> Option<String> {
    let declared: BTreeSet<(&str, &str)> = usages
        .iter()
        .filter_map(|dep| Some((dep.member.as_str(), dep.spec.version.as_deref()?)))
        .collect();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, version) in declared {
        *counts.entry(version).or_default() += 1;
    }
    let (version, highest) = counts.iter().max_by_key(|&(_, count)| count)?;
    let tied = counts.values().filter(|&count| count == highest).count() > 1;
    (!tied).then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::fixture;
    use crate::workspace::parse_workspace;

    fn check(name: &str) -> Vec<Diagnostic> {
        let ws = parse_workspace(&fixture(name)).unwrap();
        run_checks(&ws, 2)
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
    fn test_sources_decide_what_is_compared_and_promoted() {
        let diags = check("sources");
        let b = "crates/b/Cargo.toml";
        assert_eq!(
            summarize(&diags),
            [("core", "crates/a/Cargo.toml"), ("foo", b), ("idx", b)]
        );
    }

    #[test]
    fn test_renamed_deps_follow_the_key() {
        let diags = check("renamed_deps");
        assert_eq!(summarize(&diags), [("serde1", "crates/a/Cargo.toml")]);
    }

    #[test]
    fn test_ignore_rule_member_scopes() {
        let diags = check("ignore_scopes");
        let barista = "crates/barista/Cargo.toml";
        assert_eq!(summarize(&diags), [("rand", barista), ("serde", barista)]);
    }

    #[test]
    fn test_ignore_rule_matches_key_or_package() {
        let diags = check("ignore_key_or_package");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn test_promotion_counts_crates_and_suggests_the_most_common_version() {
        let diags = check("promotion_counts");
        let found: Vec<_> = diags
            .iter()
            .map(|d| match &d.kind {
                DiagnosticKind::PromotionCandidate {
                    count,
                    suggested_version,
                    suggested_registry,
                    ..
                } => (
                    d.dependency.as_str(),
                    *count,
                    suggested_version.as_deref(),
                    suggested_registry.as_deref(),
                ),
                _ => panic!("{diags:?}"),
            })
            .collect();
        assert_eq!(
            found,
            [
                ("itertools", 2, None, None),
                ("my-crate", 2, Some("1.0"), Some("my-registry")),
                ("rand", 3, Some("0.9"), None),
            ]
        );
    }

    /// The (dependency, suggestion) of each not-inherited error.
    fn suggestions(name: &str) -> Vec<(String, Result<String, String>)> {
        check(name)
            .into_iter()
            .filter_map(|d| match d.kind {
                DiagnosticKind::NotInherited { suggestion, .. } => Some((d.dependency, suggestion)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn test_not_inherited_suggests_the_entry_that_fix_writes_or_why_it_skips() {
        let ok = |key: &str, entry: &str| -> (String, Result<String, String>) {
            (key.into(), Ok(entry.into()))
        };
        assert_eq!(
            suggestions("fix_inheritable_keys"),
            [
                ok(
                    "core",
                    r#"{ workspace = true, features = ["x"], optional = true }"#
                ),
                ok("serde1", "{ workspace = true }"),
                ok("log", "{ workspace = true }"),
                ok(
                    "tokio",
                    r#"{ workspace = true, features = ["full"], default-features = true }"#
                ),
            ]
        );
        let skipped = |key: &str, blocker: &str| -> (String, Result<String, String>) {
            (key.into(), Err(blocker.into()))
        };
        assert_eq!(
            suggestions("fix_skips"),
            [
                skipped(
                    "serde",
                    "sets `default-features = false`, so the workspace entry must set it too"
                ),
                skipped(
                    "core",
                    "requires version 0.1, but the workspace entry has no version"
                ),
            ]
        );
    }

    #[test]
    fn test_reads_every_dependency_table_except_a_shadowed_one() {
        let diags = check("dependency_tables");
        let app = "crates/app/Cargo.toml";
        assert_eq!(
            summarize(&diags),
            [("serde", app), ("cc", app), ("cc", app)]
        );
    }
}
