//! `midnight update`: explicit, verified self-update from GitHub Releases.
//!
//! Rules enforced here:
//! * Nothing is contacted unless the user ran `update`; there is no background check.
//! * Only the release list (`api.github.com`) and the release's own assets are fetched. Every URL
//!   (including each redirect hop) must be `https` on a GitHub host; the release JSON is treated as
//!   data, so an asset URL pointing elsewhere is refused.
//! * The archive is accepted only if its SHA-256 equals the release's `.sha256` asset, and the
//!   binary inside equals the hash recorded in the archive's own `SHA256SUMS`. The staged binary
//!   must run and report the expected version before it replaces the installed one; the swap is an
//!   atomic rename in the same directory, so a failure leaves the old binary untouched.
//! * Development builds (`target/debug`, `target/release`) are never overwritten.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const REPO: &str = "ferxalbs/aether-midnight";
const DEFAULT_API: &str = "https://api.github.com";
/// Test/mirror override. Must be `https`, or `http` on a loopback address.
pub const API_ENV: &str = "AETHER_MIDNIGHT_UPDATE_API";
const MAX_JSON_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

// ---------------------------------------------------------------------------------------------
// SemVer (only what release selection needs)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ident {
    Num(u64),
    Alpha(String),
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Num(_), Ident::Alpha(_)) => Ordering::Less,
            (Ident::Alpha(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Alpha(a), Ident::Alpha(b)) => a.cmp(b),
        }
    }
}
impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A SemVer 2.0 version (build metadata is ignored for precedence).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    core: (u64, u64, u64),
    pre: Vec<Ident>,
}

impl Version {
    /// Parses `1.2.3`, `v1.2.3` and `1.2.3-rc.1`. Anything else is `None`.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut numbers = core.split('.');
        let mut next = || -> Option<u64> {
            let part = numbers.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            if part.len() > 1 && part.starts_with('0') {
                return None;
            }
            part.parse().ok()
        };
        let core = (next()?, next()?, next()?);
        if numbers.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|id| {
                    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    {
                        None
                    } else if id.bytes().all(|b| b.is_ascii_digit()) {
                        id.parse().ok().map(Ident::Num)
                    } else {
                        Some(Ident::Alpha(id.to_string()))
                    }
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Version { core, pre })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.core.0, self.core.1, self.core.2)?;
        if !self.pre.is_empty() {
            let pre: Vec<String> = self
                .pre
                .iter()
                .map(|i| match i {
                    Ident::Num(n) => n.to_string(),
                    Ident::Alpha(a) => a.clone(),
                })
                .collect();
            write!(f, "-{}", pre.join("."))?;
        }
        Ok(())
    }
}

pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml version is valid SemVer")
}

// ---------------------------------------------------------------------------------------------
// Release selection (pure)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Release {
    pub tag: String,
    #[serde(serialize_with = "ser_version")]
    pub version: Version,
    pub prerelease: bool,
    pub archive_url: String,
    pub sha256_url: String,
}

fn ser_version<S: serde::Serializer>(v: &Version, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(v)
}

pub fn archive_name(version: &Version) -> String {
    format!("aether-midnight-{version}-macos-universal2")
}

/// Releases in `list` (the GitHub `releases` JSON array) that carry this platform's archive and
/// checksum, newest first. Drafts and tags that are not SemVer are ignored.
pub fn candidates(list: &Value) -> Vec<Release> {
    let mut out: Vec<Release> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["draft"] != true)
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            let version = Version::parse(tag)?;
            let name = format!("{}.tar.gz", archive_name(&version));
            let asset = |wanted: &str| -> Option<String> {
                r["assets"].as_array()?.iter().find_map(|a| {
                    (a["name"].as_str() == Some(wanted))
                        .then(|| a["browser_download_url"].as_str().map(str::to_string))
                        .flatten()
                })
            };
            Some(Release {
                tag: tag.to_string(),
                prerelease: r["prerelease"] == true || version.is_prerelease(),
                archive_url: asset(&name)?,
                sha256_url: asset(&format!("{name}.sha256"))?,
                version,
            })
        })
        .collect();
    out.sort_by(|a, b| b.version.cmp(&a.version));
    out
}

/// What `update` should consider the target.
pub enum Want {
    /// Newest release; prereleases only when allowed.
    Latest { include_prerelease: bool },
    /// Exactly this version (may be older than the current one).
    Exact(Version),
}

pub fn select(releases: &[Release], want: &Want) -> Option<Release> {
    match want {
        Want::Latest { include_prerelease } => releases
            .iter()
            .find(|r| *include_prerelease || !r.prerelease)
            .cloned(),
        Want::Exact(v) => releases.iter().find(|r| r.version == *v).cloned(),
    }
}

