//! ONNX Runtime distribution: pinned, checksummed, explicit.
//!
//! Midnight loads ONNX Runtime dynamically (`ort-load-dynamic`) because Microsoft publishes no
//! `ort`-compatible static build for every target (and, for macOS x86_64, no release after 1.23.2).
//! This module owns everything about that library:
//!
//! * a table of *pinned* official Microsoft release archives (URL, size, archive SHA-256, SHA-256 of
//!   the extracted library), one entry per platform that was actually verified;
//! * `runtime install`: the only code path that downloads the runtime. It is explicit, refuses
//!   platforms without a pin, verifies size and SHA-256 before anything is extracted, extracts only
//!   named members, and records a manifest;
//! * `inspect`: locates the library (`ORT_DYLIB_PATH`, managed install, system paths), loads it with
//!   `dlopen`, reads its version through `OrtGetApiBase`, and reports actionable diagnostics.
//!
//! Lexical commands never call into this module.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString, c_char, c_void};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Oldest ONNX Runtime the pinned `ort` (2.0.0-rc.11 via FastEmbed 5.13.0) can drive.
pub const MIN_VERSION: (u32, u32) = (1, 22);

/// A verified, official ONNX Runtime release archive.
#[derive(Debug, Clone)]
pub struct PinnedRuntime {
    pub platform: &'static str,
    pub version: &'static str,
    pub url: &'static str,
    pub archive_size: u64,
    pub archive_sha256: &'static str,
    /// Archive member holding the shared library.
    pub library_member: &'static str,
    pub library_sha256: &'static str,
    /// Licence files copied next to the library.
    pub notice_members: &'static [&'static str],
}

/// Platforms with a pinned runtime. A platform that is not listed is *unsupported by
/// `runtime install`*; it may still work through `ORT_DYLIB_PATH`, but that is not claimed.
pub const PINNED: &[PinnedRuntime] = &[PinnedRuntime {
    platform: "macos-x86_64",
    version: "1.23.2",
    url: "https://github.com/microsoft/onnxruntime/releases/download/v1.23.2/onnxruntime-osx-x86_64-1.23.2.tgz",
    archive_size: 11_676_322,
    archive_sha256: "d10359e16347b57d9959f7e80a225a5b4a66ed7d7e007274a15cae86836485a6",
    library_member: "onnxruntime-osx-x86_64-1.23.2/lib/libonnxruntime.1.23.2.dylib",
    library_sha256: "8c9c78de65ea3786f987c0d980e9c1b13a3a5fbc6b3e2965ba05b450e6e4c054",
    notice_members: &[
        "onnxruntime-osx-x86_64-1.23.2/LICENSE",
        "onnxruntime-osx-x86_64-1.23.2/ThirdPartyNotices.txt",
    ],
}];

pub fn host_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub fn pinned_for_host() -> Option<&'static PinnedRuntime> {
    let platform = host_platform();
    PINNED.iter().find(|p| p.platform == platform)
}

pub fn library_file_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else if cfg!(windows) {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so"
    }
}

