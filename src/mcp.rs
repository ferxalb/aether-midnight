use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::{VERSION, code, memory};

const PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_MCP_MESSAGE_BYTES: usize = 1024 * 1024;

/// Least-privilege limits applied to every tool call on any transport.
/// The default policy is unrestricted so existing stdio users are unaffected.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Policy {
    /// When set, only these tools are listed or callable.
    pub allowed_tools: Option<Vec<String>>,
    /// When set, memory tools may only name one of these scopes. An entry
    /// ending in `*` matches by prefix (for example `project:*`).
    pub memory_scopes: Option<Vec<String>>,
    /// When set, code tools may only name a repository inside one of these roots.
    pub repo_roots: Option<Vec<PathBuf>>,
}

const MEMORY_TOOLS: [&str; 5] = [
    "remember",
    "remember_batch",
    "recall",
    "list_memories",
    "forget",
];
pub const WRITE_TOOLS: [&str; 5] = [
    "index_repository",
    "refresh_indexed_file",
    "remember",
    "remember_batch",
    "forget",
];

impl Policy {
    pub fn read_only_tools() -> Vec<String> {
        tools()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .filter(|n| !WRITE_TOOLS.contains(n))
            .map(str::to_owned)
            .collect()
    }

    fn tool_allowed(&self, name: &str) -> bool {
        self.allowed_tools
            .as_ref()
            .is_none_or(|list| list.iter().any(|t| t == name))
    }

    /// Checks a call and returns the arguments to execute. When the policy
    /// pins exactly one literal memory scope, the scope is injected so a model
    /// can neither omit nor override it: authority comes from the policy, not
    /// from model-generated arguments.
    fn authorize(&self, name: &str, args: &Value) -> Result<Value> {
        let mut args = args.clone();
        if !self.tool_allowed(name) {
            bail!("permission denied: tool {name} is not enabled for this server");
        }
        if let Some(scopes) = &self.memory_scopes
            && MEMORY_TOOLS.contains(&name)
        {
            let pinned = match scopes.as_slice() {
                [only] if !only.ends_with('*') => Some(only.clone()),
                _ => None,
            };
            if let (Some(pinned), Some(map)) = (&pinned, args.as_object_mut()) {
                if name == "remember_batch" {
                    if let Some(items) = map.get_mut("memories").and_then(Value::as_array_mut) {
                        for item in items {
                            if let Some(m) = item.as_object_mut() {
                                m.entry("scope").or_insert_with(|| json!(pinned));
                            }
                        }
                    }
                } else {
                    map.entry("scope").or_insert_with(|| json!(pinned));
                }
            }
            let scope = required_str(&args, "scope").ok();
            let batch: Vec<&str> = args
                .get("memories")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|m| m.get("scope").and_then(Value::as_str).unwrap_or(""))
                        .collect()
                })
                .unwrap_or_default();
            let requested: Vec<&str> = scope.into_iter().chain(batch).collect();
            if requested.is_empty() {
                bail!("permission denied: memory scope is required");
            }
            for scope in requested {
                let ok = scopes
                    .iter()
                    .any(|allowed| match allowed.strip_suffix('*') {
                        Some(prefix) => scope.starts_with(prefix),
                        None => allowed == scope,
                    });
                if !ok {
                    bail!(
                        "permission denied: memory scope {scope:?} is not enabled for this server"
                    );
                }
            }
        }
        if let Some(roots) = &self.repo_roots
            && args.get("repo_path").is_some()
        {
            let repo = std::fs::canonicalize(required_str(&args, "repo_path")?)
                .context("permission denied: repository path is not accessible")?;
            let allowed = roots
                .iter()
                .any(|root| std::fs::canonicalize(root).is_ok_and(|root| repo.starts_with(root)));
            if !allowed {
                bail!("permission denied: repository is outside the roots enabled for this server");
            }
        }
        Ok(args)
    }
}

/// Handles one decoded JSON-RPC message. Returns `None` for notifications.
/// Returns `Err(())` equivalent via the `exit` flag when the client asks to stop.
pub fn handle_message(
    db_path: &Path,
    policy: &Policy,
    memory_session: &mut Option<memory::MemorySession>,
    request: &Value,
    cancellation: &AtomicBool,
) -> Option<Value> {
    let id = request.get("id").cloned()?;
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    Some(match method {
        "initialize" => initialize(id, &params),
        "ping" => ok_response(id, json!({})),
        "tools/list" => {
            let visible: Vec<Value> = tools()
                .into_iter()
                .filter(|t| t["name"].as_str().is_some_and(|n| policy.tool_allowed(n)))
                .collect();
            ok_response(id, json!({"tools":visible}))
        }
        "tools/call" => {
            let outcome = params
                .get("name")
                .and_then(Value::as_str)
                .context("missing string argument name")
                .and_then(|name| {
                    let args = params
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    policy.authorize(name, &args)
                })
                .and_then(|args| {
                    let mut checked = params.clone();
                    checked["arguments"] = args;
                    call_tool(db_path, memory_session, &checked, cancellation)
                });
            match outcome {
                Ok(result) => ok_response(
                    id,
                    json!({
                        "content":[{"type":"text","text":result.to_string()}],
                        "isError":false
                    }),
                ),
                Err(error) => ok_response(
                    id,
                    json!({"content":[{"type":"text","text":format!("{error:#}")}],"isError":true}),
                ),
            }
        }
        _ => error_response(id, -32601, "method not found"),
    })
}

