//! The diagnostics that the check reports, and their human and JSON output.

use serde::Serialize;

use crate::fix::dep_value;

/// How serious a [`Diagnostic`] is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A problem that makes the check fail.
    Error,
    /// A suggestion that does not make the check fail.
    Warning,
}

/// A problem with one dependency, as the check reports it.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    /// The key of the dependency entry.
    pub dependency: String,
    #[serde(flatten)]
    pub kind: DiagnosticKind,
}

/// What the check found. The JSON output names the variant in its `check` field.
///
/// Each `member` is a manifest path from [`crate::workspace::relative_manifest`].
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "check", rename_all = "kebab-case")]
pub enum DiagnosticKind {
    /// A member declares its own entry instead of inheriting the workspace entry.
    NotInherited {
        /// The member's version requirement, if the entry gives one.
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        member: String,
        /// The workspace entry's version requirement, if it gives one.
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace_version: Option<String>,
    },
    /// Like [`Self::NotInherited`], but the member and the workspace entry give different
    /// versions.
    VersionMismatch {
        version: String,
        member: String,
        workspace_version: String,
    },
    /// Crates declare a dependency that `[workspace.dependencies]` does not have.
    PromotionCandidate {
        /// The number of crates that declare it.
        count: usize,
        /// The manifest path of each crate that declares it.
        members: Vec<String>,
        /// The version that the most crates declare. `None` means a tie, or that no crate
        /// gives one.
        #[serde(skip_serializing_if = "Option::is_none")]
        suggested_version: Option<String>,
        /// The registry that the crates name, or `None` for crates.io.
        #[serde(skip_serializing_if = "Option::is_none")]
        suggested_registry: Option<String>,
    },
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => f.write_str("error"),
            Severity::Warning => f.write_str("warning"),
        }
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

impl Diagnostic {
    /// The diagnostic as text for a terminal. A promotion candidate takes several lines.
    pub fn format_human(&self) -> String {
        let dep = &self.dependency;
        let message = match &self.kind {
            DiagnosticKind::NotInherited {
                version, member, ..
            } => format!(
                "`{dep} = \"{}\"` in {member} should use `{dep} = {{ workspace = true }}`",
                version.as_deref().unwrap_or("?"),
            ),
            DiagnosticKind::VersionMismatch {
                version,
                member,
                workspace_version,
            } => format!(
                "`{dep} = \"{version}\"` in {member} has a different version than workspace `{dep} = \"{workspace_version}\"`",
            ),
            DiagnosticKind::PromotionCandidate {
                count,
                members,
                suggested_version,
                suggested_registry,
            } => {
                let mut lines = vec![format!(
                    "`{dep}` appears in {count} crate{} but is not in [workspace.dependencies]",
                    plural(*count),
                )];
                lines.extend(members.iter().map(|m| format!("  --> {m}")));
                if let Some(version) = suggested_version {
                    let value = dep_value(version, suggested_registry.as_deref(), true);
                    lines.push(format!(
                        "  hint: consider adding `{dep} = {value}` to [workspace.dependencies]"
                    ));
                }
                lines.join("\n")
            }
        };
        format!("{}: {message}", self.severity)
    }
}

/// The diagnostics of one check, and how many there are of each severity.
#[derive(Debug, Serialize)]
pub struct DiagnosticReport {
    pub diagnostics: Vec<Diagnostic>,
    pub summary: Summary,
}

/// How many diagnostics a [`DiagnosticReport`] holds of each severity.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub errors: usize,
    pub warnings: usize,
}

impl DiagnosticReport {
    /// Make a report of `diagnostics`, and count them by severity.
    pub fn new(diagnostics: Vec<Diagnostic>) -> Self {
        let (mut errors, mut warnings) = (0, 0);
        for d in &diagnostics {
            match d.severity {
                Severity::Error => errors += 1,
                Severity::Warning => warnings += 1,
            }
        }
        Self {
            diagnostics,
            summary: Summary { errors, warnings },
        }
    }

    /// Each diagnostic as text for a terminal, then a summary line.
    pub fn format_human(&self) -> String {
        let Summary { errors, warnings } = self.summary;
        let summary = format!(
            "{errors} error{}, {warnings} warning{} found",
            plural(errors),
            plural(warnings),
        );
        let diagnostics = self.diagnostics.iter().map(Diagnostic::format_human);
        let parts: Vec<String> = diagnostics.chain([summary]).collect();
        parts.join("\n\n")
    }