// ---------------------------------------------------------------------------------------------
// Network (bounded, host-restricted)
// ---------------------------------------------------------------------------------------------

struct Hosts {
    /// `host` or `host:port` entries allowed exactly (the override server).
    exact: Vec<String>,
    /// Always-allowed GitHub hosts over https.
    github: bool,
}

/// `(scheme, host_with_port)` of `url`, or `None` for anything with userinfo or an odd shape.
fn split_url(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some((scheme, authority))
}

fn host_only(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    authority.split(':').next().unwrap_or("")
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

impl Hosts {
    fn allows(&self, url: &str) -> bool {
        let Some((scheme, authority)) = split_url(url) else {
            return false;
        };
        let host = host_only(authority).to_ascii_lowercase();
        if self
            .exact
            .iter()
            .any(|e| e == &authority.to_ascii_lowercase())
        {
            return scheme == "https" || (scheme == "http" && is_loopback(&host));
        }
        self.github
            && scheme == "https"
            && !authority.contains(':')
            && (host == "api.github.com"
                || host == "github.com"
                || host.ends_with(".githubusercontent.com"))
    }
}

fn api_base() -> Result<(String, Hosts)> {
    match std::env::var(API_ENV).ok().filter(|v| !v.is_empty()) {
        None => Ok((
            DEFAULT_API.to_string(),
            Hosts {
                exact: Vec::new(),
                github: true,
            },
        )),
        Some(base) => {
            let base = base.trim_end_matches('/').to_string();
            let (scheme, authority) =
                split_url(&base).with_context(|| format!("{API_ENV} is not a valid URL"))?;
            if !(scheme == "https" || (scheme == "http" && is_loopback(host_only(authority)))) {
                bail!("{API_ENV} must be https, or http on a loopback address");
            }
            let hosts = Hosts {
                exact: vec![authority.to_ascii_lowercase()],
                github: false,
            };
            Ok((base, hosts))
        }
    }
}

fn http_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => t.kind().to_string(),
    }
}

/// GET `url` (following up to five redirects, each re-validated) into memory, at most `limit`
/// bytes.
fn fetch(url: &str, hosts: &Hosts, limit: u64, accept: &str) -> Result<Vec<u8>> {
    let agent = {
        let connector = native_tls::TlsConnector::new().context("initialize TLS")?;
        ureq::AgentBuilder::new()
            .tls_connector(std::sync::Arc::new(connector))
            .redirects(0)
            .timeout_connect(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(600))
            .user_agent(concat!("aether-midnight/", env!("CARGO_PKG_VERSION")))
            .build()
    };
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        if !hosts.allows(&current) {
            bail!(
                "refusing to contact {}: not an allowed release host",
                redact(&current)
            );
        }
        let response = agent
            .get(&current)
            .set("Accept", accept)
            .call()
            .map_err(|e| {
                anyhow::anyhow!("request to {} failed: {}", redact(&current), http_error(&e))
            })?;
        if (300..400).contains(&response.status()) {
            current = response
                .header("location")
                .context("redirect without a location")?
                .to_string();
            continue;
        }
        let mut body = Vec::new();
        response
            .into_reader()
            .take(limit + 1)
            .read_to_end(&mut body)
            .context("read response")?;
        if body.len() as u64 > limit {
            bail!(
                "response from {} is larger than {limit} bytes",
                redact(&current)
            );
        }
        return Ok(body);
    }
    bail!("too many redirects from {}", redact(url))
}

/// URL without query string or fragment (signed asset URLs carry tokens there).
fn redact(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// Releases of the repository that carry this platform's archive, newest first.
pub fn fetch_releases() -> Result<Vec<Release>> {
    let (base, hosts) = api_base()?;
    let url = format!("{base}/repos/{REPO}/releases?per_page=50");
    let body = fetch(&url, &hosts, MAX_JSON_BYTES, "application/vnd.github+json")?;
    let json: Value = serde_json::from_slice(&body).context("release list is not valid JSON")?;
    Ok(candidates(&json))
}

// ---------------------------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// First 64-hex token of a `.sha256` file.
fn parse_sha256_file(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).context("checksum file is not text")?;
    let token = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("checksum file does not start with a SHA-256");
    }
    Ok(token)
}

