//! Human-facing CLI layer: first-run wizard, consent, status and readable result rendering.
//!
//! Conventions (clig.dev): data goes to stdout, prompts/progress/warnings to stderr; prompts only
//! when asked for (`setup` without flags) and never on the data path; every failure names the fix;
//! secrets are read without echo, never from arguments, and never printed.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::cloud::{self, Provider};
use crate::code;
use crate::config::{self, Choice, KeySource, Settings};
use crate::db;
use crate::embed::{self, TextEmbedder, TextKind};
use crate::runtime;

pub fn stdin_is_tty() -> bool {
    // SAFETY: isatty only inspects a file descriptor.
    unsafe { libc::isatty(0) == 1 }
}
pub fn stdout_is_tty() -> bool {
    // SAFETY: as above.
    unsafe { libc::isatty(1) == 1 }
}

/// Line reader shared by every prompt of one command, so piped answers are consumed in order.
pub struct Prompter {
    lines: std::io::StdinLock<'static>,
}

impl Prompter {
    pub fn new() -> Self {
        Prompter {
            lines: std::io::stdin().lock(),
        }
    }

    fn line(&mut self) -> Result<String> {
        let mut buf = String::new();
        let n = self.lines.read_line(&mut buf)?;
        if n == 0 {
            bail!(
                "input ended before setup finished; answer interactively or pass flags, e.g. `midnight setup --provider lexical`"
            );
        }
        Ok(buf.trim().to_owned())
    }

    pub fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String> {
        match default {
            Some(d) => eprint!("{question} [{d}]: "),
            None => eprint!("{question}: "),
        }
        std::io::stderr().flush().ok();
        let answer = self.line()?;
        Ok(if answer.is_empty() {
            default.unwrap_or("").to_owned()
        } else {
            answer
        })
    }

    pub fn confirm(&mut self, question: &str, default_yes: bool) -> Result<bool> {
        let hint = if default_yes { "Y/n" } else { "y/N" };
        loop {
            eprint!("{question} [{hint}]: ");
            std::io::stderr().flush().ok();
            match self.line()?.to_ascii_lowercase().as_str() {
                "" => return Ok(default_yes),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => eprintln!("Please answer y or n."),
            }
        }
    }

    /// Read a secret; echo is disabled when stdin is a terminal.
    pub fn secret(&mut self, question: &str) -> Result<String> {
        eprint!("{question}: ");
        std::io::stderr().flush().ok();
        let tty = stdin_is_tty();
        // SAFETY: termios calls on fd 0 with a zero-initialised struct that tcgetattr fills.
        let saved = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if tty && libc::tcgetattr(0, &mut t) == 0 {
                let saved = t;
                t.c_lflag &= !libc::ECHO;
                libc::tcsetattr(0, libc::TCSANOW, &t);
                Some(saved)
            } else {
                None
            }
        };
        let result = self.line();
        if let Some(saved) = saved {
            // SAFETY: restores the attributes captured above.
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved) };
            eprintln!();
        }
        result
    }
}

impl Default for Prompter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------- setup

#[derive(Default)]
pub struct SetupArgs {
    pub provider: Option<Choice>,
    pub model: Option<String>,
    pub dimensions: Option<usize>,
    pub endpoint: Option<String>,
    pub api_key_stdin: bool,
    pub allow_upload: bool,
    pub no_validate: bool,
    pub no_input: bool,
}

fn probe(settings: &Settings, key: &str) -> Result<()> {
    let cfg = settings.cloud_config()?;
    let var = cfg.provider.key_env().unwrap_or_default();
    let mut embedder = cloud::CloudEmbedder::new(cfg, key.to_owned(), var)?;
    // Fixed, non-user text: setup never uploads repository content.
    embedder.embed(TextKind::Query, &["midnight setup check"])?;
    Ok(())
}

