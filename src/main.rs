use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aether_midnight::{auth, benchmark, cloud, code, db, embed, mcp, mcp_http, memory, runtime};

#[derive(Parser)]
#[command(
    name = "aether-midnight",
    version,
    about = "Local-first code and explicit agent-memory retrieval"
)]
struct Cli {
    /// SQLite database. Defaults to the OS local application-data directory.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Machine-readable JSON output (all commands emit JSON; accepted everywhere for stability).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create the database and apply migrations (idempotent).
    Init,
    /// Explicit memories: add, get, list, delete, search (namespace-isolated).
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Manage local embedding models (explicit download; never implicit).
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },
    /// Compute or refresh persistent embeddings for one namespace and/or one indexed repository.
    /// Needs an installed model (`models pull`); incremental and safe to interrupt.
    Embed {
        /// Memory namespace whose active memories are embedded.
        #[arg(long, alias = "scope")]
        namespace: Option<String>,
        /// Repository/worktree whose indexed files are embedded (run `index` first).
        #[arg(long)]
        repo: Option<PathBuf>,
        #[command(flatten)]
        embedding: EmbeddingArgs,
    },
    /// ONNX Runtime used by local embeddings: status, and explicit pinned install.
    Runtime {
        #[command(subcommand)]
        action: RuntimeAction,
    },
    /// Check database, FTS5, integrity and writability; exits non-zero on failure.
    Doctor,
    /// Incrementally index a local repository/worktree.
    Index {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Include otherwise excluded generated/vendor directories. Credential files remain excluded.
        #[arg(long)]
        include_generated: bool,
        /// Re-read and hash every eligible file even when size and high-resolution mtime match.
        #[arg(long)]
        verify_hashes: bool,
    },
    /// Refresh one explicit, repository-relative file without walking the repository.
    /// A later full index run is required to restore complete catalog status.
    RefreshFile {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        path: String,
    },
    /// Search paths, symbols, whole-token content, and identifier-boundary fragments; arbitrary in-token substrings are unsupported.
    Search {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Query text (positional or via --query).
        #[arg(value_name = "QUERY", required_unless_present = "query_flag")]
        query_pos: Option<String>,
        #[arg(long = "query", conflicts_with = "query_pos")]
        query_flag: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Approximate context-token budget (Midnight heuristic, not a model tokenizer).
        #[arg(long, default_value_t = 2048)]
        token_budget: usize,
        /// lexical (default, unchanged output), semantic, or hybrid (RRF of lexical + semantic).
        #[arg(long, value_enum, default_value_t = SearchModeArg::Lexical)]
        mode: SearchModeArg,
        #[command(flatten)]
        embedding: EmbeddingArgs,
    },
    /// Read a bounded range from the current source file.
    OpenRange {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        path: String,
        #[arg(long)]
        start: u32,
        #[arg(long)]
        end: u32,
    },
    /// List extracted structural symbols.
    ListSymbols {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Summarize files, languages, symbols, and top-level directories.
    RepoMap {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Report index generation, completeness, counts, skips, and freshness timestamps.
    IndexStatus {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Persist a raw source or a provenance-linked derived fact. Scope must be namespaced.
    Remember {
        #[arg(long)]
        operation_key: Option<String>,
        #[arg(long)]
        scope: String,
        #[arg(long,value_enum,default_value_t=MemoryKind::Source)]
        kind: MemoryKind,
        #[arg(long)]
        content: String,
        #[arg(long)]
        source_uri: Option<String>,
        #[arg(long)]
        source_id: Option<i64>,
        /// Keep a derived fact's history and close its earlier validity period when replacing it.
        #[arg(long)]
        supersedes_id: Option<i64>,
        #[arg(long)]
        observed_at: Option<String>,
        #[arg(long)]
        valid_from: Option<String>,
        #[arg(long)]
        valid_to: Option<String>,
    },
    /// Recall only memories within one namespace and active at the requested UTC instant.
    Recall {
        #[arg(long)]
        scope: String,
        #[arg(long)]
        query: String,
        #[arg(long)]
        at: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// List active, non-forgotten memories within one namespace.
    ListMemories {
        #[arg(long)]
        scope: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Immediately suppress a memory in its exact namespace.
    Forget {
        #[arg(long)]
        scope: String,
        #[arg(long)]
        id: i64,
    },
    /// Optimize FTS segments, checkpoint WAL, and physically compact SQLite pages.
    Compact,
    /// Scrub a bounded tombstone batch and merge up to 16 FTS pages per index.
    Maintain {
        #[arg(long, default_value_t = 128)]
        max_records: usize,
    },
    /// Verify SQLite, FTS membership and namespace counters.
    VerifyIntegrity,
    /// Report local runtime/database diagnostics.
    Diagnose,
    /// Run the fixed local retrieval/index/RG/resource benchmark and emit JSON.
    Bench {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value = ".")]
        heldout_repo: PathBuf,
        #[arg(long, default_value = "benchmark/manifest.json")]
        manifest: PathBuf,
        #[arg(long, default_value_t = 20)]
        samples: usize,
        #[arg(long, default_value_t = 3)]
        index_samples: usize,
        #[arg(long, default_value_t = 10)]
        stress_cycles: usize,
    },
    /// Run the isolated 1k/10k/100k memory workload without the repository-index campaign.
    BenchMemory {
        /// Comma-separated record counts. Maximum is 100000 to keep the default local run bounded.
        #[arg(long, value_delimiter = ',', default_value = "1000,10000,100000")]
        sizes: Vec<usize>,
        /// Use 1 for a single large namespace or 4 for the distributed baseline.
        #[arg(long, default_value_t = 4)]
        namespace_count: usize,
        /// Collect per-phase retrieval timings and SQLite counters for 100 warm recall samples.
        #[arg(long)]
        profile_recall: bool,
    },
    /// Paired staged-vs-v0.3.1 recall measurement on a deterministic Zipf corpus.
    BenchRetrieval {
        #[cfg(feature = "retrieval-experiments")]
        #[arg(long)]
        kernel_ab: bool,
        #[cfg(feature = "retrieval-experiments")]
        #[arg(long, requires = "kernel_ab")]
        kernel_fixed: bool,
        #[cfg(feature = "retrieval-experiments")]
        #[arg(long, requires = "kernel_ab")]
        kernel_broad: bool,
        /// Interleave original, descending and production exact stages; assert identical results.
        #[arg(long)]
        scan_ab: bool,
        /// Use the tied bench-memory corpus instead of Zipf (requires an explicit comparison mode).
        #[arg(long)]
        tied_corpus: bool,
        #[arg(long, default_value_t = 100_000)]
        size: usize,
        #[arg(long, default_value_t = 1)]
        namespace_count: usize,
        #[arg(long, default_value_t = 100)]
        samples: usize,
    },
    #[command(hide = true)]
    FtsEval {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long, default_value = "benchmark/manifest.json")]
        manifest: PathBuf,
        #[arg(long, default_value_t = 30)]
        samples: usize,
    },
    /// Serve the local MCP tool interface over newline-delimited stdio JSON-RPC.
    Mcp {
        #[command(flatten)]
        policy: PolicyArgs,
    },
    /// Serve MCP over loopback-only Streamable HTTP (for clients such as AETHER Dots).
    ServeHttp {
        /// Loopback address to bind. Port 0 picks a free port (printed on stdout).
        #[arg(long, default_value = "127.0.0.1:0")]
        bind: std::net::SocketAddr,
    },
    /// Manage bearer tokens required by `serve-http` (hashed at rest, revocable).
    Token {
        #[command(subcommand)]
        action: TokenAction,
    },
}

