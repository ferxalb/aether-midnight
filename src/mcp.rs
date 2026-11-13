use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::{VERSION, code, memory};

mod search;

/// State is private to one MCP connection. Query-vector reuse is bounded, expires and is never
/// persisted; memory retains its existing lazily opened SQLite connection.
#[derive(Default)]
pub struct Session {
    memory: Option<memory::MemorySession>,
    search: search::QueryCache,
    local: search::LocalModel,
    /// Ranges already sent on this connection (hashes only); see `context.rs`.
    ledger: crate::context::SharedLedger,
}

/// Newest protocol revision this server implements; offered when the client asks for an unknown one.
const PROTOCOL_VERSION: &str = "2025-11-25";
/// Revisions whose `tools` surface this server speaks. The reply echoes the client's request when
/// it is listed here, so clients pinned to an older revision do not drop the connection.
const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] =
    ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

fn negotiated_version(params: &Value) -> &'static str {
    params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .and_then(|asked| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|v| **v == asked))
        .copied()
        .unwrap_or(PROTOCOL_VERSION)
}
const MAX_MCP_MESSAGE_BYTES: usize = 1024 * 1024;

thread_local! {
    /// Working directory of the client that owns this connection thread (daemon only). The daemon's
    /// own working directory says nothing about the repository a client is in.
    static CONNECTION_CWD: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Set the client working directory for default-repository lookup and relative `repo_path`
/// arguments on the current connection thread.
pub fn set_connection_cwd(cwd: Option<PathBuf>) {
    CONNECTION_CWD.with(|c| *c.borrow_mut() = cwd);
}

fn connection_cwd() -> Option<PathBuf> {
    CONNECTION_CWD.with(|c| c.borrow().clone())
}

/// Least-privilege limits applied to every tool call on any transport.
/// The default permits local tools; remote embeddings require a separate grant. The stdio CLI
/// grants remote capability unless --local-only is set, but saved upload consent is still required.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Policy {
    /// When set, only these tools are listed or callable.
    pub allowed_tools: Option<Vec<String>>,
    /// When set, memory tools may only name one of these scopes. An entry
    /// ending in `*` matches by prefix (for example `project:*`).
    pub memory_scopes: Option<Vec<String>>,
    /// When set, code tools may only name a repository inside one of these roots.
    pub repo_roots: Option<Vec<PathBuf>>,
    /// Remote embedding calls additionally require saved provider-bound upload consent.
    /// Missing on existing HTTP tokens means false; model arguments cannot grant this.
    #[serde(default)]
    pub allow_remote_embeddings: bool,
}

