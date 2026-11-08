//! `midnight uninstall`: remove the installed program, and only what the user asks beyond it.
//!
//! By default only the binary and the `midnight` / `aether-midnight` links that point at it are
//! removed. Configuration (`config.json`, `credentials.json`, `trusted_projects.json`) and global
//! data (database, token registry, models, ONNX Runtime) need `--purge-config` / `--purge-data`.
//! Files are removed by exact name, never by deleting a whole directory that may be shared (on
//! macOS the config and data directories are the same folder). A project's `.midnight/`
//! directory is never touched. Entries registered in clients are reported, and removed only
//! with `--remove-client-entries`.

use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::connect::{self, Client, Scope};

#[derive(Clone, Copy, Default)]
pub struct Options {
    pub purge_config: bool,
    pub purge_data: bool,
    pub remove_client_entries: bool,
}

#[derive(Debug, Serialize)]
pub struct ClientEntry {
    pub client: &'static str,
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    /// The binary and the links to it.
    pub program: Vec<PathBuf>,
    /// User-scope client configs that still register Midnight.
    pub client_entries: Vec<ClientEntry>,
    /// Whether those entries will be removed.
    pub remove_client_entries: bool,
    /// Files and directories deleted because of `--purge-*`.
    pub purge: Vec<PathBuf>,
    /// What stays on disk, with a label.
    pub remaining: Vec<(String, PathBuf)>,
}

const CONFIG_FILES: [&str; 3] = ["config.json", "credentials.json", "trusted_projects.json"];

fn existing(path: PathBuf) -> Option<PathBuf> {
    fs::symlink_metadata(&path).ok().map(|_| path)
}

/// Links named `midnight` / `aether-midnight` beside `binary` that resolve to it.
fn links_to(binary: &Path) -> Vec<PathBuf> {
    let Some(dir) = binary.parent() else {
        return Vec::new();
    };
    ["midnight", "aether-midnight"]
        .iter()
        .map(|name| dir.join(name))
        .filter(|p| p != binary)
        .filter(|p| {
            fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
                && p.canonicalize().ok().as_deref() == Some(binary)
        })
        .collect()
}

fn db_files(db: &Path) -> Vec<PathBuf> {
    let mut out = vec![db.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        let mut name = db.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(suffix);
        out.push(db.with_file_name(name));
    }
    out.push(crate::auth::registry_path(db));
    out
}

pub fn plan(binary: &Path, options: Options) -> Result<Plan> {
    let mut program = vec![binary.to_path_buf()];
    program.extend(links_to(binary));

    let mut client_entries = Vec::new();
    if let Ok(dirs) = connect::Dirs::from_env() {
        for (client, label) in [
            (Client::ClaudeCode, "claude-code"),
            (Client::ClaudeDesktop, "claude-desktop"),
            (Client::Codex, "codex"),
        ] {
            let Ok(path) = connect::config_path(client, Scope::User, Path::new("."), &dirs) else {
                continue;
            };
            if connect::uninstall(&path, client, true, None).is_ok_and(|o| o.changed) {
                client_entries.push(ClientEntry {
                    client: label,
                    path,
                });
            }
        }
    }

    let config_dir = crate::config::config_dir()?;
    let config_items: Vec<PathBuf> = CONFIG_FILES
        .iter()
        .filter_map(|f| existing(config_dir.join(f)))
        .collect();
    let db = crate::db::default_db_path()?;
    let mut data_items: Vec<PathBuf> = db_files(&db).into_iter().filter_map(existing).collect();
    if let Ok(models) = crate::embed::models_dir()
        && let Some(models) = existing(models)
    {
        data_items.push(models);
    }

    let mut purge = Vec::new();
    let mut remaining = Vec::new();
    for (items, label, wanted) in [
        (
            config_items,
            "configuration and credentials",
            options.purge_config,
        ),
        (
            data_items,
            "global data (database, models)",
            options.purge_data,
        ),
    ] {
        for item in items {
            if wanted {
                purge.push(item);
            } else {
                remaining.push((label.to_string(), item));
            }
        }
    }
    for project in connect::list_trusted() {
        let dir = project.join(connect::PROJECT_DIR);
        if dir.is_dir() {
            remaining.push(("project data (never touched)".to_string(), dir));
        }
    }

    Ok(Plan {
        program,
        remove_client_entries: options.remove_client_entries,
        client_entries,
        purge,
        remaining,
    })
}

#[derive(Debug, Serialize)]
pub struct Done {
    pub removed: Vec<PathBuf>,
    pub client_entries_removed: Vec<PathBuf>,
}

fn remove_any(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("remove {}", path.display()))
}

/// Order matters: data first, client entries next, the program last (so a failure part-way leaves
/// a working binary to retry with).
pub fn execute(plan: &Plan) -> Result<Done> {
    let mut removed = Vec::new();
    for path in &plan.purge {
        remove_any(path)?;
        removed.push(path.clone());
        // Tidy an emptied Midnight directory; a shared or non-empty one is left alone.
        if let Some(parent) = path.parent()
            && parent.file_name().is_some_and(|n| n == "aether-midnight")
        {
            let _ = fs::remove_dir(parent);
        }
    }
    let mut client_entries_removed = Vec::new();
    if plan.remove_client_entries {
        for entry in &plan.client_entries {
            let client = match entry.client {
                "claude-code" => Client::ClaudeCode,
                "claude-desktop" => Client::ClaudeDesktop,
                _ => Client::Codex,
            };
            connect::uninstall(&entry.path, client, false, None)?;
            client_entries_removed.push(entry.path.clone());
        }
    }
    // Links before the binary they point at.
    for path in plan.program.iter().skip(1).chain(plan.program.first()) {
        remove_any(path)?;
        removed.push(path.clone());
    }
    Ok(Done {
        removed,
        client_entries_removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn only_links_that_resolve_to_the_binary_are_included() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let bin = base.join("aether-midnight");
        fs::write(&bin, b"x").unwrap();
        std::os::unix::fs::symlink("aether-midnight", base.join("midnight")).unwrap();
        assert_eq!(links_to(&bin), vec![base.join("midnight")]);

        // A foreign `midnight` (regular file or link elsewhere) is left alone.
        fs::remove_file(base.join("midnight")).unwrap();
        fs::write(base.join("midnight"), b"other").unwrap();
        assert!(links_to(&bin).is_empty());
        fs::remove_file(base.join("midnight")).unwrap();
        let other = base.join("other");
        fs::write(&other, b"o").unwrap();
        std::os::unix::fs::symlink(&other, base.join("midnight")).unwrap();
        assert!(links_to(&bin).is_empty());
    }

    #[test]
    fn database_sidecars_and_token_registry_are_named_exactly() {
        let names: Vec<String> = db_files(Path::new("/d/midnight.sqlite3"))
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "midnight.sqlite3",
                "midnight.sqlite3-wal",
                "midnight.sqlite3-shm",
                "midnight.sqlite3.tokens.json"
            ]
        );
    }
}