#[derive(Subcommand)]
enum MemoryAction {
    /// Store a memory in a namespace.
    Add {
        #[arg(long, alias = "scope")]
        namespace: String,
        content: String,
        #[arg(long, value_enum, default_value_t = MemoryKind::Source)]
        kind: MemoryKind,
        #[arg(long)]
        source_uri: Option<String>,
    },
    /// Fetch one active memory by id within its namespace.
    Get {
        #[arg(long, alias = "scope")]
        namespace: String,
        id: i64,
    },
    /// List active memories in a namespace (newest first).
    List {
        #[arg(long, alias = "scope")]
        namespace: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Delete (suppress) a memory in its exact namespace.
    Delete {
        #[arg(long, alias = "scope")]
        namespace: String,
        id: i64,
    },
    /// BM25 search within one namespace.
    Search {
        #[arg(long, alias = "scope")]
        namespace: String,
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// lexical (default, unchanged output), semantic, or hybrid (RRF of BM25 + semantic).
        #[arg(long, value_enum, default_value_t = SearchModeArg::Lexical)]
        mode: SearchModeArg,
        #[command(flatten)]
        embedding: EmbeddingArgs,
    },
}

#[derive(Subcommand)]
enum RuntimeAction {
    /// Report the ONNX Runtime Midnight would load: path, version, compatibility, checksum.
    Status,
    /// Download the pinned official ONNX Runtime for this platform, verify its SHA-256, install it.
    Install,
}

/// Embedding source for `embed` and for `--mode semantic|hybrid`. The default is the local model;
/// a remote provider is used only when named here, and only its own credential variable is read.
#[derive(Args, Clone, Default)]
struct EmbeddingArgs {
    /// Embedding provider: local (default), openrouter, or rainy. Remote providers send the text
    /// being embedded (memories, code chunks, queries) to that provider.
    #[arg(long, value_enum, default_value_t = ProviderArg::Local)]
    provider: ProviderArg,
    /// Remote model id, e.g. openai/text-embedding-3-small. Required for remote providers; never
    /// defaulted or substituted.
    #[arg(long)]
    model: Option<String>,
    /// Vector size of the remote model. Required for remote providers; part of the space identity,
    /// sent as `dimensions`, and checked on every response.
    #[arg(long)]
    dimensions: Option<usize>,
    /// API base URL (without /embeddings). OpenRouter default: https://openrouter.ai/api/v1.
    /// Rainy default: the rainy-sdk endpoint. HTTPS only (HTTP allowed on loopback for testing).
    #[arg(long)]
    endpoint: Option<String>,
    /// Per-request timeout in seconds for remote providers.
    #[arg(long, default_value_t = cloud::DEFAULT_TIMEOUT_SECS)]
    timeout_secs: u64,
    /// Optional provider request field KEY=JSON (openrouter: input_type, provider, user; rainy:
    /// user). Repeatable. Changing it changes the embedding-space identity.
    #[arg(long = "param", value_name = "KEY=JSON")]
    params: Vec<String>,
}

#[derive(Clone, Copy, Default, ValueEnum)]
enum ProviderArg {
    #[default]
    Local,
    Openrouter,
    Rainy,
}

impl EmbeddingArgs {
    fn provider(&self) -> cloud::Provider {
        match self.provider {
            ProviderArg::Local => cloud::Provider::Local,
            ProviderArg::Openrouter => cloud::Provider::OpenRouter,
            ProviderArg::Rainy => cloud::Provider::Rainy,
        }
    }