    /// The report as pretty-printed JSON.
    pub fn format_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("DiagnosticReport is always serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn not_inherited() -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            dependency: "lru".into(),
            kind: DiagnosticKind::NotInherited {
                version: Some("0.11".into()),
                member: "crates/crypto/Cargo.toml".into(),
                workspace_version: Some("0.12".into()),
            },
        }
    }

    fn version_mismatch() -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            dependency: "rand".into(),
            kind: DiagnosticKind::VersionMismatch {
                version: "0.7".into(),
                member: "crates/utils/Cargo.toml".into(),
                workspace_version: "0.8".into(),
            },
        }
    }

    fn promotion(registry: Option<&str>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Warning,
            dependency: "serde_yaml".into(),
            kind: DiagnosticKind::PromotionCandidate {
                count: 2,
                members: vec![
                    "crates/config/Cargo.toml".into(),
                    "crates/node/Cargo.toml".into(),
                ],
                suggested_version: Some("0.9".into()),
                suggested_registry: registry.map(String::from),
            },
        }
    }

    #[test]
    fn test_not_inherited_human_format() {
        assert_eq!(
            not_inherited().format_human(),
            "error: `lru = \"0.11\"` in crates/crypto/Cargo.toml should use `lru = { workspace = true }`"
        );
    }

    #[test]
    fn test_version_mismatch_human_format() {
        assert_eq!(
            version_mismatch().format_human(),
            "error: `rand = \"0.7\"` in crates/utils/Cargo.toml has a different version than \
             workspace `rand = \"0.8\"`"
        );
    }

    #[test]
    fn test_promotion_candidate_human_format() {
        assert_eq!(
            promotion(None).format_human(),
            "warning: `serde_yaml` appears in 2 crates but is not in [workspace.dependencies]\n  \
             --> crates/config/Cargo.toml\n  \
             --> crates/node/Cargo.toml\n  \
             hint: consider adding `serde_yaml = \"0.9\"` to [workspace.dependencies]"
        );
    }

    #[test]
    fn test_promotion_candidate_with_registry_human_format() {
        let output = promotion(Some("my-registry")).format_human();
        assert!(
            output.ends_with(
                "\n  hint: consider adding \
                 `serde_yaml = { version = \"0.9\", registry = \"my-registry\" }` \
                 to [workspace.dependencies]"
            ),
            "{output}"
        );
    }

    #[test]
    fn test_report_summary_human() {
        let report = DiagnosticReport::new(vec![not_inherited()]);
        assert!(
            report
                .format_human()
                .ends_with("\n\n1 error, 0 warnings found")
        );
        let empty = DiagnosticReport::new(Vec::new());
        assert_eq!(empty.format_human(), "0 errors, 0 warnings found");
    }

    #[test]
    fn test_report_json_contract() {
        let mut no_versions = not_inherited();
        no_versions.kind = DiagnosticKind::NotInherited {
            version: None,
            member: "crates/a/Cargo.toml".into(),
            workspace_version: None,
        };
        let report = DiagnosticReport::new(vec![
            not_inherited(),
            no_versions,
            version_mismatch(),
            promotion(None),
            promotion(Some("my-registry")),
        ]);
        let expected = r#"{
  "diagnostics": [
    {
      "severity": "error",
      "dependency": "lru",
      "check": "not-inherited",
      "version": "0.11",
      "member": "crates/crypto/Cargo.toml",
      "workspace_version": "0.12"
    },
    {
      "severity": "error",
      "dependency": "lru",
      "check": "not-inherited",
      "member": "crates/a/Cargo.toml"
    },
    {
      "severity": "error",
      "dependency": "rand",
      "check": "version-mismatch",
      "version": "0.7",
      "member": "crates/utils/Cargo.toml",
      "workspace_version": "0.8"
    },
    {
      "severity": "warning",
      "dependency": "serde_yaml",
      "check": "promotion-candidate",
      "count": 2,
      "members": [
        "crates/config/Cargo.toml",
        "crates/node/Cargo.toml"
      ],
      "suggested_version": "0.9"
    },
    {
      "severity": "warning",
      "dependency": "serde_yaml",
      "check": "promotion-candidate",
      "count": 2,
      "members": [
        "crates/config/Cargo.toml",
        "crates/node/Cargo.toml"
      ],
      "suggested_version": "0.9",
      "suggested_registry": "my-registry"
    }
  ],
  "summary": {
    "errors": 3,
    "warnings": 2
  }
}"#;
        assert_eq!(report.format_json(), expected);
    }
}
