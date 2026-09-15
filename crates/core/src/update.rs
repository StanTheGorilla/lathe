// The update check. Amendment A31.
//
// One anonymous GET to GitHub's releases API, at most once a day, compared against the
// version compiled into the binary. Nothing is downloaded and nothing about the machine
// is sent: the request carries a User-Agent because GitHub refuses requests without
// one, and that is the whole of it. Brief section 2 ruled out "update pinging"; the
// owner asked for this one, and it can be switched off under About.

use anyhow::{anyhow, Context as _, Result};
use serde::Serialize;

/// The public repository the releases come from.
pub const REPOSITORY: &str = "StanTheGorilla/lathe";

/// A published release newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Available {
    /// Without the leading `v`, as the About screen shows it.
    pub version: String,
    /// The release page, for a browser. The installers are attached there.
    pub url: String,
}

/// Asks GitHub for the latest published release. Drafts and pre-releases are excluded
/// by the endpoint itself. Returns `None` when the running build is already the latest
/// (or newer, on a development build).
pub fn check(current: &str) -> Result<Option<Available>> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .new_agent();
    let mut response = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", &format!("lathe/{current}"))
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(code) => anyhow!("GitHub returned HTTP {code}"),
            other => anyhow!("could not reach GitHub: {other}"),
        })?;
    let text = response
        .body_mut()
        .read_to_string()
        .context("reading the release listing")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&text).context("the release listing was not JSON")?;
    let tag = parsed
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("the release listing had no tag_name"))?;
    let page = parsed
        .get("html_url")
        .and_then(|u| u.as_str())
        .unwrap_or("https://github.com/StanTheGorilla/lathe/releases/latest");
    Ok(newer(tag, current).then(|| Available {
        version: tag.trim_start_matches('v').to_string(),
        url: page.to_string(),
    }))
}

/// Whether the release tag names a version above the running one. Tags look like
/// `v0.1.8`; a tag that does not parse is treated as not newer rather than as an
/// upgrade, so a mislabelled release can never nag.
pub fn newer(tag: &str, current: &str) -> bool {
    match (parse(tag), parse(current)) {
        (Some(t), Some(c)) => t > c,
        _ => false,
    }
}

fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next()??;
    let patch = parts.next()??;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::{check, newer};

    /// Talks to GitHub, so it is opted into by hand: `cargo test --release
    /// -- --ignored release_listing`. Proves the request shape and the JSON fields
    /// against the real endpoint.
    #[test]
    #[ignore]
    fn release_listing_parses_and_an_old_build_sees_the_latest() {
        let found = check("0.0.1").expect("GitHub reachable").expect("a release exists");
        assert!(newer(&found.version, "0.0.1"));
        assert!(found.url.starts_with("https://github.com/"));
        assert_eq!(check(&found.version).expect("GitHub reachable"), None);
    }

    #[test]
    fn a_higher_tag_is_newer_and_the_same_or_lower_is_not() {
        assert!(newer("v0.1.8", "0.1.7"));
        assert!(newer("v0.2.0", "0.1.9"));
        assert!(newer("v1.0.0", "0.9.9"));
        assert!(!newer("v0.1.7", "0.1.7"));
        assert!(!newer("v0.1.6", "0.1.7"));
        assert!(!newer("0.1.8", "0.1.10"));
    }

    #[test]
    fn a_tag_that_is_not_a_version_never_counts_as_an_update() {
        assert!(!newer("latest", "0.1.7"));
        assert!(!newer("v0.2", "0.1.7"));
        assert!(!newer("v0.1.8-rc1", "0.1.7"));
        assert!(!newer("", "0.1.7"));
    }
}
