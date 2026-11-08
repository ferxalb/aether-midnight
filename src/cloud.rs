//! Opt-in cloud embeddings (OpenRouter and Rainy-compatible endpoints) over `rainy-sdk`.
//!
//! Rules enforced here:
//! * Nothing is sent unless the caller explicitly selected a remote provider; the provider is never
//!   inferred from the environment, and a missing credential is an error, never a fallback to
//!   another provider or model.
//! * Credentials come from environment variables only (`OPENROUTER_API_KEY`, `RAINY_API_KEY`). They
//!   are held in the SDK's `SecretString`, never formatted, and redacted from every message that
//!   leaves this module.
//! * The vector space is identified by `provider:model`, the endpoint/parameter fingerprint and the
//!   declared dimensions; a response whose shape does not match is rejected, never coerced.
//! * One bounded request per batch, no automatic retry; failures surface as [`CloudError`] with a
//!   stable category so callers can degrade to lexical search.

use anyhow::{Result, bail};
use rainy_sdk::{
    AuthConfig, EmbeddingEncodingFormat, EmbeddingInput, EmbeddingValue, EmbeddingsRequest,
    RainyClient, RainyError,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

use crate::embed::{ModelIdentity, TextEmbedder, TextKind, l2_normalize};

pub const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1";
pub const OPENROUTER_KEY_ENV: &str = "OPENROUTER_API_KEY";
pub const RAINY_KEY_ENV: &str = "RAINY_API_KEY";
/// Texts per request. Small enough for provider batch limits and request-size caps.
pub const REMOTE_BATCH: usize = 16;
/// Characters per input after which text is truncated before sending (documented, deterministic).
pub const MAX_INPUT_CHARS: usize = 8_000;
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;
const MAX_ERROR_CHARS: usize = 240;
const MAX_DIMENSIONS: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Local,
    OpenRouter,
    Rainy,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Local => "local",
            Provider::OpenRouter => "openrouter",
            Provider::Rainy => "rainy",
        }
    }
    pub fn key_env(self) -> Option<&'static str> {
        match self {
            Provider::Local => None,
            Provider::OpenRouter => Some(OPENROUTER_KEY_ENV),
            Provider::Rainy => Some(RAINY_KEY_ENV),
        }
    }
    /// Optional request fields each provider documents; anything else is rejected.
    fn allowed_params(self) -> &'static [&'static str] {
        match self {
            Provider::Local => &[],
            // OpenRouter embeddings request schema: input_type, provider (routing), user.
            Provider::OpenRouter => &["input_type", "provider", "user"],
            // rainy-sdk's typed request: user.
            Provider::Rainy => &["user"],
        }
    }
}

/// Validated remote-provider configuration (no secrets).
#[derive(Debug, Clone)]
pub struct CloudConfig {
    pub provider: Provider,
    pub model: String,
    pub dimensions: usize,
    /// Versioned API base URL, without `/embeddings`. `None` = the SDK's default Rainy endpoint.
    pub endpoint: Option<String>,
    pub timeout_secs: u64,
    pub params: BTreeMap<String, Value>,
}

impl CloudConfig {
    /// Build and validate. `params` are `KEY=JSON` strings (a bare non-JSON value is a string).
    pub fn new(
        provider: Provider,
        model: Option<String>,
        dimensions: Option<usize>,
        endpoint: Option<String>,
        timeout_secs: u64,
        params: &[String],
    ) -> Result<Self> {
        if provider == Provider::Local {
            bail!("CloudConfig is only for remote providers");
        }
        let Some(model) = model
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
        else {
            bail!(
                "provider {} needs an explicit --model (Midnight never substitutes a model)",
                provider.as_str()
            );
        };
        if model.len() > 200 || model.chars().any(|c| c.is_control() || c.is_whitespace()) {
            bail!("--model must be a single token of at most 200 characters");
        }
        let Some(dimensions) = dimensions else {
            bail!(
                "provider {} needs --dimensions <n>: the vector size is part of the embedding-space identity and is validated on every response",
                provider.as_str()
            );
        };
        if dimensions == 0 || dimensions > MAX_DIMENSIONS {
            bail!("--dimensions must be between 1 and {MAX_DIMENSIONS}");
        }
        if timeout_secs == 0 || timeout_secs > MAX_TIMEOUT_SECS {
            bail!("--timeout-secs must be between 1 and {MAX_TIMEOUT_SECS}");
        }
        let endpoint = match (provider, endpoint) {
            (Provider::OpenRouter, None) => Some(OPENROUTER_ENDPOINT.to_string()),
            (_, e) => e.map(|e| e.trim_end_matches('/').to_string()),
        };
        if let Some(url) = &endpoint
            && url.ends_with("/embeddings")
        {
            bail!("--endpoint is the API base (e.g. https://host/v1); /embeddings is appended");
        }
        let mut map = BTreeMap::new();
        for raw in params {
            let Some((key, value)) = raw.split_once('=') else {
                bail!("--param expects KEY=VALUE, got {raw:?}");
            };
            if !provider.allowed_params().contains(&key) {
                bail!(
                    "--param {key} is not supported for provider {} (supported: {:?}); `dimensions` is set with --dimensions",
                    provider.as_str(),
                    provider.allowed_params()
                );
            }
            let parsed = serde_json::from_str::<Value>(value)
                .unwrap_or_else(|_| Value::String(value.to_string()));
            if map.insert(key.to_string(), parsed).is_some() {
                bail!("--param {key} given twice");
            }
        }
        Ok(Self {
            provider,
            model,
            dimensions,
            endpoint,
            timeout_secs,
            params: map,
        })
    }