/// Checks the archive against `expected_sha`, returns the binary it contains after verifying it
/// against the archive's own `SHA256SUMS`.
pub fn verified_binary(archive: &[u8], expected_sha: &str, version: &Version) -> Result<Vec<u8>> {
    let actual = sha256_hex(archive);
    if actual != expected_sha {
        bail!("SHA-256 mismatch: expected {expected_sha}, got {actual}; nothing was changed");
    }
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(archive)
        .take(MAX_ARCHIVE_BYTES * 4)
        .read_to_end(&mut tar)
        .context("archive is not a readable gzip stream")?;
    let name = archive_name(version);
    let binary_member = format!("{name}/bin/aether-midnight");
    let sums_member = format!("{name}/SHA256SUMS");
    let members = crate::runtime::extract_members(&tar, &[&binary_member, &sums_member])?;
    let get = |wanted: &str| {
        members
            .iter()
            .find(|(n, _)| n == wanted)
            .map(|(_, bytes)| bytes.clone())
    };
    let binary = get(&binary_member)
        .with_context(|| format!("archive is not release {version} (missing {binary_member})"))?;
    let sums = String::from_utf8(
        get(&sums_member).with_context(|| format!("archive has no {sums_member}"))?,
    )
    .context("SHA256SUMS is not text")?;
    let recorded = sums
        .lines()
        .find_map(|line| {
            let (hash, file) = line.split_once(char::is_whitespace)?;
            (file.trim().trim_start_matches('*') == "./bin/aether-midnight")
                .then(|| hash.to_string())
        })
        .context("SHA256SUMS does not list bin/aether-midnight")?;
    if sha256_hex(&binary) != recorded.to_ascii_lowercase() {
        bail!("the binary does not match the archive's SHA256SUMS; nothing was changed");
    }
    Ok(binary)
}

/// The installed program this process is running from, resolved through symlinks. Refuses
/// Cargo build outputs.
pub fn installed_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("locate the running executable")?;
    if is_cargo_output(&exe) {
        bail!(
            "{} is a development build, not an installed release; refusing to modify it",
            exe.display()
        );
    }
    Ok(exe)
}

fn is_cargo_output(path: &Path) -> bool {
    let parts: Vec<_> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let profile = |name: &str| matches!(name, "debug" | "release" | "profiling");
    parts.windows(2).any(|w| w[0] == "target" && profile(&w[1]))
        || parts.windows(3).any(|w| w[0] == "target" && profile(&w[2]))
}

#[derive(Debug, Serialize)]
pub struct Applied {
    pub path: PathBuf,
    pub from: String,
    pub to: String,
}

/// Download, verify and swap in `release`. `target` is the installed binary.
pub fn apply(release: &Release, target: &Path) -> Result<Applied> {
    if !cfg!(target_os = "macos") && std::env::var_os(API_ENV).is_none() {
        bail!("update installs the macOS Universal 2 build; on this platform rebuild from source");
    }
    let (_, hosts) = api_base()?;
    let sha_file = fetch(
        &release.sha256_url,
        &hosts,
        4096,
        "application/octet-stream",
    )?;
    let expected = parse_sha256_file(&sha_file)?;
    let archive = fetch(
        &release.archive_url,
        &hosts,
        MAX_ARCHIVE_BYTES,
        "application/octet-stream",
    )?;
    let binary = verified_binary(&archive, &expected, &release.version)?;

    let dir = target
        .parent()
        .context("installed binary has no directory")?;
    let staged = dir.join(format!(".aether-midnight.update.{}", std::process::id()));
    let result = stage_and_swap(&binary, &staged, target, &release.version);
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result?;
    Ok(Applied {
        path: target.to_path_buf(),
        from: current_version().to_string(),
        to: release.version.to_string(),
    })
}

