//! Native file watching for the code index (design: `docs/design/v0.2-watcher.md`).
//!
//! OS events only (FSEvents, inotify, ReadDirectoryChangesW through `notify`); `PollWatcher` is
//! never constructed. The debounce thread blocks on its channel while nothing is pending, so an
//! idle watcher wakes up never. Per-path events coalesce; storms (checkout, rebase, queue
//! overflow, watcher rescan notices, ignore-rule edits, removed directories) collapse into one
//! incremental reconciliation instead of per-file work.
//!
//! The watcher may keep the catalog complete (`code::refresh_watched_file`) because it
//! reconciles whenever it could have missed a change: at start, on overflow, on branch change and
//! on backend errors. Freshness labels stay truthful regardless: searches compare file hashes at
//! read time, so a result is `stale` between an edit and its refresh.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecursiveMode, Watcher};
use serde::Serialize;

use crate::code;

/// A path must be quiet this long before it is refreshed.
pub const QUIET: Duration = Duration::from_millis(40);
/// A continuously changing path is refreshed at least this often.
pub const MAX_WAIT: Duration = Duration::from_millis(200);
/// A storm must be quiet this long before the single reconciliation starts.
pub const STORM_QUIET: Duration = Duration::from_millis(300);
/// A storm that never goes quiet reconciles after this long.
pub const STORM_MAX_WAIT: Duration = Duration::from_secs(2);
/// More distinct pending paths than this turn into a reconciliation.
pub const MAX_PENDING_PATHS: usize = 512;
/// Bound of the event channel between the OS callback and the debounce thread.
pub const EVENT_QUEUE: usize = 4096;
/// Files enumerated when a new directory appears before falling back to a reconciliation.
const MAX_DIR_SCAN: usize = 512;
/// Directories above which Linux registration is refused when `max_user_watches` is unknown.
const FALLBACK_WATCH_LIMIT: usize = 8192;

// ---------- pure debounce state ----------

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Refresh(Vec<String>),
    Reconcile(String),
}

#[derive(Debug)]
pub struct Debouncer {
    pending: BTreeMap<String, (Instant, Instant)>, // path -> (first seen, last seen)
    storm: Option<(Instant, Instant, String)>,     // (first, last, reason)
    max_pending: usize,
}

impl Debouncer {
    pub fn new(max_pending: usize) -> Self {
        Self {
            pending: BTreeMap::new(),
            storm: None,
            max_pending,
        }
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn push_path(&mut self, path: String, now: Instant) {
        if self.storm.is_some() {
            // The coming reconciliation covers it; keep the storm window moving.
            self.push_storm("storm", now);
            return;
        }
        self.pending
            .entry(path)
            .and_modify(|(_, last)| *last = now)
            .or_insert((now, now));
        if self.pending.len() > self.max_pending {
            self.push_storm("too many pending paths", now);
        }
    }

    pub fn push_storm(&mut self, reason: &str, now: Instant) {
        self.pending.clear();
        match &mut self.storm {
            Some((_, last, _)) => *last = now,
            None => self.storm = Some((now, now, reason.to_string())),
        }
    }

    /// The instant at which `poll` next has something to do; `None` means block indefinitely.
    pub fn deadline(&self) -> Option<Instant> {
        if let Some((first, last, _)) = &self.storm {
            return Some((*last + STORM_QUIET).min(*first + STORM_MAX_WAIT));
        }
        self.pending
            .values()
            .map(|(first, last)| (*last + QUIET).min(*first + MAX_WAIT))
            .min()
    }

    pub fn poll(&mut self, now: Instant) -> Option<Action> {
        if let Some((first, last, reason)) = &self.storm {
            if now >= *last + STORM_QUIET || now >= *first + STORM_MAX_WAIT {
                let reason = reason.clone();
                self.storm = None;
                return Some(Action::Reconcile(reason));
            }
            return None;
        }
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, (first, last))| now >= *last + QUIET || now >= *first + MAX_WAIT)
            .map(|(path, _)| path.clone())
            .collect();
        if due.is_empty() {
            return None;
        }
        for path in &due {
            self.pending.remove(path);
        }
        Some(Action::Refresh(due))
    }
}

// ---------- path classification ----------

