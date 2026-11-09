//! Persisted user preferences and credentials for the `midnight` CLI.
//!
//! * Preferences (`config.json`) hold the chosen retrieval provider, model, dimensions, endpoint and
//!   the remote-upload consent record. They never contain secrets.
//! * Credentials (`credentials.json`, mode 0600 in a 0700 directory) hold an API key only when the
//!   user typed one in `setup`. An environment variable always wins and is never copied to disk.
//!   Keys are never printed, never passed as arguments, and never appear in `status` output.
//! * `AETHER_MIDNIGHT_CONFIG_DIR` overrides the directory (tests, automation).
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cloud::{self, CloudConfig, Provider};

pub const CONFIG_VERSION: u32 = 1;

/// Retrieval provider chosen in `setup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    /// Keyword search only. Nothing is embedded and nothing leaves the machine.
    Lexical,
    /// On-device ONNX model (explicit, separately installed).
    Local,
    Openrouter,
    Rainy,
}

impl Choice {
    pub fn as_str(self) -> &'static str {
        match self {
            Choice::Lexical => "lexical",
            Choice::Local => "local",
            Choice::Openrouter => "openrouter",
            Choice::Rainy => "rainy",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "1" | "rainy" => Some(Choice::Rainy),
            "2" | "openrouter" => Some(Choice::Openrouter),
            "3" | "local" => Some(Choice::Local),
            "4" | "lexical" | "keyword" => Some(Choice::Lexical),
            _ => None,
        }
    }
    pub fn provider(self) -> Option<Provider> {
        match self {
            Choice::Rainy => Some(Provider::Rainy),
            Choice::Openrouter => Some(Provider::OpenRouter),
            Choice::Local => Some(Provider::Local),
            Choice::Lexical => None,
        }
    }
    pub fn is_remote(self) -> bool {
        matches!(self, Choice::Rainy | Choice::Openrouter)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub version: u32,
    pub provider: Choice,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub dimensions: Option<usize>,
    #[serde(default)]
    pub endpoint: Option<String>,
    /// `provider|destination|model` the user agreed to upload source chunks to. Any change to the
    /// provider, destination or model invalidates it.
    #[serde(default)]
    pub upload_consent: Option<String>,
    #[serde(default)]
    pub validated_at: Option<String>,
}

impl Settings {
    pub fn lexical() -> Self {
        Settings {
            version: CONFIG_VERSION,
            provider: Choice::Lexical,
            model: None,
            dimensions: None,
            endpoint: None,
            upload_consent: None,
            validated_at: None,
        }
    }

    /// Validated remote configuration (no network, no secrets). Errors are actionable.
    pub fn cloud_config(&self) -> Result<CloudConfig> {
        let Some(provider) = self.provider.provider().filter(|p| *p != Provider::Local) else {
            bail!(
                "provider {} is not a cloud provider",
                self.provider.as_str()
            );
        };
        CloudConfig::new(
            provider,
            self.model.clone(),
            self.dimensions,
            self.endpoint.clone(),
            cloud::DEFAULT_TIMEOUT_SECS,
            &[],
        )
        .context("saved provider settings are incomplete; run `midnight setup` again")
    }

    pub fn consent_token(&self) -> Result<String> {
        let cfg = self.cloud_config()?;
        Ok(format!(
            "{}|{}|{}",
            self.provider.as_str(),
            cfg.destination(),
            cfg.model
        ))
    }

    pub fn upload_consented(&self) -> bool {
        self.consent_token()
            .ok()
            .is_some_and(|t| self.upload_consent.as_deref() == Some(t.as_str()))
    }

