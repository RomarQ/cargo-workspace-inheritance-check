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
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nserde = \"1.0\"\n\
             tokio = { version = \"1\", default-features = false }\n",
        ),
        (
            ".cargo/config.toml",
            "[registries.my-registry]\nindex = \"sparse+https://example.invalid/index/\"\n",
        ),
        ("crates/a/Cargo.toml", &a),
        ("crates/a/src/lib.rs", ""),
        ("crates/b/Cargo.toml", &b),
        ("crates/b/src/lib.rs", ""),
    ]);

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
    assert!(run(tmp.path(), &["--fix", "--no-fail"]).status.success());
}