    /// Stable identity of the vector space. The revision fingerprints the endpoint and request
    /// parameters, because either can change what the same model name returns.
    pub fn identity(&self) -> ModelIdentity {
        let mut hasher = Sha256::new();
        hasher.update(self.endpoint.as_deref().unwrap_or("rainy-default"));
        hasher.update(b"\n");
        hasher.update(serde_json::to_string(&self.params).unwrap_or_default());
        let fingerprint: String = hasher
            .finalize()
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect();
        ModelIdentity {
            name: format!("{}:{}", self.provider.as_str(), self.model),
            revision: format!("remote-{fingerprint}"),
            dimensions: self.dimensions,
        }
    }

    pub fn destination(&self) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| "the Rainy default endpoint".to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudErrorKind {
    Config,
    Auth,
    Payment,
    RateLimit,
    Server,
    Timeout,
    Network,
    Rejected,
    Malformed,
    Dimensions,
}

/// Bounded, secret-free provider failure.
#[derive(Debug, Clone)]
pub struct CloudError {
    pub kind: CloudErrorKind,
    pub message: String,
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CloudError {}

fn bounded(text: &str, secret: &str) -> String {
    let mut clean = if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "[redacted]")
    };
    clean.retain(|c| !c.is_control() || c == ' ');
    if clean.chars().count() > MAX_ERROR_CHARS {
        clean = clean.chars().take(MAX_ERROR_CHARS).collect::<String>() + "…";
    }
    clean
}

fn classify(error: &RainyError, secret: &str) -> CloudError {
    let text = bounded(&error.to_string(), secret);
    let lower = text.to_lowercase();
    let (kind, message) = match error {
        RainyError::Authentication { .. } | RainyError::AccessDenied { .. } => (
            CloudErrorKind::Auth,
            format!("authentication failed (HTTP 401/403): check the API key. {text}"),
        ),
        RainyError::InsufficientCredits { .. } => (
            CloudErrorKind::Payment,
            format!("provider reports insufficient credits (HTTP 402): {text}"),
        ),
        RainyError::RateLimit { retry_after, .. } => (
            CloudErrorKind::RateLimit,
            match retry_after {
                Some(s) => format!("rate limited (HTTP 429); retry after {s}s: {text}"),
                None => format!("rate limited (HTTP 429): {text}"),
            },
        ),
        RainyError::Timeout { .. } => (
            CloudErrorKind::Timeout,
            "request timed out; raise --timeout-secs or retry".to_string(),
        ),
        RainyError::Network { .. } => (CloudErrorKind::Network, format!("network error: {text}")),
        RainyError::Api { status_code, .. } if *status_code >= 500 => (
            CloudErrorKind::Server,
            format!("provider server error (HTTP {status_code}): {text}"),
        ),
        RainyError::Provider { .. } => (
            CloudErrorKind::Server,
            format!("upstream provider error: {text}"),
        ),
        RainyError::Serialization { .. } => (
            CloudErrorKind::Malformed,
            "provider returned a response that is not a valid embeddings payload".to_string(),
        ),
        // The SDK reports every other 4xx as InvalidRequest and drops the status line, so a 402
        // whose body lacks the INSUFFICIENT_CREDITS code is recognised by its message.
        RainyError::InvalidRequest { .. }
            if lower.contains("credit")
                || lower.contains("payment")
                || lower.contains("billing")
                || lower.contains("balance") =>
        {
            (
                CloudErrorKind::Payment,
                format!("provider reports a payment/credit problem (HTTP 402): {text}"),
            )
        }
        _ => (
            CloudErrorKind::Rejected,
            format!("provider rejected the request: {text}"),
        ),
    };
    CloudError { kind, message }
}

fn truncate_input(text: &str) -> String {
    match text.char_indices().nth(MAX_INPUT_CHARS) {
        Some((end, _)) => text[..end].to_string(),
        None => text.to_string(),
    }
}

pub struct CloudEmbedder {
    runtime: tokio::runtime::Runtime,
    client: RainyClient,
    config: CloudConfig,
    identity: ModelIdentity,
    secret: String,
}

impl CloudEmbedder {
    /// Connect using the provider's environment credential. No network traffic happens here.
    pub fn from_env(config: CloudConfig) -> Result<Self> {
        let var = config
            .provider
            .key_env()
            .expect("remote provider has a key");
        let key = std::env::var(var).unwrap_or_default();
        Self::new(config, key, var)
    }

