use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aether_midnight::{auth, benchmark, code, db, mcp, mcp_http, memory};

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
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Approximate context-token budget (Midnight heuristic, not a model tokenizer).
        #[arg(long, default_value_t = 2048)]
        token_budget: usize,
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
        /// Interleave original, descending and production exact stages; assert identical results.
        #[arg(long)]
        scan_ab: bool,
        /// Use the tied bench-memory corpus instead of Zipf (requires --scan-ab).
        #[arg(long, requires = "scan_ab")]
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
            query,
            limit,
            token_budget,
        } => print_json(&code::search_code_with_budget(
            &db_path,
            &repo,
            &query,
            limit,
            token_budget,
        )?)?,
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
            scan_ab,
            tied_corpus,
            size,
            namespace_count,
            samples,
        } => {
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
