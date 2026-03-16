use std::path::PathBuf;
use std::process::Command;

fn cargo_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cargo-workspace-inheritance-check"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn test_clean_workspace_exits_zero() {
    let output = Command::new(cargo_bin())
        .args(["--path", fixture("valid_workspace").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn test_not_inherited_exits_one() {
    let output = Command::new(cargo_bin())
        .args(["--path", fixture("not_inherited").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("error:"));
    assert!(stdout.contains("serde"));
    assert!(stdout.contains("workspace = true"));
}

#[test]
fn test_version_mismatch_exits_one() {
    let output = Command::new(cargo_bin())
        .args(["--path", fixture("version_mismatch").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("error:"));
    assert!(stdout.contains("rand"));
    assert!(stdout.contains("0.7"));
    assert!(stdout.contains("0.8"));
}

#[test]
fn test_promotion_candidate_warning() {
    let output = Command::new(cargo_bin())
        .args(["--path", fixture("promotion_candidate").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "warnings alone should not cause failure"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("warning:"));
    assert!(stdout.contains("serde_yaml"));
}

#[test]
fn test_promotion_failure_flag() {
    let output = Command::new(cargo_bin())
        .args([
            "--path",
            fixture("promotion_candidate").to_str().unwrap(),
            "--promotion-failure",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--promotion-failure should cause exit 1"
    );
}

#[test]
fn test_no_fail_flag() {
    let output = Command::new(cargo_bin())
        .args([
            "--path",
            fixture("not_inherited").to_str().unwrap(),
            "--no-fail",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "--no-fail should always exit 0");
}

#[test]
fn test_json_format() {
    let output = Command::new(cargo_bin())
        .args([
            "--path",
            fixture("not_inherited").to_str().unwrap(),
            "--format",
            "json",
            "--no-fail",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(parsed["diagnostics"].is_array());
    assert_eq!(parsed["diagnostics"][0]["check"], "not-inherited");
    assert!(parsed["summary"]["errors"].as_u64().unwrap() > 0);
}

#[test]
fn test_promotion_threshold() {
    let output = Command::new(cargo_bin())
        .args([
            "--path",
            fixture("promotion_candidate").to_str().unwrap(),
            "--promotion-threshold",
            "3",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("serde_yaml"),
        "threshold 3 should not flag serde_yaml in 2 crates"
    );
}