/// Directory of the managed runtime inside the models directory.
pub fn managed_dir(models: &Path) -> PathBuf {
    models.join("onnxruntime")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    EnvVar,
    Managed,
    System,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    pub platform: String,
    pub pinned_version: Option<&'static str>,
    pub install_supported: bool,
    pub source: Option<Source>,
    pub library: Option<PathBuf>,
    pub version: Option<String>,
    /// Loads, exposes `OrtGetApiBase`, and is at least [`MIN_VERSION`].
    pub compatible: bool,
    /// Managed install whose library hash equals the pinned hash.
    pub checksum_verified: bool,
    pub problems: Vec<String>,
    pub remedies: Vec<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

#[repr(C)]
struct OrtApiBase {
    get_api: unsafe extern "C" fn(u32) -> *const c_void,
    get_version_string: unsafe extern "C" fn() -> *const c_char,
}

/// Load `path` and return its ONNX Runtime version string. All failures are descriptive strings
/// (architecture mismatch, missing symbol, unreadable file), never panics.
pub fn probe_version(path: &Path) -> std::result::Result<String, String> {
    if !path.is_file() {
        return Err(format!(
            "{} does not exist or is not a file",
            path.display()
        ));
    }
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| "library path contains a NUL byte".to_string())?;
    // SAFETY: dlopen/dlsym/dlclose on a path the user configured; the symbol is only called after it
    // resolves, with the documented ONNX Runtime C ABI (`OrtGetApiBase` returns a static struct).
    unsafe {
        let handle = libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if handle.is_null() {
            let err = libc::dlerror();
            let detail = if err.is_null() {
                "unknown loader error".to_string()
            } else {
                CStr::from_ptr(err).to_string_lossy().into_owned()
            };
            return Err(format!("cannot load library: {detail}"));
        }
        let symbol = libc::dlsym(handle, c"OrtGetApiBase".as_ptr());
        let result = if symbol.is_null() {
            Err("library loads but does not export OrtGetApiBase (not ONNX Runtime)".to_string())
        } else {
            let get_base: unsafe extern "C" fn() -> *const OrtApiBase = std::mem::transmute(symbol);
            let base = get_base();
            if base.is_null() {
                Err("OrtGetApiBase returned null".to_string())
            } else {
                let version = ((*base).get_version_string)();
                if version.is_null() {
                    Err("runtime reported no version".to_string())
                } else {
                    Ok(CStr::from_ptr(version).to_string_lossy().into_owned())
                }
            }
        };
        libc::dlclose(handle);
        result
    }
}

fn parse_version(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

fn version_ok(version: &str) -> bool {
    parse_version(version).is_some_and(|v| v.0 == MIN_VERSION.0 && v >= MIN_VERSION)
}

/// Find the library without loading it. Order: `ORT_DYLIB_PATH`, managed install, system paths.
pub fn locate(models: &Path) -> Option<(PathBuf, Source)> {
    if let Some(explicit) = std::env::var_os("ORT_DYLIB_PATH") {
        return Some((PathBuf::from(explicit), Source::EnvVar));
    }
    let name = library_file_name();
    let managed = managed_dir(models).join(name);
    if managed.is_file() {
        return Some((managed, Source::Managed));
    }
    for dir in ["/opt/homebrew/lib", "/usr/local/lib", "/usr/lib"] {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return Some((candidate, Source::System));
        }
    }
    None
}

fn install_remedy(pin: Option<&PinnedRuntime>) -> Vec<String> {
    match pin {
        Some(p) => vec![format!(
            "run `aether-midnight runtime install` (downloads ONNX Runtime {} from {} after verifying its SHA-256)",
            p.version, p.url
        )],
        None => vec![format!(
            "no pinned ONNX Runtime exists for {}; install ONNX Runtime >= {}.{} yourself and set ORT_DYLIB_PATH (unverified by Midnight)",
            host_platform(),
            MIN_VERSION.0,
            MIN_VERSION.1
        )],
    }
}

