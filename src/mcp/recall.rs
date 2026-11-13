use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde_json::{Value, json};

use super::{Policy, Session, optional_str, required_str, search, session_connection};
use crate::embed::{self, SearchMode, TextEmbedder};
use crate::memory::{self, BroadTermPolicy};

pub(super) const GLOBAL_SCOPE: &str = "global:user";

pub(super) fn call(
    db_path: &Path,
    policy: &Policy,
    session: &mut Option<Session>,
    args: &Value,
    default_scope: bool,
) -> Result<Value> {
    let scope = required_str(args, "scope")?;
    let query = required_str(args, "query")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 100) as usize;
    let at = optional_str(args, "at");
    let broad = match optional_str(args, "broad_terms") {
        None | Some("exact") => BroadTermPolicy::Exact,
        Some("recent") => BroadTermPolicy::Recent,
        Some(other) => bail!("broad_terms must be exact or recent, not {other:?}"),
    };
    let explicit_mode = args.get("mode").is_some();
    let lexical_only = at.is_some() || broad == BroadTermPolicy::Recent;
    let repo = policy.default_repo();
    let mut lexical_args = json!({"mode":"lexical"});
    let (mode, settings) = search::resolve(
        if lexical_only && !explicit_mode {
            &lexical_args
        } else {
            args
        },
        repo.as_deref(),
        policy,
    )?;
    if lexical_only && mode != SearchMode::Lexical {
        bail!("semantic/hybrid recall requires current time and broad_terms=exact");
    }
    let state = session.get_or_insert_with(Session::default);
    let cache = state.search.clone();
    let local = state.local.clone();
    let conn = session_connection(db_path, session)?;
    let run_layer = |conn: &Connection, scope: &str| -> Result<(Vec<Value>, Option<Value>)> {
        let mut loader = search::loader(
            settings.clone(),
            policy.allow_remote_embeddings,
            cache.clone(),
            local.clone(),
            format!("memory:{scope}"),
        );
        if mode != SearchMode::Lexical
            && (explicit_mode || {
                let candidate: Option<Box<dyn TextEmbedder>> = loader().ok();
                match candidate {
                    Some(candidate) => {
                        embed::has_eligible_memory_vectors(conn, scope, candidate.identity())?
                    }
                    None => false,
                }
            })
        {
            let response = embed::search_memory_on(conn, scope, query, mode, limit, &mut loader)?;
            let results = response
                .results
                .into_iter()
                .map(serde_json::to_value)
                .collect::<Result<Vec<_>, _>>()?;
            return Ok((results, Some(serde_json::to_value(response.info)?)));
        }
        let results = memory::recall_with_policy_on(conn, scope, query, at, limit, broad)?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((results, None))
    };
    let (mut results, metadata) = run_layer(conn, scope)?;
    let global =
        if default_scope && scope.starts_with("project:") && policy.scope_allowed(GLOBAL_SCOPE) {
            global_path(db_path, repo.as_deref())
        } else {
            None
        };
    let layered = global.is_some();
    if let Some(path) = global {
        let other = if path != db_path && path.is_file() {
            Some(crate::db::open_db(&path).context("open global memory database")?)
        } else {
            None
        };
        let (mut fallback, fallback_meta) = if path == db_path || other.is_some() {
            run_layer(other.as_ref().unwrap_or(conn), GLOBAL_SCOPE)?
        } else {
            (Vec::new(), None)
        };
        let project_content: std::collections::HashSet<_> = results
            .iter()
            .filter_map(|result| result["memory"]["content"].as_str())
            .map(memory::normalized_duplicate_key)
            .collect();
        fallback.retain(|result| {
            result["memory"]["content"].as_str().is_none_or(|content| {
                !project_content.contains(&memory::normalized_duplicate_key(content))
            })
        });
        for item in &mut results {
            item["layer"] = json!("project");
        }
        for item in &mut fallback {
            item["layer"] = json!("global");
        }
        results.append(&mut fallback);
        // Preserve each layer's order and prefer the project at the same evidence level.
        if metadata.is_none() && fallback_meta.is_none() {
            results.sort_by_key(|result| result["match_reason"] != "memory_exact_phrase");
        }
        results.truncate(limit);
        lexical_args = json!({"results":results,"layers":[
            {"scope":scope,"layer":"project","search":layer_info(metadata)},
            {"scope":GLOBAL_SCOPE,"layer":"global","search":layer_info(fallback_meta)}
        ]});
    } else if let Some(mut metadata) = metadata {
        metadata["results"] = json!(results);
        lexical_args = metadata;
    } else {
        lexical_args = json!(results);
    }
    let Some(budget) = args.get("token_budget") else {
        return Ok(lexical_args);
    };
    let budget = budget
        .as_u64()
        .filter(|n| (64..=8192).contains(n))
        .context("token_budget must be an integer between 64 and 8192")? as usize;
    pack(lexical_args, budget, layered)
}