fn now_utc() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Run `setup`. Interactive when no provider flag/`--no-input` is given; otherwise fully scripted.
/// Returns a machine-readable summary; human text goes to stderr/stdout as described above.
pub fn run_setup(args: SetupArgs) -> Result<Value> {
    let scripted = args.provider.is_some()
        || args.no_input
        || std::env::var_os("MIDNIGHT_NO_INPUT").is_some_and(|v| !v.is_empty());
    if scripted && args.provider.is_none() {
        bail!("no prompts in --no-input mode: pass --provider rainy|openrouter|local|lexical");
    }
    let mut prompt = Prompter::new();
    let existing = config::load()?;
    if !scripted {
        eprintln!("AETHER Midnight setup\n");
        if let Some(s) = &existing {
            eprintln!(
                "Current choice: {}{}\n",
                s.provider.as_str(),
                s.model
                    .as_deref()
                    .map(|m| format!(" ({m})"))
                    .unwrap_or_default()
            );
        }
        eprintln!("How should Midnight search your code?");
        eprintln!(
            "  1) Rainy       cloud embeddings + keyword search (code chunks are sent to Rainy)"
        );
        eprintln!(
            "  2) OpenRouter  cloud embeddings + keyword search (code chunks are sent to OpenRouter)"
        );
        eprintln!(
            "  3) Local       on-device model; needs ONNX Runtime and a ~470 MB model download"
        );
        eprintln!(
            "  4) Lexical     keyword search only; nothing leaves this machine, nothing to install"
        );
    }
    let choice = match args.provider {
        Some(c) => c,
        None => loop {
            let answer = prompt.ask("Choose 1-4", Some("4"))?;
            match Choice::parse(&answer) {
                Some(c) => break c,
                None => eprintln!("'{answer}' is not an option; enter 1, 2, 3 or 4."),
            }
        },
    };

    let mut settings = Settings::lexical();
    settings.provider = choice;
    let mut new_key: Option<String> = None;
    let mut validated = false;
    let mut notes: Vec<String> = Vec::new();

    if choice.is_remote() {
        let provider = choice.provider().expect("remote");
        let var = provider.key_env().unwrap_or_default();
        settings.endpoint = args.endpoint.clone();

        // Credential: stdin (scripted), environment, stored, or hidden prompt.
        let key = if args.api_key_stdin {
            let key = prompt.line()?;
            if key.is_empty() {
                bail!("--api-key-stdin: standard input was empty");
            }
            new_key = Some(key.clone());
            key
        } else if let Some((key, source)) = config::resolve_key(choice)? {
            if !scripted {
                eprintln!(
                    "\nUsing the API key from {}.",
                    match source {
                        KeySource::Environment => format!("${var} (not copied to disk)"),
                        KeySource::Stored => "Midnight's credential store".to_owned(),
                    }
                );
            }
            key
        } else if scripted {
            bail!(
                "no API key for {}: export {var}=… or pass --api-key-stdin (keys are never accepted as arguments)",
                choice.as_str()
            );
        } else {
            eprintln!(
                "\nNo {var} found. The key is stored in a private file (mode 0600), never shown again."
            );
            let key = prompt.secret(&format!("{} API key (input hidden)", choice.as_str()))?;
            if key.is_empty() {
                bail!("no key entered; nothing was saved");
            }
            new_key = Some(key.clone());
            key
        };

        // Model discovery (OpenRouter only) and selection.
        let suggested = config::suggested_models(choice);
        let mut model = args.model.clone();
        let mut dims = args.dimensions;
        if model.is_none() && !scripted {
            eprintln!();
            let listed = match cloud::discover_models(
                provider,
                args.endpoint.as_deref(),
                &key,
                cloud::DEFAULT_TIMEOUT_SECS,
            ) {
                Ok(ids) => Some(ids),
                Err(e) => {
                    eprintln!("Model discovery: {e:#}");
                    None
                }
            };
            let options: Vec<String> = match &listed {
                Some(ids) => config::model_options(choice, ids),
                None => suggested.iter().map(|(m, _)| (*m).to_owned()).collect(),
            };
            match &listed {
                Some(_) => eprintln!(
                    "{} text embedding models available from {} (image-only models hidden):",
                    options.len(),
                    choice.as_str()
                ),
                None if !options.is_empty() => eprintln!("Common choices (suggestions only):"),
                None => {}
            }
            // Show a few at a time; `m` reveals the next few, and any id can be typed directly.
            const PAGE: usize = 5;
            let mut shown = 0usize;
            let reveal = |shown: &mut usize| {
                let next = (*shown + PAGE).min(options.len());
                for (i, id) in options.iter().enumerate().take(next).skip(*shown) {
                    eprintln!("  {}) {id}", i + 1);
                }
                *shown = next;
            };
            reveal(&mut shown);
            let default = options.first().map(String::as_str);
            loop {
                let more = options.len() - shown;
                let question = if more > 0 {
                    format!("Model number, id, or m for {more} more")
                } else {
                    "Model number or id".to_string()
                };
                let answer = prompt.ask(&question, default)?;
                if more > 0 && matches!(answer.to_ascii_lowercase().as_str(), "m" | "more") {
                    reveal(&mut shown);
                    continue;
                }
                model = Some(match answer.parse::<usize>() {
                    Ok(n) if n >= 1 && n <= options.len() => options[n - 1].clone(),
                    _ => answer,
                });
                break;
            }
        }
        let Some(model) = model.filter(|m| !m.trim().is_empty()) else {
            bail!("--model is required with --provider {}", choice.as_str());
        };
        if dims.is_none() {
            let suggestion = suggested.iter().find(|(m, _)| *m == model).map(|(_, d)| *d);
            if scripted {
                bail!(
                    "--dimensions is required with --provider {}",
                    choice.as_str()
                );
            }
            let answer = prompt.ask(
                "Vector dimensions of this model",
                suggestion.map(|d| d.to_string()).as_deref(),
            )?;
            dims = Some(answer.parse().with_context(|| {
                format!(
                    "'{answer}' is not a number; see the model's documentation for its vector size"
                )
            })?);
        }
        settings.model = Some(model);
        settings.dimensions = dims;
        settings.cloud_config()?; // reject bad model/dimensions/endpoint before any network call

        if args.no_validate {
            notes.push("validation skipped (--no-validate)".into());
        } else {
            if !scripted {
                eprintln!(
                    "\nChecking the provider with one tiny fixed test request (no repository content)…"
                );
            }
            match probe(&settings, &key) {
                Ok(()) => {
                    validated = true;
                    settings.validated_at = Some(now_utc());
                    if !scripted {
                        eprintln!(
                            "ok: the provider accepted the model and returned the expected vector size."
                        );
                    }
                }
                Err(e) => {
                    let message = format!("validation failed: {e:#}");
                    if scripted {
                        bail!("{message}; nothing was saved (use --no-validate to save anyway)");
                    }
                    eprintln!("{message}");
                    if !prompt.confirm("Save these settings anyway?", false)? {
                        bail!(
                            "nothing was saved; fix the key/model/dimensions and run `midnight setup` again"
                        );
                    }
                    notes.push(message);
                }
            }
        }

        // Explicit upload consent, bound to provider + destination + model.
        let dest = settings.destination();
        let consent = if scripted {
            args.allow_upload
        } else {
            eprintln!(
                "\nIndexing sends chunks of your source files (up to {} characters each), and searches send your query text, to {dest} using {}.",
                cloud::MAX_INPUT_CHARS,
                settings.model.as_deref().unwrap_or("")
            );
            eprintln!(
                "Credential files are never indexed. Lexical search keeps working without this."
            );
            prompt.confirm(
                "Allow Midnight to send this content to that provider?",
                false,
            )?
        };
        if consent {
            settings.upload_consent = Some(settings.consent_token()?);
        } else {
            notes.push(
                "remote upload not authorised: `midnight index` will ask (or need --yes) before sending anything"
                    .into(),
            );
        }
    }

    if let Some(key) = &new_key {
        config::store_key(choice, key)?;
    }
    let path = config::save(&settings)?;

    if choice == Choice::Local
        && let Ok(dir) = embed::models_dir()
    {
        if !embed::model_status(&dir).installed {
            notes.push(format!(
                "local model not installed yet: run `midnight models pull {}` (~470 MB, the only download)",
                embed::MODEL_NAME
            ));
        }
        if !runtime::inspect(&dir).compatible {
            notes.push("ONNX Runtime not found: run `midnight runtime install`".into());
        }
    }

    let summary = json!({
        "saved": true,
        "config_path": path,
        "provider": choice.as_str(),
        "model": settings.model,
        "dimensions": settings.dimensions,
        "endpoint": settings.endpoint,
        "validated": validated,
        "upload_consent": settings.upload_consented(),
        "api_key_stored": new_key.is_some(),
        "notes": notes,
    });
    Ok(summary)
}

