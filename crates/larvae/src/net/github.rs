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

/*
Every release of owner/repo, newest first.

A version range needs the list. `latest` answers only the newest, and a
project pinned to `^0.2.0` while 0.3.0 exists has to see the 0.2.x releases to
find the one it asked for.

One page is enough. A worm with more than a hundred releases can pin an exact
version, and paging every repository to serve that case costs a request per
install for everyone else.
*/
pub fn releases(repo: &str) -> Result<Vec<Release>> {
    get_list(&format!(
        "https://api.github.com/repos/{repo}/releases?per_page=100"
    ))
}

fn get_list(url: &str) -> Result<Vec<Release>> {
    let auth = token();
    let headers = headers(auth.as_deref());

    crate::net::http::get_json(url, &headers)
}

fn get(url: &str) -> Result<Release> {
    let auth = token();
    let headers = headers(auth.as_deref());

    crate::net::http::get_json(url, &headers)
}

/// The bearer value when the environment carries a token.
fn token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .ok()
        .map(|t| format!("Bearer {t}"))
}

/// The headers for a call. The token borrows, so the caller owns it.
fn headers(auth: Option<&str>) -> Vec<(&str, &str)> {
    let mut out = vec![("Accept", "application/vnd.github.v3+json")];

    if let Some(auth) = auth {
        out.push(("Authorization", auth));
    }

    out
}
