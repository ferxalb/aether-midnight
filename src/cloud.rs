//! Opt-in cloud embeddings using rainy-sdk request/config types and bounded HTTP transport.
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
//! * Bounded requests (<= 32 texts / 64 KB), eight in flight by default and adaptive on 429; only
//!   transient answers (408, 429, 5xx, in-flight-budget 402) are retried, at most three times;
//!   failures surface as
//!   [`CloudError`] with a stable category so callers can degrade to lexical search.

use anyhow::{Result, bail};
use rainy_sdk::{
    AuthConfig, EmbeddingEncodingFormat, EmbeddingInput, EmbeddingValue, EmbeddingsRequest,
    RainyClient,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::embed::{ModelIdentity, TextEmbedder, TextKind, l2_normalize};

pub const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1";
pub const OPENROUTER_KEY_ENV: &str = "OPENROUTER_API_KEY";
pub const RAINY_KEY_ENV: &str = "RAINY_API_KEY";
/// Texts per request. Measured: per-request latency is mostly fixed overhead, so larger batches
/// raise throughput until the provider's per-request token quota (see `docs/embeddings.md`).
pub const REMOTE_BATCH_MAX_ITEMS: usize = 32;
/// Input bytes per request after truncation (~14-21k tokens for code). A request is closed early
/// when the next text would exceed this, so one request stays far below the token quota.
pub const REMOTE_BATCH_MAX_BYTES: usize = 64_000;
/// Starting (and default ceiling) concurrency; it adapts downward on 429 and back up on success.
pub const DEFAULT_REMOTE_CONCURRENCY: usize = 8;
pub const MAX_REMOTE_CONCURRENCY: usize = 32;
/// Extra attempts for transient answers only (408, 429, 5xx, in-flight-budget 402).
pub const REMOTE_RETRIES: usize = 3;
/// Upper bound for any provider-suggested or computed retry delay.
const MAX_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
pub const REMOTE_CONCURRENCY_ENV: &str = "MIDNIGHT_EMBED_CONCURRENCY";
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

fn truncate_input(text: &str) -> String {
    match text.char_indices().nth(MAX_INPUT_CHARS) {
        Some((end, _)) => text[..end].to_string(),
        None => text.to_string(),
    }
}

// Optional metadata is intentionally ignored; vector data remains mandatory and strictly typed.
#[derive(Deserialize)]
struct WireResponse {
    data: Vec<WireEmbedding>,
}
#[derive(Deserialize)]
struct WireEmbedding {
    index: Option<usize>,
    embedding: EmbeddingValue,
}

type BatchResult = std::result::Result<Vec<Vec<f32>>, CloudError>;

pub struct CloudEmbedder {
    agent: ureq::Agent,
    api_base_url: String,
    config: CloudConfig,
    identity: ModelIdentity,
    secret: String,
    concurrency: usize,
    retry_backoff: std::time::Duration,
    max_retries: usize,
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
        let concurrency = match std::env::var(REMOTE_CONCURRENCY_ENV) {
            Ok(value) => value
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=MAX_REMOTE_CONCURRENCY).contains(n))
                .ok_or_else(|| CloudError {
                    kind: CloudErrorKind::Config,
                    message: format!(
                        "{REMOTE_CONCURRENCY_ENV} must be between 1 and {MAX_REMOTE_CONCURRENCY}"
                    ),
                })?,
            Err(std::env::VarError::NotPresent) => DEFAULT_REMOTE_CONCURRENCY,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(CloudError {
                    kind: CloudErrorKind::Config,
                    message: format!("{REMOTE_CONCURRENCY_ENV} must be valid text"),
                }
                .into());
            }
        };
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
        let api_base_url = client.api_base_url();
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .user_agent(concat!("aether-midnight/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .tls_connector(std::sync::Arc::new(native_tls::TlsConnector::new()?))
            .build();
        let identity = config.identity();
        Ok(Self {
            agent,
            api_base_url,
            config,
            identity,
            secret: api_key,
            concurrency,
            retry_backoff: std::time::Duration::from_millis(1000),
            max_retries: REMOTE_RETRIES,
        })
    }

    pub fn config(&self) -> &CloudConfig {
        &self.config
    }

    /// Interactive MCP queries fall back to lexical after one failed attempt. Bulk embedding
    /// retains its bounded retry policy; a transient outage must not stall an agent's tool loop.
    pub(crate) fn without_retries(mut self) -> Self {
        self.max_retries = 0;
        self
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

    /// Retries only transient provider answers, at most [`REMOTE_RETRIES`] times, waiting for the
    /// provider's own hint (`Retry-After` or "retry after N sec") when it gave one and otherwise
    /// 1 s, 2 s, 4 s plus jitter. Auth, payment, malformed and dimension errors are never retried,
    /// and a retry always goes to the same provider. A 429 also lowers the shared concurrency.
    fn embed_batch_with_retry(
        &self,
        batch: &[String],
        gate: &Gate,
    ) -> std::result::Result<Vec<Vec<f32>>, CloudError> {
        let mut attempt = 0;
        loop {
            let mut hint = RetryHint::default();
            match self.embed_batch(batch, &mut hint) {
                Ok(vectors) => {
                    gate.on_success();
                    return Ok(vectors);
                }
                Err(e) => {
                    if hint.rate_limited {
                        gate.on_rate_limit();
                    }
                    if !hint.retryable || attempt >= self.max_retries {
                        return Err(e);
                    }
                    attempt += 1;
                    if !self.retry_backoff.is_zero() {
                        std::thread::sleep(retry_delay(&hint, self.retry_backoff, attempt));
                    }
                }
            }
        }
    }

    fn embed_batch(
        &self,
        batch: &[String],
        hint: &mut RetryHint,
    ) -> std::result::Result<Vec<Vec<f32>>, CloudError> {
        let request = self.request(batch);
        // The SDK's typed decoder requires optional OpenRouter fields and its error mapping
        // loses generic 402 statuses. Keep SDK request/config validation, but retain wire status.
        let body = serde_json::to_vec(&request).map_err(|_| CloudError {
            kind: CloudErrorKind::Config,
            message: "invalid embedding request".into(),
        })?;
        let result = self
            .agent
            .post(&format!("{}/embeddings", self.api_base_url))
            .set("Authorization", &format!("Bearer {}", self.secret))
            .set("Content-Type", "application/json")
            .send_bytes(&body);
        let response = match result {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => {
                let timeout = error.kind() == ureq::ErrorKind::Io
                    && error.to_string().to_lowercase().contains("timed out");
                return Err(CloudError {
                    kind: if timeout { CloudErrorKind::Timeout } else { CloudErrorKind::Network },
                    message: "provider request failed or timed out; check connectivity and --timeout-secs".into(),
                });
            }
        };
        let status = response.status();
        if !(200..300).contains(&status) {
            let header_delay = response
                .header("retry-after")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(std::time::Duration::from_secs);
            // The body is read only to classify the answer and find a retry hint; it can contain
            // submitted text or credentials, so it is never echoed.
            let mut body = Vec::new();
            let _ = response.into_reader().take(8 * 1024).read_to_end(&mut body);
            let body = String::from_utf8_lossy(&body);
            *hint = classify_failure(status, header_delay, &body);
            return Err(CloudError {
                kind: match status {
                    401 | 403 => CloudErrorKind::Auth,
                    402 => CloudErrorKind::Payment,
                    408 => CloudErrorKind::Timeout,
                    429 => CloudErrorKind::RateLimit,
                    500..=599 => CloudErrorKind::Server,
                    _ => CloudErrorKind::Rejected,
                },
                message: format!(
                    "{} (HTTP {status}); check credentials, quota and provider configuration",
                    match status {
                        401 | 403 => "authentication failed",
                        402 => "payment required",
                        408 => "provider request timed out",
                        429 => "rate limited",
                        500..=599 => "provider server error",
                        _ => "provider rejected embedding request",
                    }
                ),
            });
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| CloudError {
                kind: CloudErrorKind::Network,
                message: "failed reading provider response".into(),
            })?;
        let response: WireResponse = if bytes.len() > 8 * 1024 * 1024 {
            None
        } else {
            serde_json::from_slice(&bytes).ok()
        }
        .ok_or_else(|| CloudError {
            kind: CloudErrorKind::Malformed,
            message: "provider returned an invalid or oversized embeddings payload".into(),
        })?;
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
        let indexed = response
            .data
            .iter()
            .filter(|item| item.index.is_some())
            .count();
        if indexed != 0 && indexed != response.data.len() {
            return Err(malformed(
                "provider mixed indexed and unindexed embeddings".into(),
            ));
        }
        for (position, item) in response.data.into_iter().enumerate() {
            let index = item.index.unwrap_or(position);
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
            if vector.iter().any(|v| !v.is_finite()) || vector.iter().all(|v| *v == 0.0) {
                return Err(malformed(
                    "provider vector cannot be normalized safely".into(),
                ));
            }
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

    fn preferred_batch_size(&self) -> usize {
        REMOTE_BATCH_MAX_ITEMS * self.concurrency
    }

    fn embed(&mut self, _kind: TextKind, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // Query and passage are embedded identically: the remote API has no E5 prefix contract.
        let inputs: Vec<String> = texts.iter().map(|t| truncate_input(t)).collect();
        if inputs.iter().any(|t| t.trim().is_empty()) {
            bail!("cannot embed empty text");
        }
        let batches: Vec<&[String]> = plan_batches(&inputs);
        let workers = self.concurrency.min(batches.len());
        let this: &CloudEmbedder = self;
        // Work queue: a worker first obtains a concurrency permit, then takes the next batch, so a
        // taken batch is always sent and a slow request does not hold back the others. Results are
        // stored by batch index to keep input order.
        let gate = Gate::new(self.concurrency);
        let next = AtomicUsize::new(0);
        let abort = AtomicBool::new(false);
        let slots: Mutex<Vec<Option<BatchResult>>> =
            Mutex::new((0..batches.len()).map(|_| None).collect());
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    while gate.acquire(&abort) {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(batch) = batches.get(i) else {
                            gate.release();
                            break;
                        };
                        let result = this.embed_batch_with_retry(batch, &gate);
                        if result.is_err() {
                            abort.store(true, Ordering::Relaxed);
                        }
                        if let Ok(mut guard) = slots.lock() {
                            guard[i] = Some(result);
                        }
                        gate.release();
                    }
                });
            }
        });
        let slots = slots.into_inner().map_err(|_| CloudError {
            kind: CloudErrorKind::Network,
            message: "provider request worker failed".into(),
        })?;
        // The first error in input order wins; a missing slot is only possible after an abort.
        let mut out = Vec::with_capacity(texts.len());
        let mut incomplete = false;
        for slot in slots {
            match slot {
                Some(result) => out.extend(result?),
                None => incomplete = true,
            }
        }
        if incomplete {
            bail!("embedding aborted after a provider failure");
        }
        Ok(out)
    }
}

