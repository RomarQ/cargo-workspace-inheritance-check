//! The diagnostics that the check reports, and their human and JSON output.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub dependency: String,
    #[serde(flatten)]
    pub kind: DiagnosticKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "check", rename_all = "kebab-case")]
pub enum DiagnosticKind {
    NotInherited {
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        member: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace_version: Option<String>,
    },
    VersionMismatch {
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        member: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace_version: Option<String>,
    },
    PromotionCandidate {
        count: usize,
        members: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        suggested_version: Option<String>,
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

/// The plural suffix for `count` items.
fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Format a dependency value for display, e.g. `"1.0"` or `{ version = "1.0", registry = "r" }`.
pub fn format_dep_value(version: &str, registry: Option<&str>) -> String {
    if let Some(reg) = registry {
        format!("{{ version = \"{version}\", registry = \"{reg}\" }}")
    } else {
        format!("\"{version}\"")
    }
}

impl Diagnostic {
    pub fn format_human(&self) -> String {
        match &self.kind {
            DiagnosticKind::NotInherited {
                version, member, ..
            } => {
                let ver = version.as_deref().unwrap_or("?");
                format!(
                    "error: `{dep} = \"{ver}\"` in {member} should use `{dep} = {{ workspace = true }}`",
                    dep = self.dependency,
                )
            }
            DiagnosticKind::VersionMismatch {
                version,
                member,
                workspace_version,
            } => {
                let ver = version.as_deref().unwrap_or("?");
                let ws_ver = workspace_version.as_deref().unwrap_or("?");
                format!(
                    "error: `{dep} = \"{ver}\"` in {member} has a different version than workspace `{dep} = \"{ws_ver}\"`",
                    dep = self.dependency,
                )
            }
            DiagnosticKind::PromotionCandidate {
                count,
                members,
                suggested_version,
                suggested_registry,
            } => {
                let mut lines = vec![format!(
                    "{}: `{}` appears in {count} crate{} but is not in [workspace.dependencies]",
                    self.severity,
                    self.dependency,
                    plural(*count),
                )];
                for m in members {
                    lines.push(format!("  --> {m}"));
                }
                if let Some(ver) = suggested_version {
                    let value = format_dep_value(ver, suggested_registry.as_deref());
                    lines.push(format!(
                        "  hint: consider adding `{} = {}` to [workspace.dependencies]",
                        self.dependency, value,
                    ));
                }
                lines.join("\n")
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DiagnosticReport {
    pub diagnostics: Vec<Diagnostic>,
    pub summary: Summary,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub errors: usize,
    pub warnings: usize,
}

impl DiagnosticReport {
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

    pub fn format_human(&self) -> String {
        let mut parts: Vec<String> = self
            .diagnostics
            .iter()
            .map(Diagnostic::format_human)
            .collect();
        let Summary { errors, warnings } = self.summary;
        parts.push(format!(
            "{errors} error{}, {warnings} warning{} found",
            plural(errors),
            plural(warnings),
        ));
        parts.join("\n\n")
    }

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
                version: Some("0.7".into()),
                member: "crates/utils/Cargo.toml".into(),
                workspace_version: Some("0.8".into()),
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
