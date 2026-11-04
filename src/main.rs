use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aether_midnight::{benchmark, code, db, mcp, memory};

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
    /// Search exact paths/symbols and deterministic lexical content.
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
        #[arg(long, default_value = "benchmark/manifest.json")]
        manifest: PathBuf,
        #[arg(long, default_value_t = 20)]
        samples: usize,
        #[arg(long, default_value_t = 3)]
        index_samples: usize,
        #[arg(long, default_value_t = 10)]
        stress_cycles: usize,
    },
    /// Serve the local MCP tool interface over newline-delimited stdio JSON-RPC.
    Mcp,
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
                "fts5_trigram":true,
                "tree_sitter_version":"0.27.1",
                "max_file_bytes":aether_midnight::MAX_FILE_BYTES,
                "network":"unused by core and MCP server"
            }))?;
        }
        Commands::Bench {
            repo,
            manifest,
            samples,
            index_samples,
            stress_cycles,
        } => {
            let result = benchmark::run(
                &db_path,
                &repo,
                &manifest,
                samples,
                index_samples,
                stress_cycles,
            )?;
            print_json(&result)?;
        }
        Commands::Mcp => mcp::run_stdio(&db_path, cancellation)?,
    }
    Ok(())
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