    /// Validate flags and build the embedder. Remote providers do no network I/O here.
    fn build(&self) -> Result<Box<dyn embed::TextEmbedder>> {
        let provider = self.provider();
        if provider == cloud::Provider::Local {
            if self
                .model
                .as_deref()
                .is_some_and(|m| m != embed::MODEL_NAME)
            {
                anyhow::bail!(
                    "local provider only serves {}; use --provider openrouter|rainy for other models",
                    embed::MODEL_NAME
                );
            }
            if self.dimensions.is_some_and(|d| d != embed::MODEL_DIMS)
                || self.endpoint.is_some()
                || !self.params.is_empty()
            {
                anyhow::bail!(
                    "--dimensions/--endpoint/--param do not apply to the local provider ({} is fixed at {} dimensions)",
                    embed::MODEL_NAME,
                    embed::MODEL_DIMS
                );
            }
            return Ok(Box::new(embed::FastEmbedder::load_installed()?));
        }
        let config = cloud::CloudConfig::new(
            provider,
            self.model.clone(),
            self.dimensions,
            self.endpoint.clone(),
            self.timeout_secs,
            &self.params,
        )?;
        Ok(Box::new(cloud::CloudEmbedder::from_env(config)?))
    }
}

#[derive(Subcommand)]
enum ModelsAction {
    /// Hash every pinned model file against its SHA-256 (offline).
    Verify,
    /// List known models and whether each is installed locally.
    List,
    /// Download a model into the local cache (the only command that uses the network).
    Pull {
        /// Model name, e.g. multilingual-e5-small.
        name: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum SearchModeArg {
    Lexical,
    Semantic,
    Hybrid,
}

impl From<SearchModeArg> for embed::SearchMode {
    fn from(mode: SearchModeArg) -> Self {
        match mode {
            SearchModeArg::Lexical => embed::SearchMode::Lexical,
            SearchModeArg::Semantic => embed::SearchMode::Semantic,
            SearchModeArg::Hybrid => embed::SearchMode::Hybrid,
        }
    }
}

/// Lazy model loader: ONNX Runtime is initialised only if a semantic stage actually needs it.
fn model_loader(args: &EmbeddingArgs) -> impl FnMut() -> Result<Box<dyn embed::TextEmbedder>> {
    let args = args.clone();
    move || args.build()
}

#[derive(Subcommand)]
enum TokenAction {
    /// Mint a token; the plaintext is printed once on stdout.
    Create {
        #[arg(long)]
        name: String,
        /// Memory scope the token may touch (repeatable). A single literal
        /// scope is also injected into calls that omit one.
        #[arg(long = "memory-scope")]
        memory_scopes: Vec<String>,
        /// Allow every memory scope (explicit opt-in).
        #[arg(long)]
        all_scopes: bool,
        /// Comma-separated tools (default: recall,list_memories,remember).
        #[arg(long, value_delimiter = ',')]
        tools: Option<Vec<String>>,
        #[arg(long)]
        read_only: bool,
        /// Repository roots for code tools (repeatable); required for code tools.
        #[arg(long = "repo-root")]
        repo_roots: Vec<PathBuf>,
        #[arg(long)]
        ttl_secs: Option<u64>,
    },
    /// List tokens (never prints secrets).
    List,
    /// Revoke by id or name; effective on the next request.
    Revoke { token: String },
}

#[derive(clap::Args, Clone)]
struct PolicyArgs {
    /// Expose only read/recall tools (no remember, forget, or indexing).
    #[arg(long)]
    read_only: bool,
    /// Comma-separated allowlist of tool names (default: all, subject to --read-only).
    #[arg(long, value_delimiter = ',')]
    allow_tools: Option<Vec<String>>,
    /// Memory namespaces tools may touch; a trailing * matches a prefix (repeatable).
    #[arg(long = "memory-scope")]
    memory_scopes: Option<Vec<String>>,
    /// Directory roots that repo_path must be inside (repeatable).
    #[arg(long = "repo-root")]
    repo_roots: Option<Vec<PathBuf>>,
}

impl PolicyArgs {
    fn into_policy(self) -> mcp::Policy {
        let allowed_tools = match (self.allow_tools, self.read_only) {
            (Some(list), true) => {
                let ro = mcp::Policy::read_only_tools();
                Some(list.into_iter().filter(|t| ro.contains(t)).collect())
            }
            (Some(list), false) => Some(list),
            (None, true) => Some(mcp::Policy::read_only_tools()),
            (None, false) => None,
        };
        mcp::Policy {
            allowed_tools,
            memory_scopes: self.memory_scopes,
            repo_roots: self.repo_roots,
        }
    }
}

#[derive(Clone, ValueEnum)]
enum MemoryKind {
    Source,
    Derived,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or(db::default_db_path()?);
    let cancellation = Arc::new(AtomicBool::new(false));
    let cancellation_for_signal = Arc::clone(&cancellation);
    ctrlc::set_handler(move || cancellation_for_signal.store(true, Ordering::Relaxed))
        .context("install Ctrl-C cancellation handler")?;
    match cli.command {
        Commands::Init => {
            let conn = db::open_db(&db_path)?;
            let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            drop(conn);
            if cli.json {
                print_json(
                    &serde_json::json!({"initialized":true,"database_path":db_path,"schema_version":v}),
                )?;
            } else {
                println!("initialized {} (schema version {v})", db_path.display());
            }
        }
        Commands::Memory { action } => memory_command(&db_path, action, cli.json)?,
        Commands::Doctor => {
            if !doctor(&db_path, cli.json)? {
                std::process::exit(1);
            }
        }
        Commands::Index {
            repo,
            include_generated,
            verify_hashes,
        } => {
            let result = code::index_repository(
                &db_path,
                &repo,
                include_generated,
                verify_hashes,
                &cancellation,
            )?;
            print_json(&result)?;
            if result.cancelled {
                std::process::exit(130);
            }
        }
        Commands::RefreshFile { repo, path } => {
            print_json(&code::refresh_indexed_file(
                &db_path,
                &repo,
                &path,
                &cancellation,
            )?)?;
        }
        Commands::Search {
            repo,
            query_pos,
            query_flag,
            limit,
            token_budget,
            mode,
            embedding,
        } => {
            let query = query_pos.or(query_flag).unwrap_or_default();
            if matches!(mode, SearchModeArg::Lexical) {
                // Unchanged legacy contract and code path.
                print_json(&code::search_code_with_budget(
                    &db_path,
                    &repo,
                    &query,
                    limit,
                    token_budget,
                )?)?
            } else {
                let response = embed::search_code(
                    &db_path,
                    &repo,
                    &query,
                    mode.into(),
                    limit,
                    token_budget,
                    &mut model_loader(&embedding),
                )?;
                if response.info.degraded
                    && let Some(notice) = &response.info.notice
                {
                    eprintln!("warning: {notice}");
                }
                print_json(&response)?
            }
        }
        Commands::Models { action } => models_command(action, cli.json)?,
        Commands::Embed {
            namespace,
            repo,
            embedding,
        } => {
            if namespace.is_none() && repo.is_none() {
                anyhow::bail!("embed needs --namespace <ns> and/or --repo <path>");
            }
            let mut embedder = embedding.build()?;
            let id = embedder.identity().clone();
            if embedding.provider() != cloud::Provider::Local {
                eprintln!(
                    "note: sending the text of the selected {} to {} ({}) for embedding; lexical search is unaffected",
                    match (&namespace, &repo) {
                        (Some(_), Some(_)) => "memories and indexed code chunks",
                        (Some(_), None) => "memories",
                        _ => "indexed code chunks",
                    },
                    embedding.provider().as_str(),
                    cloud::CloudConfig::new(
                        embedding.provider(),
                        embedding.model.clone(),
                        embedding.dimensions,
                        embedding.endpoint.clone(),
                        embedding.timeout_secs,
                        &embedding.params,
                    )?
                    .destination()
                );
            }
            let mut reports = Vec::new();
            let mut failure: Option<anyhow::Error> = None;
            if let Some(namespace) = namespace {
                match embed::embed_memories(&db_path, &namespace, embedder.as_mut(), &cancellation)
                {
                    Ok(r) => reports.push(r),
                    Err(e) => failure = Some(e),
                }
            }
            if failure.is_none()
                && let Some(repo) = repo
            {
                match embed::embed_code(&db_path, &repo, embedder.as_mut(), &cancellation) {
                    Ok(r) => reports.push(r),
                    Err(e) => failure = Some(e),
                }
            }
            if let Some(e) = failure {
                // Vectors committed before the failure are kept; lexical search is unaffected.
                if !reports.is_empty() {
                    print_json(&reports)?;
                }
                return Err(e.context(format!(
                    "embedding with {}/{}d did not complete (committed vectors are kept, lexical search is unaffected; re-run `embed` to resume)",
                    id.name, id.dimensions
                )));
            }
            print_json(&reports)?;
            if reports.iter().any(|r| r.cancelled) {
                std::process::exit(130);
            }
        }
        Commands::Runtime { action } => runtime_command(action, cli.json)?,
        Commands::OpenRange {
            repo,
            path,
            start,
            end,
        } => print_json(&code::open_range(&db_path, &repo, &path, start, end)?)?,
        Commands::ListSymbols { repo, name, limit } => print_json(&code::list_symbols(
            &db_path,
            &repo,
            name.as_deref(),
            limit,
        )?)?,
        Commands::RepoMap { repo } => print_json(&code::repo_map(&db_path, &repo)?)?,
        Commands::IndexStatus { repo } => print_json(&code::index_status(&db_path, &repo)?)?,
        Commands::Remember {
            operation_key,
            scope,
            kind,
            content,
            source_uri,
            source_id,
            supersedes_id,
            observed_at,
            valid_from,
            valid_to,
        } => {
            let kind = match kind {
                MemoryKind::Source => "source",
                MemoryKind::Derived => "derived",
            };
            if let Some(key) = operation_key {
                print_json(&memory::remember_idempotent(
                    &db_path,
                    &key,
                    &memory::MemoryInput {
                        scope,
                        kind: kind.to_string(),
                        content,
                        source_uri,
                        source_id,
                        supersedes_id,
                        observed_at,
                        valid_from,
                        valid_to,
                    },
                )?)?;
                return Ok(());
            }
            print_json(&memory::remember(
                &db_path,
                &scope,
                kind,
                &content,
                source_uri.as_deref(),
                source_id,
                supersedes_id,
                observed_at.as_deref(),
                valid_from.as_deref(),
                valid_to.as_deref(),
            )?)?;
        }
        Commands::Recall {
            scope,
            query,
            at,
            limit,
        } => print_json(&memory::recall(
            &db_path,
            &scope,
            &query,
            at.as_deref(),
            limit,
        )?)?,
        Commands::ListMemories { scope, limit } => {
            print_json(&memory::list_memories(&db_path, &scope, limit)?)?
        }
        Commands::Forget { scope, id } => {
            print_json(&serde_json::json!({"forgotten":memory::forget(&db_path,&scope,id)?}))?
        }
        Commands::Maintain { max_records } => {
            print_json(&serde_json::json!({"scrubbed": memory::maintain(&db_path, max_records)?}))?;
        }
        Commands::VerifyIntegrity => {
            memory::verify_integrity(&db_path)?;
            print_json(&serde_json::json!({"integrity":"ok"}))?;
        }
        Commands::Compact => {
            memory::compact(&db_path)?;
            println!("{{\"compacted\":true}}");
        }
        Commands::Diagnose => {
            let sqlite =
                db::open_db(&db_path)?
                    .query_row("SELECT sqlite_version()", [], |row| row.get::<_, String>(0))?;
            let fts5: bool = db::open_db(&db_path)?
                .query_row(
                    "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
                    [],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap_or(false);
            print_json(&serde_json::json!({
                "version":aether_midnight::VERSION,
                "os":std::env::consts::OS,
                "arch":std::env::consts::ARCH,
                "build_profile":if cfg!(debug_assertions){"debug"}else{"release"},
                "database_path":db_path,
                "database_format":aether_midnight::INDEX_FORMAT_VERSION,
                "sqlite_version":sqlite,
                "fts5_enabled":fts5,
                "fts5_code_tokenizer":"unicode61 remove_diacritics 2",
                "tree_sitter_version":"0.27.1",
                "max_file_bytes":aether_midnight::MAX_FILE_BYTES,
                "network":"unused by core and MCP server"
            }))?;
        }
        Commands::Bench {
            repo,
            heldout_repo,
            manifest,
            samples,
            index_samples,
            stress_cycles,
        } => {
            let result = benchmark::run(
                &db_path,
                &repo,
                &heldout_repo,
                &manifest,
                samples,
                index_samples,
                stress_cycles,
            )?;
            print_json(&result)?;
        }
        Commands::BenchMemory {
            sizes,
            namespace_count,
            profile_recall,
        } => {
            print_json(&benchmark::run_memory_scaling_with_layout(
                &sizes,
                profile_recall,
                namespace_count,
            )?)?;
        }
        Commands::BenchRetrieval {
            #[cfg(feature = "retrieval-experiments")]
            kernel_ab,
            #[cfg(feature = "retrieval-experiments")]
            kernel_fixed,
            #[cfg(feature = "retrieval-experiments")]
            kernel_broad,
            scan_ab,
            tied_corpus,
            size,
            namespace_count,
            samples,
        } => {
            #[cfg(feature = "retrieval-experiments")]
            if kernel_ab {
                print_json(&benchmark::run_kernel_ab(
                    size,
                    namespace_count,
                    samples,
                    tied_corpus,
                    kernel_fixed,
                    kernel_broad,
                )?)?;
                return Ok(());
            }
            if tied_corpus && !scan_ab {
                anyhow::bail!("--tied-corpus requires --scan-ab or --kernel-ab");
            }
            let result = if scan_ab {
                benchmark::run_retrieval_scan_ab(size, namespace_count, samples, tied_corpus)?
            } else {
                benchmark::run_retrieval_ab(size, namespace_count, samples)?
            };
            print_json(&result)?;
        }
        Commands::FtsEval {
            repo,
            manifest,
            samples,
        } => {
            print_json(&benchmark::evaluate_fts5(&repo, &manifest, samples)?)?;
        }
        Commands::Mcp { policy } => mcp::run_stdio(&db_path, &policy.into_policy(), cancellation)?,
        Commands::Token { action } => token_command(&db_path, action)?,
        Commands::ServeHttp { bind } => mcp_http::serve(&db_path, bind, cancellation)?,
    }
    Ok(())
}

fn memory_command(db_path: &std::path::Path, action: MemoryAction, json: bool) -> Result<()> {
    let line = |m: &memory::MemoryRecord| {
        format!(
            "{}\t{}\t{}\t{}",
            m.id,
            m.scope,
            m.kind,
            m.content.replace('\n', " ")
        )
    };
    match action {
        MemoryAction::Add {
            namespace,
            content,
            kind,
            source_uri,
        } => {
            let kind = match kind {
                MemoryKind::Source => "source",
                MemoryKind::Derived => "derived",
            };
            let m = memory::remember(
                db_path,
                &namespace,
                kind,
                &content,
                source_uri.as_deref(),
                None,
                None,
                None,
                None,
                None,
            )?;
            if json {
                print_json(&m)?
            } else {
                println!("added {}", line(&m))
            }
        }
        MemoryAction::Get { namespace, id } => match memory::get_memory(db_path, &namespace, id)? {
            Some(m) => {
                if json {
                    print_json(&m)?
                } else {
                    println!("{}", line(&m))
                }
            }
            None => anyhow::bail!("memory {id} not found in namespace {namespace:?}"),
        },
        MemoryAction::List { namespace, limit } => {
            let ms = memory::list_memories(db_path, &namespace, limit)?;
            if json {
                print_json(&ms)?
            } else {
                ms.iter().for_each(|m| println!("{}", line(m)))
            }
        }
        MemoryAction::Delete { namespace, id } => {
            let deleted = memory::forget(db_path, &namespace, id)?;
            if json {
                print_json(&serde_json::json!({"deleted": deleted}))?
            } else {
                println!("deleted: {deleted}")
            }
            if !deleted {
                std::process::exit(1);
            }
        }
        MemoryAction::Search {
            namespace,
            query,
            limit,
            mode,
            embedding,
        } => {
            if !matches!(mode, SearchModeArg::Lexical) {
                let response = embed::search_memory(
                    db_path,
                    &namespace,
                    &query,
                    mode.into(),
                    limit,
                    &mut model_loader(&embedding),
                )?;
                if let Some(notice) = &response.info.notice {
                    eprintln!("warning: {notice}");
                }
                if json {
                    print_json(&response)?
                } else {
                    for r in &response.results {
                        println!("{:.4}\t{}\t{}", r.score, line(&r.memory), r.match_reason);
                    }
                }
                return Ok(());
            }
            let rs = memory::recall(db_path, &namespace, &query, None, limit)?;
            if json {
                print_json(&rs)?
            } else {
                for r in &rs {
                    println!("{:.4}\t{}", r.score, line(&r.memory));
                }
            }
        }
    }
    Ok(())
}

fn doctor(db_path: &std::path::Path, json: bool) -> Result<bool> {
    let mut checks: Vec<serde_json::Value> = Vec::new();
    let mut check = |name: &str, r: Result<String>| {
        let (ok, detail) = match r {
            Ok(d) => (true, d),
            Err(e) => (false, format!("{e:#}")),
        };
        checks.push(serde_json::json!({"check": name, "ok": ok, "detail": detail}));
    };
    check(
        "database_open",
        db::open_db(db_path).map(|_| db_path.display().to_string()),
    );
    check(
        "sqlite_version",
        db::open_db(db_path).and_then(|c| {
            Ok(c.query_row("SELECT sqlite_version()", [], |r| r.get::<_, String>(0))?)
        }),
    );
    check(
        "fts5",
        db::open_db(db_path).and_then(|c| {
            c.execute_batch(
                "CREATE VIRTUAL TABLE temp.doctor_fts USING fts5(x); DROP TABLE temp.doctor_fts;",
            )?;
            Ok("available".to_owned())
        }),
    );
    check(
        "integrity",
        memory::verify_integrity(db_path).map(|_| "ok".to_owned()),
    );
    // Optional capability checks (local/cloud embeddings): reported with remedies, never fatal,
    // never networked. A failing optional check does not change the exit status.
    let models = embed::models_dir();
    let mut optional = |name: &str, ok: bool, detail: String, remedy: Vec<String>| {
        checks.push(serde_json::json!({
            "check": name, "ok": ok, "optional": true, "detail": detail, "remedy": remedy
        }));
    };
    match &models {
        Ok(dir) => {
            let rt = runtime::inspect(dir);
            let detail = if rt.compatible {
                format!(
                    "{} {} at {}{}",
                    rt.source.map_or("-", |s| match s {
                        runtime::Source::EnvVar => "ORT_DYLIB_PATH",
                        runtime::Source::Managed => "managed",
                        runtime::Source::System => "system",
                    }),
                    rt.version.as_deref().unwrap_or("?"),
                    rt.library
                        .as_ref()
                        .map_or(String::new(), |p| p.display().to_string()),
                    if rt.checksum_verified {
                        " (SHA-256 verified)"
                    } else {
                        ""
                    }
                )
            } else {
                rt.problems.join("; ")
            };
            optional("onnx_runtime", rt.compatible, detail, rt.remedies.clone());
            let status = embed::model_status(dir);
            if status.installed {
                match embed::verify_model(dir) {
                    Ok(_) => optional(
                        "local_model",
                        true,
                        format!(
                            "{} @ {} (SHA-256 verified)",
                            embed::MODEL_NAME,
                            embed::MODEL_REVISION
                        ),
                        vec![],
                    ),
                    Err(e) => optional(
                        "local_model",
                        false,
                        format!("{e:#}"),
                        vec![format!(
                            "delete {} and run `aether-midnight models pull {}`",
                            dir.display(),
                            embed::MODEL_NAME
                        )],
                    ),
                }
            } else {
                let mut detail = format!(
                    "{} @ {} not installed (missing or wrong size: {:?})",
                    embed::MODEL_NAME,
                    embed::MODEL_REVISION,
                    status.missing_files
                );
                if !status.ignored_revisions.is_empty() {
                    detail.push_str(&format!(
                        "; ignoring unpinned cached revisions {:?}",
                        status.ignored_revisions
                    ));
                }
                optional(
                    "local_model",
                    false,
                    detail,
                    vec![format!(
                        "run `aether-midnight models pull {}` (~470 MB download)",
                        embed::MODEL_NAME
                    )],
                );
            }
        }
        Err(e) => optional("local_model", false, format!("{e:#}"), vec![]),
    }
    for provider in [cloud::Provider::OpenRouter, cloud::Provider::Rainy] {
        let var = provider.key_env().unwrap_or_default();
        let set = std::env::var(var).is_ok_and(|v| !v.trim().is_empty());
        optional(
            &format!("cloud_{}", provider.as_str()),
            set,
            if set {
                format!("{var} is set (value never printed; no request is made by doctor)")
            } else {
                format!(
                    "{var} is not set; provider {} is unavailable (optional)",
                    provider.as_str()
                )
            },
            if set {
                vec![]
            } else {
                vec![format!(
                    "export {var}=… to enable `--provider {}`",
                    provider.as_str()
                )]
            },
        );
    }
    let ok = checks
        .iter()
        .all(|c| c["ok"] == true || c["optional"] == true);
    if json {
        print_json(
            &serde_json::json!({"ok": ok, "version": aether_midnight::VERSION, "checks": checks}),
        )?;
    } else {
        for c in &checks {
            println!(
                "{} {}: {}",
                match (c["ok"] == true, c["optional"] == true) {
                    (true, _) => "ok  ",
                    (false, true) => "warn",
                    (false, false) => "FAIL",
                },
                c["check"].as_str().unwrap_or(""),
                c["detail"].as_str().unwrap_or("")
            );
            for r in c["remedy"].as_array().into_iter().flatten() {
                println!("       fix: {}", r.as_str().unwrap_or(""));
            }
        }
    }
    Ok(ok)
}

fn runtime_command(action: RuntimeAction, json: bool) -> Result<()> {
    let dir = embed::models_dir()?;
    match action {
        RuntimeAction::Status => {
            let status = runtime::inspect(&dir);
            if json {
                print_json(&status)?;
            } else {
                println!(
                    "platform {} | library {} | version {} | compatible {} | checksum-verified {}",
                    status.platform,
                    status
                        .library
                        .as_ref()
                        .map_or("-".into(), |p| p.display().to_string()),
                    status.version.as_deref().unwrap_or("-"),
                    status.compatible,
                    status.checksum_verified
                );
                for p in &status.problems {
                    println!("problem: {p}");
                }
                for r in &status.remedies {
                    println!("fix: {r}");
                }
            }
            if !status.compatible {
                std::process::exit(1);
            }
        }
        RuntimeAction::Install => {
            let report = runtime::install(&dir)?;
            if json {
                print_json(&report)?;
            } else if report.already_installed {
                println!(
                    "ONNX Runtime {} already installed and verified at {}",
                    report.version,
                    report.library.display()
                );
            } else {
                println!(
                    "installed ONNX Runtime {} ({}) at {}; archive SHA-256 {} verified",
                    report.version,
                    report.platform,
                    report.library.display(),
                    report.archive_sha256
                );
            }
        }
    }
    Ok(())
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn token_command(db_path: &std::path::Path, action: TokenAction) -> Result<()> {
    match action {
        TokenAction::Create {
            name,
            memory_scopes,
            all_scopes,
            tools,
            read_only,
            repo_roots,
            ttl_secs,
        } => {
            let code_tools = ["search_code", "open_range", "repo_map", "index_status"];
            let mut tools = tools.unwrap_or_else(|| {
                let mut t = vec!["recall".to_owned(), "list_memories".to_owned()];
                if !read_only {
                    t.push("remember".to_owned());
                }
                t
            });
            if read_only {
                let ro = mcp::Policy::read_only_tools();
                tools.retain(|t| ro.contains(t));
            }
            let memory_tools = [
                "remember",
                "remember_batch",
                "recall",
                "list_memories",
                "forget",
            ];
            if tools.iter().any(|t| memory_tools.contains(&t.as_str()))
                && memory_scopes.is_empty()
                && !all_scopes
            {
                anyhow::bail!("memory tools need --memory-scope <scope> (or --all-scopes)");
            }
            if tools.iter().any(|t| code_tools.contains(&t.as_str())) && repo_roots.is_empty() {
                anyhow::bail!("code tools need at least one --repo-root");
            }
            let known: Vec<String> = mcp::tools()
                .iter()
                .filter_map(|t| t["name"].as_str().map(str::to_owned))
                .collect();
            if let Some(bad) = tools.iter().find(|t| !known.contains(t)) {
                anyhow::bail!("unknown tool {bad:?}");
            }
            let policy = mcp::Policy {
                allowed_tools: Some(tools),
                memory_scopes: (!all_scopes).then_some(memory_scopes),
                repo_roots: (!repo_roots.is_empty()).then_some(repo_roots),
            };
            let (record, token) = auth::create(db_path, &name, policy, ttl_secs)?;
            eprintln!("created token {} ({}); shown once", record.id, record.name);
            println!("{token}");
        }
        TokenAction::List => {
            for t in auth::list(db_path)? {
                println!(
                    "{}\t{}\t{}\tscopes={:?}\ttools={:?}",
                    t.id,
                    t.name,
                    if t.revoked { "revoked" } else { "active" },
                    t.policy.memory_scopes,
                    t.policy.allowed_tools
                );
            }
        }
        TokenAction::Revoke { token } => {
            let n = auth::revoke(db_path, &token)?;
            eprintln!("revoked {n} token(s)");
        }
    }
    Ok(())
}

fn models_command(action: ModelsAction, json: bool) -> Result<()> {
    let dir = embed::models_dir()?;
    match action {
        ModelsAction::Verify => {
            let status = embed::verify_model(&dir)?;
            if json {
                print_json(&serde_json::json!({"verified": true, "model": status}))?;
            } else {
                println!(
                    "verified {} revision {} (all {} files match their SHA-256 pins)",
                    status.name,
                    embed::MODEL_REVISION,
                    embed::MODEL_FILE_PINS.len()
                );
            }
        }
        ModelsAction::List => {
            let status = embed::model_status(&dir);
            if json {
                print_json(&serde_json::json!({
                    "models_dir": dir,
                    "models": [status],
                    "providers": [
                        {"provider": "local", "model": embed::MODEL_NAME, "dimensions": embed::MODEL_DIMS},
                        {"provider": "openrouter", "credential_env": cloud::OPENROUTER_KEY_ENV, "endpoint": cloud::OPENROUTER_ENDPOINT, "model": "caller-chosen (--model, --dimensions required)"},
                        {"provider": "rainy", "credential_env": cloud::RAINY_KEY_ENV, "endpoint": "rainy-sdk default or --endpoint", "model": "caller-chosen (--model, --dimensions required)"}
                    ],
                }))?;
            } else {
                println!(
                    "{}\t{}d\t{}\t{}",
                    status.name,
                    status.dimensions,
                    if status.installed {
                        "installed"
                    } else {
                        "not installed"
                    },
                    status.revision.as_deref().unwrap_or("-")
                );
            }
        }
        ModelsAction::Pull { name } => {
            if name != embed::MODEL_NAME {
                anyhow::bail!("unknown model {name:?}; available: {}", embed::MODEL_NAME);
            }
            let status = embed::pull_model(&dir, !json)?;
            if json {
                print_json(&status)?;
            } else {
                println!(
                    "installed {} (revision {}) in {}",
                    status.name,
                    status.revision.as_deref().unwrap_or("-"),
                    dir.display()
                );
            }
        }
    }
    Ok(())
}