pub fn render_setup(summary: &Value) {
    println!(
        "Saved settings to {}",
        summary["config_path"].as_str().unwrap_or("")
    );
    println!(
        "  provider   {}",
        summary["provider"].as_str().unwrap_or("")
    );
    if let Some(m) = summary["model"].as_str() {
        println!(
            "  model      {m} ({} dimensions)",
            summary["dimensions"].as_u64().unwrap_or(0)
        );
    }
    if summary["provider"] != "lexical" && summary["provider"] != "local" {
        println!(
            "  validated  {}",
            if summary["validated"] == true {
                "yes"
            } else {
                "no"
            }
        );
        println!(
            "  uploads    {}",
            if summary["upload_consent"] == true {
                "allowed"
            } else {
                "not yet allowed"
            }
        );
    }
    for note in summary["notes"].as_array().into_iter().flatten() {
        println!("note: {}", note.as_str().unwrap_or(""));
    }
    println!("\nNext:\n  midnight index .\n  midnight search \"your question\"\n  midnight status");
}

// ---------------------------------------------------------------------------- index / search

/// Ensure remote uploads are authorised for this run; may prompt on a terminal.
pub fn require_upload_consent(settings: &mut Settings, assume_yes: bool) -> Result<()> {
    if settings.upload_consented() {
        return Ok(());
    }
    let dest = settings.destination();
    let model = settings.model.clone().unwrap_or_default();
    if assume_yes {
        eprintln!("note: --yes given; sending source chunks to {dest} ({model}) for this run");
        return Ok(());
    }
    if stdin_is_tty() {
        eprintln!("Indexing will send chunks of your source files to {dest} using {model}.");
        let mut prompt = Prompter::new();
        if prompt.confirm("Allow this and remember the choice?", false)? {
            settings.upload_consent = Some(settings.consent_token()?);
            config::save(settings)?;
            return Ok(());
        }
    }
    bail!(
        "remote embedding was not authorised, so source files were not sent to {dest}. The keyword index is complete. Authorise with `midnight setup` or re-run with --yes, or choose `midnight setup --provider lexical`"
    )
}

