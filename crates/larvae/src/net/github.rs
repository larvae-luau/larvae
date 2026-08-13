//! The GitHub releases API (the part that `self update` needs)

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

/// One release by its tag, with or without a leading v
pub fn release_by_tag(repo: &str, version: &str) -> Result<Release> {
    let tag = version.trim_start_matches('v');

    // A project can tag either way, so try the bare form and then the v form.
    get(&format!(
        "https://api.github.com/repos/{repo}/releases/tags/{tag}"
    ))
    .or_else(|_| {
        get(&format!(
            "https://api.github.com/repos/{repo}/releases/tags/v{tag}"
        ))
    })
}

/// The latest non prerelease of owner/repo; the request uses GITHUB_TOKEN
pub fn latest_release(repo: &str) -> Result<Release> {
    get(&format!(
        "https://api.github.com/repos/{repo}/releases/latest"
    ))
}

fn get(url: &str) -> Result<Release> {
    let url = url.to_owned();
    let token = std::env::var("GITHUB_TOKEN").ok();

    let auth = token.map(|t| format!("Bearer {t}"));
    let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/vnd.github.v3+json")];

    if let Some(auth) = &auth {
        headers.push(("Authorization", auth));
    }

    crate::net::http::get_json(&url, &headers)
}