pub fn run_stdio(db_path: &Path, policy: &Policy, cancellation: Arc<AtomicBool>) -> Result<()> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut stdout = std::io::stdout().lock();
    let mut memory_session = None;
    loop {
        let Some((bytes, too_long)) = read_bounded_line(&mut input, MAX_MCP_MESSAGE_BYTES)? else {
            break;
        };
        if too_long {
            write_json(
                &mut stdout,
                &json!({
                    "jsonrpc":"2.0","id":null,
                    "error":{"code":-32600,"message":"MCP message exceeds the 1 MiB limit"}
                }),
            )?;
            continue;
        }
        let line = match String::from_utf8(bytes) {
            Ok(line) => line,
            Err(error) => {
                write_json(
                    &mut stdout,
                    &json!({
                        "jsonrpc":"2.0","id":null,
                        "error":{"code":-32700,"message":format!("parse error: {error}")}
                    }),
                )?;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                write_json(
                    &mut stdout,
                    &json!({
                        "jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":format!("parse error: {error}")}
                    }),
                )?;
                continue;
            }
        };
        if request.get("id").is_none() {
            if request.get("method").and_then(Value::as_str) == Some("exit") {
                break;
            }
            continue;
        }
        if let Some(response) = handle_message(
            db_path,
            policy,
            &mut memory_session,
            &request,
            &cancellation,
        ) {
            write_json(&mut stdout, &response)?;
        }
    }
    Ok(())
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<(Vec<u8>, bool)>> {
    let mut line = Vec::with_capacity(4096.min(max_bytes));
    let mut too_long = false;
    loop {
        let (consumed, has_newline, eof) = {
            let available = reader.fill_buf().context("read MCP stdio input")?;
            if available.is_empty() {
                (0, false, true)
            } else {
                let newline = available.iter().position(|byte| *byte == b'\n');
                let consumed = newline.map_or(available.len(), |position| position + 1);
                if !too_long {
                    let content_bytes = consumed - usize::from(newline.is_some());
                    let keep = content_bytes.min(max_bytes.saturating_sub(line.len()) + 1);
                    line.extend_from_slice(&available[..keep]);
                    if keep < content_bytes || line.len() > max_bytes {
                        too_long = true;
                    } else if newline.is_some() {
                        line.push(b'\n');
                    }
                }
                (consumed, newline.is_some(), false)
            }
        };
        if eof {
            return Ok((!line.is_empty() || too_long).then_some((line, too_long)));
        }
        reader.consume(consumed);
        if has_newline {
            return Ok(Some((line, too_long)));
        }
    }
}

fn initialize(id: Value, _params: &Value) -> Value {
    ok_response(
        id,
        json!({
            "protocolVersion":PROTOCOL_VERSION,
            "capabilities":{"tools":{"listChanged":false}},
            "serverInfo":{"name":"aether-midnight","version":VERSION},
            "instructions":"Local-only repository retrieval and explicitly scoped agent memory. Repository paths and memory scopes are required for each operation."
        }),
    )
}