/// Build the embedder for a saved choice. `None` for lexical.
pub fn embedder_for(settings: &Settings) -> Result<Option<Box<dyn TextEmbedder>>> {
    Ok(match settings.provider {
        Choice::Lexical => None,
        Choice::Local => Some(Box::new(embed::FastEmbedder::load_installed().context(
            "local embeddings are configured but unavailable (see `midnight status`)",
        )?)),
        Choice::Openrouter | Choice::Rainy => Some(Box::new(config::cloud_embedder(settings)?)),
    })
}

/// Embed the freshly indexed repository with the configured provider.
pub fn embed_repository(
    db_path: &Path,
    repo: &Path,
    settings: &mut Settings,
    assume_yes: bool,
    cancel: &AtomicBool,
) -> Result<Option<embed::EmbedReport>> {
    embed_repository_with_progress(db_path, repo, settings, assume_yes, cancel, &mut |_| {})
}

/// `embed_repository`, reporting progress before each file (consent is asked first).
pub fn embed_repository_with_progress(
    db_path: &Path,
    repo: &Path,
    settings: &mut Settings,
    assume_yes: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&embed::EmbedProgress),
) -> Result<Option<embed::EmbedReport>> {
    if settings.provider.is_remote() {
        // Validate configuration and credential before asking for consent.
        settings.cloud_config()?;
        if config::resolve_key(settings.provider)?.is_none() {
            bail!(
                "no API key for {}: export {}=… or run `midnight setup`",
                settings.provider.as_str(),
                settings
                    .provider
                    .provider()
                    .and_then(Provider::key_env)
                    .unwrap_or("")
            );
        }
        require_upload_consent(settings, assume_yes)?;
    }
    let Some(mut embedder) = embedder_for(settings)? else {
        return Ok(None);
    };
    Ok(Some(embed::embed_code_with_progress(
        db_path,
        repo,
        embedder.as_mut(),
        cancel,
        progress,
    )?))
}

