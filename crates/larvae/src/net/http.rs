//! Minimal blocking HTTP helpers over ureq

use anyhow::{Context, Result, bail};

const USER_AGENT: &str = concat!("larvae/", env!("CARGO_PKG_VERSION"));

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(60))
        .build()
}

/// GET a JSON document, with optional extra headers
pub fn get_json<T: serde::de::DeserializeOwned>(url: &str, headers: &[(&str, &str)]) -> Result<T> {
    let mut req = agent().get(url).set("User-Agent", USER_AGENT);

    for (k, v) in headers {
        req = req.set(k, v);
    }

    let resp = req.call().with_context(|| format!("GET {url} failed"))?;
    resp.into_json()
        .with_context(|| format!("invalid JSON from {url}"))
}

/// GET raw bytes and follow redirects by hand; ureq 2 skips 307/308, and GitHub uses them
pub fn get_bytes(url: &str) -> Result<Vec<u8>> {
    let mut url = url.to_string();

    for _ in 0..8 {
        let resp = agent()
            .get(&url)
            .set("User-Agent", USER_AGENT)
            .call()
            .with_context(|| format!("GET {url} failed"))?;

        if (300..400).contains(&resp.status()) {
            let Some(next) = resp.header("Location") else {
                bail!("redirect from {url} without a Location header");
            };

            url = next.to_string();
            continue;
        }

        let mut bytes = Vec::new();
        use std::io::Read;

        resp.into_reader()
            .take(512 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .with_context(|| format!("download from {url} failed"))?;
        return Ok(bytes);
    }

    bail!("too many redirects fetching {url}");
}