/// Splits inputs into consecutive requests of at most [`REMOTE_BATCH_MAX_ITEMS`] texts and
/// [`REMOTE_BATCH_MAX_BYTES`] bytes. Every text is already truncated, so each fits alone.
fn plan_batches(inputs: &[String]) -> Vec<&[String]> {
    let mut batches = Vec::new();
    let (mut start, mut bytes) = (0usize, 0usize);
    for (i, text) in inputs.iter().enumerate() {
        let full = i - start >= REMOTE_BATCH_MAX_ITEMS
            || (i > start && bytes + text.len() > REMOTE_BATCH_MAX_BYTES);
        if full {
            batches.push(&inputs[start..i]);
            start = i;
            bytes = 0;
        }
        bytes += text.len();
    }
    if start < inputs.len() {
        batches.push(&inputs[start..]);
    }
    batches
}

/// What the last failed attempt tells the retry loop.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RetryHint {
    retryable: bool,
    rate_limited: bool,
    delay: Option<std::time::Duration>,
}

/// Retry policy from the OpenRouter error guide: 408/502/5xx back off, 429 and 503 honor the
/// provider's delay, 402 is retried only for the in-flight budget and only with `Retry-After`.
fn classify_failure(
    status: u16,
    header_delay: Option<std::time::Duration>,
    body: &str,
) -> RetryHint {
    let delay = header_delay.or_else(|| delay_from_message(body));
    match status {
        408 | 500..=599 => RetryHint {
            retryable: true,
            rate_limited: false,
            delay,
        },
        429 => RetryHint {
            retryable: true,
            rate_limited: true,
            delay,
        },
        402 if header_delay.is_some() && body.contains("openrouter_in_flight_budget") => {
            RetryHint {
                retryable: true,
                rate_limited: false,
                delay,
            }
        }
        _ => RetryHint::default(),
    }
}