#[derive(Debug, PartialEq, Eq)]
pub enum PathClass {
    Ignored,
    Source(String),
    /// `.git/HEAD`, `.git/index`, `packed-refs` or `refs/heads/**` changed.
    BranchChange,
    /// A `.gitignore` or `.aetherignore` changed: eligibility of many files may have changed.
    IgnoreRules,
}

fn git_internal(parts: &[&str]) -> PathClass {
    match parts {
        ["HEAD"] | ["index"] | ["packed-refs"] => PathClass::BranchChange,
        ["refs", "heads", ..] => PathClass::BranchChange,
        _ => PathClass::Ignored,
    }
}

/// Classify a path relative to the repository root.
pub fn classify(rel: &Path) -> PathClass {
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        return PathClass::Ignored;
    }
    if parts[0] == ".git" {
        let rest: Vec<&str> = parts[1..].iter().map(String::as_str).collect();
        return git_internal(&rest);
    }
    let name = parts.last().map(String::as_str).unwrap_or("");
    if name == ".gitignore" || name == ".aetherignore" {
        return PathClass::IgnoreRules;
    }
    if code::is_secret_path(rel)
        || code::is_vcs_metadata_path(rel)
        || code::default_generated_path(rel)
    {
        return PathClass::Ignored;
    }
    PathClass::Source(parts.join("/"))
}

fn build_matcher(root: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    for file in [".gitignore", ".aetherignore", ".git/info/exclude"] {
        let path = root.join(file);
        if path.is_file() {
            let _ = builder.add(path);
        }
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// Directory holding `HEAD`/`index` for this worktree when it lives outside the root (linked
/// worktrees and submodules have a `.git` *file* containing `gitdir: <path>`).
fn external_git_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return None;
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let path = Path::new(target);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    path.canonicalize().ok()
}

// ---------- status ----------

#[derive(Debug, Clone, Serialize)]
pub struct WatchStatus {
    /// `off`, `catching_up`, `active`, `degraded` or `unavailable`.
    pub state: String,
    pub reason: Option<String>,
    pub pending: usize,
    pub watches: usize,
    pub refresh_count: u64,
    pub reconcile_count: u64,
    pub overflow_count: u64,
    pub error_count: u64,
    pub last_error: Option<String>,
}

impl WatchStatus {
    fn new(state: &str) -> Self {
        Self {
            state: state.into(),
            reason: None,
            pending: 0,
            watches: 0,
            refresh_count: 0,
            reconcile_count: 0,
            overflow_count: 0,
            error_count: 0,
            last_error: None,
        }
    }
}

type SharedStatus = Arc<Mutex<WatchStatus>>;

fn set_status(status: &SharedStatus, f: impl FnOnce(&mut WatchStatus)) {
    if let Ok(mut guard) = status.lock() {
        f(&mut guard);
    }
}

// ---------- sink ----------

/// Where debounced work goes. Implementations serialize their own writes.
pub trait WatchSink: Send + 'static {
    fn refresh(&self, rel_path: &str) -> Result<()>;
    fn reconcile(&self, reason: &str) -> Result<()>;
}

/// The production sink: single-file refresh and incremental reconciliation on one database.
pub struct IndexSink {
    db_path: PathBuf,
    root: PathBuf,
    cancel: Arc<AtomicBool>,
}

