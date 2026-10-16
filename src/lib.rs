//! Checks and fixes workspace dependency inheritance in Cargo workspaces.
//!
//! [`workspace::parse_workspace`] reads a workspace, [`check::run_checks`] reports the problems
//! and plans a fix for each one, and [`fix::apply_fixes`] writes the fixes.

pub mod check;
pub mod diagnostic;
pub mod fix;
pub mod workspace;

#[cfg(test)]
pub(crate) fn fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Create a temp workspace with a root Cargo.toml and member crates under `crates/`.
///
/// `workspace_deps` goes under `[workspace.dependencies]`, and each member is a tuple of
/// (name, fragment under `[dependencies]`). A fragment can open later tables. The workspace
/// configures the alternate registry `my-registry`.
#[cfg(test)]
pub(crate) fn temp_workspace(workspace_deps: &str, members: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let write = |path: &str, content: String| {
        let path = tmp.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write(
        "Cargo.toml",
        format!(
            "[workspace]\nmembers = [\"crates/*\"]\n\n\
             [workspace.dependencies]\n{workspace_deps}\n"
        ),
    );
    write(
        ".cargo/config.toml",
        "[registries.my-registry]\nindex = \"sparse+https://example.invalid/index/\"\n".into(),
    );
    for (name, deps) in members {
        write(
            &format!("crates/{name}/Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                 [dependencies]\n{deps}\n"
            ),
        );
        write(&format!("crates/{name}/src/lib.rs"), String::new());
    }
    tmp
}