/// Upstream providers put the hint in the message ("Retry the request after 2 sec").
fn delay_from_message(body: &str) -> Option<std::time::Duration> {
    let lower = body.to_ascii_lowercase();
    let rest = &lower[lower.find("after ")? + "after ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let tail = rest[digits.len()..].trim_start();
    if digits.is_empty() || !tail.starts_with("sec") {
        return None;
    }
    digits
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_secs)
}

/// Provider hint if present, else `base * 2^(attempt-1)`, plus up to 250 ms of jitter, capped.
fn retry_delay(hint: &RetryHint, base: std::time::Duration, attempt: usize) -> std::time::Duration {
    let backoff = base * 2u32.saturating_pow(attempt.saturating_sub(1) as u32);
    let jitter = std::time::Duration::from_millis(u64::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos())
            % 250,
    ));
    (hint.delay.unwrap_or(backoff) + jitter).min(MAX_RETRY_DELAY)
}

/// Adaptive concurrency permit counter (additive increase, multiplicative decrease). It starts at
/// the configured ceiling, halves on a 429 (at most once per second, so a burst of simultaneous
/// 429s counts once) and regains one slot after as many consecutive successes as slots.
struct Gate {
    state: Mutex<GateState>,
    changed: std::sync::Condvar,
    max: usize,
}