fn stage_and_swap(binary: &[u8], staged: &Path, target: &Path, version: &Version) -> Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o755);
    let mut file = options.open(staged).with_context(|| {
        format!(
            "cannot write next to {}; fix permissions or reinstall with install.sh",
            target.display()
        )
    })?;
    file.write_all(binary)?;
    file.sync_all()?;
    drop(file);
    let output = std::process::Command::new(staged)
        .arg("--version")
        .output()
        .context("the downloaded binary does not run on this machine")?;
    let reported = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !reported.trim_end().ends_with(&format!(" {version}")) {
        bail!(
            "the downloaded binary reported `{}` instead of version {version}; nothing was changed",
            reported.trim()
        );
    }
    std::fs::rename(staged, target).with_context(|| format!("replace {}", target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn semver_precedence_follows_the_spec() {
        let ordered = [
            "0.1.0-alpha",
            "0.1.0-alpha.1",
            "0.1.0-alpha.beta",
            "0.1.0-beta.2",
            "0.1.0-beta.11",
            "0.1.0-rc.1",
            "0.1.0-rc.2",
            "0.1.0",
            "0.1.1",
            "0.2.0",
            "1.0.0",
            "1.10.0",
        ];
        for pair in ordered.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(v("v0.1.0+build.5"), v("0.1.0"));
        assert_eq!(v("1.2.3-rc.1").to_string(), "1.2.3-rc.1");
        for bad in [
            "", "1", "1.2", "1.2.3.4", "01.2.3", "1.2.x", "1.2.3-", "latest", "v",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    fn release_json(tag: &str, pre: bool, draft: bool, with_assets: bool) -> Value {
        let version = Version::parse(tag).unwrap();
        let name = format!("{}.tar.gz", archive_name(&version));
        let assets = if with_assets {
            json!([
                {"name": name, "browser_download_url": format!("https://github.com/x/y/releases/download/{tag}/{name}")},
                {"name": format!("{name}.sha256"), "browser_download_url": format!("https://github.com/x/y/releases/download/{tag}/{name}.sha256")},
                {"name": "install.sh", "browser_download_url": "https://github.com/x/y/install.sh"}
            ])
        } else {
            json!([])
        };
        json!({"tag_name": tag, "prerelease": pre, "draft": draft, "assets": assets})
    }

    #[test]
    fn selection_skips_drafts_incomplete_releases_and_respects_prereleases() {
        let list = json!([
            release_json("v0.1.0-rc.1", true, false, true),
            release_json("v0.1.0-rc.2", true, false, true),
            release_json("v0.2.0", false, true, true),
            release_json("v0.1.1", false, false, false),
            release_json("v0.1.0", false, false, true),
            {"tag_name": "nightly", "prerelease": true, "draft": false, "assets": []},
        ]);
        let all = candidates(&list);
        let tags: Vec<_> = all.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["v0.1.0", "v0.1.0-rc.2", "v0.1.0-rc.1"]);

        let stable = select(
            &all,
            &Want::Latest {
                include_prerelease: false,
            },
        )
        .unwrap();
        assert_eq!(stable.tag, "v0.1.0");
        let only_pre = candidates(&json!([
            release_json("v0.1.0-rc.1", true, false, true),
            release_json("v0.1.0-rc.2", true, false, true)
        ]));
        assert!(
            select(
                &only_pre,
                &Want::Latest {
                    include_prerelease: false
                }
            )
            .is_none()
        );
        assert_eq!(
            select(
                &only_pre,
                &Want::Latest {
                    include_prerelease: true
                }
            )
            .unwrap()
            .tag,
            "v0.1.0-rc.2"
        );
        assert_eq!(
            select(&all, &Want::Exact(v("0.1.0-rc.1"))).unwrap().tag,
            "v0.1.0-rc.1"
        );
        assert!(select(&all, &Want::Exact(v("9.9.9"))).is_none());
    }

    #[test]
    fn only_github_and_the_override_host_are_contactable() {
        let github = Hosts {
            exact: vec![],
            github: true,
        };
        for ok in [
            "https://api.github.com/repos/a/b/releases",
            "https://github.com/a/b/releases/download/v1/x.tar.gz",
            "https://release-assets.githubusercontent.com/x?sig=1",
            "https://objects.githubusercontent.com/x",
        ] {
            assert!(github.allows(ok), "{ok}");
        }
        for bad in [
            "http://github.com/x",
            "https://evilgithub.com/x",
            "https://github.com.evil.example/x",
            "https://user@github.com/x",
            "https://github.com:8443/x",
            "https://evil.example/githubusercontent.com",
            "https://x.githubusercontent.com.evil.example/",
            "ftp://github.com/x",
            "file:///etc/passwd",
            "github.com/x",
        ] {
            assert!(!github.allows(bad), "{bad}");
        }
        let local = Hosts {
            exact: vec!["127.0.0.1:9".into()],
            github: false,
        };
        assert!(local.allows("http://127.0.0.1:9/a"));
        assert!(!local.allows("http://127.0.0.1:10/a"));
        assert!(!local.allows("https://api.github.com/"));
        let remote_http = Hosts {
            exact: vec!["example.com".into()],
            github: false,
        };
        assert!(
            !remote_http.allows("http://example.com/"),
            "plain http only on loopback"
        );
    }

    #[test]
    fn cargo_outputs_are_never_updated() {
        assert!(is_cargo_output(Path::new(
            "/w/aether-midnight/target/debug/midnight"
        )));
        assert!(is_cargo_output(Path::new(
            "/w/x/target/release/aether-midnight"
        )));
        assert!(is_cargo_output(Path::new(
            "/w/x/target/aarch64-apple-darwin/release/aether-midnight"
        )));
        assert!(!is_cargo_output(Path::new(
            "/Users/a/.local/bin/aether-midnight"
        )));
        assert!(!is_cargo_output(Path::new(
            "/Users/a/target-practice/bin/aether-midnight"
        )));
    }

    #[test]
    fn checksum_file_parsing_is_strict() {
        let hash = "a".repeat(64);
        assert_eq!(
            parse_sha256_file(format!("{hash}  file.tar.gz\n").as_bytes()).unwrap(),
            hash
        );
        assert!(parse_sha256_file(b"short  file").is_err());
        assert!(parse_sha256_file(format!("{}z", "a".repeat(63)).as_bytes()).is_err());
    }
}
