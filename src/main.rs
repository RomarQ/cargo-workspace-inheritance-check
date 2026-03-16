mod check;
mod diagnostic;
mod workspace;

use clap::Parser;
use diagnostic::DiagnosticReport;
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(name = "cargo-workspace-inheritance-check")]
#[command(about = "Check workspace dependency hygiene in Cargo workspaces")]
struct Cli {
    /// Path to the workspace root
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// Minimum crate count before suggesting workspace promotion
    #[arg(long, default_value_t = 2)]
    promotion_threshold: usize,

    /// Treat promotion candidates as errors
    #[arg(long)]
    promotion_failure: bool,

    /// Output format: human, json
    #[arg(long, default_value = "human")]
    format: String,

    /// Exit 0 even on errors
    #[arg(long)]
    no_fail: bool,

    // Support `cargo workspace-inheritance-check` invocation where cargo
    // passes the subcommand name as the first argument.
    #[arg(hide = true, required = false)]
    _subcommand: Option<String>,
}

fn main() {
    let cli = Cli::parse();

    let workspace = match workspace::parse_workspace(&cli.path) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    let mut diagnostics = check::run_checks(&workspace, cli.promotion_threshold);

    // Promote warnings to errors if requested
    if cli.promotion_failure {
        for d in &mut diagnostics {
            if matches!(d.check, diagnostic::CheckKind::PromotionCandidate) {
                d.severity = diagnostic::Severity::Error;
            }
        }
    }

    let report = DiagnosticReport::new(diagnostics);

    match cli.format.as_str() {
        "json" => println!("{}", report.format_json()),
        _ => {
            let output = report.format_human();
            if !output.is_empty() {
                println!("{output}");
            }
        }
    }

    if !cli.no_fail && report.summary.errors > 0 {
        process::exit(1);
    }
}
