//! Bearer-token registry for the HTTP MCP transport.
//!
//! Loopback reachability is not authority: any local process can connect to a
//! loopback port. Every HTTP request must carry a token minted with
//! `aether-midnight token create`. The registry stores only SHA-256 digests,
//! each token carries its own immutable [`Policy`] (tools, memory scopes,
//! repository roots) and may be revoked or expire. The server re-checks the
//! registry on every request, so revocation takes effect immediately.
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::mcp::Policy;

pub const TOKEN_PREFIX: &str = "mdn_";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenRecord {
    pub id: String,
    pub name: String,
    /// Lowercase hex SHA-256 of the token. The token itself is never stored.
    pub sha256: String,
    pub policy: Policy,
    pub created_at: u64,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    tokens: Vec<TokenRecord>,
}

pub fn registry_path(db_path: &Path) -> PathBuf {
    let mut name = db_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tokens.json");
    db_path.with_file_name(name)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn digest(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .context("read OS randomness from /dev/urandom")?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Constant-time equality for equal-length hex digests.
fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn with_lock<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_name)
        .context("open token registry lock")?;
    lock.lock_exclusive()?;
    let result = f();
    let _ = lock.unlock();
    result
}

fn load(path: &Path) -> Result<Registry> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("token registry is corrupt"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(e) => Err(e).context("read token registry"),
    }
}

fn save(path: &Path, registry: &Registry) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).context("write token registry")?;
    file.write_all(&serde_json::to_vec_pretty(registry)?)?;
    file.sync_all()?;
    fs::rename(&tmp, path).context("replace token registry")?;
    Ok(())
}

/// Mints a token. The plaintext is returned once and cannot be recovered.
pub fn create(
    db_path: &Path,
    name: &str,
    policy: Policy,
    ttl_secs: Option<u64>,
) -> Result<(TokenRecord, String)> {
    if name.trim().is_empty() || name.len() > 120 {
        bail!("token name must be 1-120 characters");
    }
    let path = registry_path(db_path);
    let token = format!("{TOKEN_PREFIX}{}", random_hex(32)?);
    let record = TokenRecord {
        id: format!("tok_{}", random_hex(6)?),
        name: name.trim().to_owned(),
        sha256: digest(&token),
        policy,
        created_at: now(),
        expires_at: ttl_secs.map(|t| now().saturating_add(t)),
        revoked: false,
    };
    with_lock(&path, || {
        let mut registry = load(&path)?;
        registry.tokens.push(record.clone());
        save(&path, &registry)
    })?;
    Ok((record, token))
}

pub fn revoke(db_path: &Path, id_or_name: &str) -> Result<usize> {
    let path = registry_path(db_path);
    with_lock(&path, || {
        let mut registry = load(&path)?;
        let mut count = 0;
        for t in &mut registry.tokens {
            if !t.revoked && (t.id == id_or_name || t.name == id_or_name) {
                t.revoked = true;
                count += 1;
            }
        }
        if count == 0 {
            bail!("no active token matches {id_or_name:?}");
        }
        save(&path, &registry)?;
        Ok(count)
    })
}

pub fn list(db_path: &Path) -> Result<Vec<TokenRecord>> {
    Ok(load(&registry_path(db_path))?.tokens)
}

type Stamp = (SystemTime, u64, u64);

/// Cached view of the registry; reloads when the file changes on disk.
pub struct Authenticator {
    path: PathBuf,
    cache: Mutex<(Option<Stamp>, Vec<TokenRecord>)>,
}

impl Authenticator {
    pub fn new(db_path: &Path) -> Self {
        Self {
            path: registry_path(db_path),
            cache: Mutex::new((None, Vec::new())),
        }
    }

    /// Resolves a bearer token to its record. `None` covers unknown, revoked,
    /// expired, and unreadable-registry cases alike (fail closed).
    pub fn authenticate(&self, token: &str) -> Option<TokenRecord> {
        if !token.starts_with(TOKEN_PREFIX) || token.len() > 256 {
            return None;
        }
        let meta = fs::metadata(&self.path).ok()?;
        let stamp = (meta.modified().ok()?, meta.len(), meta.ino());
        let mut cache = self.cache.lock().ok()?;
        if cache.0 != Some(stamp) {
            match load(&self.path) {
                Ok(r) => *cache = (Some(stamp), r.tokens),
                Err(_) => {
                    cache.0 = None;
                    cache.1.clear();
                    return None;
                }
            }
        }
        let wanted = digest(token);
        let mut found = None;
        for record in &cache.1 {
            if ct_eq(&record.sha256, &wanted) {
                found = Some(record.clone());
            }
        }
        let record = found?;
        if record.revoked || record.expires_at.is_some_and(|e| e <= now()) {
            return None;
        }
        Some(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_hashed_revocable_and_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        let policy = Policy {
            memory_scopes: Some(vec!["dot:a".into()]),
            ..Policy::default()
        };
        let (record, token) = create(&db, "a", policy, None).unwrap();
        let on_disk = fs::read_to_string(registry_path(&db)).unwrap();
        assert!(
            !on_disk.contains(&token),
            "plaintext token must not be stored"
        );
        let auth = Authenticator::new(&db);
        assert_eq!(auth.authenticate(&token).unwrap().id, record.id);
        assert!(auth.authenticate("mdn_deadbeef").is_none());
        assert!(auth.authenticate("").is_none());
        revoke(&db, &record.id).unwrap();
        assert!(
            auth.authenticate(&token).is_none(),
            "revocation is immediate"
        );
    }

    #[test]
    fn expired_tokens_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        let (_, token) = create(&db, "short", Policy::default(), Some(0)).unwrap();
        assert!(Authenticator::new(&db).authenticate(&token).is_none());
    }
}
