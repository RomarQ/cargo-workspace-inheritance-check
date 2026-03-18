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
    },
}

impl Diagnostic {
    pub fn format_human(&self) -> String {
        match &self.kind {
            DiagnosticKind::NotInherited { version, member, .. } => {
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
            } => {
                let severity = match self.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                };
                let mut lines = vec![format!(
                    "{severity}: `{}` appears in {count} crates but is not in [workspace.dependencies]",
                    self.dependency,
                )];
                for m in members {
                    lines.push(format!("  --> {m}"));
                }
                if let Some(ver) = suggested_version {
                    lines.push(format!(
                        "  hint: consider adding `{} = \"{}\"` to [workspace.dependencies]",
                        self.dependency, ver,
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
        let mut parts: Vec<String> = self.diagnostics.iter().map(|d| d.format_human()).collect();
        let error_word = if self.summary.errors == 1 {
            "error"
        } else {
            "errors"
        };
        let warning_word = if self.summary.warnings == 1 {
            "warning"
        } else {
            "warnings"
        };
        parts.push(format!(
            "{} {}, {} {} found",
            self.summary.errors, error_word, self.summary.warnings, warning_word,
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

    #[test]
    fn test_not_inherited_human_format() {
        let d = Diagnostic {
            severity: Severity::Error,
            dependency: "lru".into(),
            kind: DiagnosticKind::NotInherited {
                version: Some("0.12".into()),
                member: "crates/crypto/Cargo.toml".into(),
                workspace_version: Some("0.12".into()),
            },
        };
        let output = d.format_human();
        assert!(output.contains("error:"));
        assert!(output.contains("lru"));
        assert!(output.contains("crates/crypto/Cargo.toml"));
        assert!(output.contains("workspace = true"));
    }

    #[test]
    fn test_version_mismatch_human_format() {
        let d = Diagnostic {
            severity: Severity::Error,
            dependency: "rand".into(),
            kind: DiagnosticKind::VersionMismatch {
                version: Some("0.7".into()),
                member: "crates/utils/Cargo.toml".into(),
                workspace_version: Some("0.8".into()),
            },
        };
        let output = d.format_human();
        assert!(output.contains("error:"));
        assert!(output.contains("rand"));
        assert!(output.contains("0.7"));
        assert!(output.contains("0.8"));
    }

    #[test]
    fn test_promotion_candidate_human_format() {
        let d = Diagnostic {
            severity: Severity::Warning,
            dependency: "serde_yaml".into(),
            kind: DiagnosticKind::PromotionCandidate {
                count: 3,
                members: vec![
                    "crates/config/Cargo.toml".into(),
                    "crates/node/Cargo.toml".into(),
                ],
                suggested_version: Some("0.9".into()),
            },
        };
        let output = d.format_human();
        assert!(output.contains("warning:"));
        assert!(output.contains("serde_yaml"));
        assert!(output.contains("3 crates"));
        assert!(output.contains("hint:"));
    }

    #[test]
    fn test_report_summary_human() {
        let report = DiagnosticReport::new(vec![Diagnostic {
            severity: Severity::Error,
            dependency: "lru".into(),
            kind: DiagnosticKind::NotInherited {
                version: Some("0.12".into()),
                member: "crates/crypto/Cargo.toml".into(),
                workspace_version: Some("0.12".into()),
            },
        }]);
        let output = report.format_human();
        assert!(output.contains("1 error"));
    }

    #[test]
    fn test_report_json() {
        let report = DiagnosticReport::new(vec![Diagnostic {
            severity: Severity::Error,
            dependency: "lru".into(),
            kind: DiagnosticKind::NotInherited {
                version: Some("0.12".into()),
                member: "crates/crypto/Cargo.toml".into(),
                workspace_version: Some("0.12".into()),
            },
        }]);
        let json = report.format_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["summary"]["errors"], 1);
        assert_eq!(parsed["summary"]["warnings"], 0);
        assert_eq!(parsed["diagnostics"][0]["check"], "not-inherited");
    }
}