impl IndexSink {
    pub fn new(db_path: &Path, root: &Path) -> Self {
        Self {
            db_path: db_path.to_path_buf(),
            root: root.to_path_buf(),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl WatchSink for IndexSink {
    fn refresh(&self, rel_path: &str) -> Result<()> {
        let _gate = crate::daemon::gate_active().then(|| crate::daemon::write_gate().acquire());
        code::refresh_watched_file(&self.db_path, &self.root, rel_path, &self.cancel).map(|_| ())
    }
    fn reconcile(&self, _reason: &str) -> Result<()> {
        let _gate = crate::daemon::gate_active().then(|| crate::daemon::write_gate().acquire());
        code::index_repository(&self.db_path, &self.root, false, false, &self.cancel).map(|_| ())
    }
}

// ---------- the watcher ----------

enum Msg {
    Event(notify::Result<notify::Event>),
    Stop,
}

pub struct RepoWatcher {
    tx: Option<SyncSender<Msg>>,
    thread: Option<JoinHandle<()>>,
    status: SharedStatus,
}

impl Drop for RepoWatcher {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Stop);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl RepoWatcher {
    pub fn status(&self) -> WatchStatus {
        self.status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| WatchStatus::new("unavailable"))
    }

    /// Start watching `root`. Never fails: when the OS backend cannot be used the returned
    /// watcher reports `degraded`/`unavailable` and the reason, and does nothing else (queries
    /// still compare hashes, so nothing is silently stale).
    pub fn start(root: &Path, sink: impl WatchSink, catch_up: bool) -> RepoWatcher {
        let status: SharedStatus = Arc::new(Mutex::new(WatchStatus::new("catching_up")));
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let overflow = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel::<Msg>(EVENT_QUEUE);
        let handler_tx = tx.clone();
        let handler_overflow = overflow.clone();
        let handler = move |res: notify::Result<notify::Event>| {
            if let Err(TrySendError::Full(_)) = handler_tx.try_send(Msg::Event(res)) {
                handler_overflow.store(true, Ordering::Relaxed);
            }
        };
        let mut backend = match notify::recommended_watcher(handler) {
            Ok(w) => w,
            Err(error) => {
                set_status(&status, |s| {
                    s.state = "unavailable".into();
                    s.reason = Some(format!("file watching backend failed to start: {error}"));
                });
                return RepoWatcher {
                    tx: None,
                    thread: None,
                    status,
                };
            }
        };
        match register(&mut backend, &root) {
            Ok(count) => set_status(&status, |s| s.watches = count),
            Err(reason) => {
                set_status(&status, |s| {
                    s.state = "degraded".into();
                    s.reason = Some(reason);
                });
                return RepoWatcher {
                    tx: None,
                    thread: None,
                    status,
                };
            }
        }
        let thread_status = status.clone();
        let thread_root = root.clone();
        let thread = thread::Builder::new()
            .name("midnight-watch".into())
            .spawn(move || {
                run(
                    thread_root,
                    backend,
                    sink,
                    rx,
                    overflow,
                    thread_status,
                    catch_up,
                )
            })
            .ok();
        if thread.is_none() {
            set_status(&status, |s| {
                s.state = "unavailable".into();
                s.reason = Some("could not start the watcher thread".into());
            });
        }
        RepoWatcher {
            tx: Some(tx),
            thread,
            status,
        }
    }
}

/// Register the OS watches. Returns the number of watches, or the reason registration was refused.
fn register(
    backend: &mut notify::RecommendedWatcher,
    root: &Path,
) -> std::result::Result<usize, String> {
    let mut count = 0usize;
    if cfg!(target_os = "linux") {
        // inotify needs one watch per directory; count ignore-aware first and refuse to
        // register half a tree.
        let dirs = directories(root);
        let limit = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
            .ok()
            .and_then(|t| t.trim().parse::<usize>().ok())
            .unwrap_or(FALLBACK_WATCH_LIMIT);
        if dirs.len() + 64 > limit {
            return Err(format!(
                "{} directories need inotify watches but fs.inotify.max_user_watches is {limit}; raise it (sysctl) or use .aetherignore. Searches still compare file hashes, so results are labeled stale rather than wrong",
                dirs.len()
            ));
        }
        for dir in &dirs {
            backend
                .watch(dir, RecursiveMode::NonRecursive)
                .map_err(|e| format!("inotify registration failed after {count} watches: {e}"))?;
            count += 1;
        }
    } else {
        backend
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| format!("watch registration failed: {e}"))?;
        count += 1;
    }
    if let Some(git_dir) = external_git_dir(root) {
        backend
            .watch(&git_dir, RecursiveMode::NonRecursive)
            .map_err(|e| format!("git directory watch failed: {e}"))?;
        count += 1;
    } else if cfg!(target_os = "linux") {
        let git = root.join(".git");
        if git.is_dir() {
            backend
                .watch(&git, RecursiveMode::NonRecursive)
                .map_err(|e| format!("git directory watch failed: {e}"))?;
            count += 1;
        }
    }
    Ok(count)
}

/// Ignore-aware directory list (root included), never descending into `.git`.
fn directories(root: &Path) -> Vec<PathBuf> {
    let walker = ignore::WalkBuilder::new(root)
        .follow_links(false)
        .hidden(false)
        .add_custom_ignore_filename(".aetherignore")
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    walker
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_dir()))
        .map(|e| e.into_path())
        .collect()
}