pub fn render_index(
    report: &code::IndexReport,
    embed: Option<&Result<embed::EmbedReport, String>>,
) {
    println!(
        "Indexed {}: {} files ({} updated, {} unchanged){}",
        report.root,
        report.eligible_files,
        report.indexed_or_updated,
        report.unchanged,
        if report.complete {
            ""
        } else {
            " — index incomplete"
        }
    );
    let skipped: u64 = report.skipped.values().sum();
    if skipped > 0 {
        println!("  skipped {skipped} files (generated, vendored, oversized or credential files)");
    }
    match embed {
        Some(Ok(r)) => println!(
            "  embeddings: {} new, {} unchanged with {}",
            r.embedded, r.unchanged, r.model
        ),
        Some(Err(_)) => println!(
            "  embeddings: not completed (see the error below; the keyword index is usable)"
        ),
        None => println!(
            "  embeddings: off (keyword search). Run `midnight setup` for semantic search."
        ),
    }
    println!("\nNext: midnight search \"your question\"");
}

pub fn render_search(
    query: &str,
    info: Option<&embed::SearchInfo>,
    response: &code::CodeSearchResponse,
) {
    let results = &response.results;
    let mode = info.map_or("lexical".to_owned(), |i| {
        format!("{:?}", i.mode_used).to_lowercase()
    });
    println!(
        "{} result{} for \"{}\" · {}{}",
        results.len(),
        if results.len() == 1 { "" } else { "s" },
        query,
        mode,
        info.and_then(|i| i.model.as_ref())
            .map(|m| format!(" ({})", m.name))
            .unwrap_or_default()
    );
    if let Some(c) = info.and_then(|i| i.semantic_coverage.as_ref()) {
        println!(
            "semantic coverage: {}/{} files embedded",
            c.embedded, c.eligible
        );
    }
    for (i, r) in results.iter().enumerate() {
        let lines = match (r.line_start, r.line_end) {
            (Some(a), Some(b)) if a != b => format!(":{a}-{b}"),
            (Some(a), _) => format!(":{a}"),
            _ => String::new(),
        };
        println!("\n{:>2}. {}{}   [{}]", i + 1, r.path, lines, r.match_reason);
        if let Some(snippet) = &r.snippet {
            for line in snippet.lines().take(6) {
                println!("      {}", line.trim_end());
            }
            if snippet.lines().count() > 6 || r.snippet_truncated {
                println!("      …");
            }
        }
    }
    if results.is_empty() {
        println!("\nNo matches. Try different words, or run `midnight index .` if files changed.");
    } else {
        println!("\nOpen a range: midnight open-range --path <file> --start <n> --end <n>");
    }
}

// ----------------------------------------------------------------------------------- status

