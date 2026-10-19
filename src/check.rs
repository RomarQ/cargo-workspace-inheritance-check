//! Compares each member's own dependency entries with `[workspace.dependencies]`, and decides
//! what `--fix` does about each problem.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use semver::{Op, VersionReq};
use toml_edit::{InlineTable, Value};

use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::fix::{Edit, Fix, Promotion};
use crate::workspace::{
    DepSource, DepSpec, MemberDep, Registry, WorkspaceInfo, has_inline_comment,
};

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
        // Cargo ignores or rejects a member's `default-features = false` unless the workspace
        // entry also sets it, so one such member decides the workspace entry.
        let default_features = usages.iter().all(|dep| dep.spec.default_features);
        let diagnostic = Diagnostic {
            severity: Severity::Warning,
            dependency: key.to_string(),
            kind: DiagnosticKind::PromotionCandidate {
                count: members.len(),
                members: members.iter().map(|m| m.to_string()).collect(),
                suggested_version: most_common_version(&usages),
                suggested_registry: registry.map(String::from),
                suggested_default_features: default_features,
            },
        };
        let fix = if workspace.root_inline_comment {
            Err(format!(
                "`{key}` is not promoted, because an inline table in the root manifest {LOSES_COMMENT}"
            ))
        } else {
            promotion_fix(
                key,
                registry,
                default_features,
                &usages,
                &members,
                &root_manifest,
            )
        };
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
        (Some(from), Some(to)) if !same_requirement(from, to) => (
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
                    None => Ok(rewrite.suggestion()),
                },
            },
            format!("`{key}` in {member} now uses workspace inheritance"),
        ),
    };
    let fix = match blocker {
        Some(blocker) => Err(format!("`{key}` in {member} {blocker}")),
        None if has_inline_comment(&dep.entry) => {
            Err(format!("`{key}` in {member} {LOSES_COMMENT}"))
        }
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

/// Why `--fix` skips an entry that [`has_inline_comment`].
const LOSES_COMMENT: &str = "has a comment between its braces, and `--fix` would lose it";

/// Why `dep` cannot inherit `ws` as it is, as a clause after the entry's name: inheriting would
/// change what cargo resolves, or cargo would reject the inherited entry.
fn blocker(dep: &MemberDep, ws: &DepSpec) -> Option<String> {
    if !dep.spec.default_features && ws.default_features {
        Some("sets `default-features = false`, so the workspace entry must set it too".into())
    } else if let (Some(version), None) = (&dep.spec.version, &ws.version) {
        Some(format!(
            "requires version {version}, but the workspace entry has no version"
        ))
    } else if let Some(version) = ws
        .version
        .as_ref()
        .filter(|v| VersionReq::parse(v).is_err())
    {
        Some(format!(
            "would inherit version `{version}`, which cargo rejects as a version requirement"
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
    default_features: bool,
    usages: &[&MemberDep],
    members: &[&str],
    root_manifest: &Path,
) -> Result<Fix, String> {
    let first = usages[0].spec.version.as_deref();
    let same_as_first = |dep: &&MemberDep| match (dep.spec.version.as_deref(), first) {
        (Some(version), Some(first)) => same_requirement(version, first),
        (version, first) => version == first,
    };
    if !usages.iter().all(same_as_first) {
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
    // The promotion writes the version that the hint suggests.
    let Some(version) = most_common_version(usages) else {
        return Err(format!(
            "`{key}` is not promoted, because no member gives a version"
        ));
    };
    if let Some(dep) = usages.iter().find(|dep| has_inline_comment(&dep.entry)) {
        return Err(format!(
            "`{key}` is not promoted, because the entry in {} {LOSES_COMMENT}",
            dep.member,
        ));
    }
    let value = dep_value(&version, registry, default_features);
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

/// The requirement that the most crates declare, written as the first crate writes it.
fn most_common_version(usages: &[&MemberDep]) -> Option<String> {
    let declared: BTreeSet<(&str, &str)> = usages
        .iter()
        .filter_map(|dep| Some((dep.member.as_str(), dep.spec.version.as_deref()?)))
        .collect();
    // Each requirement, and the crates that declare it in any spelling.
    let mut crates: Vec<(&str, BTreeSet<&str>)> = Vec::new();
    for (member, version) in declared {
        match crates
            .iter_mut()
            .find(|(seen, _)| same_requirement(seen, version))
        {
            Some((_, members)) => {
                members.insert(member);
            }
            None => crates.push((version, BTreeSet::from([member]))),
        }
    }
    let highest = crates.iter().map(|(_, members)| members.len()).max()?;
    let mut most = crates
        .iter()
        .filter(|(_, members)| members.len() == highest);
    let (version, _) = most.next()?;
    most.next().is_none().then(|| version.to_string())
}

/// `true` when `a` and `b` match the same versions. A caret requirement without a pre-release, such
/// as `1`, `^1.0`, or `^1.*`, compares by its bounds. So `1` and `1.0` are the same, and `0` and
/// `0.0` are not. Any other requirement compares as written.
fn same_requirement(a: &str, b: &str) -> bool {
    match (caret_bounds(a), caret_bounds(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// The lower bound and the excluded upper bound of a single caret requirement without a
/// pre-release, by cargo's caret rules. Build metadata does not count. `None` for any other
/// requirement, and for one that cargo rejects, such as `01`.
fn caret_bounds(requirement: &str) -> Option<([u64; 3], [u64; 3])> {
    let parsed = VersionReq::parse(requirement).ok()?;
    let [comparator] = &parsed.comparators[..] else {
        return None;
    };
    if comparator.op != Op::Caret || !comparator.pre.is_empty() {
        return None;
    }
    let (major, minor, patch) = (comparator.major, comparator.minor, comparator.patch);
    let upper = match (major, minor, patch) {
        (0, Some(0), Some(patch)) => [0, 0, patch.checked_add(1)?],
        (0, Some(minor), _) => [0, minor.checked_add(1)?, 0],
        (major, _, _) => [major.checked_add(1)?, 0, 0],
    };
    Some(([major, minor.unwrap_or(0), patch.unwrap_or(0)], upper))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::fixtures::fixture;
    use crate::workspace::{DepTable, parse_workspace, read_manifest};

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

    #[test]
    fn test_versions_compare_by_what_they_match() {
        let ws = parse_workspace(&fixture("version_spellings")).unwrap();
        let found: Vec<_> = run_checks(&ws, 2)
            .into_iter()
            .map(|f| {
                let kind = match f.diagnostic.kind {
                    DiagnosticKind::NotInherited { .. } => "not-inherited".to_string(),
                    DiagnosticKind::VersionMismatch { .. } => "mismatch".to_string(),
                    DiagnosticKind::PromotionCandidate {
                        suggested_version, ..
                    } => {
                        let written = f.fix.as_ref().ok().and_then(|fix| fix.promotion.as_ref());
                        let written = written.map(|p| p.value.to_string());
                        format!("promotion {suggested_version:?} writes {written:?}")
                    }
                };
                (f.diagnostic.dependency, kind, f.fix.is_ok())
            })
            .collect();
        let expected = [
            ("serde", "not-inherited", true),
            ("log", "not-inherited", true),
            ("zero", "mismatch", true),
            ("rand", "mismatch", true),
            ("lead", "mismatch", false),
            ("tab", "mismatch", false),
            ("pre", "mismatch", true),
            ("multi", "mismatch", true),
            (
                "itoa",
                "promotion Some(\"1\") writes Some(\"\\\"1\\\"\")",
                true,
            ),
        ];
        assert_eq!(
            found,
            expected.map(|(dep, kind, fixed)| (dep.to_string(), kind.to_string(), fixed))
        );
    }

    #[test]
    fn test_a_workspace_key_in_a_workspace_entry_is_ignored() {
        assert_eq!(check("workspace_key_in_workspace_deps").len(), 4);
        let no_version = Err("requires version 1, but the workspace entry has no version".into());
        let inherited = Ok("{ workspace = true }".into());
        let expected = [("itoa", &no_version), ("ryu", &inherited)].repeat(2);
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(key, suggestion)| (key.to_string(), suggestion.clone()))
            .collect();
        assert_eq!(suggestions("workspace_key_in_workspace_deps"), expected);
    }

    #[test]
    fn test_promotion_hint_disables_default_features_when_a_crate_does() {
        let found: Vec<_> = check("fix_promotion_default_features")
            .into_iter()
            .filter_map(|d| match d.kind {
                DiagnosticKind::PromotionCandidate {
                    suggested_default_features,
                    ..
                } => Some((d.dependency, suggested_default_features)),
                _ => None,
            })
            .collect();
        assert_eq!(
            found,
            [("ed25519-dalek".into(), false), ("my-crate".into(), true)]
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

    /// A workspace with entries from the `inline_comments` manifest. Cargo 1.88, the minimum
    /// supported version, cannot load TOML 1.1, so the test builds the workspace by hand.
    fn inline_comment_workspace(root_inline_comment: bool) -> WorkspaceInfo {
        let manifest = read_manifest(&fixture("inline_comments").join("Cargo.toml")).unwrap();
        let entries = manifest.doc["dependencies"].as_table_like().unwrap();
        let spec = |key: &str| DepSpec {
            package: key.into(),
            source: DepSource::Registry(Registry::CratesIo),
            version: Some("1.0".into()),
            default_features: true,
            underscore_default_features: false,
        };
        let dep = |key: &str, member: &str, entry: &str| MemberDep {
            manifest: PathBuf::from(member),
            member: member.into(),
            edition: "2021".into(),
            table: DepTable {
                target: None,
                kind: "dependencies",
            },
            key: key.into(),
            spec: spec(key),
            entry: entries.get(entry).unwrap().clone(),
        };
        WorkspaceInfo {
            root_path: PathBuf::new(),
            workspace_deps: ["commented", "plain"]
                .map(|key| (key.to_string(), spec(key)))
                .into(),
            member_deps: vec![
                dep("commented", "a", "before_key"),
                dep("plain", "a", "after_entry"),
                dep("clean", "a", "after_entry"),
                dep("promoted", "a", "nested"),
                dep("clean", "b", "multi_line"),
                dep("promoted", "b", "multi_line"),
            ],
            root_inline_comment,
        }
    }

    #[test]
    fn test_fix_skips_a_rewrite_that_loses_a_comment() {
        let skipped = |ws: WorkspaceInfo| -> Vec<(String, bool)> {
            run_checks(&ws, 2)
                .into_iter()
                .map(|f| (f.diagnostic.dependency, f.fix.is_err()))
                .collect()
        };
        let expected = |root: bool| {
            [
                ("commented", true),
                ("plain", false),
                ("clean", root),
                ("promoted", true),
            ]
            .map(|(key, skip)| (key.to_string(), skip))
        };
        assert_eq!(skipped(inline_comment_workspace(false)), expected(false));
        assert_eq!(skipped(inline_comment_workspace(true)), expected(true));
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
