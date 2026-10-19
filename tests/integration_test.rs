use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cargo_workspace_inheritance_check::check::run_checks;
use cargo_workspace_inheritance_check::fix::{FixSummary, apply_fixes};
use cargo_workspace_inheritance_check::workspace::parse_workspace;

const BIN: &str = env!("CARGO_BIN_EXE_cargo-workspace-inheritance-check");

/// Run the binary as `cargo workspace-inheritance-check` does, with the subcommand name first.
fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("workspace-inheritance-check")
        .arg("--path")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

/// The fixture workspace `name`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A copy of the fixture workspace `name` in a temp dir, for a test that writes to it.
fn fixture_copy(name: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let source = fixture(name);
    for file in files(&source) {
        let target = tmp.path().join(&file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(source.join(&file), target).unwrap();
    }
    tmp
}

/// Assert that `dir`, a copy of the fixture `name` after `--fix`, holds each file of
/// `tests/fixtures/<name>.fixed`, and that its other files are unchanged.
fn assert_fixed(dir: &Path, name: &str) {
    let source = fixture(name);
    let fixed = fixture(&format!("{name}.fixed"));
    let after = files(dir);
    assert_eq!(after, files(&source), "--fix created or removed a file");
    let changed = files(&fixed);
    for file in &changed {
        let message = format!("{name}.fixed/{} has no source", file.display());
        assert!(source.join(file).exists(), "{message}");
    }
    // Lossy, because a file that git does not track, such as `.DS_Store`, can be binary.
    let read = |path: &Path| String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
    for file in after {
        let expected = if changed.contains(&file) {
            &fixed
        } else {
            &source
        };
        let actual = read(&dir.join(&file));
        assert_eq!(actual, read(&expected.join(&file)), "{}", file.display());
    }
}

/// The files under `dir`, relative to it.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in std::fs::read_dir(dir.join(&relative)).unwrap() {
            let entry = entry.unwrap();
            let path = relative.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The workspace as cargo reads it.
fn cargo_metadata(root: &Path) -> serde_json::Value {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--offline"])
        .args(["--format-version", "1"])
        .current_dir(root)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn test_promotion_candidates_fail_only_with_promotion_failure() {
    let root = Path::new("tests/fixtures/promotion_candidate");
    assert!(run(root, &[]).status.success());
    let output = run(root, &["--promotion-failure"]);
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "error: `serde_yaml` appears in 2 crates but is not in [workspace.dependencies]\n  \
         --> crates/one/Cargo.toml\n  \
         --> crates/two/Cargo.toml\n  \
         hint: consider adding `serde_yaml = \"0.9\"` to [workspace.dependencies]\n\n\
         1 error, 0 warnings found\n"
    );
    let higher_threshold = ["--promotion-failure", "--promotion-threshold", "3"];
    assert!(run(root, &higher_threshold).status.success());
}

#[test]
fn test_version_mismatch_fails_with_a_json_report() {
    let output = Command::new(BIN)
        .current_dir("tests/fixtures")
        .args(["--path", "./version_mismatch", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["diagnostics"][0]["member"], "crates/bar/Cargo.toml");
}

#[test]
fn test_fix_keeps_what_cargo_resolves() {
    let tmp = fixture_copy("fix_keeps_resolution");
    let before = cargo_metadata(tmp.path());
    let output = run(tmp.path(), &["--fix"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("6 fix(es) applied"), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "0 errors, 0 warnings found\n");
    assert_eq!(cargo_metadata(tmp.path()), before);
}

#[test]
fn test_problems_left_by_fix_fail_unless_no_fail() {
    let tmp = fixture_copy("fix_skips");
    let output = run(tmp.path(), &["--fix"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("skipped: `serde`"), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.ends_with("2 errors, 1 warning found\n"), "{stdout}");
    let output = run(tmp.path(), &["--fix"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no fixable issues found"), "{stderr}");
    assert!(run(tmp.path(), &["--fix", "--no-fail"]).status.success());
}

#[test]
fn test_missing_cargo_says_how_to_provide_it() {
    let with_cargo = |cargo: &Path| {
        let output = Command::new(BIN)
            .args(["--path", "tests/fixtures/valid_workspace"])
            .env("CARGO", cargo)
            .output()
            .unwrap();
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let hint = "The tool needs cargo: set CARGO to its absolute path, or unset CARGO and put cargo on PATH";
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let stderr = with_cargo(&root.join("no-such-cargo"));
    assert!(stderr.contains(hint), "{stderr}");
}

/// Fix a copy of the fixture `name`, and assert that the copy matches `<name>.fixed`.
fn fix(name: &str) -> FixSummary {
    let tmp = fixture_copy(name);
    let findings = run_checks(&parse_workspace(tmp.path()).unwrap(), 2);
    let summary = apply_fixes(&findings).unwrap();
    assert_fixed(tmp.path(), name);
    summary
}

#[test]
fn test_fix_keeps_only_inheritable_keys_and_comments() {
    let summary = fix("fix_inheritable_keys");
    assert_eq!(
        summary.fixed[..2],
        [
            "fixed: `rand` in crates/a/Cargo.toml changed from 0.7 to 0.8 (workspace version)",
            "fixed: `core` in crates/a/Cargo.toml now uses workspace inheritance",
        ]
    );
}

#[test]
fn test_fix_keeps_crlf_line_endings() {
    let tmp = fixture_copy("fix_inheritable_keys");
    let crlf = |path: &Path| std::fs::read_to_string(path).unwrap().replace('\n', "\r\n");
    for manifest in [
        "Cargo.toml",
        "crates/a/Cargo.toml",
        "crates/core/Cargo.toml",
    ] {
        let path = tmp.path().join(manifest);
        std::fs::write(&path, crlf(&path)).unwrap();
    }
    apply_fixes(&run_checks(&parse_workspace(tmp.path()).unwrap(), 2)).unwrap();
    let written = std::fs::read_to_string(tmp.path().join("crates/a/Cargo.toml")).unwrap();
    let fixed = fixture("fix_inheritable_keys.fixed").join("crates/a/Cargo.toml");
    assert_eq!(written, crlf(&fixed));
}

#[test]
fn test_fix_keeps_the_newlines_of_a_string_in_a_file_with_mixed_line_endings() {
    let tmp = fixture_copy("fix_inheritable_keys");
    let manifest = tmp.path().join("crates/a/Cargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let mixed = text
        .replace('\n', "\r\n")
        .replace("Two lines\r\nof text", "Two lines\nof text");
    std::fs::write(&manifest, mixed).unwrap();
    apply_fixes(&run_checks(&parse_workspace(tmp.path()).unwrap(), 2)).unwrap();
    let written = std::fs::read_to_string(&manifest).unwrap();
    assert!(written.contains("Two lines\nof text"), "{written:?}");
}

#[test]
fn test_fix_skips_a_fix_that_changes_what_cargo_resolves() {
    let summary = fix("fix_skips");
    assert_eq!(
        summary.skipped,
        [
            "skipped: `serde` in crates/a/Cargo.toml sets `default-features = false`, so the \
             workspace entry must set it too",
            "skipped: `core` in crates/a/Cargo.toml requires version 0.1, but the workspace \
             entry has no version",
            "skipped: `my-crate` is not promoted, because [workspace.dependencies] already has it",
            "skipped: `rand` is not promoted, because members use different versions: \
             crates/a/Cargo.toml (0.7), crates/b/Cargo.toml (0.8)",
        ]
    );
    assert_eq!(summary.files_modified, 3);
}

#[test]
fn test_fix_skips_a_workspace_spelling_that_the_member_edition_rejects() {
    let summary = fix("fix_underscore_default_features");
    let reason = "uses edition 2024, so the workspace entry must use `default-features`, not \
                  `default_features`";
    assert_eq!(
        summary.skipped,
        ["log", "itoa", "ryu"]
            .map(|key| format!("skipped: `{key}` in crates/b/Cargo.toml {reason}"))
    );
}

#[test]
fn test_fix_promotion_into_an_inline_workspace_of_a_root_package() {
    fix("fix_inline_workspace_root_package");
}