    /// Human description of where text would be sent.
    pub fn destination(&self) -> String {
        self.cloud_config()
            .map(|c| c.destination())
            .unwrap_or_else(|_| "an unconfigured provider".into())
    }
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("AETHER_MIDNIGHT_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::config_dir().context("OS configuration directory is unavailable")?;
    Ok(base.join("aether-midnight"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.json"))
}

pub fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials.json"))
}

/// `None` when `setup` has never been run.
pub fn load() -> Result<Option<Settings>> {
    let path = config_path()?;
    match fs::read(&path) {
        Ok(bytes) => {
            let settings: Settings = serde_json::from_slice(&bytes).with_context(|| {
                format!(
                    "{} is not valid; fix it or run `midnight setup` to rewrite it",
                    path.display()
                )
            })?;
            if settings.version > CONFIG_VERSION {
                bail!(
                    "{} was written by a newer Midnight (config version {}); upgrade Midnight",
                    path.display(),
                    settings.version
                );
            }
            Ok(Some(settings))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict {}", dir.display()))?;
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp)
        .with_context(|| format!("write {}", tmp.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

pub fn save(settings: &Settings) -> Result<PathBuf> {
    let path = config_path()?;
    private_dir(&config_dir()?)?;
    write_private(&path, &serde_json::to_vec_pretty(settings)?)?;
    Ok(path)
}

#[derive(Default, Serialize, Deserialize)]
struct Credentials {
    #[serde(default)]
    keys: BTreeMap<String, String>,
}

fn load_credentials() -> Result<Credentials> {
    let path = credentials_path()?;
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is corrupt; delete it and re-run setup", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Credentials::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Store a key for a remote provider (0600). Replaces any earlier key for that provider.
pub fn store_key(choice: Choice, key: &str) -> Result<PathBuf> {
    if !choice.is_remote() {
        bail!("{} does not use an API key", choice.as_str());
    }
    let key = key.trim();
    if key.is_empty() || key.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("the API key must be a single token without spaces or control characters");
    }
    let mut creds = load_credentials()?;
    creds
        .keys
        .insert(choice.as_str().to_owned(), key.to_owned());
    private_dir(&config_dir()?)?;
    let path = credentials_path()?;
    write_private(&path, &serde_json::to_vec_pretty(&creds)?)?;
    Ok(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    Environment,
    Stored,
}

/// The key `choice` would use, preferring the environment variable. Value is never logged.
pub fn resolve_key(choice: Choice) -> Result<Option<(String, KeySource)>> {
    let Some(var) = choice.provider().and_then(Provider::key_env) else {
        return Ok(None);
    };
    if let Ok(value) = std::env::var(var)
        && !value.trim().is_empty()
    {
        return Ok(Some((value, KeySource::Environment)));
    }
    Ok(load_credentials()?
        .keys
        .remove(choice.as_str())
        .filter(|k| !k.trim().is_empty())
        .map(|k| (k, KeySource::Stored)))
}

/// Build the configured cloud embedder, or explain exactly what is missing.
pub fn cloud_embedder(settings: &Settings) -> Result<cloud::CloudEmbedder> {
    build_cloud_embedder(settings, false)
}

/// MCP's interactive query budget: one attempt with a five-second transport timeout. Timeouts
/// are not part of the embedding-space identity, so existing vectors remain compatible.
pub(crate) fn cloud_query_embedder(settings: &Settings) -> Result<cloud::CloudEmbedder> {
    build_cloud_embedder(settings, true)
}

fn build_cloud_embedder(settings: &Settings, query: bool) -> Result<cloud::CloudEmbedder> {
    let mut cfg = settings.cloud_config()?;
    if query {
        cfg.timeout_secs = 5;
    }
    let var = cfg.provider.key_env().unwrap_or_default();
    let Some((key, _)) = resolve_key(settings.provider)? else {
        bail!(
            "no API key for {}: export {var}=… or run `midnight setup`",
            settings.provider.as_str()
        );
    };
    let embedder = cloud::CloudEmbedder::new(cfg, key, var)?;
    Ok(if query {
        embedder.without_retries()
    } else {
        embedder
    })
}

/// Known-good starting points offered during setup. These are suggestions, not defaults: the saved
/// model is always the one the user confirmed, and dimensions are verified by the setup probe.
pub fn suggested_models(choice: Choice) -> &'static [(&'static str, usize)] {
    match choice {
        // Text embedding models with their documented native vector size. Order is the order
        // offered first in the wizard.
        Choice::Openrouter => &[
            ("openai/text-embedding-3-small", 1536),
            ("openai/text-embedding-3-large", 3072),
            ("qwen/qwen3-embedding-8b", 4096),
            ("qwen/qwen3-embedding-4b", 2560),
            ("google/gemini-embedding-001", 3072),
            ("baai/bge-m3", 1024),
        ],
        _ => &[],
    }
}

/// True for models that embed images (they cannot index source code): `titan-embed-image`,
/// CLIP and similar.
pub fn is_image_embedding_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    ["image", "clip", "vision"]
        .iter()
        .any(|word| id.contains(word))
}

/// The models to offer in the wizard, best first: image-only models removed, the ones in
/// `suggested_models` (that the provider really lists) first in that order, then the rest
/// alphabetically. Duplicates are dropped.
pub fn model_options(choice: Choice, listed: &[String]) -> Vec<String> {
    let mut text: Vec<String> = listed
        .iter()
        .filter(|id| !is_image_embedding_model(id))
        .cloned()
        .collect();
    text.sort();
    text.dedup();
    let mut out: Vec<String> = suggested_models(choice)
        .iter()
        .filter(|(id, _)| text.iter().any(|t| t == id))
        .map(|(id, _)| (*id).to_owned())
        .collect();
    let first = out.clone();
    out.extend(text.into_iter().filter(|id| !first.contains(id)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wizard_models_drop_image_models_and_lead_with_the_suggested_ones() {
        let listed: Vec<String> = [
            "amazon/titan-embed-image-v1",
            "amazon/titan-embed-text-v2",
            "baai/bge-m3",
            "openai/clip-vit",
            "qwen/qwen3-embedding-8b",
            "zeta/other-embed",
            "openai/text-embedding-3-small",
            "qwen/qwen3-embedding-8b",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            model_options(Choice::Openrouter, &listed),
            [
                "openai/text-embedding-3-small",
                "qwen/qwen3-embedding-8b",
                "baai/bge-m3",
                "amazon/titan-embed-text-v2",
                "zeta/other-embed"
            ]
        );
        // A suggested model the provider does not list is never invented.
        assert_eq!(
            model_options(Choice::Openrouter, &["a/b".to_string()]),
            ["a/b"]
        );
        assert!(is_image_embedding_model("Amazon/Titan-Embed-Image-v1"));
        assert!(!is_image_embedding_model("google/gemini-embedding-001"));
    }

    #[test]
    fn choice_parses_numbers_and_names() {
        assert_eq!(Choice::parse("1"), Some(Choice::Rainy));
        assert_eq!(Choice::parse(" OpenRouter "), Some(Choice::Openrouter));
        assert_eq!(Choice::parse("keyword"), Some(Choice::Lexical));
        assert_eq!(Choice::parse("9"), None);
    }

    #[test]
    fn consent_is_bound_to_provider_destination_and_model() {
        let mut s = Settings::lexical();
        s.provider = Choice::Openrouter;
        s.model = Some("a/b".into());
        s.dimensions = Some(8);
        assert!(!s.upload_consented());
        s.upload_consent = Some(s.consent_token().unwrap());
        assert!(s.upload_consented());
        s.model = Some("a/c".into());
        assert!(!s.upload_consented(), "model change invalidates consent");
    }
}
