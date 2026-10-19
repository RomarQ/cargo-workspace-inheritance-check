use std::path::Path;
use std::process::{Command, Output};

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

/// A copy of the fixture workspace `name`, because `--fix` writes to it.
fn fixture_copy(name: &str) -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    copy(&Path::new("tests/fixtures").join(name), tmp.path());
    tmp
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
fn test_version_mismatch_json_names_the_member_relative_to_the_workspace_root() {
    // Run the binary on its own, without the subcommand name. The relative `--path` catches a
    // member path made from `--path` instead of the workspace root.
    let output = Command::new(BIN)
        .current_dir("tests/fixtures")
        .args(["--path", "./version_mismatch", "--format", "json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let diagnostic = &parsed["diagnostics"][0];
    assert_eq!(diagnostic["check"], "version-mismatch");
    assert_eq!(diagnostic["version"], "0.7");
    assert_eq!(diagnostic["workspace_version"], "0.8");
    assert_eq!(diagnostic["member"], "crates/bar/Cargo.toml");
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
    let tmp = fixture_copy("fix_leaves_problems");
    let output = run(tmp.path(), &["--fix"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("skipped: `serde`"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.ends_with("1 error, 0 warnings found\n"), "{stdout}");
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
    let hint = "The tool needs cargo: set CARGO to its path, or unset CARGO and put cargo on PATH";
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let stderr = with_cargo(&root.join("no-such-cargo"));
    assert!(stderr.contains(hint), "{stderr}");
    // A directory exists but cannot run: the hint is only for a missing cargo.
    let stderr = with_cargo(root);
    assert!(!stderr.contains(hint), "{stderr}");
}

#[cfg(unix)]
#[test]
fn test_relative_cargo_resolves_from_the_current_directory() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("bin")).unwrap();
    std::os::unix::fs::symlink(env!("CARGO"), tmp.path().join("bin/cargo")).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/valid_workspace");
    let output = Command::new(BIN)
        .arg("--path")
        .arg(root)
        .current_dir(tmp.path())
        .env("CARGO", "bin/cargo")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
}

#[test]
fn test_empty_cargo_uses_cargo_on_path() {
    let output = Command::new(BIN)
        .args(["--path", "tests/fixtures/valid_workspace"])
        .env("CARGO", "")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
}
