//! The crates.io API (the part that `worm update` needs)

use anyhow::Result;

#[derive(serde::Deserialize)]
struct Index {
    #[serde(rename = "crate")]
    package: Package,
}

#[derive(serde::Deserialize)]
struct Package {
    /// The newest version that is not a prerelease and not yanked
    max_stable_version: Option<String>,
    /// The newest version of any kind, the fallback for a crate with
    /// prereleases only
    max_version: String,
}

/// The latest version of one crate, stable when the crate has one
pub fn latest_version(package: &str) -> Result<String> {
    let index: Index =
        super::http::get_json(&format!("https://crates.io/api/v1/crates/{package}"), &[])?;

    Ok(index
        .package
        .max_stable_version
        .unwrap_or(index.package.max_version))
}