/// Gather everything `status` reports. Never prints a secret; does no network I/O.
pub fn status_report(db_path: &Path, repo: &Path) -> Result<Value> {
    let mut problems: Vec<String> = Vec::new();
    let mut next: Vec<String> = Vec::new();

    // Configuration and provider.
    let settings = match config::load() {
        Ok(s) => s,
        Err(e) => {
            problems.push(format!("{e:#}"));
            None
        }
    };
    let config_path = config::config_path().ok();
    let provider = match &settings {
        None => {
            next.push("run `midnight setup` to choose semantic or keyword search".into());
            json!({"configured": false, "effective": "lexical"})
        }
        Some(s) => {
            let mut p = json!({
                "configured": true,
                "provider": s.provider.as_str(),
                "model": s.model,
                "dimensions": s.dimensions,
                "endpoint": s.endpoint,
                "validated_at": s.validated_at,
            });
            if s.provider.is_remote() {
                let var = s
                    .provider
                    .provider()
                    .and_then(Provider::key_env)
                    .unwrap_or("");
                match s.cloud_config() {
                    Ok(_) => {}
                    Err(e) => problems.push(format!("{e:#}")),
                }
                match config::resolve_key(s.provider) {
                    Ok(Some((_, src))) => p["credential"] = json!(src),
                    Ok(None) => {
                        p["credential"] = json!("missing");
                        problems.push(format!(
                            "no API key for {}: export {var}=… or run `midnight setup`",
                            s.provider.as_str()
                        ));
                    }
                    Err(e) => problems.push(format!("{e:#}")),
                }
                p["upload_consent"] = json!(s.upload_consented());
                if !s.upload_consented() {
                    next.push("remote uploads are not authorised: `midnight index . --yes` or `midnight setup`".into());
                }
            }
            if s.provider == Choice::Local
                && let Ok(dir) = embed::models_dir()
            {
                let installed = embed::model_status(&dir).installed;
                let rt = runtime::inspect(&dir);
                p["local_model_installed"] = json!(installed);
                p["onnx_runtime_compatible"] = json!(rt.compatible);
                if !installed {
                    problems.push(format!(
                        "local model missing: run `midnight models pull {}`",
                        embed::MODEL_NAME
                    ));
                }
                if !rt.compatible {
                    problems.push(
                        "ONNX Runtime missing or incompatible: run `midnight runtime install`"
                            .into(),
                    );
                }
            }
            p
        }
    };

    // Database and repository.
    let mut database = json!({"path": db_path, "exists": db_path.exists()});
    let mut repository = json!({"path": repo, "indexed": false});
    if db_path.exists() {
        match db::open_db(db_path) {
            Ok(conn) => {
                database["size_bytes"] =
                    json!(std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0));
                database["schema_version"] = json!(
                    conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                        .ok()
                );
                database["active_memories"] = json!(
                    conn.query_row(
                        "SELECT COUNT(*) FROM memories WHERE deleted_at IS NULL",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .ok()
                );
                database["ok"] = json!(true);
                drop(conn);
                match code::index_status(db_path, repo) {
                    Ok(st) if st.generation > 0 => {
                        repository = json!({
                            "path": st.root,
                            "indexed": true,
                            "generation": st.generation,
                            "complete": st.complete,
                            "indexed_files": st.indexed_files,
                            "eligible_files": st.eligible_files,
                            "last_indexed": st.last_index_finished,
                        });
                        if !st.complete {
                            next.push("index is incomplete: run `midnight index .`".into());
                        }
                        if let Some(s) = &settings
                            && s.provider != Choice::Lexical
                            && let Ok(conn) = db::open_db(db_path)
                        {
                            let (model, revision) = match s.cloud_config() {
                                Ok(c) => {
                                    let id = c.identity();
                                    (id.name, Some(id.revision))
                                }
                                Err(_) => (embed::MODEL_NAME.to_owned(), None),
                            };
                            let scope = format!("{}:{}", st.repo_id, st.worktree_id);
                            let embedded: i64 = conn
                                .query_row(
                                    "SELECT COUNT(DISTINCT owner_id) FROM embeddings WHERE kind='code' AND scope=?1 AND model=?2 AND (?3 IS NULL OR revision=?3)",
                                    rusqlite::params![scope, model, revision],
                                    |r| r.get(0),
                                )
                                .unwrap_or(0);
                            repository["embedded_files"] = json!(embedded);
                            repository["embedding_model"] = json!(model);
                            if (embedded as u64) < st.indexed_files {
                                next.push(format!(
                                    "{embedded} of {} files have vectors: run `midnight index .` to embed the rest",
                                    st.indexed_files
                                ));
                            }
                        }
                    }
                    _ => next.push(format!(
                        "index this repository: `midnight index {}`",
                        repo.display()
                    )),
                }
            }
            Err(e) => {
                database["ok"] = json!(false);
                problems.push(format!(
                    "database cannot be opened: {e:#}; run `midnight doctor`"
                ));
            }
        }
    } else {
        next.push(format!(
            "index a repository: `midnight index .` (creates {})",
            db_path.display()
        ));
    }

    // Project storage and per-project settings (read-only; nothing is created).
    let project = match crate::connect::project_state(repo) {
        None => Value::Null,
        Some(state) => {
            let uses_project_db = db_path.starts_with(state.root.join(crate::connect::PROJECT_DIR));
            let mut p = json!({
                "root": state.root,
                "storage": if uses_project_db { "project" } else { "global" },
                "midnight_dir": state.has_dir,
                "trusted": state.trusted,
            });
            if state.has_dir && !state.trusted {
                next.push(
                    "this repository has a `.midnight/` you did not set up, so it is ignored: run `midnight init --storage project` to adopt it"
                        .into(),
                );
            }
            if state.trusted {
                match crate::project::load_for(repo) {
                    Ok(Some(cfg)) => {
                        p["config"] = json!(crate::project::path_for(&state.root));
                        p["embeddings"] = json!(!cfg.embeddings_disabled());
                        p["mcp_read_only"] = json!(cfg.mcp.read_only == Some(true));
                    }
                    Ok(None) => {}
                    Err(e) => problems.push(format!("{e:#}")),
                }
            }
            p
        }
    };

    Ok(json!({
        "ok": problems.is_empty(),
        "version": crate::VERSION,
        "config_path": config_path,
        "provider": provider,
        "project": project,
        "database": database,
        "repository": repository,
        "problems": problems,
        "next": next,
    }))
}