pub fn tools() -> Vec<Value> {
    vec![
        tool(
            "index_repository",
            "Incrementally index eligible local files in one repository. Returns partial/complete status and skip counts.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"include_generated":{"type":"boolean"},"verify_hashes":{"type":"boolean"}},"required":["repo_path"]}),
        ),
        tool(
            "refresh_indexed_file",
            "Refresh one explicit repository-relative file without walking the repository. Current ignore/security rules are applied; the catalog remains partial until index_repository reconciles all changes.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"path":{"type":"string"}},"required":["repo_path","path"]}),
        ),
        tool(
            "search_code",
            "Search exact paths, extracted symbols, whole lexical tokens, and identifier-boundary fragments. Arbitrary substrings inside a token are unsupported. Returns current-source evidence and packs snippets within a deterministic estimated-token budget.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20},"token_budget":{"type":"integer","minimum":64,"maximum":8192}},"required":["repo_path","query"]}),
        ),
        tool(
            "open_range",
            "Read a bounded line range from the current repository file and report whether it matches the indexed hash.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"path":{"type":"string"},"line_start":{"type":"integer","minimum":1},"line_end":{"type":"integer","minimum":1}},"required":["repo_path","path","line_start","line_end"]}),
        ),
        tool(
            "list_symbols",
            "List extracted Tree-sitter symbols in one repository/worktree.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"name":{"type":"string"},"limit":{"type":"integer"}},"required":["repo_path"]}),
        ),
        tool(
            "repo_map",
            "Return deterministic file/language/symbol counts for one worktree.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"}},"required":["repo_path"]}),
        ),
        tool(
            "index_status",
            "Report whether this repository/worktree index is complete and its generation, counts, and exclusions.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"}},"required":["repo_path"]}),
        ),
        tool(
            "remember",
            "Persist a source or derived fact in a required namespace such as user:alice or project:repo-name. Derived records must cite a source id or URI; superseding a fact preserves history and closes its prior validity interval.",
            json!({"type":"object","properties":{"operation_key":{"type":"string","maxLength":256},"scope":{"type":"string"},"kind":{"type":"string","enum":["source","derived"]},"content":{"type":"string"},"source_uri":{"type":"string"},"source_id":{"type":"integer"},"supersedes_id":{"type":"integer"},"observed_at":{"type":"string"},"valid_from":{"type":"string"},"valid_to":{"type":"string"}},"required":["scope","kind","content"]}),
        ),
        tool(
            "remember_batch",
            "Atomically persist 1 to 512 explicit source or derived memories. A failed record rolls back the entire batch; larger imports should be split into caller-managed batches.",
            json!({"type":"object","properties":{"memories":{"type":"array","minItems":1,"maxItems":512,"items":{"type":"object","properties":{"scope":{"type":"string"},"kind":{"type":"string","enum":["source","derived"]},"content":{"type":"string"},"source_uri":{"type":"string"},"source_id":{"type":"integer"},"supersedes_id":{"type":"integer"},"observed_at":{"type":"string"},"valid_from":{"type":"string"},"valid_to":{"type":"string"}},"required":["scope","kind","content"]}}},"required":["memories"]}),
        ),
        tool(
            "recall",
            "Search active memories in exactly one namespace and time; repository code is never included. Results carry candidate_coverage: complete, exact_top_k (exact under the documented ordering), bm25_top_k (bounded window for queries with more than four informative terms), or recency_window (only when broad_terms is recent).",
            json!({"type":"object","properties":{"scope":{"type":"string"},"query":{"type":"string"},"at":{"type":"string"},"limit":{"type":"integer"},"broad_terms":{"type":"string","enum":["exact","recent"],"description":"Default exact BM25 ranking. recent opts a single query term found in at least half of the namespace into a fast newest-first window that can omit older evidence."}},"required":["scope","query"]}),
        ),
        tool(
            "list_memories",
            "List non-forgotten source and derived records in exactly one memory namespace.",
            json!({"type":"object","properties":{"scope":{"type":"string"},"limit":{"type":"integer"}},"required":["scope"]}),
        ),
        tool(
            "forget",
            "Immediately suppress one memory and remove it from the memory search index.",
            json!({"type":"object","properties":{"scope":{"type":"string"},"id":{"type":"integer"}},"required":["scope","id"]}),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema})
}

