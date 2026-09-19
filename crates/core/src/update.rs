// The update check. Amendment A31.
//
// One anonymous GET to GitHub's releases API, at most once a day, compared against the
// version compiled into the binary. Nothing is downloaded and nothing about the machine
// is sent: the request carries a User-Agent because GitHub refuses requests without
// one, and that is the whole of it. Brief section 2 ruled out "update pinging"; the
// owner asked for this one, and it can be switched off under About.

use anyhow::{anyhow, Context as _, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The public repository the releases come from.
pub const REPOSITORY: &str = "StanTheGorilla/lathe";

/// A file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// A published release newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Available {
    /// Without the leading `v`, as the About screen shows it.
    pub version: String,
    /// The release page, for a browser. The installers are attached there.
    pub url: String,
    /// This platform's installer, when the release carries one the app knows how to
    /// run: the NSIS `-setup.exe` on Windows. Elsewhere the page is the way.
    pub installer: Option<Asset>,
    /// The `SHA256SUMS` the release workflow attaches, for checking the installer.
    pub checksums: Option<Asset>,
}

/// The attached file this platform can install from, if any.
pub fn pick_installer(assets: &[Asset]) -> Option<Asset> {
    let wanted = |name: &str| {
        if cfg!(windows) {
            name.ends_with("-setup.exe")
        } else {
            false
        }
    };
    assets.iter().find(|a| wanted(&a.name)).cloned()
}

/// Fetches the installer into `dir`, reporting (bytes so far, total), and checks it
/// against the release's checksum list when there is one. Returns the file's path.
/// A release that lists checksums but not this file is refused: that is a broken
/// release, not a missing feature.
pub fn download_installer(
    available: &Available,
    dir: &Path,
    mut progress: impl FnMut(u64, u64),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    let installer = available
        .installer
        .as_ref()
        .ok_or_else(|| anyhow!("this release has no installer for this platform"))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(&installer.name);
    let part = dir.join(format!("{}.part", installer.name));

    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(60)))
        .build()
        .new_agent();

    let expected = match &available.checksums {
        Some(list) => {
            let text = agent
                .get(&list.url)
                .header("User-Agent", "lathe")
                .call()
                .map_err(|e| anyhow!("could not fetch the checksum list: {e}"))?
                .body_mut()
                .read_to_string()
                .context("reading the checksum list")?;
            Some(
                listed_sha256(&text, &installer.name)
                    .ok_or_else(|| anyhow!("{} is not in the release's SHA256SUMS", installer.name))?,
            )
        }
        None => None,
    };

    let mut response = agent
        .get(&installer.url)
        .header("User-Agent", "lathe")
        .call()
        .map_err(|e| anyhow!("could not start the download: {e}"))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(installer.size);
    progress(0, total);

    let mut file = std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;
    let mut reader = response.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        if cancel() {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(anyhow!("download cancelled"));
        }
        let read = reader.read(&mut buffer).context("reading from GitHub")?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).with_context(|| format!("writing {}", part.display()))?;
        hasher.update(&buffer[..read]);
        done += read as u64;
        progress(done, total);
    }
    file.flush()?;
    drop(file);

    if total > 0 && done < total {
        let _ = std::fs::remove_file(&part);
        return Err(anyhow!("the download ended early: got {done} bytes of {total}"));
    }
    let actual = format!("{:x}", hasher.finalize());
    if let Some(expected) = expected {
        if actual != expected {
            let _ = std::fs::remove_file(&part);
            return Err(anyhow!(
                "the installer does not match the release's checksum; not installing it"
            ));
        }
    }
    std::fs::rename(&part, &path).with_context(|| format!("moving into place: {}", path.display()))?;
    Ok(path)
}

/// The hash `sha256sum` wrote for `name` in a `SHA256SUMS` file: one `<hex>  <name>`
/// per line, a `*` before the name for binary mode, paths reduced to their last part.
pub fn listed_sha256(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim().trim_start_matches('*');
        let file = file.rsplit(['/', '\\']).next().unwrap_or(file);
        (file == name && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
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
    let assets: Vec<Asset> = parsed
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some(Asset {
                        name: a.get("name")?.as_str()?.to_string(),
                        url: a.get("browser_download_url")?.as_str()?.to_string(),
                        size: a.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(newer(tag, current).then(|| Available {
        version: tag.trim_start_matches('v').to_string(),
        url: page.to_string(),
        installer: pick_installer(&assets),
        checksums: assets.iter().find(|a| a.name == "SHA256SUMS").cloned(),
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
    use super::*;

    #[test]
    fn the_checksum_list_is_read_the_way_sha256sum_writes_it() {
        let text = "ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789  Lathe_0.1.10_x64-setup.exe\n\
                    1111111111111111111111111111111111111111111111111111111111111111 *./deb/lathe_0.1.10_amd64.deb\n";
        assert_eq!(
            listed_sha256(text, "Lathe_0.1.10_x64-setup.exe").as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
        assert_eq!(
            listed_sha256(text, "lathe_0.1.10_amd64.deb").as_deref(),
            Some("1111111111111111111111111111111111111111111111111111111111111111")
        );
        assert_eq!(listed_sha256(text, "Lathe.dmg"), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_takes_the_nsis_installer_and_nothing_else() {
        let asset = |name: &str| Asset {
            name: name.into(),
            url: String::new(),
            size: 1,
        };
        let assets = [
            asset("Lathe_0.1.10_aarch64.dmg"),
            asset("lathe_0.1.10_amd64.AppImage"),
            asset("Lathe_0.1.10_x64-setup.exe"),
            asset("SHA256SUMS"),
        ];
        assert_eq!(pick_installer(&assets).unwrap().name, "Lathe_0.1.10_x64-setup.exe");
        assert_eq!(pick_installer(&assets[..2]), None);
    }

    /// A mismatched checksum must leave nothing on disk that could be run.
    #[test]
    fn a_wrong_checksum_refuses_the_installer() {
        use std::io::{BufRead, BufReader};
        let body = b"not really an installer".to_vec();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                let payload: Vec<u8> = if first.contains("SHA256SUMS") {
                    format!("{}  x-setup.exe\n", "0".repeat(64)).into_bytes()
                } else {
                    body.clone()
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&payload).unwrap();
            }
        });
        let available = Available {
            version: "9.9.9".into(),
            url: String::new(),
            installer: Some(Asset { name: "x-setup.exe".into(), url: format!("{base}/x-setup.exe"), size: 0 }),
            checksums: Some(Asset { name: "SHA256SUMS".into(), url: format!("{base}/SHA256SUMS"), size: 0 }),
        };
        let dir = std::env::temp_dir().join(format!("lathe-update-test-{}", std::process::id()));
        let err = download_installer(&available, &dir, |_, _| {}, &|| false).unwrap_err();
        assert!(err.to_string().contains("checksum"), "{err}");
        assert!(!dir.join("x-setup.exe").exists());
        assert!(!dir.join("x-setup.exe.part").exists());
    }

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
