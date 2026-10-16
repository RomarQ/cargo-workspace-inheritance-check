use std::path::PathBuf;
use std::process::ExitCode;

use cargo_workspace_inheritance_check::check::{self, Finding};
use cargo_workspace_inheritance_check::diagnostic::{DiagnosticKind, DiagnosticReport, Severity};
use cargo_workspace_inheritance_check::fix::{self, FixSummary};
use cargo_workspace_inheritance_check::workspace;
use clap::builder::{PossibleValuesParser, RangedU64ValueParser};
use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Parser)]
#[command(name = "cargo-workspace-inheritance-check", version)]
#[command(about = "Check workspace dependency hygiene in Cargo workspaces")]
struct Cli {
    /// Path to the workspace root
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// Minimum crate count before suggesting workspace promotion
    #[arg(
        long,
        default_value_t = 2,
        value_parser = RangedU64ValueParser::<usize>::new().range(1..)
    )]
    promotion_threshold: usize,

    /// Treat promotion candidates as errors
    #[arg(long)]
    promotion_failure: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,

    /// Exit 0 even on errors
    #[arg(long)]
    no_fail: bool,

    /// Automatically fix reported problems, then report the problems that remain
    #[arg(long)]
    fix: bool,

    // Support `cargo workspace-inheritance-check` invocation where cargo
    // passes the subcommand name as the first argument.
    #[arg(
        hide = true,
        value_parser = PossibleValuesParser::new(["workspace-inheritance-check"])
    )]
    _subcommand: Option<String>,
}

fn main() -> ExitCode {
    match run(&Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, String> {
    let mut findings = check(cli)?;

    if cli.fix {
        let summary =
            fix::apply_fixes(&findings).map_err(|e| format!("failed to apply fixes: {e}"))?;
        print_fix_summary(&summary);
        // The report and the exit code cover the problems that the fixes left.
        findings = check(cli)?;
    }

    let report = DiagnosticReport::new(findings.into_iter().map(|f| f.diagnostic).collect());
    match cli.format {
        OutputFormat::Json => println!("{}", report.format_json()),
        OutputFormat::Human => println!("{}", report.format_human()),
    }

    Ok(if !cli.no_fail && report.summary.errors > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Parse the workspace and run the checks. With `--promotion-failure`, promotion candidates
/// are errors.
fn check(cli: &Cli) -> Result<Vec<Finding>, String> {
    let workspace = workspace::parse_workspace(&cli.path)?;
    let mut findings = check::run_checks(&workspace, cli.promotion_threshold);
    if cli.promotion_failure {
        for d in findings.iter_mut().map(|f| &mut f.diagnostic) {
            if matches!(d.kind, DiagnosticKind::PromotionCandidate { .. }) {
                d.severity = Severity::Error;
            }
        }
    }
    Ok(findings)
}

fn print_fix_summary(summary: &FixSummary) {
    for line in summary.fixed.iter().chain(&summary.skipped) {
        eprintln!("{line}");
    }
    if summary.fixed.is_empty() {
        eprintln!("no fixable issues found");
    } else {
        eprintln!(
            "\n{} fix(es) applied across {} file(s)",
            summary.fixed.len(),
            summary.files_modified,
        );
    }
}