fn layer_info(meta: Option<Value>) -> Value {
    meta.unwrap_or_else(|| json!({"mode_used":"lexical","semantic_coverage":null}))
}

/// Custom databases remain isolated unless they are the active trusted project database.
pub(super) fn global_path(db_path: &Path, repo: Option<&Path>) -> Option<std::path::PathBuf> {
    let global = crate::db::default_db_path().ok()?;
    let active = db_path.canonicalize().ok()?;
    if global.canonicalize().is_ok_and(|path| path == active) {
        return Some(db_path.to_path_buf());
    }
    let project = crate::connect::discover_project_db(repo?).and_then(|p| p.canonicalize().ok())?;
    (project == active).then_some(global)
}

fn pack(value: Value, budget: usize, layered: bool) -> Result<Value> {
    let mut envelope = if value.is_array() {
        json!({"results":value})
    } else {
        value
    };
    let mut results = std::mem::take(&mut envelope["results"])
        .as_array_mut()
        .map(std::mem::take)
        .unwrap_or_default();
    let mut level = vec![0; results.len()];
    envelope["token_budget"] = json!(budget);
    envelope["estimated_tokens"] = json!(9999);
    envelope["truncated"] = json!(false);
    loop {
        envelope["results"] = json!(results);
        let tokens = crate::code::estimate_tokens(&envelope.to_string());
        if tokens <= budget {
            envelope["estimated_tokens"] = json!(tokens);
            return Ok(envelope);
        }
        envelope["truncated"] = json!(true);
        if let Some(index) = level.iter().rposition(|level| *level < 2) {
            let result = &mut results[index];
            if level[index] == 0 {
                if let Some(content) = result["memory"]["content"].as_str() {
                    let preview: String = content.chars().take(160).collect();
                    result["memory"]["content"] = json!(preview);
                    result["detail"] = json!("preview");
                }
            } else {
                let reference = &result["memory"];
                let mut stub = json!({"memory":{"id":reference["id"],"scope":reference["scope"],"origin":reference["origin"]},
                    "candidate_coverage":result["candidate_coverage"],"detail":"reference"});
                if layered {
                    stub["layer"] = result["layer"].clone();
                }
                *result = stub;
            }
            level[index] += 1;
        } else if results.pop().is_some() {
            level.pop();
        } else {
            bail!("token_budget is too small for the recall metadata");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_recall_keeps_rank_and_respects_the_budget() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = temp.path().join("memories");
        for scope in ["project:allowed", "project:allowed-extra", "global:user"] {
            memory::remember(
                &db,
                scope,
                "source",
                &format!("build ledger {}", "notes ".repeat(300)),
                None,
                None,
                None,
                None,
                None,
                None,
            )?;
        }
        let policy = Policy {
            memory_scopes: Some(vec!["project:allowed".into()]),
            ..Policy::default()
        };
        let value = call(
            &db,
            &policy,
            &mut None,
            &json!({"scope":"project:allowed","query":"build ledger","mode":"lexical","token_budget":96}),
            true,
        )?;
        assert_eq!(value["results"][0]["memory"]["scope"], "project:allowed");
        assert_eq!(value["results"][0]["memory"]["origin"], "user");
        assert_eq!(value["results"].as_array().unwrap().len(), 1);
        assert!(crate::code::estimate_tokens(&value.to_string()) <= 96);
        Ok(())
    }
}