    pub fn new(config: CloudConfig, api_key: String, key_env: &str) -> Result<Self> {
        if api_key.trim().is_empty() {
            return Err(CloudError {
                kind: CloudErrorKind::Config,
                message: format!(
                    "{} is not set; export {key_env} to use provider {} (content is never sent without it)",
                    key_env,
                    config.provider.as_str()
                ),
            }
            .into());
        }
        let mut auth = AuthConfig::new(api_key.clone())
            .with_timeout(config.timeout_secs)
            .with_user_agent(concat!("aether-midnight/", env!("CARGO_PKG_VERSION")));
        // No automatic retries: one explicit, bounded attempt per batch.
        auth.enable_retry = false;
        auth.max_retries = 0;
        if let Some(endpoint) = &config.endpoint {
            auth = auth.with_api_base_url(endpoint.clone());
        }
        let client = RainyClient::with_config(auth).map_err(|e| CloudError {
            kind: CloudErrorKind::Config,
            message: format!(
                "invalid provider configuration: {}",
                bounded(&e.to_string(), &api_key)
            ),
        })?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let identity = config.identity();
        Ok(Self {
            runtime,
            client,
            config,
            identity,
            secret: api_key,
        })
    }

    pub fn config(&self) -> &CloudConfig {
        &self.config
    }

    fn request(&self, batch: &[String]) -> EmbeddingsRequest {
        let mut request = EmbeddingsRequest {
            model: self.config.model.clone(),
            input: EmbeddingInput::Texts(batch.to_vec()),
            encoding_format: Some(EmbeddingEncodingFormat::Float),
            dimensions: Some(self.config.dimensions as u32),
            user: None,
            metadata: None,
            extra: BTreeMap::new(),
        };
        for (key, value) in &self.config.params {
            if key == "user" {
                request.user = value.as_str().map(str::to_string);
            } else {
                request.extra.insert(key.clone(), value.clone());
            }
        }
        request
    }

    fn embed_batch(&self, batch: &[String]) -> std::result::Result<Vec<Vec<f32>>, CloudError> {
        let request = self.request(batch);
        let response = self
            .runtime
            .block_on(self.client.create_embeddings(request))
            .map_err(|e| classify(&e, &self.secret))?;
        let malformed = |message: String| CloudError {
            kind: CloudErrorKind::Malformed,
            message,
        };
        if response.data.len() != batch.len() {
            return Err(malformed(format!(
                "provider returned {} embeddings for {} inputs",
                response.data.len(),
                batch.len()
            )));
        }
        // Responses are placed by their `index`, not by arrival order.
        let mut slots: Vec<Option<Vec<f32>>> = vec![None; batch.len()];
        for item in response.data {
            let index = item.index as usize;
            let Some(slot) = slots.get_mut(index) else {
                return Err(malformed(format!(
                    "provider returned out-of-range index {index} for {} inputs",
                    batch.len()
                )));
            };
            if slot.is_some() {
                return Err(malformed(format!("provider returned index {index} twice")));
            }
            let EmbeddingValue::Float(mut vector) = item.embedding else {
                return Err(malformed(
                    "provider returned base64 vectors although float was requested".into(),
                ));
            };
            if vector.len() != self.config.dimensions {
                return Err(CloudError {
                    kind: CloudErrorKind::Dimensions,
                    message: format!(
                        "provider returned {}-dimensional vectors, expected {} (--dimensions); refusing to store an incompatible vector",
                        vector.len(),
                        self.config.dimensions
                    ),
                });
            }
            if vector.iter().any(|v| !v.is_finite()) {
                return Err(malformed(
                    "provider returned a non-finite vector value".into(),
                ));
            }
            if vector.iter().all(|v| *v == 0.0) {
                return Err(malformed("provider returned an all-zero vector".into()));
            }
            l2_normalize(&mut vector);
            *slot = Some(vector);
        }
        slots
            .into_iter()
            .map(|s| s.ok_or_else(|| malformed("provider response is missing an index".into())))
            .collect()
    }
}

impl TextEmbedder for CloudEmbedder {
    fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    fn embed(&mut self, _kind: TextKind, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // Query and passage are embedded identically: the remote API has no E5 prefix contract.
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(REMOTE_BATCH) {
            let inputs: Vec<String> = batch.iter().map(|t| truncate_input(t)).collect();
            if inputs.iter().any(|t| t.trim().is_empty()) {
                bail!("cannot embed empty text");
            }
            out.extend(self.embed_batch(&inputs)?);
        }
        Ok(out)
    }
}

/// True when `error` (from an embedder) is a cloud failure, for callers that want to say so.
pub fn as_cloud_error(error: &anyhow::Error) -> Option<&CloudError> {
    error.downcast_ref::<CloudError>()
}

#[cfg(test)]
mod tests;