fn run(
    root: PathBuf,
    mut backend: notify::RecommendedWatcher,
    sink: impl WatchSink,
    rx: mpsc::Receiver<Msg>,
    overflow: Arc<AtomicBool>,
    status: SharedStatus,
    catch_up: bool,
) {
    let git_dir = external_git_dir(&root);
    let mut matcher = build_matcher(&root);
    let mut deb = Debouncer::new(MAX_PENDING_PATHS);
    let mut retried: HashMap<String, ()> = HashMap::new();
    let reconcile = |reason: &str, status: &SharedStatus| match sink.reconcile(reason) {
        Ok(()) => set_status(status, |s| {
            s.reconcile_count += 1;
            s.state = "active".into();
            s.reason = None;
        }),
        Err(error) => set_status(status, |s| {
            s.error_count += 1;
            s.state = "degraded".into();
            s.last_error = Some(format!("{error:#}"));
            s.reason = Some("reconciliation failed; results are labeled by hash comparison".into());
        }),
    };
    if catch_up {
        reconcile("start", &status);
    } else {
        set_status(&status, |s| s.state = "active".into());
    }
    loop {
        let message = match deb.deadline() {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
        };
        let now = Instant::now();
        match message {
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Msg::Event(Err(error))) => {
                set_status(&status, |s| {
                    s.error_count += 1;
                    s.last_error = Some(error.to_string());
                });
                deb.push_storm("watcher error", now);
            }
            Ok(Msg::Event(Ok(event))) => {
                handle_event(
                    &event,
                    &root,
                    git_dir.as_deref(),
                    &mut matcher,
                    &mut deb,
                    &mut backend,
                    now,
                );
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if overflow.swap(false, Ordering::Relaxed) {
            set_status(&status, |s| s.overflow_count += 1);
            deb.push_storm("event queue overflow", now);
        }
        while let Some(action) = deb.poll(Instant::now()) {
            match action {
                Action::Refresh(paths) => {
                    for path in paths {
                        match sink.refresh(&path) {
                            Ok(()) => {
                                retried.remove(&path);
                                set_status(&status, |s| s.refresh_count += 1)
                            }
                            Err(error) => {
                                set_status(&status, |s| {
                                    s.error_count += 1;
                                    s.last_error = Some(format!("{path}: {error:#}"));
                                });
                                // One retry per path; a second failure waits for the next event
                                // or reconciliation.
                                if retried.insert(path.clone(), ()).is_none() {
                                    deb.push_path(path, Instant::now());
                                }
                            }
                        }
                    }
                }
                Action::Reconcile(reason) => {
                    retried.clear();
                    matcher = build_matcher(&root);
                    reconcile(&reason, &status);
                }
            }
        }
        set_status(&status, |s| s.pending = deb.pending());
    }
}

fn handle_event(
    event: &notify::Event,
    root: &Path,
    git_dir: Option<&Path>,
    matcher: &mut Gitignore,
    deb: &mut Debouncer,
    backend: &mut notify::RecommendedWatcher,
    now: Instant,
) {
    if event.need_rescan() {
        deb.push_storm("watcher rescan notice", now);
        return;
    }
    if matches!(event.kind, EventKind::Access(_)) {
        return;
    }
    for path in &event.paths {
        let class = if let Some(rel) = path
            .strip_prefix(root)
            .ok()
            .filter(|r| !r.as_os_str().is_empty())
        {
            classify(rel)
        } else if let Some(rel) = git_dir.and_then(|g| path.strip_prefix(g).ok()) {
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
            git_internal(&parts)
        } else {
            PathClass::Ignored
        };
        match class {
            PathClass::Ignored => {}
            PathClass::BranchChange => deb.push_storm("branch or index change", now),
            PathClass::IgnoreRules => deb.push_storm("ignore rules changed", now),
            PathClass::Source(rel) => {
                let is_dir = path.is_dir();
                if matcher
                    .matched_path_or_any_parents(path, is_dir)
                    .is_ignore()
                {
                    continue;
                }
                let removed_dir = matches!(
                    event.kind,
                    EventKind::Remove(notify::event::RemoveKind::Folder)
                ) || (matches!(
                    event.kind,
                    EventKind::Modify(notify::event::ModifyKind::Name(_))
                ) && !path.exists()
                    && path.extension().is_none());
                if removed_dir {
                    deb.push_storm("directory removed or renamed", now);
                } else if is_dir {
                    new_directory(path, root, matcher, deb, backend, now);
                } else {
                    deb.push_path(rel, now);
                }
            }
        }
    }
}

/// A directory appeared (create or rename-into). Register it on inotify and enqueue its files;
/// a large tree becomes a reconciliation.
fn new_directory(
    dir: &Path,
    root: &Path,
    matcher: &Gitignore,
    deb: &mut Debouncer,
    backend: &mut notify::RecommendedWatcher,
    now: Instant,
) {
    let mut files = 0usize;
    let walker = ignore::WalkBuilder::new(dir)
        .follow_links(false)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        if cfg!(target_os = "linux") && is_dir {
            if backend.watch(path, RecursiveMode::NonRecursive).is_err() {
                deb.push_storm("could not watch a new directory", now);
                return;
            }
            continue;
        }
        if is_dir || matcher.matched_path_or_any_parents(path, false).is_ignore() {
            continue;
        }
        files += 1;
        if files > MAX_DIR_SCAN {
            deb.push_storm("large directory appeared", now);
            return;
        }
        if let Ok(rel) = path.strip_prefix(root)
            && let PathClass::Source(rel) = classify(rel)
        {
            deb.push_path(rel, now);
        }
    }
}

