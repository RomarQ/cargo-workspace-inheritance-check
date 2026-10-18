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
            },
            format!("`{key}` in {member} now uses workspace inheritance"),
        ),
    };
    let fix = if !dep.spec.default_features && ws.default_features {
        Err(format!(
            "`{key}` in {member} sets `default-features = false`, so the workspace entry \
             must set it too"
        ))
    } else if let (Some(version), None) = (&dep.spec.version, &ws.version) {
        Err(format!(
            "`{key}` in {member} requires version {version}, but the workspace entry has \
             no version"
        ))
    } else {
        Ok(Fix {
            promotion: None,
            edits: vec![edit(dep, ws.default_features)],
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
    use crate::temp_workspace;
    use crate::workspace::parse_workspace;

    fn check(root: &Path) -> Vec<Diagnostic> {
        let ws = parse_workspace(root).unwrap();
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
        let tmp = temp_workspace(
            "core = { path = \"crates/core\", version = \"0.1\" }\n\
             reg = { path = \"crates/reg\", version = \"0.1\" }\n\
             foo = { git = \"https://example.invalid/foo\", version = \"0.1\" }\nidx = \"0.1\"",
            &[
                ("core", ""),
                ("reg", ""),
                ("other", ""),
                (
                    "a",
                    "core = { path = \"../core\", version = \"0.1\" }\n\
                     reg = { path = \"../reg\", version = \"0.1\", registry = \"my-registry\" }\n\
                     foo = { git = \"https://example.invalid/foo\", branch = \"dev\", version = \"0.1\" }\n\
                     other = { path = \"../other\", version = \"0.1\" }\n\
                     idx = { version = \"0.1\", registry-index = \"sparse+https://example.invalid/index/\" }",
                ),
                (
                    "b",
                    "core = { path = \"../core\" }\nother = { path = \"../other\", version = \"0.1\" }\n\
                     foo = { git = \"https://example.invalid/foo.git/\", version = \"0.1\" }\n\
                     idx = { version = \"0.1\", registry = \"crates-io\" }",
                ),
            ],
        );
        let diags = check(tmp.path());
        let b = "crates/b/Cargo.toml";
        assert_eq!(
            summarize(&diags),
            [("core", "crates/a/Cargo.toml"), ("foo", b), ("idx", b)]
        );
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
        let diags = check(tmp.path());
        assert_eq!(summarize(&diags), [("serde1", "crates/a/Cargo.toml")]);
    }

    #[test]
    fn test_ignore_rule_member_scopes() {
        let tmp = temp_workspace(
            "rand = \"0.8\"\nserde = \"1.0\"\n\n[workspace.metadata.inheritance-check]\nignore = [\n\
             { dependency = \"rand\", member = \"crates/bar\" },\n\
             { dependency = \"rand\", member = \"./crates/baz/\" },\n\
             { dependency = \"serde\", member = \"crates/baz/Cargo.toml\" },\n\
             { dependency = \"openssl\", member = \".\" },\n\
             { dependency = \"log\" },\n]",
            &[
                ("bar", "rand = \"0.7\"\nopenssl = \"0.10\"\nlog = \"0.4\""),
                ("barista", "rand = \"0.7\"\nserde = \"1.0\"\nlog = \"0.4\""),
                ("baz", "rand = \"0.7\"\nserde = \"1.0\"\nopenssl = \"0.10\""),
            ],
        );
        let diags = check(tmp.path());
        let barista = "crates/barista/Cargo.toml";
        assert_eq!(summarize(&diags), [("rand", barista), ("serde", barista)]);
    }

    #[test]
    fn test_ignore_rule_matches_key_or_package() {
        let renamed = "serde1 = { package = \"serde\", version = \"1.0\" }\n\
                       md5 = { package = \"md-5\", version = \"0.10\" }";
        let ignore = "[[workspace.metadata.inheritance-check.ignore]]";
        let tmp = temp_workspace(
            &format!(
                "{renamed}\n\n{ignore}\ndependency = \"serde\"\n\n{ignore}\ndependency = \"md5\""
            ),
            &[("a", renamed)],
        );
        let diags = check(tmp.path());
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn test_promotion_counts_crates_and_suggests_the_most_common_version() {
        let registry_dep = "my-crate = { version = \"1.0\", registry = \"my-registry\" }";
        let tmp = temp_workspace(
            "",
            &[
                (
                    "a",
                    &format!("rand = \"0.9\"\nitertools = \"0.10\"\n{registry_dep}"),
                ),
                (
                    "b",
                    &format!(
                        "rand = \"0.9\"\nitertools = \"0.9\"\n{registry_dep}\n\n\
                         [dev-dependencies]\nitertools = \"0.9\""
                    ),
                ),
                ("c", "rand = \"0.8\""),
            ],
        );
        let diags = check(tmp.path());
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

    #[test]
    fn test_reads_every_dependency_table_except_a_shadowed_one() {
        let tmp = temp_workspace(
            "serde = \"1.0\"\ncc = \"1.0\"",
            &[(
                "app",
                "\n[dev-dependencies]\nserde = \"1.0\"\n\n[dev_dependencies]\ncc = \"1.0\"\n\n\
                 [build_dependencies]\ncc = \"1.0\"\n\n\
                 [target]\n\"cfg(unix)\" = { dev_dependencies = { cc = \"1.0\" } }",
            )],
        );
        let diags = check(tmp.path());
        let app = "crates/app/Cargo.toml";
        assert_eq!(
            summarize(&diags),
            [("serde", app), ("cc", app), ("cc", app)]
        );
    }
}
