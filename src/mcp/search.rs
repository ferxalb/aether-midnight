//! Provider selection and connection-local query-vector reuse. No provider configuration is
//! accepted from model-generated tool arguments. Authorization is rechecked on every call.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde_json::Value;

use super::Policy;
use crate::config::{self, Settings};
use crate::embed::{self, ModelIdentity, SearchMode, TextEmbedder, TextKind};

const CACHE_CAPACITY: usize = 32;
const CACHE_TTL: Duration = Duration::from_secs(60);

struct Entry {
    scope: String,
    model: ModelIdentity,
    query_hash: String,
    vector: Vec<f32>,
    created: Instant,
}

#[derive(Clone, Default)]
pub(super) struct QueryCache(Rc<RefCell<VecDeque<Entry>>>);

pub(super) fn resolve(
    args: &Value,
    repo: Option<&Path>,
    policy: &Policy,
    memory: bool,
) -> Result<(SearchMode, Option<Settings>)> {
    let requested = match args.get("mode") {
        None => None,
        Some(Value::String(s)) => Some(match s.as_str() {
            "lexical" => SearchMode::Lexical,
            "semantic" => SearchMode::Semantic,
            "hybrid" => SearchMode::Hybrid,
            _ => bail!("mode must be lexical, semantic or hybrid"),
        }),
        Some(_) => bail!("mode must be lexical, semantic or hybrid"),
    };
    // Explicit lexical bypasses configuration, credentials, model loading and the cache.
    if requested == Some(SearchMode::Lexical) || (memory && requested.is_none()) {
        return Ok((SearchMode::Lexical, None));
    }
    let disabled = repo
        .map(crate::project::load_for)
        .transpose()?
        .flatten()
        .is_some_and(|p| p.embeddings_disabled());
    let settings = if disabled {
        None
    } else {
        config::load()?.filter(|s| s.provider != config::Choice::Lexical)
    };
    let default = if settings
        .as_ref()
        .is_some_and(|s| !s.provider.is_remote() || policy.allow_remote_embeddings)
    {
        SearchMode::Hybrid
    } else {
        SearchMode::Lexical
    };
    Ok((requested.unwrap_or(default), settings))
}

pub(super) fn loader(
    settings: Option<Settings>,
    remote_allowed: bool,
    cache: QueryCache,
    scope: String,
) -> impl FnMut() -> Result<Box<dyn TextEmbedder>> {
    move || {
        let Some(settings) = settings.as_ref() else {
            bail!("embeddings are disabled or no provider is configured; run `midnight setup`");
        };
        let identity = if settings.provider.is_remote() {
            if !remote_allowed {
                bail!(
                    "remote embeddings are not permitted by this MCP policy; mint an HTTP token with --allow-remote-embeddings"
                );
            }
            if !settings.upload_consented() {
                bail!(
                    "remote query upload requires provider-bound consent; run `midnight setup --provider openrouter --yes` (or select your provider explicitly)"
                );
            }
            settings.cloud_config()?.identity()
        } else {
            ModelIdentity {
                name: embed::MODEL_NAME.into(),
                revision: embed::MODEL_REVISION.into(),
                dimensions: embed::MODEL_DIMS,
            }
        };
        Ok(Box::new(CachedEmbedder {
            settings: settings.clone(),
            identity,
            cache: cache.clone(),
            scope: scope.clone(),
        }) as Box<dyn TextEmbedder>)
    }
}

pub(super) fn index_settings(repo: &Path, policy: &Policy) -> Result<Option<Settings>> {
    let (_, settings) = resolve(&serde_json::json!({}), Some(repo), policy, false)?;
    if let Some(settings) = &settings
        && settings.provider.is_remote()
    {
        if !policy.allow_remote_embeddings {
            bail!(
                "remote embeddings are not permitted by this MCP policy; the lexical index is ready"
            );
        }
        if !settings.upload_consented() {
            bail!(
                "source and query uploads require saved provider-bound consent; run `midnight setup`; the lexical index is ready"
            );
        }
    }
    Ok(settings)
}

struct CachedEmbedder {
    settings: Settings,
    identity: ModelIdentity,
    cache: QueryCache,
    scope: String,
}

impl TextEmbedder for CachedEmbedder {
    fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    fn embed(&mut self, kind: TextKind, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if kind != TextKind::Query || texts.len() != 1 {
            bail!("MCP search only embeds a single query; use the CLI to embed source content");
        }
        let query_hash = crate::db::hash_bytes(texts[0].as_bytes());
        {
            let mut cache = self.cache.0.borrow_mut();
            cache.retain(|e| e.created.elapsed() < CACHE_TTL);
            if let Some(entry) = cache.iter().find(|e| {
                e.model == self.identity && e.scope == self.scope && e.query_hash == query_hash
            }) {
                return Ok(vec![entry.vector.clone()]);
            }
        }
        let mut embedder: Box<dyn TextEmbedder> = if self.settings.provider.is_remote() {
            Box::new(config::cloud_query_embedder(&self.settings)?)
        } else {
            crate::ux::consented_embedder_for(&self.settings)?
                .ok_or_else(|| anyhow::anyhow!("no embedding provider configured"))?
        };
        let vectors = embedder.embed(kind, texts)?;
        if vectors.len() == 1
            && vectors[0].len() == self.identity.dimensions
            && vectors[0].iter().all(|v| v.is_finite())
        {
            let mut cache = self.cache.0.borrow_mut();
            if cache.len() >= CACHE_CAPACITY {
                cache.pop_front();
            }
            cache.push_back(Entry {
                scope: self.scope.clone(),
                model: self.identity.clone(),
                query_hash,
                vector: vectors[0].clone(),
                created: Instant::now(),
            });
        }
        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_vectors_expire_and_are_isolated_by_scope_and_model() {
        let identity = ModelIdentity {
            name: "test".into(),
            revision: "v1".into(),
            dimensions: 2,
        };
        let cache = QueryCache::default();
        cache.0.borrow_mut().push_back(Entry {
            scope: "repo:a".into(),
            model: identity.clone(),
            query_hash: crate::db::hash_bytes(b"query"),
            vector: vec![1.0, 0.0],
            created: Instant::now(),
        });
        // Lexical settings cannot create an embedder on a miss. Only a valid cache hit succeeds.
        let mut cached = CachedEmbedder {
            settings: Settings::lexical(),
            identity,
            cache: cache.clone(),
            scope: "repo:a".into(),
        };
        assert_eq!(
            cached.embed(TextKind::Query, &["query"]).unwrap(),
            vec![vec![1.0, 0.0]]
        );
        cached.scope = "repo:b".into();
        assert!(cached.embed(TextKind::Query, &["query"]).is_err());
        cached.scope = "repo:a".into();
        cached.identity.revision = "v2".into();
        assert!(cached.embed(TextKind::Query, &["query"]).is_err());
        cached.identity.revision = "v1".into();
        cache.0.borrow_mut()[0].created = Instant::now().checked_sub(CACHE_TTL).unwrap();
        assert!(cached.embed(TextKind::Query, &["query"]).is_err());
        assert!(cache.0.borrow().is_empty());
    }
}