fn call_tool(
    db_path: &Path,
    memory_session: &mut Option<memory::MemorySession>,
    params: &Value,
    cancellation: &AtomicBool,
) -> Result<Value> {
    let name = required_str(params, "name")?;
    let args = params
        .get("arguments")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    match name {
        "index_repository" => {
            let repo = path_arg(&args, "repo_path")?;
            let include_generated = args
                .get("include_generated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let verify_hashes = args
                .get("verify_hashes")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let result = code::index_repository(
                db_path,
                &repo,
                include_generated,
                verify_hashes,
                cancellation,
            )?;
            Ok(serde_json::to_value(result)?)
        }
        "refresh_indexed_file" => {
            let repo = path_arg(&args, "repo_path")?;
            let result = code::refresh_indexed_file(
                db_path,
                &repo,
                required_str(&args, "path")?,
                cancellation,
            )?;
            Ok(serde_json::to_value(result)?)
        }
        "search_code" => {
            let repo = path_arg(&args, "repo_path")?;
            let result = code::search_code_with_budget(
                db_path,
                &repo,
                required_str(&args, "query")?,
                args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize,
                args.get("token_budget")
                    .and_then(Value::as_u64)
                    .unwrap_or(2048) as usize,
            )?;
            Ok(serde_json::to_value(result)?)
        }
        "open_range" => {
            let repo = path_arg(&args, "repo_path")?;
            let result = code::open_range(
                db_path,
                &repo,
                required_str(&args, "path")?,
                required_u32(&args, "line_start")?,
                required_u32(&args, "line_end")?,
            )?;
            Ok(serde_json::to_value(result)?)
        }
        "list_symbols" => {
            let repo = path_arg(&args, "repo_path")?;
            let name = args.get("name").and_then(Value::as_str);
            Ok(serde_json::to_value(code::list_symbols(
                db_path,
                &repo,
                name,
                args.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize,
            )?)?)
        }
        "repo_map" => {
            let repo = path_arg(&args, "repo_path")?;
            Ok(serde_json::to_value(code::repo_map(db_path, &repo)?)?)
        }
        "index_status" => {
            let repo = path_arg(&args, "repo_path")?;
            Ok(serde_json::to_value(code::index_status(db_path, &repo)?)?)
        }
        "remember" => {
            if let Some(key) = optional_str(&args, "operation_key") {
                let input: memory::MemoryInput = serde_json::from_value(args.clone())?;
                let session = ensure_memory_session(db_path, memory_session)?;
                return Ok(serde_json::to_value(
                    session.remember_idempotent(key, &input)?,
                )?);
            }
            let result = ensure_memory_session(db_path, memory_session)?.remember(
                required_str(&args, "scope")?,
                required_str(&args, "kind")?,
                required_str(&args, "content")?,
                optional_str(&args, "source_uri"),
                args.get("source_id").and_then(Value::as_i64),
                args.get("supersedes_id").and_then(Value::as_i64),
                optional_str(&args, "observed_at"),
                optional_str(&args, "valid_from"),
                optional_str(&args, "valid_to"),
            )?;
            Ok(serde_json::to_value(result)?)
        }
        "remember_batch" => {
            let inputs: Vec<memory::MemoryInput> = serde_json::from_value(
                args.get("memories")
                    .cloned()
                    .context("missing memories array")?,
            )?;
            let result = ensure_memory_session(db_path, memory_session)?.remember_batch(&inputs)?;
            Ok(serde_json::to_value(result)?)
        }
        "recall" => Ok(serde_json::to_value(
            ensure_memory_session(db_path, memory_session)?.recall_with_policy(
                required_str(&args, "scope")?,
                required_str(&args, "query")?,
                optional_str(&args, "at"),
                args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize,
                match optional_str(&args, "broad_terms") {
                    None | Some("exact") => memory::BroadTermPolicy::Exact,
                    Some("recent") => memory::BroadTermPolicy::Recent,
                    Some(other) => bail!("broad_terms must be exact or recent, not {other:?}"),
                },
            )?,
        )?),
        "list_memories" => Ok(serde_json::to_value(
            ensure_memory_session(db_path, memory_session)?.list_memories(
                required_str(&args, "scope")?,
                args.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize,
            )?,
        )?),
        "forget" => Ok(
            json!({"forgotten":ensure_memory_session(db_path,memory_session)?.forget(
            required_str(&args,"scope")?,args.get("id").and_then(Value::as_i64).context("missing integer id")?
        )?}),
        ),
        _ => bail!("unknown tool {name}"),
    }
}

fn ensure_memory_session<'a>(
    db_path: &Path,
    session: &'a mut Option<memory::MemorySession>,
) -> Result<&'a mut memory::MemorySession> {
    if session.is_none() {
        *session = Some(memory::MemorySession::open(db_path)?);
    }
    session
        .as_mut()
        .context("memory database session failed to initialize")
}

fn path_arg(args: &Value, key: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(required_str(args, key)?))
}
fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("missing string argument {key}"))
}
fn optional_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn required_u32(value: &Value, key: &str) -> Result<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .with_context(|| format!("missing integer argument {key}"))
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn write_json(output: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PROTOCOL_VERSION, initialize, read_bounded_line};
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn initialize_responds_with_the_only_supported_protocol_version() {
        let response = initialize(json!(1), &json!({"protocolVersion":"2099-01-01"}));
        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(response.get("error").is_none());
    }

    #[test]
    fn oversized_stdio_lines_are_bounded_and_drained() {
        let mut input = Cursor::new(b"123456789\nnext\n".to_vec());
        let (line, too_long) = read_bounded_line(&mut input, 4).unwrap().unwrap();
        assert_eq!(line, b"12345");
        assert!(too_long);
        let (line, too_long) = read_bounded_line(&mut input, 4).unwrap().unwrap();
        assert_eq!(line, b"next\n");
        assert!(!too_long);
        assert!(read_bounded_line(&mut input, 4).unwrap().is_none());
    }
}
