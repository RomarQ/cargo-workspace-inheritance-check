use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cargo_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cargo-workspace-inheritance-check"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Run the binary as `cargo workspace-inheritance-check` does, with the subcommand name first.
fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(cargo_bin())
        .arg("workspace-inheritance-check")
        .arg("--path")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

/// Write `files` (relative path, content) into a new temp dir.
fn temp_tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let path = tmp.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    tmp
}

/// Each member's dependencies as cargo resolves them, by package name.
fn cargo_deps(root: &Path) -> serde_json::Value {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    packages
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_string(),
                p["dependencies"].clone(),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

const REGISTRY_CONFIG: &str =
    "[registries.my-registry]\nindex = \"sparse+https://example.invalid/index/\"\n";

#[test]
fn test_not_inherited_exits_one() {
    let output = run(&fixture("not_inherited"), &[]);
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("error:"));
    assert!(stdout.contains("serde"));
    assert!(stdout.contains("workspace = true"));
}

#[test]
fn test_promotion_candidate_warning() {
    let output = run(&fixture("promotion_candidate"), &[]);
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
    let output = run(&fixture("promotion_candidate"), &["--promotion-failure"]);
    assert!(
        !output.status.success(),
        "--promotion-failure should cause exit 1"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("error: `serde_yaml` appears in 2 crates"),
        "{stdout}"
    );
    assert!(
        stdout.ends_with("\n\n1 error, 0 warnings found\n"),
        "{stdout}"
    );
}

#[test]
fn test_no_fail_flag() {
    let output = run(&fixture("not_inherited"), &["--no-fail"]);
    assert!(output.status.success(), "--no-fail should always exit 0");
}

#[test]
fn test_json_format() {
    let output = run(
        &fixture("not_inherited"),
        &["--format", "json", "--no-fail"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(parsed["diagnostics"].is_array());
    assert_eq!(parsed["diagnostics"][0]["check"], "not-inherited");
    assert!(parsed["summary"]["errors"].as_u64().unwrap() > 0);
}

#[test]
fn test_promotion_threshold() {
    let output = run(
        &fixture("promotion_candidate"),
        &["--promotion-threshold", "3"],
    );
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("serde_yaml"),
        "threshold 3 should not flag serde_yaml in 2 crates"
    );
}

#[test]
fn test_member_paths_are_relative_to_the_workspace_root() {
    let output = Command::new(cargo_bin())
        .current_dir(fixture(""))
        .args([
            "--path",
            "./version_mismatch",
            "--format",
            "json",
            "--no-fail",
        ])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["diagnostics"][0]["member"], "crates/bar/Cargo.toml");
}

#[test]
fn test_fix_keeps_what_cargo_resolves() {
    let member = |name: &str, deps: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n{deps}")
    };
    let a = member(
        "a",
        "[dependencies]\n\
         serde = { version = \"1.0\", features = [\"derive\"], optional = true }\n\
         tokio = \"1\"\n\
         ed25519-dalek = { version = \"2.1\", default-features = false }\n\
         my-crate = { version = \"1.0\", registry = \"my-registry\" }\n\n\
         [dev-dependencies.rand]\nversion = \"0.8\"\n\n\
         [target.'cfg(windows)'.dependencies]\nwinapi = \"0.3\"\n",
    );
    let b = member(
        "b",
        "[dependencies]\n\
         ed25519-dalek = \"2.1\"\n\
         my-crate = { version = \"1.0\", registry = \"my-registry\" }\n\n\
         [dev-dependencies]\nrand = \"0.8\"\n\n\
         [target.'cfg(windows)'.dependencies]\n\
         winapi = { version = \"0.3\", features = [\"winuser\"] }\n",
    );
    let tmp = temp_tree(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n\n\
             [workspace.dependencies]\nserde = \"1.0\"\n\
             tokio = { version = \"1\", default-features = false }\n",
        ),
        (".cargo/config.toml", REGISTRY_CONFIG),
        ("crates/a/Cargo.toml", &a),
        ("crates/a/src/lib.rs", ""),
        ("crates/b/Cargo.toml", &b),
        ("crates/b/src/lib.rs", ""),
    ]);

    let before = cargo_deps(tmp.path());
    let output = run(tmp.path(), &["--fix"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("6 fix(es) applied"), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "0 errors, 0 warnings found\n");
    assert_eq!(cargo_deps(tmp.path()), before);
}

#[test]
fn test_fix_reports_the_problems_it_leaves() {
    let tmp = temp_tree(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nserde = \"1.0\"\n",
        ),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nserde = { version = \"1.0\", default-features = false }\n",
        ),
        ("crates/a/src/lib.rs", ""),
    ]);

    let output = run(tmp.path(), &["--fix"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("skipped: `serde`"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.ends_with("1 error, 0 warnings found\n"), "{stdout}");

    let output = run(tmp.path(), &["--fix", "--no-fail", "--format", "json"]);
    assert!(output.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["summary"]["errors"], 1);
}