// ---------- process-wide registry (hosted by a long-lived process: MCP session or daemon) ----------

static DEFAULT_ENABLED: AtomicU64 = AtomicU64::new(0);

/// A host that owns the single writer (the daemon) turns watching on by default.
pub fn set_default_enabled(on: bool) {
    DEFAULT_ENABLED.store(u64::from(on), Ordering::Relaxed);
}

/// `MIDNIGHT_WATCH=1` enables, `MIDNIGHT_WATCH=0` disables, otherwise the host default applies.
pub fn enabled() -> bool {
    match std::env::var("MIDNIGHT_WATCH").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => DEFAULT_ENABLED.load(Ordering::Relaxed) != 0,
    }
}

fn registry() -> &'static Mutex<HashMap<PathBuf, RepoWatcher>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, RepoWatcher>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Start (once) a watcher for an indexed repository root when watching is enabled. A repository
/// without an index is skipped: there is nothing to keep fresh yet.
pub fn ensure(db_path: &Path, root: &Path) {
    ensure_inner(db_path, root, true);
}

/// Like [`ensure`] right after a complete `index_repository`: the scan that just finished is the
/// catch-up, so the watcher does not repeat it.
pub fn ensure_after_index(db_path: &Path, root: &Path, complete: bool) {
    ensure_inner(db_path, root, !complete);
}

fn ensure_inner(db_path: &Path, root: &Path, catch_up: bool) {
    if !enabled() {
        return;
    }
    let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let Ok(mut map) = registry().lock() else {
        return;
    };
    if map.contains_key(&key) {
        return;
    }
    let indexed = crate::db::repo_scope(&key)
        .ok()
        .and_then(|scope| {
            let conn = crate::db::open_db(db_path).ok()?;
            conn.query_row(
                "SELECT 1 FROM repositories WHERE repo_id=?1 AND worktree_id=?2",
                rusqlite::params![scope.repo_id, scope.worktree_id],
                |_| Ok(()),
            )
            .ok()
        })
        .is_some();
    if !indexed {
        return;
    }
    let watcher = RepoWatcher::start(&key, IndexSink::new(db_path, &key), catch_up);
    map.insert(key, watcher);
}

/// Watcher state for `index_status`: `off` when none is running for this root.
pub fn status_json(root: &Path) -> serde_json::Value {
    let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let status = registry()
        .lock()
        .ok()
        .and_then(|map| map.get(&key).map(RepoWatcher::status));
    match status {
        Some(status) => serde_json::to_value(status).unwrap_or(serde_json::Value::Null),
        None => serde_json::json!({
            "state": "off",
            "reason": if enabled() { "no index for this repository yet" } else { "file watching is not enabled in this process (MIDNIGHT_WATCH=1, or the daemon)" },
        }),
    }
}