pub fn render_status(s: &Value) {
    let p = &s["provider"];
    println!("AETHER Midnight {}", s["version"].as_str().unwrap_or(""));
    if p["configured"] == true {
        println!(
            "Provider    {}{}",
            p["provider"].as_str().unwrap_or(""),
            p["model"]
                .as_str()
                .map(|m| format!(" · {m} · {}d", p["dimensions"]))
                .unwrap_or_default()
        );
        if let Some(c) = p["credential"].as_str() {
            println!("Credential  {c}");
        }
        if let Some(c) = p["upload_consent"].as_bool() {
            println!(
                "Uploads     {}",
                if c { "allowed" } else { "not authorised" }
            );
        }
    } else {
        println!("Provider    not configured (keyword search only)");
    }
    let pr = &s["project"];
    if pr.is_object() {
        let mut line = format!(
            "Project     {} · data in {}",
            pr["root"].as_str().unwrap_or(""),
            pr["storage"].as_str().unwrap_or("")
        );
        if pr["midnight_dir"] == true && pr["trusted"] != true {
            line.push_str(" · .midnight/ not adopted (ignored)");
        }
        if pr["embeddings"] == false {
            line.push_str(" · keyword search only (midnight.json)");
        }
        if pr["mcp_read_only"] == true {
            line.push_str(" · MCP read-only");
        }
        println!("{line}");
    }
    let d = &s["database"];
    if d["exists"] == true && d["ok"] == true {
        println!(
            "Database    {} · schema v{} · {} bytes · {} memories",
            d["path"].as_str().unwrap_or(""),
            d["schema_version"],
            d["size_bytes"],
            d["active_memories"]
        );
    } else {
        println!(
            "Database    {} (not created yet)",
            d["path"].as_str().unwrap_or("")
        );
    }
    let r = &s["repository"];
    if r["indexed"] == true {
        println!(
            "Repository  {} · {} files indexed · generation {} · {}",
            r["path"].as_str().unwrap_or(""),
            r["indexed_files"],
            r["generation"],
            if r["complete"] == true {
                "complete"
            } else {
                "incomplete"
            }
        );
        if let Some(n) = r["embedded_files"].as_i64() {
            println!(
                "Embeddings  {n}/{} files · {}",
                r["indexed_files"],
                r["embedding_model"].as_str().unwrap_or("")
            );
        }
    } else {
        println!(
            "Repository  {} (not indexed)",
            r["path"].as_str().unwrap_or("")
        );
    }
    for p in s["problems"].as_array().into_iter().flatten() {
        println!("\nproblem: {}", p.as_str().unwrap_or(""));
    }
    if let Some(next) = s["next"].as_array().filter(|n| !n.is_empty()) {
        println!("\nNext:");
        for n in next {
            println!("  {}", n.as_str().unwrap_or(""));
        }
    }
}