/// Inspect the runtime the embedder would use. Never downloads; loads the library only to read its
/// version.
pub fn inspect(models: &Path) -> RuntimeStatus {
    let pin = pinned_for_host();
    let mut status = RuntimeStatus {
        platform: host_platform(),
        pinned_version: pin.map(|p| p.version),
        install_supported: pin.is_some(),
        source: None,
        library: None,
        version: None,
        compatible: false,
        checksum_verified: false,
        problems: Vec::new(),
        remedies: Vec::new(),
    };
    let Some((path, source)) = locate(models) else {
        status.problems.push(format!(
            "ONNX Runtime ({}) not found in ORT_DYLIB_PATH, {}, /opt/homebrew/lib, /usr/local/lib or /usr/lib",
            library_file_name(),
            managed_dir(models).display()
        ));
        status.remedies = install_remedy(pin);
        return status;
    };
    status.source = Some(source);
    status.library = Some(path.clone());
    match probe_version(&path) {
        Ok(version) => {
            if version_ok(&version) {
                status.compatible = true;
            } else {
                status.problems.push(format!(
                    "ONNX Runtime {version} is older than the required {}.{}",
                    MIN_VERSION.0, MIN_VERSION.1
                ));
                status.remedies = install_remedy(pin);
            }
            status.version = Some(version);
        }
        Err(e) => {
            status.problems.push(e);
            if source == Source::EnvVar {
                status.remedies.push(
                    "unset ORT_DYLIB_PATH, or point it at an ONNX Runtime library built for this CPU architecture".into(),
                );
            }
            status.remedies.extend(install_remedy(pin));
        }
    }
    if source == Source::Managed {
        match (pin, sha256_file(&path)) {
            (Some(p), Ok(actual)) if actual == p.library_sha256 => status.checksum_verified = true,
            (Some(_), Ok(_)) => {
                status.compatible = false;
                status.problems.push(
                    "managed ONNX Runtime does not match the pinned SHA-256 (modified or corrupt)"
                        .into(),
                );
                status.remedies.push(format!(
                    "delete {} and run `aether-midnight runtime install`",
                    managed_dir(models).display()
                ));
            }
            (_, Err(e)) => status.problems.push(format!("cannot hash runtime: {e:#}")),
            (None, _) => {}
        }
    }
    status
}

/// Return the library path the embedder should use, or an actionable error. Sets
/// `ORT_DYLIB_PATH` for the `ort` crate.
pub fn ensure_loadable(models: &Path) -> Result<PathBuf> {
    let status = inspect(models);
    if !status.compatible {
        let mut message = status.problems.join("; ");
        if message.is_empty() {
            message = "ONNX Runtime is unusable".into();
        }
        if !status.remedies.is_empty() {
            message.push_str(". Fix: ");
            message.push_str(&status.remedies.join(" or "));
        }
        bail!("{message} (diagnose with `aether-midnight doctor`)");
    }
    let path = status.library.expect("compatible implies a library path");
    // Single-threaded CLI path: nothing else reads the environment concurrently.
    unsafe { std::env::set_var("ORT_DYLIB_PATH", &path) };
    Ok(path)
}

// ---------------------------------------------------------------- install

fn tls_agent() -> Result<ureq::Agent> {
    let connector = native_tls::TlsConnector::new().context("initialize TLS")?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(connector))
        .timeout_connect(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(600))
        .user_agent(concat!("aether-midnight/", env!("CARGO_PKG_VERSION")))
        .build())
}

/// Extract the named regular-file members from an uncompressed ustar stream held in memory.
/// Anything not named is skipped; names are matched exactly (no path traversal is possible).
fn extract_members(tar: &[u8], wanted: &[&str]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    while offset + 512 <= tar.len() {
        let header = &tar[offset..offset + 512];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let field = |range: std::ops::Range<usize>| {
            let raw = &header[range];
            let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
            String::from_utf8_lossy(&raw[..end]).into_owned()
        };
        let mut name = field(0..100);
        let prefix = field(345..500);
        if !prefix.is_empty() {
            name = format!("{prefix}/{name}");
        }
        let size = usize::from_str_radix(field(124..136).trim(), 8)
            .map_err(|_| anyhow::anyhow!("corrupt tar header for {name}"))?;
        let typeflag = header[156];
        offset += 512;
        let end = offset
            .checked_add(size)
            .filter(|e| *e <= tar.len())
            .context("truncated tar archive")?;
        let name = name.trim_start_matches("./").to_string();
        if (typeflag == b'0' || typeflag == 0) && wanted.contains(&name.as_str()) {
            found.push((name, tar[offset..end].to_vec()));
        }
        offset += size.div_ceil(512) * 512;
    }
    Ok(found)
}

