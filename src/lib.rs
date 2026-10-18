//! Checks and fixes workspace dependency inheritance in Cargo workspaces.
//!
//! [`workspace::parse_workspace`] reads a workspace, [`check::run_checks`] reports the problems
//! and plans a fix for each one, and [`fix::apply_fixes`] writes the fixes.

pub mod check;
pub mod diagnostic;
pub mod fix;
pub mod workspace;

/// The fixture workspaces in `tests/fixtures`.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::path::{Path, PathBuf};

    /// The fixture workspace `name`.
    pub(crate) fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// A copy of the fixture workspace `name` in a temp dir, for a test that writes to it.
    pub(crate) fn fixture_copy(name: &str) -> tempfile::TempDir {
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
    pub(crate) fn assert_fixed(dir: &Path, name: &str) {
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
        let read =
            |path: &Path| String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
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
}
