//! Project-local storage and MCP client registration.
//!
//! Two independent features share this module because both are about "where does Midnight live
//! for this project": `.midnight/` project storage (opt-in, git-ignored) and writing the
//! `aether-midnight` server entry into an MCP client's configuration. Everything that decides
//! file contents is a pure function over strings so it can be tested without touching a real
//! client configuration.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Name of the server entry written into client configurations.
pub const SERVER_NAME: &str = "aether-midnight";
/// Directory (inside the project root) that holds project-scoped storage.
pub const PROJECT_DIR: &str = ".midnight";
const PROJECT_DB: &str = "midnight.sqlite3";
const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------------------------
// Project storage
// ---------------------------------------------------------------------------------------------

/// Nearest ancestor of `start` that contains `.git` (directory or worktree file); `start` itself
/// when there is none.
pub fn project_root(start: &Path) -> Result<PathBuf> {
    let start = start
        .canonicalize()
        .with_context(|| format!("resolve {}", start.display()))?;
    let mut current = Some(start.as_path());
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return Ok(dir.to_path_buf());
        }
        current = dir.parent();
    }
    Ok(start)
}

pub fn project_db_path(root: &Path) -> PathBuf {
    root.join(PROJECT_DIR).join(PROJECT_DB)
}

/// Project database found by walking up from `start`, if one exists.
///
/// A repository can ship a `.midnight` directory it did not create on this machine, and SQLite
/// files are not safe to open from strangers, so on Unix the database must be a regular file
/// owned by the current user. Anything else is ignored and the global database is used.
pub fn discover_project_db(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = project_db_path(dir);
        let Ok(meta) = fs::symlink_metadata(&candidate) else {
            continue;
        };
        if meta.is_file() && owned_by_current_user(&meta) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn owned_by_current_user(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    meta.uid() == unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn owned_by_current_user(_meta: &fs::Metadata) -> bool {
    true
}

/// Result of preparing project storage.
pub struct ProjectStorage {
    pub root: PathBuf,
    pub db_path: PathBuf,
    pub gitignore_path: PathBuf,
    pub gitignore_changed: bool,
}

/// Create `<root>/.midnight/` (private) and make sure `.gitignore` ignores it. Creates the
/// `.gitignore` when missing; never rewrites existing lines. The database itself is created by
/// the caller through `db::open_db`.
pub fn prepare_project_storage(start: &Path) -> Result<ProjectStorage> {
    let root = project_root(start)?;
    let dir = root.join(PROJECT_DIR);
    create_private_dir(&dir)?;
    let gitignore_path = root.join(".gitignore");
    let gitignore_changed = ensure_gitignore(&gitignore_path)?;
    Ok(ProjectStorage {
        db_path: project_db_path(&root),
        root,
        gitignore_path,
        gitignore_changed,
    })
}

fn create_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict {}", dir.display()))?;
    }
    Ok(())
}

/// Returns whether the file changed.
fn ensure_gitignore(path: &Path) -> Result<bool> {
    let existing = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {
            if meta.len() > MAX_CONFIG_BYTES {
                bail!("{} is too large to edit safely", path.display());
            }
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
        }
        Ok(_) => bail!(
            "{} is not a regular file; add `{PROJECT_DIR}/` to your ignore rules manually",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    let Some(updated) = gitignore_with_project_dir(&existing) else {
        return Ok(false);
    };
    atomic_write(path, &updated, false)?;
    Ok(true)
}

/// `None` when `existing` already ignores `.midnight`; otherwise the text with the rule appended.
fn gitignore_with_project_dir(existing: &str) -> Option<String> {
    let covered = existing.lines().any(|line| {
        matches!(
            line.trim(),
            ".midnight" | ".midnight/" | "/.midnight" | "/.midnight/"
        )
    });
    if covered {
        return None;
    }
    let mut text = existing.to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str("# AETHER Midnight local data\n.midnight/\n");
    Some(text)
}

// ---------------------------------------------------------------------------------------------
// MCP client registration
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Configuration that lives in the project and applies only there.
    Project,
    /// The user's own configuration, applied to every project.
    User,
}