const CODE_TOOLS: [&str; 7] = [
    "index_repository",
    "refresh_indexed_file",
    "search_code",
    "open_range",
    "list_symbols",
    "repo_map",
    "index_status",
];
/// Tools that name exactly one memory scope (or one per batch item). `list_scopes` is not one:
/// it names none, and its result is filtered by the same policy.
const MEMORY_TOOLS: [&str; 6] = [
    "remember",
    "remember_batch",
    "recall",
    "get_memory",
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

    /// Repository used when a code tool omits `repo_path`: the single policy root, else the nearest
    /// Git repository root at or above the server's working directory. Never `/` or the home
    /// directory itself (a dotfiles repository there must not turn into "index everything"),
    /// which GUI clients often use as the working directory.
    fn default_repo(&self) -> Option<PathBuf> {
        let is_repo = |dir: &Path| dir.join(".git").exists();
        if let Some([only]) = self.repo_roots.as_deref()
            && is_repo(only)
        {
            return Some(only.clone());
        }
        let cwd = connection_cwd().or_else(|| std::env::current_dir().ok())?;
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        repo_root_above(&cwd, home.as_deref().map(Path::new))
    }

    /// Namespace used when a memory tool omits `scope` and the policy pins none:
    /// `project:<name of the default repository's directory>`. `None` outside a repository, or
    /// when the policy's scope list would not allow it.
    pub(crate) fn default_memory_scope(&self) -> Option<String> {
        let scope = memory::project_scope_for(&self.default_repo()?)?;
        self.scope_allowed(&scope).then_some(scope)
    }

    fn scope_allowed(&self, scope: &str) -> bool {
        self.memory_scopes.as_ref().is_none_or(|scopes| {
            scopes
                .iter()
                .any(|allowed| match allowed.strip_suffix('*') {
                    Some(prefix) => scope.starts_with(prefix),
                    None => allowed == scope,
                })
        })
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
        if MEMORY_TOOLS.contains(&name) {
            // A pinned scope always wins; otherwise a missing scope falls back to the default
            // project namespace. An explicit scope is never replaced.
            let pinned = match self.memory_scopes.as_deref() {
                Some([only]) if !only.ends_with('*') => Some(only.clone()),
                _ => None,
            };
            let fallback = pinned.or_else(|| self.default_memory_scope());
            if let (Some(pinned), Some(map)) = (&fallback, args.as_object_mut()) {
                if name == "remember_batch" {
                    if let Some(items) = map.get_mut("memories").and_then(Value::as_array_mut) {
                        for item in items {
                            if let Some(m) = item.as_object_mut() {
                                m.entry("scope").or_insert_with(|| json!(pinned));
                            }
                        }
                    }
                } else if map
                    .get("scope")
                    .is_none_or(|v| v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty()))
                {
                    map.insert("scope".into(), json!(pinned));
                }
            }
        }
        if let Some(scopes) = &self.memory_scopes
            && MEMORY_TOOLS.contains(&name)
        {
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
        if CODE_TOOLS.contains(&name)
            && args
                .get("repo_path")
                .is_none_or(|v| v.is_null() || v.as_str().is_some_and(|p| p.trim().is_empty()))
        {
            let Some(repo) = self.default_repo() else {
                bail!("repo_path is required: this server was not started inside a Git repository");
            };
            if let Some(map) = args.as_object_mut() {
                map.insert("repo_path".into(), json!(repo.to_string_lossy()));
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
    memory_session: &mut Option<Session>,
    request: &Value,
    cancellation: &AtomicBool,
) -> Option<Value> {
    let id = request.get("id").cloned()?;
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    Some(match method {
        "initialize" => {
            if let Some(session) = memory_session.as_ref() {
                use crate::context::DeliveryLedger;
                session.ledger.borrow_mut().clear();
            }
            initialize(id, &params, policy)
        }
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
                    call_tool(db_path, policy, memory_session, &checked, cancellation)
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
    run_session(&mut input, &mut stdout, db_path, policy, &cancellation)
}

/// The newline-delimited JSON-RPC loop over any reader/writer pair: stdio in-process, or a daemon
/// connection. One request at a time, 1 MiB per message.
pub fn run_session<R: BufRead, W: Write>(
    input: &mut R,
    stdout: &mut W,
    db_path: &Path,
    policy: &Policy,
    cancellation: &AtomicBool,
) -> Result<()> {
    let mut memory_session = None;
    loop {
        let Some((bytes, too_long)) = read_bounded_line(input, MAX_MCP_MESSAGE_BYTES)? else {
            break;
        };
        if too_long {
            write_json(
                &mut *stdout,
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
                    &mut *stdout,
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
                    &mut *stdout,
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
        if let Some(response) =
            handle_message(db_path, policy, &mut memory_session, &request, cancellation)
        {
            write_json(&mut *stdout, &response)?;
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

fn initialize(id: Value, params: &Value, policy: &Policy) -> Value {
    ok_response(
        id,
        json!({
            "protocolVersion":negotiated_version(params),
            "capabilities":{"tools":{"listChanged":false}},
            "serverInfo":{"name":"aether-midnight","version":VERSION},
            "instructions":instructions(policy)
        }),
    )
}

/// Server instructions: what the tools are for, when to use memory, and the scope this
/// connection uses when a memory call omits one.
fn instructions(policy: &Policy) -> String {
    let pinned = match policy.memory_scopes.as_deref() {
        Some([only]) if !only.ends_with('*') => Some(only.clone()),
        _ => None,
    };
    let scope = match (pinned, policy.default_memory_scope()) {
        (Some(pinned), _) => format!(
            "Memory calls in this connection always use scope {pinned:?} (pinned by policy)."
        ),
        (None, Some(default)) => format!(
            "Memory calls that omit scope use {default:?}; pass scope (for example user:<name>) for other namespaces. list_scopes shows existing ones."
        ),
        (None, None) => "Memory calls need a scope such as project:<repo> or user:<name>; list_scopes shows existing ones.".to_string(),
    };
    let mode = crate::daemon::mode();
    format!(
        "Local code retrieval and explicit, namespaced agent memory.\n\
         Code: use search_code to find files, symbols and phrases (repo_path is optional inside a Git repository or with one policy root), open_range to read exact current lines, list_symbols and repo_map to orient. Results carry freshness; run index_repository when the index is missing or stale.\n\
         Memory: at the start of a task, recall what is already known about it. When you learn something worth keeping (a decision, a user preference, a fact about this project), remember it: store the evidence with kind=source, then the conclusion with kind=derived and source_id set to that source's id (or source_uri for a file or URL). When a fact changes, store the new one with supersedes_id and valid_from instead of forgetting the old one. Memories are never captured automatically; only what you store is kept. {scope}\n\
         Server mode: {mode}.\n\
         Retrieval modes: search_code defaults to lexical unless an embedding provider is configured and permitted, then hybrid; recall defaults to lexical. Explicit lexical never contacts a provider. Remote modes use only the user's saved provider and consent."
    )
}

pub fn tools() -> Vec<Value> {
    let mut tools = vec![
        tool(
            "index_repository",
            "Incrementally index eligible files in one repository. With a configured, consented and permitted provider, also update embeddings (remote providers receive source chunks). Otherwise build the lexical index. Returns completeness, skip counts, and embedding status or an actionable embedding notice.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"include_generated":{"type":"boolean"},"verify_hashes":{"type":"boolean"}}}),
        ),
        tool(
            "refresh_indexed_file",
            "Refresh one explicit repository-relative file without walking the repository. Current ignore/security rules are applied; the catalog remains partial until index_repository reconciles all changes.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"path":{"type":"string"}},"required":["path"]}),
        ),
        tool(
            "search_code",
            "Search code using lexical, semantic, or hybrid retrieval. Defaults to hybrid with a configured, permitted provider; otherwise lexical. Hybrid protects exact paths, symbols and literal phrases before RRF. Remote modes send only the query to the saved provider and require user consent. Never configures providers or uploads source files. Returns current-source evidence within an estimated-token budget.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20},"token_budget":{"type":"integer","minimum":64,"maximum":8192},"context_lines":{"type":"integer","minimum":1,"maximum":8,"description":"Snippet lines per result, starting at the matching line (default 3). Lines longer than 200 characters are cut around the match."},"fresh_context":{"type":"boolean","description":"Resend ranges this connection already received instead of returning references."},"explain":{"type":"boolean","description":"Include score and score_breakdown per result."},"verbose":{"type":"boolean","description":"Return the pre-0.2 response shape (snippet per result)."}},"required":["query"]}),
        ),
        tool(
            "open_range",
            "Read a bounded line range from the current repository file and report whether it matches the indexed hash.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"path":{"type":"string"},"line_start":{"type":"integer","minimum":1},"line_end":{"type":"integer","minimum":1},"ref":{"type":"string","description":"A ref from search_code (path#start-end@hash). Replaces path and line numbers; a ref whose file changed returns stale_handle and no content."}}}),
        ),
        tool(
            "list_symbols",
            "List extracted Tree-sitter symbols in one repository/worktree.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"name":{"type":"string"},"limit":{"type":"integer"}}}),
        ),
        tool(
            "repo_map",
            "Return deterministic file/language/symbol counts for one worktree plus a ranked, token-budgeted file list (files).",
            json!({"type":"object","properties":{"repo_path":{"type":"string"},"query":{"type":"string","description":"Bias the file ranking toward paths and symbols matching these terms."},"token_budget":{"type":"integer","minimum":64,"maximum":8192,"description":"Budget for the ranked files list (default 1024)."}}}),
        ),
        tool(
            "index_status",
            "Report whether this repository/worktree index is complete and its generation, counts, and exclusions.",
            json!({"type":"object","properties":{"repo_path":{"type":"string"}}}),
        ),
        tool(
            "remember",
            "Persist one memory in a namespace such as project:repo-name or user:alice (scope defaults to this repository's project namespace). kind=source stores raw evidence (a quote, a decision as stated, a log line). kind=derived stores a conclusion and must cite source_id (a source memory's id) or source_uri (a file path or URL). Typical flow: remember {kind:source, content:'User: always use pnpm'} -> id 7; remember {kind:derived, content:'Package manager is pnpm', source_id:7}. To change a fact, store the new derived fact with supersedes_id and valid_from; history is kept and the old validity interval closes. With a configured, consented and permitted embedding provider the memory is also embedded for semantic recall (embed=false skips it); the response then carries embedding or embedding_notice.",
            json!({"type":"object","properties":{"operation_key":{"type":"string","maxLength":256,"description":"Retry key: repeating a call with the same key and payload returns the first result instead of storing a duplicate."},"scope":{"type":"string"},"kind":{"type":"string","enum":["source","derived"]},"content":{"type":"string","maxLength":65536},"source_uri":{"type":"string"},"source_id":{"type":"integer"},"supersedes_id":{"type":"integer"},"observed_at":{"type":"string","description":"RFC 3339 time the evidence was observed."},"valid_from":{"type":"string","description":"RFC 3339; required with supersedes_id."},"valid_to":{"type":"string"},"embed":{"type":"boolean","description":"Default true. false stores the memory without embedding it now."}},"required":["kind","content"]}),
        ),
        tool(
            "remember_batch",
            "Atomically persist 1 to 512 explicit source or derived memories (same fields and rules as remember; scope defaults per item). A failed record rolls back the entire batch; larger imports should be split into caller-managed batches. With a configured provider the stored memories are embedded afterwards; an embedding failure never undoes the batch and recall reports partial semantic coverage.",
            json!({"type":"object","properties":{"memories":{"type":"array","minItems":1,"maxItems":512,"items":{"type":"object","properties":{"scope":{"type":"string"},"kind":{"type":"string","enum":["source","derived"]},"content":{"type":"string"},"source_uri":{"type":"string"},"source_id":{"type":"integer"},"supersedes_id":{"type":"integer"},"observed_at":{"type":"string"},"valid_from":{"type":"string"},"valid_to":{"type":"string"}},"required":["kind","content"]}},"embed":{"type":"boolean"}},"required":["memories"]}),
        ),
        tool(
            "recall",
            "Search active memories in exactly one namespace and time; repository code is never included. Results carry candidate_coverage: complete, exact_top_k (exact under the documented ordering), bm25_top_k (bounded window for queries with more than four informative terms), or recency_window (only when broad_terms is recent).",
            json!({"type":"object","properties":{"scope":{"type":"string"},"query":{"type":"string"},"at":{"type":"string"},"limit":{"type":"integer"},"broad_terms":{"type":"string","enum":["exact","recent"],"description":"Default exact BM25 ranking. recent opts a single query term found in at least half of the namespace into a fast newest-first window that can omit older evidence."}},"required":["query"]}),
        ),
        tool(
            "get_memory",
            "Fetch one active memory by id within exactly one namespace, for example the source a derived fact cites.",
            json!({"type":"object","properties":{"scope":{"type":"string"},"id":{"type":"integer"}},"required":["id"]}),
        ),
        tool(
            "list_memories",
            "List non-forgotten source and derived records in exactly one memory namespace, newest first. Page with offset; filter with kind.",
            json!({"type":"object","properties":{"scope":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":1000},"offset":{"type":"integer","minimum":0,"maximum":100000},"kind":{"type":"string","enum":["source","derived"]}}}),
        ),
        tool(
            "list_scopes",
            "List memory namespaces that hold active memories, with their counts. Only namespaces this server's policy allows are shown.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "forget",
            "Immediately suppress one memory and remove it from the memory search index.",
            json!({"type":"object","properties":{"scope":{"type":"string"},"id":{"type":"integer"}},"required":["id"]}),
        ),
    ];
    for tool in &mut tools {
        if matches!(tool["name"].as_str(), Some("search_code" | "recall")) {
            tool["inputSchema"]["properties"]["mode"] = json!({
                "type":"string", "enum":["lexical","semantic","hybrid"],
                "description":"lexical never contacts a provider. search_code defaults to hybrid with a configured, permitted provider; recall defaults to lexical. Remote modes require saved user consent and cannot select a provider."
            });
        }
    }
    tools
}

/// Tools whose configured behavior can contact an embedding provider.
const OPEN_WORLD_TOOLS: [&str; 5] = [
    "index_repository",
    "search_code",
    "recall",
    "remember",
    "remember_batch",
];

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    // Search may contact a configured provider. Hints describe capability, not authority;
    // policy and saved consent are enforced before any remote request.
    let read_only = !WRITE_TOOLS.contains(&name);
    let mut annotations =
        json!({"readOnlyHint":read_only,"openWorldHint":OPEN_WORLD_TOOLS.contains(&name)});
    if !read_only {
        annotations["destructiveHint"] = json!(name == "forget");
        if matches!(name, "index_repository" | "refresh_indexed_file" | "forget") {
            annotations["idempotentHint"] = json!(true);
        }
    }
    json!({"name":name,"description":description,"inputSchema":input_schema,"annotations":annotations})
}

fn call_tool(
    db_path: &Path,
    policy: &Policy,
    memory_session: &mut Option<Session>,
    params: &Value,
    cancellation: &AtomicBool,
) -> Result<Value> {
    let name = required_str(params, "name")?;
    // Inside the daemon every writer takes the FIFO write gate, so two clients can never contend
    // for SQLite's write lock. A no-op everywhere else.
    let _write_gate = (crate::daemon::gate_active() && WRITE_TOOLS.contains(&name))
        .then(|| crate::daemon::write_gate().acquire());
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
            let finished_complete = result.complete && !result.cancelled;
            let mut result = serde_json::to_value(result)?;
            crate::watch::ensure_after_index(db_path, &repo, finished_complete);
            if !cancellation.load(std::sync::atomic::Ordering::Relaxed) {
                let embedded = (|| -> Result<Option<crate::embed::EmbedReport>> {
                    let Some(settings) = search::index_settings(&repo, policy)? else {
                        return Ok(None);
                    };
                    let Some(mut embedder) = crate::ux::consented_embedder_for(&settings)? else {
                        return Ok(None);
                    };
                    Ok(Some(crate::embed::embed_code(
                        db_path,
                        &repo,
                        embedder.as_mut(),
                        cancellation,
                    )?))
                })();
                match embedded {
                    Ok(Some(report)) => result["embedding"] = serde_json::to_value(report)?,
                    Ok(None) => {}
                    Err(error) => {
                        result["embedding_notice"] = json!(format!(
                            "{error:#}; lexical indexing succeeded; use `midnight index` to complete embeddings"
                        ))
                    }
                }
            }
            Ok(result)
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
            let (mode, settings) = search::resolve(&args, Some(&repo), policy, false)?;
            let query = required_str(&args, "query")?;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
            let user_budget = args
                .get("token_budget")
                .and_then(Value::as_u64)
                .unwrap_or(2048) as usize;
            let context_lines = args
                .get("context_lines")
                .and_then(Value::as_u64)
                .map_or(code::DEFAULT_CONTEXT_LINES, |n| n.min(64) as usize);
            let verbose = args
                .get("verbose")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let flag = |name: &str| args.get(name).and_then(Value::as_bool).unwrap_or(false);
            let opts = crate::context::CompactOptions {
                token_budget: user_budget,
                limit: limit.clamp(1, 20),
                fresh_context: flag("fresh_context"),
                explain: flag("explain"),
            };
            // Compact shape: retrieve with the largest budget so the legacy packer cuts nothing,
            // then let the compact renderer apply the caller's budget (order never changes).
            let backend_budget = if verbose {
                user_budget
            } else {
                code::MAX_TOKEN_BUDGET
            };
            let ledger = memory_session
                .get_or_insert_with(Session::default)
                .ledger
                .clone();
            if mode != crate::embed::SearchMode::Lexical {
                let session = memory_session.get_or_insert_with(Session::default);
                let mut loader = search::loader(
                    settings,
                    policy.allow_remote_embeddings,
                    session.search.clone(),
                    session.local.clone(),
                    repo.to_string_lossy().into_owned(),
                );
                let scope = crate::db::repo_scope(&repo)?;
                let conn = session_connection(db_path, memory_session)?;
                let response = crate::embed::search_code_on(
                    conn,
                    &scope,
                    query,
                    mode,
                    limit,
                    backend_budget,
                    context_lines,
                    &mut loader,
                )?;
                if verbose {
                    return Ok(serde_json::to_value(response)?);
                }
                let info = serde_json::to_value(&response.info)?;
                let mut value = crate::context::compact_search(
                    conn,
                    &scope,
                    response.search,
                    query,
                    context_lines,
                    &opts,
                    &mut *ledger.borrow_mut(),
                )?;
                crate::context::merge_info(&mut value, info);
                return Ok(value);
            }
            let scope = crate::db::repo_scope(&repo)?;
            let conn = session_connection(db_path, memory_session)?;
            let result = code::search_code_scoped(
                conn,
                &scope,
                query,
                limit,
                backend_budget,
                context_lines,
            )?;
            if verbose {
                return Ok(serde_json::to_value(result)?);
            }
            Ok(crate::context::compact_search(
                conn,
                &scope,
                result,
                query,
                context_lines,
                &opts,
                &mut *ledger.borrow_mut(),
            )?)
        }
        "open_range" => {
            let repo = path_arg(&args, "repo_path")?;
            let scope = crate::db::repo_scope(&repo)?;
            let ledger = memory_session
                .get_or_insert_with(Session::default)
                .ledger
                .clone();
            let conn = session_connection(db_path, memory_session)?;
            if let Some(handle) = optional_str(&args, "ref") {
                return crate::context::open_by_handle(
                    conn,
                    &scope,
                    handle,
                    &mut *ledger.borrow_mut(),
                );
            }
            let result = code::open_range_scoped(
                conn,
                &scope,
                required_str(&args, "path")?,
                required_u32(&args, "line_start")?,
                required_u32(&args, "line_end")?,
            )?;
            Ok(serde_json::to_value(result)?)
        }
        "list_symbols" => {
            let repo = path_arg(&args, "repo_path")?;
            let name = args.get("name").and_then(Value::as_str);
            let scope = crate::db::repo_scope(&repo)?;
            Ok(serde_json::to_value(code::list_symbols_scoped(
                session_connection(db_path, memory_session)?,
                &scope,
                name,
                args.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize,
            )?)?)
        }
        "repo_map" => {
            let repo = path_arg(&args, "repo_path")?;
            let scope = crate::db::repo_scope(&repo)?;
            let conn = session_connection(db_path, memory_session)?;
            let ranked = crate::context::ranked_repo_files(
                conn,
                &scope,
                optional_str(&args, "query"),
                args.get("token_budget")
                    .and_then(Value::as_u64)
                    .unwrap_or(1024) as usize,
            )?;
            let mut value = serde_json::to_value(code::repo_map_scoped(conn, scope)?)?;
            crate::context::merge_info(&mut value, ranked);
            Ok(value)
        }
        "index_status" => {
            let repo = path_arg(&args, "repo_path")?;
            let scope = crate::db::repo_scope(&repo)?;
            crate::watch::ensure(db_path, &scope.root);
            let root = scope.root.clone();
            let mut value = serde_json::to_value(code::index_status_scoped(
                session_connection(db_path, memory_session)?,
                scope,
            )?)?;
            value["watcher"] = crate::watch::status_json(&root);
            Ok(value)
        }
        "remember" => {
            let record = if let Some(key) = optional_str(&args, "operation_key") {
                let input: memory::MemoryInput = serde_json::from_value(args.clone())?;
                ensure_memory_session(db_path, memory_session)?.remember_idempotent(key, &input)?
            } else {
                ensure_memory_session(db_path, memory_session)?.remember(
                    required_str(&args, "scope")?,
                    required_str(&args, "kind")?,
                    required_str(&args, "content")?,
                    optional_str(&args, "source_uri"),
                    args.get("source_id").and_then(Value::as_i64),
                    args.get("supersedes_id").and_then(Value::as_i64),
                    optional_str(&args, "observed_at"),
                    optional_str(&args, "valid_from"),
                    optional_str(&args, "valid_to"),
                )?
            };
            let mut result = serde_json::to_value(&record)?;
            if wants_embedding(&args)? {
                annotate_embedding(
                    &mut result,
                    embed_stored(db_path, policy, memory_session, &record.scope, &[record.id]),
                );
            }
            Ok(result)
        }
        "remember_batch" => {
            let inputs: Vec<memory::MemoryInput> = serde_json::from_value(
                args.get("memories")
                    .cloned()
                    .context("missing memories array")?,
            )?;
            let embed = wants_embedding(&args)?;
            let records =
                ensure_memory_session(db_path, memory_session)?.remember_batch(&inputs)?;
            if embed {
                // The response stays the stored records; a failure here shows up as partial
                // semantic coverage in recall and is completed by `embed --namespace`.
                let mut by_scope: std::collections::BTreeMap<&str, Vec<i64>> = Default::default();
                for record in &records {
                    by_scope.entry(&record.scope).or_default().push(record.id);
                }
                for (scope, ids) in by_scope {
                    if let Some(Err(error)) =
                        embed_stored(db_path, policy, memory_session, scope, &ids)
                    {
                        eprintln!("midnight: memory embedding skipped for {scope}: {error:#}");
                    }
                }
            }
            Ok(serde_json::to_value(records)?)
        }
        "recall" => {
            let (mode, settings) = search::resolve(&args, None, policy, true)?;
            if mode != crate::embed::SearchMode::Lexical {
                if optional_str(&args, "at").is_some()
                    || args.get("broad_terms").is_some_and(|v| v != "exact")
                {
                    bail!(
                        "semantic/hybrid recall requires current time and broad_terms=exact; use mode=lexical for historical or recent-window recall"
                    );
                }
                let scope = required_str(&args, "scope")?;
                let query = required_str(&args, "query")?;
                let session = memory_session.get_or_insert_with(Session::default);
                let mut loader = search::loader(
                    settings,
                    policy.allow_remote_embeddings,
                    session.search.clone(),
                    session.local.clone(),
                    format!("memory:{scope}"),
                );
                return Ok(serde_json::to_value(crate::embed::search_memory_on(
                    session_connection(db_path, memory_session)?,
                    scope,
                    query,
                    mode,
                    args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize,
                    &mut loader,
                )?)?);
            }
            Ok(serde_json::to_value(
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
            )?)
        }
        "get_memory" => {
            let id = args
                .get("id")
                .and_then(Value::as_i64)
                .context("missing integer id")?;
            let scope = required_str(&args, "scope")?;
            match ensure_memory_session(db_path, memory_session)?.get_memory(scope, id)? {
                Some(record) => Ok(serde_json::to_value(record)?),
                None => bail!("no active memory {id} in scope {scope:?}"),
            }
        }
        "list_memories" => {
            let kind = optional_str(&args, "kind");
            let offset = match args.get("offset") {
                None | Some(Value::Null) => 0,
                Some(value) => value
                    .as_u64()
                    .context("offset must be a non-negative integer")?
                    .min(memory::MAX_LIST_OFFSET as u64 + 1)
                    as usize,
            };
            Ok(serde_json::to_value(
                ensure_memory_session(db_path, memory_session)?.list_memories_page(
                    required_str(&args, "scope")?,
                    kind,
                    args.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize,
                    offset,
                )?,
            )?)
        }
        "list_scopes" => {
            let scopes = ensure_memory_session(db_path, memory_session)?
                .list_scopes()?
                .into_iter()
                .filter(|summary| policy.scope_allowed(&summary.scope))
                .collect::<Vec<_>>();
            Ok(json!({
                "scopes": scopes,
                "default_scope": match policy.memory_scopes.as_deref() {
                    Some([only]) if !only.ends_with('*') => Some(only.clone()),
                    _ => policy.default_memory_scope(),
                },
            }))
        }
        "forget" => Ok(
            json!({"forgotten":ensure_memory_session(db_path,memory_session)?.forget(
            required_str(&args,"scope")?,args.get("id").and_then(Value::as_i64).context("missing integer id")?
        )?}),
        ),
        _ => bail!("unknown tool {name}"),
    }
}

/// `embed` defaults to true; only an explicit boolean false opts out.
fn wants_embedding(args: &Value) -> Result<bool> {
    match args.get("embed") {
        None | Some(Value::Null) => Ok(true),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => bail!("embed must be a boolean"),
    }
}

/// Embed memories that were just stored, with the connection's provider rules. `None` when no
/// provider is configured. The memory itself is already committed whatever this returns.
fn embed_stored(
    db_path: &Path,
    policy: &Policy,
    memory_session: &mut Option<Session>,
    scope: &str,
    ids: &[i64],
) -> Option<Result<crate::embed::EmbedReport>> {
    let session = memory_session.get_or_insert_with(Session::default);
    let mut embedder = match search::memory_write_embedder(policy, &session.local) {
        Ok(None) => return None,
        Ok(Some(embedder)) => embedder,
        Err(error) => return Some(Err(error)),
    };
    Some(
        session_connection(db_path, memory_session)
            .and_then(|conn| crate::embed::embed_memory_ids(conn, scope, ids, embedder.as_mut())),
    )
}

fn annotate_embedding(result: &mut Value, outcome: Option<Result<crate::embed::EmbedReport>>) {
    match outcome {
        None => {}
        Some(Ok(report)) => {
            result["embedding"] = serde_json::to_value(report).unwrap_or(Value::Null);
        }
        Some(Err(error)) => {
            result["embedding_notice"] = json!(format!(
                "{error:#}; the memory is stored; run `midnight embed --namespace <scope>` to embed it later"
            ));
        }
    }
}

/// The session's shared connection, opened (and migrated) once per MCP session.
fn session_connection<'a>(
    db_path: &Path,
    session: &'a mut Option<Session>,
) -> Result<&'a rusqlite::Connection> {
    Ok(ensure_memory_session(db_path, session)?.connection())
}

fn ensure_memory_session<'a>(
    db_path: &Path,
    session: &'a mut Option<Session>,
) -> Result<&'a mut memory::MemorySession> {
    let session = session.get_or_insert_with(Session::default);
    if session.memory.is_none() {
        session.memory = Some(memory::MemorySession::open(db_path)?);
    }
    session
        .memory
        .as_mut()
        .context("memory database session failed to initialize")
}

/// Nearest ancestor of `dir` (inclusive) that holds `.git`, never `home` or the filesystem root.
fn repo_root_above(dir: &Path, home: Option<&Path>) -> Option<PathBuf> {
    dir.ancestors()
        .take_while(|ancestor| ancestor.parent().is_some() && Some(*ancestor) != home)
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

fn path_arg(args: &Value, key: &str) -> Result<PathBuf> {
    let path = PathBuf::from(required_str(args, key)?);
    Ok(match connection_cwd() {
        Some(cwd) if path.is_relative() => cwd.join(path),
        _ => path,
    })
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
    use super::{PROTOCOL_VERSION, Policy, initialize, read_bounded_line};
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn initialize_offers_the_newest_version_for_an_unknown_request() {
        let response = initialize(
            json!(1),
            &json!({"protocolVersion":"2099-01-01"}),
            &Policy::default(),
        );
        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(response.get("error").is_none());
    }

    #[test]
    fn repository_root_is_found_above_a_subdirectory_but_never_at_home() {
        let temp = tempfile::TempDir::new().unwrap();
        let home = temp.path().join("home");
        let repo = home.join("work/app");
        let deep = repo.join("src/ui");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(
            super::repo_root_above(&deep, Some(&home)),
            Some(repo.clone())
        );
        assert_eq!(super::repo_root_above(&repo, Some(&home)), Some(repo));
        // A repository at HOME (dotfiles) is not a default.
        std::fs::create_dir_all(home.join(".git")).unwrap();
        assert_eq!(
            super::repo_root_above(&home.join("work"), Some(&home)),
            None
        );
        assert_eq!(super::repo_root_above(&home, Some(&home)), None);
    }

    #[test]
    fn initialize_echoes_a_supported_older_version() {
        for version in ["2025-06-18", "2025-03-26", "2024-11-05"] {
            let response = initialize(
                json!(1),
                &json!({"protocolVersion":version}),
                &Policy::default(),
            );
            assert_eq!(response["result"]["protocolVersion"], version);
        }
    }

    #[test]
    fn tools_carry_accurate_annotations() {
        for tool in super::tools() {
            let name = tool["name"].as_str().unwrap();
            let hints = &tool["annotations"];
            assert_eq!(
                hints["openWorldHint"],
                matches!(
                    name,
                    "index_repository" | "search_code" | "recall" | "remember" | "remember_batch"
                ),
                "{name}"
            );
            let writes = super::WRITE_TOOLS.contains(&name);
            assert_eq!(hints["readOnlyHint"], !writes, "{name}");
            assert_eq!(hints["destructiveHint"] == true, name == "forget", "{name}");
        }
    }

    #[test]
    fn code_tools_default_to_the_policy_root_and_never_to_a_non_repository() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let plain = temp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();

        let pinned = Policy {
            repo_roots: Some(vec![repo.clone()]),
            ..Policy::default()
        };
        let args = pinned.authorize("repo_map", &json!({})).unwrap();
        assert_eq!(args["repo_path"], json!(repo.to_string_lossy()));
        // An explicit path is never replaced.
        let args = pinned
            .authorize("repo_map", &json!({"repo_path": repo.to_string_lossy()}))
            .unwrap();
        assert_eq!(args["repo_path"], json!(repo.to_string_lossy()));

        // A single root that is not a repository gives no default (and the cwd of the test
        // process is only used when it is a repository root, which is not asserted here).
        let loose = Policy {
            repo_roots: Some(vec![plain]),
            ..Policy::default()
        };
        if loose.default_repo().is_none() {
            assert!(loose.authorize("repo_map", &json!({})).is_err());
        }
        // Memory tools are unaffected.
        assert!(
            Policy::default()
                .authorize("list_memories", &json!({"scope":"project:x"}))
                .is_ok()
        );
    }

    #[test]
    fn memory_tools_default_to_the_repository_project_scope_within_policy() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path().join("my app");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let rooted = Policy {
            repo_roots: Some(vec![repo.clone()]),
            ..Policy::default()
        };
        assert_eq!(
            rooted.default_memory_scope().as_deref(),
            Some("project:my-app")
        );
        let args = rooted
            .authorize("remember", &json!({"kind":"source","content":"x"}))
            .unwrap();
        assert_eq!(args["scope"], "project:my-app");
        let args = rooted
            .authorize("recall", &json!({"query":"x","scope":"user:alice"}))
            .unwrap();
        assert_eq!(
            args["scope"], "user:alice",
            "an explicit scope is never replaced"
        );
        let args = rooted
            .authorize(
                "remember_batch",
                &json!({"memories":[{"kind":"source","content":"a"},{"kind":"source","content":"b","scope":"user:b"}]}),
            )
            .unwrap();
        assert_eq!(args["memories"][0]["scope"], "project:my-app");
        assert_eq!(args["memories"][1]["scope"], "user:b");
        // list_scopes names no scope and is not rewritten.
        assert!(
            rooted
                .authorize("list_scopes", &json!({}))
                .unwrap()
                .get("scope")
                .is_none()
        );

        // A scope list that does not cover the default gives no default and still requires one.
        let users_only = Policy {
            repo_roots: Some(vec![repo.clone()]),
            memory_scopes: Some(vec!["user:*".into()]),
            ..Policy::default()
        };
        assert_eq!(users_only.default_memory_scope(), None);
        assert!(
            users_only
                .authorize("recall", &json!({"query":"x"}))
                .is_err()
        );
        // A prefix that covers it keeps the default.
        let projects = Policy {
            repo_roots: Some(vec![repo.clone()]),
            memory_scopes: Some(vec!["project:*".into()]),
            ..Policy::default()
        };
        assert_eq!(
            projects.authorize("recall", &json!({"query":"x"})).unwrap()["scope"],
            "project:my-app"
        );
        // A pinned scope wins over the default; a different explicit scope is refused.
        let pinned = Policy {
            repo_roots: Some(vec![repo]),
            memory_scopes: Some(vec!["user:alice".into()]),
            ..Policy::default()
        };
        assert_eq!(
            pinned.authorize("recall", &json!({"query":"x"})).unwrap()["scope"],
            "user:alice"
        );
        assert!(
            pinned
                .authorize("recall", &json!({"query":"x","scope":"project:my-app"}))
                .is_err()
        );
        let text = super::instructions(&rooted);
        assert!(text.contains("\"project:my-app\""), "{text}");
        assert!(super::instructions(&pinned).contains("pinned by policy"));
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
