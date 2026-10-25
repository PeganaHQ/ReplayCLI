//! Methodology version identity. CLI compares these to receipt's recorded
//! versions to detect mismatched binaries.

pub fn methodology_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn methodology_git_sha() -> Option<&'static str> {
    option_env!("PEGANA_GIT_SHA")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_cargo_toml() {
        assert_eq!(methodology_version(), "0.8.0");
    }

    #[test]
    fn git_sha_is_optional() {
        let _ = methodology_git_sha(); // None in test, set in release Docker build
    }
}
