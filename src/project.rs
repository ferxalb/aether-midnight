//! Per-project settings (`.midnight/midnight.json`).
//!
//! Layering, lowest to highest precedence: built-in defaults, the user's global configuration
//! (`config.json`: provider, model, credentials, upload consent), this project file, then
//! command-line flags. The project file is deliberately weak: it has no field for an endpoint, a
//! credential, a provider or upload consent, so it can only switch *off* or *narrow* behavior
//! the user already approved globally. Unknown fields are rejected so typos are not silent. The
//! file is only read for projects the user set up on this machine (see `connect::trust_project`).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "midnight.json";
const MAX_BYTES: u64 = 64 * 1024;
const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    #[serde(default)]
    pub version: Option<u32>,
    /// `false` keeps this project on keyword search even when a global provider is configured.
    #[serde(default)]
    pub embeddings: Option<bool>,
    #[serde(default)]
    pub mcp: McpConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// Serve only read/recall tools for this project (flags can still ask for it too).
    #[serde(default)]
    pub read_only: Option<bool>,
    /// Memory namespaces the MCP tools may touch here; a trailing `*` matches a prefix.
    #[serde(default)]
    pub memory_scopes: Option<Vec<String>>,
}

impl ProjectConfig {
    pub fn template() -> Self {
        ProjectConfig {
            version: Some(VERSION),
            embeddings: Some(true),
            mcp: McpConfig {
                read_only: Some(false),
                memory_scopes: None,
            },
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: ProjectConfig =
            serde_json::from_str(text).context("invalid project configuration")?;
        if config.version.is_some_and(|v| v > VERSION) {
            bail!("project configuration was written by a newer Midnight; upgrade Midnight");
        }
        Ok(config)
    }

    pub fn embeddings_disabled(&self) -> bool {
        self.embeddings == Some(false)
    }
}

/// Where the project file lives for a project rooted at `root`.
pub fn path_for(root: &Path) -> PathBuf {
    root.join(crate::connect::PROJECT_DIR).join(FILE_NAME)
}

/// Settings of the trusted project containing `start`. `Ok(None)` when there is none; an error
/// (with the file path) when the file exists but is unusable, so a typo is never ignored.
pub fn load_for(start: &Path) -> Result<Option<ProjectConfig>> {
    let Some(path) = crate::connect::discover_project_file(start, FILE_NAME) else {
        return Ok(None);
    };
    let len = fs::metadata(&path)?.len();
    if len > MAX_BYTES {
        bail!("{} is larger than {MAX_BYTES} bytes", path.display());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    ProjectConfig::parse(&text)
        .with_context(|| format!("{}", path.display()))
        .map(Some)
}

/// Create the project file with defaults unless it exists. Returns whether it was written.
pub fn write_template_if_missing(root: &Path) -> Result<bool> {
    let path = path_for(root);
    if path.exists() {
        return Ok(false);
    }
    let text = serde_json::to_string_pretty(&ProjectConfig::template())? + "\n";
    fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_round_trips_and_unknown_or_dangerous_fields_are_rejected() {
        let text = serde_json::to_string(&ProjectConfig::template()).unwrap();
        assert_eq!(
            ProjectConfig::parse(&text).unwrap(),
            ProjectConfig::template()
        );
        for bad in [
            r#"{"endpoint":"https://evil.example"}"#,
            r#"{"provider":"openrouter"}"#,
            r#"{"upload_consent":"x"}"#,
            r#"{"mcp":{"api_key":"k"}}"#,
            r#"{"version":99}"#,
            "not json",
        ] {
            assert!(ProjectConfig::parse(bad).is_err(), "{bad}");
        }
        assert!(
            ProjectConfig::parse(r#"{"embeddings":false}"#)
                .unwrap()
                .embeddings_disabled()
        );
        assert!(!ProjectConfig::parse("{}").unwrap().embeddings_disabled());
    }
}