struct GateState {
    allowed: usize,
    active: usize,
    successes: usize,
    last_cut: Option<std::time::Instant>,
}

impl Gate {
    fn new(max: usize) -> Self {
        Self {
            state: Mutex::new(GateState {
                allowed: max,
                active: 0,
                successes: 0,
                last_cut: None,
            }),
            changed: std::sync::Condvar::new(),
            max,
        }
    }

    /// Blocks until a slot is free. Returns `false` once the call was aborted.
    fn acquire(&self, abort: &AtomicBool) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        loop {
            if abort.load(Ordering::Relaxed) {
                return false;
            }
            if state.active < state.allowed {
                state.active += 1;
                return true;
            }
            state = match self
                .changed
                .wait_timeout(state, std::time::Duration::from_millis(200))
            {
                Ok((guard, _)) => guard,
                Err(_) => return false,
            };
        }
    }

    fn release(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.active = state.active.saturating_sub(1);
        }
        self.changed.notify_all();
    }

    fn on_success(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.successes += 1;
            if state.successes >= state.allowed && state.allowed < self.max {
                state.allowed += 1;
                state.successes = 0;
                self.changed.notify_all();
            }
        }
    }

    fn on_rate_limit(&self) {
        if let Ok(mut state) = self.state.lock() {
            let now = std::time::Instant::now();
            let fresh = state
                .last_cut
                .is_none_or(|t| now.duration_since(t) >= std::time::Duration::from_secs(1));
            if fresh {
                state.allowed = (state.allowed / 2).max(1);
                state.successes = 0;
                state.last_cut = Some(now);
            }
        }
    }
}

/// List embedding model ids the provider advertises (OpenRouter `GET {base}/embeddings/models`).
/// Rainy documents no model-listing endpoint, so it reports that instead of guessing. Bounded,
/// no redirects, no credential in any error. Read-only: no user content is sent.
pub fn discover_models(
    provider: Provider,
    endpoint: Option<&str>,
    api_key: &str,
    timeout_secs: u64,
) -> Result<Vec<String>> {
    if provider != Provider::OpenRouter {
        bail!(
            "{} does not advertise a model list; enter the model id manually",
            provider.as_str()
        );
    }
    let base = endpoint
        .unwrap_or(OPENROUTER_ENDPOINT)
        .trim_end_matches('/')
        .to_string();
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .user_agent(concat!("aether-midnight/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .tls_connector(std::sync::Arc::new(native_tls::TlsConnector::new()?))
        .build();
    let response = match agent
        .get(&format!("{base}/embeddings/models"))
        .set("Authorization", &format!("Bearer {api_key}"))
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => {
            bail!("model discovery failed (HTTP {code}); check the API key and endpoint")
        }
        Err(_) => bail!("model discovery failed; check connectivity"),
    };
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(4 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("failed reading the model list"))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("provider returned an unreadable model list"))?;
    let mut ids: Vec<String> = value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str())
        .filter(|id| !id.is_empty() && id.len() <= 200 && !id.chars().any(char::is_whitespace))
        .map(str::to_owned)
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        bail!("provider returned no embedding models");
    }
    Ok(ids)
}

/// True when `error` (from an embedder) is a cloud failure, for callers that want to say so.
pub fn as_cloud_error(error: &anyhow::Error) -> Option<&CloudError> {
    error.downcast_ref::<CloudError>()
}

#[cfg(test)]
mod tests;