/// Locations that clients keep their configuration in. Injected so tests never touch real ones.
pub struct Dirs {
    pub home: PathBuf,
    pub config: PathBuf,
    pub codex_home: Option<PathBuf>,
}

impl Dirs {
    pub fn from_env() -> Result<Self> {
        Ok(Dirs {
            home: dirs::home_dir().context("home directory is unavailable")?,
            config: dirs::config_dir().context("user config directory is unavailable")?,
            codex_home: std::env::var_os("CODEX_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        })
    }
}

pub fn config_path(
    client: Client,
    scope: Scope,
    project_root: &Path,
    dirs: &Dirs,
) -> Result<PathBuf> {
    Ok(match (client, scope) {
        (Client::ClaudeCode, Scope::Project) => project_root.join(".mcp.json"),
        (Client::ClaudeCode, Scope::User) => dirs.home.join(".claude.json"),
        (Client::ClaudeDesktop, Scope::User) => dirs
            .config
            .join("Claude")
            .join("claude_desktop_config.json"),
        (Client::ClaudeDesktop, Scope::Project) => {
            bail!("Claude Desktop has no per-project configuration; use --scope user")
        }
        (Client::Codex, Scope::Project) => project_root.join(".codex").join("config.toml"),
        (Client::Codex, Scope::User) => dirs
            .codex_home
            .clone()
            .unwrap_or_else(|| dirs.home.join(".codex"))
            .join("config.toml"),
    })
}

/// Arguments after the executable: `[--db PATH] mcp [--read-only]`.
pub fn server_args(db: Option<&Path>, read_only: bool) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(db) = db {
        args.push("--db".to_string());
        args.push(db.to_string_lossy().into_owned());
    }
    args.push("mcp".to_string());
    if read_only {
        args.push("--read-only".to_string());
    }
    args
}

fn is_toml(client: Client) -> bool {
    client == Client::Codex
}

/// New file contents with the server registered, or an error if `existing` cannot be edited
/// safely. Other entries are preserved.
pub fn render_install(
    client: Client,
    existing: &str,
    command: &str,
    args: &[String],
) -> Result<String> {
    if is_toml(client) {
        Ok(toml_upsert(existing, command, args))
    } else {
        json_upsert(existing, command, args)
    }
}

/// New file contents with the server entry removed (unchanged text when it is absent).
pub fn render_remove(client: Client, existing: &str) -> Result<String> {
    if is_toml(client) {
        Ok(toml_remove(existing))
    } else {
        json_remove(existing)
    }
}

fn parse_json_object(existing: &str) -> Result<Map<String, Value>> {
    if existing.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(existing)
        .context("existing configuration is not valid JSON; fix or move it first")?
    {
        Value::Object(map) => Ok(map),
        _ => bail!("existing configuration is not a JSON object"),
    }
}

fn json_upsert(existing: &str, command: &str, args: &[String]) -> Result<String> {
    let mut root = parse_json_object(existing)?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        bail!("`mcpServers` exists but is not an object");
    };
    servers.insert(
        SERVER_NAME.to_string(),
        json!({"command": command, "args": args}),
    );
    Ok(serde_json::to_string_pretty(&Value::Object(root))? + "\n")
}

fn json_remove(existing: &str) -> Result<String> {
    let mut root = parse_json_object(existing)?;
    let removed = match root.get_mut("mcpServers") {
        Some(Value::Object(servers)) => servers.remove(SERVER_NAME).is_some(),
        _ => false,
    };
    if !removed {
        return Ok(existing.to_string());
    }
    Ok(serde_json::to_string_pretty(&Value::Object(root))? + "\n")
}

fn toml_header(line: &str) -> Option<&str> {
    let t = line.trim();
    (t.starts_with('[') && t.ends_with(']') && !t.contains('=') && !t.contains(',')).then_some(t)
}

fn is_server_header(header: &str) -> bool {
    let own = format!("[mcp_servers.{SERVER_NAME}]");
    let sub = format!("[mcp_servers.{SERVER_NAME}.");
    header == own || header.starts_with(&sub)
}

/// Remove our table (and its sub-tables) from a TOML document, line-wise so that comments and
/// every other table survive byte-for-byte.
fn toml_strip(existing: &str) -> String {
    let mut out = String::with_capacity(existing.len());
    let mut skipping = false;
    for line in existing.split_inclusive('\n') {
        if let Some(header) = toml_header(line) {
            skipping = is_server_header(header);
        }
        if !skipping {
            out.push_str(line);
        }
    }
    out
}

fn toml_remove(existing: &str) -> String {
    let stripped = toml_strip(existing);
    if stripped == existing {
        return stripped;
    }
    stripped.trim_end().to_string() + if stripped.trim().is_empty() { "" } else { "\n" }
}

fn toml_string(value: &str) -> String {
    // A JSON string literal is a valid TOML basic string.
    serde_json::to_string(value).expect("string serialization cannot fail")
}

fn toml_upsert(existing: &str, command: &str, args: &[String]) -> String {
    let mut text = toml_strip(existing);
    let text_trimmed = text.trim_end().len();
    text.truncate(text_trimmed);
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    let args = args
        .iter()
        .map(|a| toml_string(a))
        .collect::<Vec<_>>()
        .join(", ");
    text.push_str(&format!(
        "[mcp_servers.{SERVER_NAME}]\ncommand = {}\nargs = [{args}]\n",
        toml_string(command)
    ));
    text
}

/// What `install` / `uninstall` did.
pub struct Outcome {
    pub path: PathBuf,
    pub changed: bool,
    pub backup: Option<PathBuf>,
    /// The text that was (or, for a dry run, would be) written.
    pub contents: String,
}

fn read_existing(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {
            if meta.len() > MAX_CONFIG_BYTES {
                bail!("{} is too large to edit safely", path.display());
            }
            Ok(Some(
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
            ))
        }
        Ok(_) => bail!("{} is not a regular file", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn commit(
    path: &Path,
    existing: Option<String>,
    updated: String,
    dry_run: bool,
) -> Result<Outcome> {
    let changed = existing.as_deref() != Some(updated.as_str());
    let mut backup = None;
    if changed && !dry_run {
        if let Some(previous) = &existing {
            let bak = PathBuf::from(format!("{}.midnight.bak", path.display()));
            atomic_write(&bak, previous, true)?;
            backup = Some(bak);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        atomic_write(path, &updated, false)?;
    }
    Ok(Outcome {
        path: path.to_path_buf(),
        changed,
        backup,
        contents: updated,
    })
}

pub fn install(
    path: &Path,
    client: Client,
    command: &str,
    args: &[String],
    dry_run: bool,
) -> Result<Outcome> {
    let existing = read_existing(path)?;
    let updated = render_install(client, existing.as_deref().unwrap_or(""), command, args)?;
    commit(path, existing, updated, dry_run)
}

pub fn uninstall(path: &Path, client: Client, dry_run: bool) -> Result<Outcome> {
    let Some(existing) = read_existing(path)? else {
        return Ok(Outcome {
            path: path.to_path_buf(),
            changed: false,
            backup: None,
            contents: String::new(),
        });
    };
    let updated = render_remove(client, &existing)?;
    commit(path, Some(existing), updated, dry_run)
}

/// Write via a sibling temporary file and rename, preserving the mode of an existing file.
/// `private` forces 0600 (used for backups, which may hold other servers' credentials).
fn atomic_write(path: &Path, contents: &str, private: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary file in {}", parent.display()))?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if private {
            0o600
        } else {
            fs::metadata(path)
                .map(|m| m.permissions().mode() & 0o777)
                .unwrap_or(0o644)
        };
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = private;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Vec<String> {
        server_args(None, false)
    }

    #[test]
    fn gitignore_is_created_appended_and_idempotent() {
        let fresh = gitignore_with_project_dir("").unwrap();
        assert_eq!(fresh, "# AETHER Midnight local data\n.midnight/\n");
        let appended = gitignore_with_project_dir("target").unwrap();
        assert_eq!(
            appended,
            "target\n\n# AETHER Midnight local data\n.midnight/\n"
        );
        assert!(gitignore_with_project_dir(&appended).is_none());
        for covered in [".midnight", "/.midnight/", "  .midnight/  \n"] {
            assert!(gitignore_with_project_dir(covered).is_none(), "{covered:?}");
        }
        assert!(gitignore_with_project_dir(".midnight-old/\n").is_some());
    }

    #[test]
    fn json_install_preserves_other_servers_and_keys() {
        let existing = r#"{"theme":"dark","mcpServers":{"other":{"command":"x","args":[]}}}"#;
        let out = render_install(Client::ClaudeCode, existing, "/bin/am", &args()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"][SERVER_NAME]["command"], "/bin/am");
        assert_eq!(v["mcpServers"][SERVER_NAME]["args"], json!(["mcp"]));
        // Idempotent.
        assert_eq!(
            render_install(Client::ClaudeCode, &out, "/bin/am", &args()).unwrap(),
            out
        );
    }

    #[test]
    fn json_install_refuses_unsafe_input_and_remove_restores() {
        assert!(render_install(Client::ClaudeCode, "{not json", "c", &args()).is_err());
        assert!(render_install(Client::ClaudeCode, "[]", "c", &args()).is_err());
        assert!(render_install(Client::ClaudeCode, r#"{"mcpServers":[]}"#, "c", &args()).is_err());
        let out = render_install(Client::ClaudeCode, "", "c", &args()).unwrap();
        let removed = render_remove(Client::ClaudeCode, &out).unwrap();
        let v: Value = serde_json::from_str(&removed).unwrap();
        assert!(v["mcpServers"].as_object().unwrap().is_empty());
        // Removing when absent leaves the text untouched.
        assert_eq!(
            render_remove(Client::ClaudeCode, "{\"a\":1}").unwrap(),
            "{\"a\":1}"
        );
    }

    #[test]
    fn toml_install_replaces_only_our_table() {
        let existing = "model = \"x\"\n\n[mcp_servers.other]\ncommand = \"o\"\n\n[mcp_servers.aether-midnight]\ncommand = \"old\"\nargs = [\"mcp\"]\n\n[mcp_servers.aether-midnight.env]\nK = \"v\"\n\n[tools]\nweb = true\n";
        let out = render_install(
            Client::Codex,
            existing,
            "/a b/\"am\"",
            &server_args(Some(Path::new("/d/db.sqlite3")), true),
        )
        .unwrap();
        assert!(out.contains("model = \"x\""));
        assert!(out.contains("[mcp_servers.other]\ncommand = \"o\""));
        assert!(out.contains("[tools]\nweb = true"));
        assert!(!out.contains("old"));
        assert!(!out.contains(".env]"));
        assert!(out.contains(
            "command = \"/a b/\\\"am\\\"\"\nargs = [\"--db\", \"/d/db.sqlite3\", \"mcp\", \"--read-only\"]"
        ));
        assert_eq!(out.matches("[mcp_servers.aether-midnight]").count(), 1);
        // Idempotent, and removal leaves everything else intact.
        let again = render_install(
            Client::Codex,
            &out,
            "/a b/\"am\"",
            &server_args(Some(Path::new("/d/db.sqlite3")), true),
        )
        .unwrap();
        assert_eq!(again, out);
        let removed = render_remove(Client::Codex, &out).unwrap();
        assert!(!removed.contains("aether-midnight"));
        assert!(removed.contains("[tools]"));
    }

    #[test]
    fn toml_install_on_empty_and_remove_to_empty() {
        let out = render_install(Client::Codex, "", "/bin/am", &args()).unwrap();
        assert_eq!(
            out,
            "[mcp_servers.aether-midnight]\ncommand = \"/bin/am\"\nargs = [\"mcp\"]\n"
        );
        assert_eq!(render_remove(Client::Codex, &out).unwrap(), "");
    }

    #[test]
    fn config_paths_follow_each_client_convention() {
        let dirs = Dirs {
            home: "/h".into(),
            config: "/c".into(),
            codex_home: None,
        };
        let root = Path::new("/p");
        let p = |c, s| config_path(c, s, root, &dirs).unwrap();
        assert_eq!(
            p(Client::ClaudeCode, Scope::Project),
            Path::new("/p/.mcp.json")
        );
        assert_eq!(
            p(Client::ClaudeCode, Scope::User),
            Path::new("/h/.claude.json")
        );
        assert_eq!(
            p(Client::Codex, Scope::User),
            Path::new("/h/.codex/config.toml")
        );
        assert_eq!(
            p(Client::Codex, Scope::Project),
            Path::new("/p/.codex/config.toml")
        );
        assert_eq!(
            p(Client::ClaudeDesktop, Scope::User),
            Path::new("/c/Claude/claude_desktop_config.json")
        );
        assert!(config_path(Client::ClaudeDesktop, Scope::Project, root, &dirs).is_err());
        let dirs = Dirs {
            codex_home: Some("/cx".into()),
            ..dirs
        };
        assert_eq!(p_with(&dirs), Path::new("/cx/config.toml"));
    }

    fn p_with(dirs: &Dirs) -> PathBuf {
        config_path(Client::Codex, Scope::User, Path::new("/p"), dirs).unwrap()
    }

    #[test]
    fn install_writes_backup_and_dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/.mcp.json");
        let dry = install(&path, Client::ClaudeCode, "/bin/am", &args(), true).unwrap();
        assert!(dry.changed && !path.exists());

        let first = install(&path, Client::ClaudeCode, "/bin/am", &args(), false).unwrap();
        assert!(first.changed && first.backup.is_none() && path.exists());
        let same = install(&path, Client::ClaudeCode, "/bin/am", &args(), false).unwrap();
        assert!(!same.changed);

        let updated = install(&path, Client::ClaudeCode, "/bin/new", &args(), false).unwrap();
        let backup = updated.backup.expect("backup of previous contents");
        assert!(fs::read_to_string(backup).unwrap().contains("/bin/am"));
        assert!(fs::read_to_string(&path).unwrap().contains("/bin/new"));

        let gone = uninstall(&path, Client::ClaudeCode, false).unwrap();
        assert!(gone.changed);
        assert!(!fs::read_to_string(&path).unwrap().contains(SERVER_NAME));
    }

    #[test]
    fn project_storage_is_prepared_and_discovered() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("src/deep")).unwrap();
        fs::write(repo.join(".gitignore"), "target").unwrap();

        let storage = prepare_project_storage(&repo.join("src/deep")).unwrap();
        assert_eq!(storage.root, repo);
        assert!(storage.gitignore_changed);
        assert!(
            fs::read_to_string(repo.join(".gitignore"))
                .unwrap()
                .ends_with(".midnight/\n")
        );
        assert!(!prepare_project_storage(&repo).unwrap().gitignore_changed);

        assert!(
            discover_project_db(&repo.join("src/deep")).is_none(),
            "no db file yet"
        );
        fs::write(&storage.db_path, b"").unwrap();
        assert_eq!(
            discover_project_db(&repo.join("src/deep")),
            Some(storage.db_path)
        );
    }
}
