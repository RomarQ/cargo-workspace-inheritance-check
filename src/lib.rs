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
}
