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