#[derive(Debug, Serialize)]
pub struct InstallReport {
    pub platform: &'static str,
    pub version: &'static str,
    pub directory: PathBuf,
    pub library: PathBuf,
    pub library_sha256: &'static str,
    pub archive_sha256: &'static str,
    pub source_url: String,
    pub already_installed: bool,
}

/// Download, verify and install the pinned runtime for this host into `<models>/onnxruntime/`.
pub fn install(models: &Path) -> Result<InstallReport> {
    let Some(pin) = pinned_for_host() else {
        bail!(
            "no pinned ONNX Runtime for {}; supported for install: {}. Install ONNX Runtime >= {}.{} manually and set ORT_DYLIB_PATH (not verified by Midnight)",
            host_platform(),
            PINNED
                .iter()
                .map(|p| p.platform)
                .collect::<Vec<_>>()
                .join(", "),
            MIN_VERSION.0,
            MIN_VERSION.1
        );
    };
    install_pinned(models, pin, pin.url)
}

/// `install` with an explicit pin and URL (tests point `url` at a local server).
pub fn install_pinned(models: &Path, pin: &PinnedRuntime, url: &str) -> Result<InstallReport> {
    let dir = managed_dir(models);
    let library = dir.join(library_file_name());
    let report = |already| InstallReport {
        platform: pin.platform,
        version: pin.version,
        directory: dir.clone(),
        library: library.clone(),
        library_sha256: pin.library_sha256,
        archive_sha256: pin.archive_sha256,
        source_url: pin.url.to_string(),
        already_installed: already,
    };
    if library.is_file() && sha256_file(&library).ok().as_deref() == Some(pin.library_sha256) {
        return Ok(report(true));
    }
    let response = tls_agent()?
        .get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("download ONNX Runtime from {url}: {}", scrub(&e)))?;
    let mut archive = Vec::new();
    response
        .into_reader()
        .take(pin.archive_size + 1)
        .read_to_end(&mut archive)
        .context("read ONNX Runtime download")?;
    if archive.len() as u64 != pin.archive_size {
        bail!(
            "ONNX Runtime download has {} bytes (pinned size {}); refusing to install",
            archive.len(),
            pin.archive_size
        );
    }
    let actual = sha256_hex(&archive);
    if actual != pin.archive_sha256 {
        bail!(
            "ONNX Runtime archive SHA-256 mismatch (expected {}, got {actual}); refusing to install",
            pin.archive_sha256
        );
    }
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(&archive[..])
        .take(512 * 1024 * 1024)
        .read_to_end(&mut tar)
        .context("decompress ONNX Runtime archive")?;
    let mut wanted = vec![pin.library_member];
    wanted.extend_from_slice(pin.notice_members);
    let members = extract_members(&tar, &wanted)?;
    let lib_bytes = members
        .iter()
        .find(|(n, _)| n == pin.library_member)
        .map(|(_, b)| b)
        .context("archive does not contain the runtime library")?;
    if sha256_hex(lib_bytes) != pin.library_sha256 {
        bail!("extracted library SHA-256 does not match the pin; refusing to install");
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let write = |name: &str, bytes: &[u8]| -> Result<()> {
        let tmp = dir.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, dir.join(name))?;
        Ok(())
    };
    write(library_file_name(), lib_bytes)?;
    for (member, bytes) in &members {
        if let Some(file) = member.rsplit('/').next()
            && member != pin.library_member
        {
            write(file, bytes)?;
        }
    }
    let manifest = serde_json::json!({
        "platform": pin.platform, "version": pin.version, "source_url": pin.url,
        "archive_sha256": pin.archive_sha256, "library_sha256": pin.library_sha256,
        "license": "MIT (Microsoft ONNX Runtime); see LICENSE and ThirdPartyNotices.txt",
    });
    write(
        "manifest.json",
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
    )?;
    Ok(report(false))
}

/// Error text without URLs' query strings or headers.
fn scrub(error: &ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => t.kind().to_string(),
    }
}

#[cfg(test)]
mod tests;