/// Stop every watcher (process shutdown).
pub fn stop_all() {
    if let Ok(mut map) = registry().lock() {
        map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn debouncer_coalesces_one_path_and_waits_for_quiet() {
        let base = Instant::now();
        let mut d = Debouncer::new(MAX_PENDING_PATHS);
        assert!(d.deadline().is_none(), "idle means block, never tick");
        d.push_path("a.rs".into(), base);
        d.push_path("a.rs".into(), t(base, 20));
        assert_eq!(d.pending(), 1);
        assert_eq!(d.poll(t(base, 50)), None, "not quiet for 40 ms yet");
        assert_eq!(
            d.poll(t(base, 61)),
            Some(Action::Refresh(vec!["a.rs".into()]))
        );
        assert!(d.deadline().is_none());
    }

    #[test]
    fn debouncer_refreshes_a_busy_path_by_the_max_wait() {
        let base = Instant::now();
        let mut d = Debouncer::new(MAX_PENDING_PATHS);
        for ms in (0..=190).step_by(10) {
            d.push_path("busy.rs".into(), t(base, ms));
        }
        assert_eq!(d.poll(t(base, 199)), None);
        assert_eq!(
            d.poll(t(base, 200)),
            Some(Action::Refresh(vec!["busy.rs".into()]))
        );
    }

    #[test]
    fn a_storm_becomes_exactly_one_reconcile_and_no_file_work() {
        let base = Instant::now();
        let mut d = Debouncer::new(MAX_PENDING_PATHS);
        for n in 0..2_000u64 {
            d.push_path(format!("f{n}.rs"), t(base, n / 100));
        }
        assert_eq!(d.pending(), 0, "pending work was discarded");
        let mut actions = vec![];
        let mut now = t(base, 10);
        while now < t(base, 1_000) {
            if let Some(a) = d.poll(now) {
                actions.push(a);
            }
            now += Duration::from_millis(10);
        }
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::Reconcile(_)));
        assert!(d.deadline().is_none());
    }

    #[test]
    fn classification_keeps_git_head_and_drops_other_git_and_excluded_paths() {
        assert_eq!(classify(Path::new(".git/HEAD")), PathClass::BranchChange);
        assert_eq!(
            classify(Path::new(".git/refs/heads/main")),
            PathClass::BranchChange
        );
        assert_eq!(
            classify(Path::new(".git/objects/ab/cdef")),
            PathClass::Ignored
        );
        assert_eq!(classify(Path::new(".git/index.lock")), PathClass::Ignored);
        assert_eq!(
            classify(Path::new("src/.gitignore")),
            PathClass::IgnoreRules
        );
        assert_eq!(classify(Path::new("target/debug/x.rs")), PathClass::Ignored);
        assert_eq!(classify(Path::new(".env")), PathClass::Ignored);
        assert_eq!(
            classify(Path::new("src/lib.rs")),
            PathClass::Source("src/lib.rs".into())
        );
    }

    struct Recorder(Arc<Mutex<Vec<String>>>);
    impl WatchSink for Recorder {
        fn refresh(&self, rel_path: &str) -> Result<()> {
            self.0.lock().unwrap().push(format!("refresh:{rel_path}"));
            Ok(())
        }
        fn reconcile(&self, reason: &str) -> Result<()> {
            self.0.lock().unwrap().push(format!("reconcile:{reason}"));
            Ok(())
        }
    }

    #[test]
    fn a_real_edit_reaches_the_sink_and_idle_stays_quiet() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let watcher = RepoWatcher::start(&root, Recorder(log.clone()), true);
        assert_ne!(watcher.status().state, "unavailable");
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(root.join("src/new.rs"), "pub fn a() {}\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if log
                .lock()
                .unwrap()
                .iter()
                .any(|e| e == "refresh:src/new.rs")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no refresh seen: {:?}",
                log.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(log.lock().unwrap()[0], "reconcile:start");
        drop(watcher);
    }
}
