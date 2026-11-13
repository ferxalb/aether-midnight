//! Single-writer daemon (design: `docs/design/v0.2-daemon.md`).
//!
//! One daemon per database file owns every write: its connections run on their own threads and
//! take a FIFO write gate around writer tools, so two clients can never contend for SQLite's write
//! lock. It also hosts what must exist once per database: the local model, the file watchers and
//! each connection's delivery history. `midnight mcp` is a thin line proxy to it, with an explicit
//! in-process fallback.
//!
//! Wire format: newline-delimited JSON. The first line from a client is a control message
//! (`hello`, `status`, `shutdown`, `shutdown_if_idle`); after a `ready` reply a `hello` connection
//! speaks exactly the stdio MCP JSON-RPC dialect. Policy is per connection (sent in `hello`) and
//! enforced by the unchanged `mcp::Policy` on the daemon side.
//!
//! Trust: the socket directory is 0700 and the socket 0600 (same user only). The policy limits a
//! model-facing front end; it is not a boundary against a hostile process of the same user, which
//! could open the database file directly.
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde_json::{Value, json};

use crate::VERSION;
use crate::mcp::{self, Policy};

/// Daemon wire protocol revision. Different values never talk to each other.
pub const PROTOCOL: u32 = 1;
pub const MAX_CONNECTIONS: usize = 32;
pub const DEFAULT_IDLE_SECS: u64 = 600;
const MAX_CONTROL_LINE: usize = 64 * 1024;
const SPAWN_WAIT: Duration = Duration::from_secs(3);
const DRAIN_WAIT: Duration = Duration::from_secs(10);

// ---------- process mode, reported in `initialize` instructions ----------

fn mode_cell() -> &'static Mutex<String> {
    static MODE: OnceLock<Mutex<String>> = OnceLock::new();
    MODE.get_or_init(|| Mutex::new("in-process".to_string()))
}

pub fn set_mode(mode: &str) {
    if let Ok(mut guard) = mode_cell().lock() {
        *guard = mode.to_string();
    }
}

/// `daemon` inside the daemon, otherwise `in-process` or `in-process (<why the daemon was not used>)`.
pub fn mode() -> String {
    mode_cell()
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "in-process".into())
}

// ---------- write gate ----------

/// FIFO ticket lock: writers run in arrival order, so none starves (SQLite's own lock is unfair).
pub struct WriteGate {
    state: Mutex<(u64, u64)>, // (next ticket, now serving)
    turn: Condvar,
}

pub struct GateGuard<'a>(&'a WriteGate);

impl WriteGate {
    const fn new() -> Self {
        Self {
            state: Mutex::new((0, 0)),
            turn: Condvar::new(),
        }
    }

    pub fn acquire(&self) -> GateGuard<'_> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ticket = state.0;
        state.0 += 1;
        while state.1 != ticket {
            state = self.turn.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        GateGuard(self)
    }
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.1 += 1;
        self.0.turn.notify_all();
    }
}

static GATE: WriteGate = WriteGate::new();
static GATE_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn write_gate() -> &'static WriteGate {
    &GATE
}

/// True only inside a running daemon.
pub fn gate_active() -> bool {
    GATE_ACTIVE.load(Ordering::Relaxed)
}

// ---------- paths ----------

#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
    pub info: PathBuf,
    pub log: PathBuf,
}

/// `MIDNIGHT_DAEMON`: `0`/`off` disables, `fallback` lets a version mismatch fall back in-process.
fn daemon_env() -> String {
    std::env::var("MIDNIGHT_DAEMON")
        .unwrap_or_default()
        .to_ascii_lowercase()
}

pub fn enabled() -> bool {
    !matches!(daemon_env().as_str(), "0" | "off" | "false" | "no")
}

pub fn idle_secs() -> u64 {
    std::env::var("MIDNIGHT_DAEMON_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_IDLE_SECS)
}

fn base_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("MIDNIGHT_RUN_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(runtime).join("midnight"));
    }
    Ok(crate::config::config_dir()?.join("run"))
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("create daemon directory {}", dir.display()))?;
    }
    let meta = fs::metadata(dir)?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if meta.uid() != unsafe { libc::geteuid() } {
        bail!(
            "daemon directory {} is not owned by the current user",
            dir.display()
        );
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict {} to the current user", dir.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    Ok(())
}

pub fn paths(db_path: &Path) -> Result<Paths> {
    let absolute = std::path::absolute(db_path).unwrap_or_else(|_| db_path.to_path_buf());
    let key = crate::db::hash_bytes(absolute.to_string_lossy().as_bytes());
    let key = &key[..8];
    let mut dir = base_dir()?;
    // `sun_path` is ~104 bytes on macOS and 108 on Linux; stay well inside it.
    if dir.join(format!("d-{key}.sock")).as_os_str().len() > 96 {
        #[cfg(unix)]
        {
            // SAFETY: geteuid has no preconditions and cannot fail.
            let uid = unsafe { libc::geteuid() };
            dir = PathBuf::from(format!("/tmp/midnight-{uid}"));
        }
    }
    private_dir(&dir)?;
    Ok(Paths {
        socket: dir.join(format!("d-{key}.sock")),
        lock: dir.join(format!("d-{key}.lock")),
        info: dir.join(format!("d-{key}.json")),
        log: dir.join(format!("d-{key}.log")),
        dir,
    })
}

// ---------- transport ----------

pub trait Stream: Read + Write + Send {
    fn try_clone_stream(&self) -> io::Result<Box<dyn Stream>>;
    fn shutdown_write(&self) -> io::Result<()>;
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

pub trait Listener: Send {
    fn accept_stream(&self) -> io::Result<Box<dyn Stream>>;
}

/// IPC mechanism. Unix domain sockets today; a named-pipe implementation (design:
/// `docs/design/v0.2-platforms.md`) slots in behind the same trait for Windows.
pub trait Transport {
    fn listen(&self, paths: &Paths) -> io::Result<Box<dyn Listener>>;
    fn connect(&self, paths: &Paths) -> io::Result<Box<dyn Stream>>;
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    impl Stream for UnixStream {
        fn try_clone_stream(&self) -> io::Result<Box<dyn Stream>> {
            Ok(Box::new(self.try_clone()?))
        }
        fn shutdown_write(&self) -> io::Result<()> {
            self.shutdown(std::net::Shutdown::Write)
        }
        fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            UnixStream::set_read_timeout(self, timeout)
        }
    }

    impl Listener for UnixListener {
        fn accept_stream(&self) -> io::Result<Box<dyn Stream>> {
            Ok(Box::new(self.accept()?.0))
        }
    }

    pub struct UnixTransport;

    impl Transport for UnixTransport {
        fn listen(&self, paths: &Paths) -> io::Result<Box<dyn Listener>> {
            let listener = UnixListener::bind(&paths.socket)?;
            fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
            Ok(Box::new(listener))
        }
        fn connect(&self, paths: &Paths) -> io::Result<Box<dyn Stream>> {
            Ok(Box::new(UnixStream::connect(&paths.socket)?))
        }
    }
}

#[cfg(unix)]
fn transport() -> Box<dyn Transport> {
    Box::new(unix::UnixTransport)
}

#[cfg(not(unix))]
fn transport() -> Box<dyn Transport> {
    struct Unsupported;
    impl Transport for Unsupported {
        fn listen(&self, _: &Paths) -> io::Result<Box<dyn Listener>> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the daemon transport is not implemented on this platform yet (named pipes are designed, not built)",
            ))
        }
        fn connect(&self, _: &Paths) -> io::Result<Box<dyn Stream>> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the daemon transport is not implemented on this platform yet",
            ))
        }
    }
    Box::new(Unsupported)
}

// ---------- line IO ----------

fn read_control_line(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(None);
        }
        let take = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        if line.len() + take > MAX_CONTROL_LINE {
            bail!("control message exceeds {MAX_CONTROL_LINE} bytes");
        }
        line.extend_from_slice(&available[..take]);
        let done = line.ends_with(b"\n");
        reader.consume(take);
        if done {
            break;
        }
    }
    Ok(Some(
        serde_json::from_slice(&line).context("control message is not JSON")?,
    ))
}

fn send(stream: &mut dyn Write, value: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()
}

// ---------- server ----------

struct Shared {
    state: Mutex<ServerState>,
    changed: Condvar,
    started: Instant,
    idle: Duration,
}

struct ServerState {
    connections: usize,
    idle_since: Instant,
    shutdown: bool,
}

/// Run the daemon in the foreground until idle shutdown, `daemon stop`, or fatal error.
pub fn serve(db_path: &Path, cancellation: Arc<AtomicBool>) -> Result<()> {
    let paths = paths(db_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&paths.lock)
        .with_context(|| format!("open {}", paths.lock.display()))?;
    // The kernel drops an advisory lock when its holder dies, so a stale lock cannot exist.
    if lock.try_lock_exclusive().is_err() {
        bail!("another daemon already serves this database");
    }
    let _ = fs::remove_file(&paths.socket); // stale socket of a crashed daemon
    let ipc = transport();
    let listener = ipc
        .listen(&paths)
        .with_context(|| format!("listen on {}", paths.socket.display()))?;
    // Make sure the database exists and is migrated before the first client arrives.
    drop(crate::db::open_db(db_path)?);
    GATE_ACTIVE.store(true, Ordering::Relaxed);
    set_mode("daemon");
    crate::watch::set_default_enabled(true);
    let info = json!({
        "pid": std::process::id(), "version": VERSION, "protocol": PROTOCOL,
        "schema": crate::db::SCHEMA_VERSION, "socket": paths.socket,
        "started": crate::db::now_utc(), "db": db_path,
    });
    fs::write(&paths.info, serde_json::to_vec_pretty(&info)?)?;
    let shared = Arc::new(Shared {
        state: Mutex::new(ServerState {
            connections: 0,
            idle_since: Instant::now(),
            shutdown: false,
        }),
        changed: Condvar::new(),
        started: Instant::now(),
        idle: Duration::from_secs(idle_secs()),
    });
    let wake = {
        let shared = shared.clone();
        let paths = paths.clone();
        move || {
            if let Ok(mut state) = shared.state.lock() {
                state.shutdown = true;
            }
            shared.changed.notify_all();
            // Unblock `accept` with a throwaway connection.
            let _ = transport().connect(&paths);
        }
    };
    // Idle monitor: wakes only on connection changes or at the idle deadline.
    {
        let shared = shared.clone();
        let wake = wake.clone();
        std::thread::Builder::new()
            .name("midnight-idle".into())
            .spawn(move || {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if state.shutdown {
                        return;
                    }
                    if state.connections > 0 || shared.idle.is_zero() {
                        state = shared
                            .changed
                            .wait(state)
                            .unwrap_or_else(|e| e.into_inner());
                        continue;
                    }
                    let due = state.idle_since + shared.idle;
                    let now = Instant::now();
                    if now >= due {
                        drop(state);
                        wake();
                        return;
                    }
                    state = shared
                        .changed
                        .wait_timeout(state, due - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            })?;
    }
    let db_path_buf = db_path.to_path_buf();
    loop {
        let stream = match listener.accept_stream() {
            Ok(stream) => stream,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                eprintln!("midnight daemon: accept failed: {error}");
                break;
            }
        };
        if shared.state.lock().map(|s| s.shutdown).unwrap_or(true) {
            break;
        }
        let shared = shared.clone();
        let db = db_path_buf.clone();
        let cancel = cancellation.clone();
        let wake = wake.clone();
        let spawned = std::thread::Builder::new().name("midnight-conn".into()).spawn(move || {
            let admitted = {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.connections >= MAX_CONNECTIONS {
                    false
                } else {
                    state.connections += 1;
                    true
                }
            };
            let mut stream = stream;
            if !admitted {
                let _ = send(&mut stream, &json!({"midnight":"error","code":"busy","message":format!("the daemon already has {MAX_CONNECTIONS} connections")}));
                return;
            }
            let outcome = handle_connection(stream, &db, &shared, &cancel, &wake);
            if let Err(error) = outcome {
                eprintln!("midnight daemon: connection ended: {error:#}");
            }
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.connections -= 1;
            state.idle_since = Instant::now();
            shared.changed.notify_all();
        });
        if let Err(error) = spawned {
            eprintln!("midnight daemon: cannot start a connection thread: {error}");
        }
    }
    // Graceful shutdown: wait for in-flight sessions, flush watchers, checkpoint, clean up.
    let drain_until = Instant::now() + DRAIN_WAIT;
    {
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.connections > 0 && Instant::now() < drain_until {
            state = shared
                .changed
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    crate::watch::stop_all();
    let _ = crate::code::wal_checkpoint(db_path);
    let _ = fs::remove_file(&paths.socket);
    let _ = fs::remove_file(&paths.info);
    GATE_ACTIVE.store(false, Ordering::Relaxed);
    drop(lock);
    Ok(())
}

fn handle_connection(
    stream: Box<dyn Stream>,
    db_path: &Path,
    shared: &Shared,
    cancel: &AtomicBool,
    wake: &(impl Fn() + Clone),
) -> Result<()> {
    let mut writer = stream.try_clone_stream()?;
    let mut reader = BufReader::new(stream);
    // A client must speak within a few seconds; the timeout is removed after the handshake.
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok();
    let Some(first) = read_control_line(&mut reader)? else {
        return Ok(());
    };
    let op = first.get("midnight").and_then(Value::as_str).unwrap_or("");
    let connections = shared.state.lock().map(|s| s.connections).unwrap_or(0);
    match op {
        "status" => {
            send(
                &mut writer,
                &json!({"midnight":"status","pid":std::process::id(),"version":VERSION,
                    "protocol":PROTOCOL,"schema":crate::db::SCHEMA_VERSION,
                    "connections":connections.saturating_sub(1),
                    "uptime_secs":shared.started.elapsed().as_secs(),
                    "idle_shutdown_secs":shared.idle.as_secs(),"db":db_path}),
            )?;
            Ok(())
        }
        "shutdown" => {
            send(&mut writer, &json!({"midnight":"bye"}))?;
            wake();
            Ok(())
        }
        "shutdown_if_idle" => {
            // This control connection counts as one.
            if connections <= 1 {
                send(&mut writer, &json!({"midnight":"bye"}))?;
                wake();
            } else {
                send(
                    &mut writer,
                    &json!({"midnight":"error","code":"busy","message":"the daemon has active connections"}),
                )?;
            }
            Ok(())
        }
        "hello" => {
            let protocol = first.get("protocol").and_then(Value::as_u64).unwrap_or(0) as u32;
            let schema = first.get("schema").and_then(Value::as_i64).unwrap_or(-1);
            if protocol != PROTOCOL || schema != crate::db::SCHEMA_VERSION {
                send(
                    &mut writer,
                    &json!({"midnight":"error","code":"version_mismatch","daemon_version":VERSION,
                        "daemon_protocol":PROTOCOL,"daemon_schema":crate::db::SCHEMA_VERSION,
                        "message":format!("daemon is {VERSION} (protocol {PROTOCOL}, schema {}); client is {} (protocol {protocol}, schema {schema})",
                            crate::db::SCHEMA_VERSION, first.get("client_version").and_then(Value::as_str).unwrap_or("?"))}),
                )?;
                return Ok(());
            }
            let policy: Policy = match first.get("policy") {
                Some(p) => serde_json::from_value(p.clone()).context("invalid policy in hello")?,
                None => Policy::default(),
            };
            mcp::set_connection_cwd(first.get("cwd").and_then(Value::as_str).map(PathBuf::from));
            send(
                &mut writer,
                &json!({"midnight":"ready","protocol":PROTOCOL,"daemon_version":VERSION,"pid":std::process::id()}),
            )?;
            reader.get_ref().set_read_timeout(None).ok();
            mcp::run_session(&mut reader, &mut writer, db_path, &policy, cancel)
        }
        other => {
            send(
                &mut writer,
                &json!({"midnight":"error","code":"bad_request","message":format!("unknown control operation {other:?}")}),
            )?;
            Ok(())
        }
    }
}

// ---------- client ----------

pub struct Ready {
    pub stream: Box<dyn Stream>,
    pub reader_buffer: Vec<u8>,
    pub pid: u64,
    pub daemon_version: String,
}

fn spawn_daemon(db_path: &Path, paths: &Paths) -> Result<()> {
    let exe = std::env::current_exe().context("locate the midnight executable")?;
    let log = {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&paths.log)?
    };
    let absolute = std::path::absolute(db_path).unwrap_or_else(|_| db_path.to_path_buf());
    let mut command = std::process::Command::new(exe);
    command
        .arg("--db")
        .arg(absolute)
        .args(["daemon", "run"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().context("start the daemon process")?;
    Ok(())
}

fn connect_stream(paths: &Paths) -> io::Result<Box<dyn Stream>> {
    transport().connect(paths)
}

/// Connect to the daemon for `db_path`, starting it when `spawn` is set and none is running.
fn connect_raw(db_path: &Path, spawn: bool) -> Result<Box<dyn Stream>> {
    let paths = paths(db_path)?;
    match connect_stream(&paths) {
        Ok(stream) => return Ok(stream),
        Err(error) if !spawn => return Err(anyhow::anyhow!("no daemon is running ({error})")),
        Err(_) => {}
    }
    spawn_daemon(db_path, &paths)?;
    let deadline = Instant::now() + SPAWN_WAIT;
    let mut pause = Duration::from_millis(10);
    loop {
        match connect_stream(&paths) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() >= deadline => {
                bail!(
                    "daemon did not start within {}s ({error}); see {}",
                    SPAWN_WAIT.as_secs(),
                    paths.log.display()
                )
            }
            Err(_) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(200));
            }
        }
    }
}

fn control(db_path: &Path, request: &Value) -> Result<Option<Value>> {
    let mut stream = match connect_raw(db_path, false) {
        Ok(stream) => stream,
        Err(_) => return Ok(None),
    };
    send(&mut stream, request)?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut reader = BufReader::new(stream);
    read_control_line(&mut reader)
}

fn hello(policy: &Policy) -> Value {
    json!({"midnight":"hello","protocol":PROTOCOL,"client_version":VERSION,
        "schema":crate::db::SCHEMA_VERSION,"policy":policy,
        "cwd":std::env::current_dir().ok()})
}

/// Open a daemon connection ready for JSON-RPC lines, or say why not. A version mismatch against
/// an idle daemon restarts it once; against a busy one it is an error (never a second writer).
pub fn open_session(db_path: &Path, policy: &Policy) -> Result<Ready> {
    if !enabled() {
        bail!("disabled by MIDNIGHT_DAEMON=0");
    }
    for attempt in 0..2 {
        let mut stream = connect_raw(db_path, true)?;
        send(&mut stream, &hello(policy))?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut reader = BufReader::new(stream.try_clone_stream()?);
        let reply = read_control_line(&mut reader)?
            .context("the daemon closed the connection during the handshake")?;
        match reply.get("midnight").and_then(Value::as_str) {
            Some("ready") => {
                stream.set_read_timeout(None).ok();
                let buffered = reader.buffer().to_vec();
                return Ok(Ready {
                    stream,
                    reader_buffer: buffered,
                    pid: reply.get("pid").and_then(Value::as_u64).unwrap_or(0),
                    daemon_version: reply
                        .get("daemon_version")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                });
            }
            Some("error") if reply["code"] == "version_mismatch" => {
                let message = reply["message"]
                    .as_str()
                    .unwrap_or("version mismatch")
                    .to_string();
                if attempt == 0
                    && let Ok(Some(answer)) =
                        control(db_path, &json!({"midnight":"shutdown_if_idle"}))
                    && answer["midnight"] == "bye"
                {
                    wait_until_gone(db_path);
                    continue;
                }
                bail!("{message}; stop it with `midnight daemon stop` or set MIDNIGHT_DAEMON=0");
            }
            Some("error") => bail!(
                "{}",
                reply["message"]
                    .as_str()
                    .unwrap_or("the daemon refused the connection")
            ),
            _ => bail!("unexpected handshake reply from the daemon"),
        }
    }
    bail!("could not replace the daemon with a matching version")
}

fn wait_until_gone(db_path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if connect_raw(db_path, false).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Relay raw lines between this process's stdio and the daemon until either side closes.
fn proxy_stdio(ready: Ready) -> Result<()> {
    let mut upstream = ready.stream.try_clone_stream()?;
    std::thread::Builder::new()
        .name("midnight-proxy-in".into())
        .spawn(move || {
            let stdin = io::stdin();
            let mut stdin = stdin.lock();
            let mut buffer = [0u8; 16 * 1024];
            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if upstream
                            .write_all(&buffer[..n])
                            .and_then(|_| upstream.flush())
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            let _ = upstream.shutdown_write();
        })?;
    let mut downstream = ready.stream;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    if !ready.reader_buffer.is_empty() {
        stdout.write_all(&ready.reader_buffer)?;
        stdout.flush()?;
    }
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match downstream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                stdout.write_all(&buffer[..n])?;
                stdout.flush()?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// `midnight mcp`: proxy to the daemon, or serve in-process with a stderr notice saying why.
pub fn run_mcp(
    db_path: &Path,
    policy: &Policy,
    cancellation: Arc<AtomicBool>,
    allow_daemon: bool,
) -> Result<()> {
    let reason = if allow_daemon {
        match open_session(db_path, policy) {
            Ok(ready) => return proxy_stdio(ready),
            Err(error) => format!("{error:#}"),
        }
    } else {
        "disabled by --no-daemon".to_string()
    };
    if enabled() && allow_daemon {
        eprintln!("midnight: daemon unavailable ({reason}); running in-process");
    }
    set_mode(&format!("in-process ({reason})"));
    mcp::run_stdio(db_path, policy, cancellation)
}

// ---------- commands ----------

pub fn command_start(db_path: &Path) -> Result<Value> {
    if !enabled() {
        bail!("the daemon is disabled by MIDNIGHT_DAEMON=0");
    }
    let mut stream = connect_raw(db_path, true)?;
    send(&mut stream, &json!({"midnight":"status"}))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut reader = BufReader::new(stream);
    read_control_line(&mut reader)?.context("the daemon did not answer")
}

pub fn command_stop(db_path: &Path) -> Result<Value> {
    match control(db_path, &json!({"midnight":"shutdown"}))? {
        Some(answer) => {
            wait_until_gone(db_path);
            Ok(json!({"running":false,"stopped":answer["midnight"]=="bye"}))
        }
        None => Ok(json!({"running":false,"stopped":false})),
    }
}

pub fn command_status(db_path: &Path) -> Result<Value> {
    Ok(match control(db_path, &json!({"midnight":"status"}))? {
        Some(mut status) => {
            status["running"] = json!(true);
            status
        }
        None => json!({"running":false}),
    })
}

// ---------- HTTP and other per-request front ends ----------

/// Where a front end's JSON-RPC session lives: in this process, or on a daemon connection.
pub enum Backend {
    Local(Option<mcp::Session>),
    Remote {
        reader: BufReader<Box<dyn Stream>>,
        writer: Box<dyn Stream>,
        policy: Policy,
    },
}

impl Backend {
    /// Connect to the daemon (starting it if needed) and fall back to in-process on any failure.
    pub fn connect(db_path: &Path, policy: &Policy) -> Backend {
        match open_session(db_path, policy) {
            Ok(ready) => match (ready.stream.try_clone_stream(), ready.stream) {
                (Ok(reader), writer) => Backend::Remote {
                    reader: BufReader::new(reader),
                    writer,
                    policy: policy.clone(),
                },
                _ => Backend::Local(None),
            },
            Err(error) => {
                if enabled() {
                    eprintln!("midnight: daemon unavailable ({error:#}); serving in-process");
                }
                Backend::Local(None)
            }
        }
    }

    pub fn matches_policy(&self, policy: &Policy) -> bool {
        match self {
            Backend::Local(_) => true,
            Backend::Remote { policy: own, .. } => {
                serde_json::to_value(own).ok() == serde_json::to_value(policy).ok()
            }
        }
    }

    /// Handle one JSON-RPC message. A broken daemon connection becomes a JSON-RPC error and the
    /// backend is reset to in-process for the next message.
    pub fn handle(
        &mut self,
        db_path: &Path,
        policy: &Policy,
        message: &Value,
        cancellation: &AtomicBool,
    ) -> Option<Value> {
        match self {
            Backend::Local(session) => {
                mcp::handle_message(db_path, policy, session, message, cancellation)
            }
            Backend::Remote { reader, writer, .. } => {
                let id = message.get("id").cloned();
                let outcome: Result<Option<Value>> = (|| {
                    send(writer.as_mut(), message)?;
                    if id.is_none() {
                        return Ok(None);
                    }
                    let mut line = String::new();
                    if reader.read_line(&mut line)? == 0 {
                        bail!("the daemon closed the connection");
                    }
                    Ok(Some(serde_json::from_str(&line)?))
                })();
                match outcome {
                    Ok(value) => value,
                    Err(error) => {
                        *self = Backend::Local(None);
                        id.map(|id| json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,
                            "message":format!("daemon connection lost: {error:#}; retry the request")}}))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_gate_serves_writers_in_arrival_order() {
        let gate = Arc::new(WriteGate::new());
        let order = Arc::new(Mutex::new(Vec::new()));
        let first = gate.acquire();
        let mut handles = vec![];
        for n in 0..4 {
            let (worker_gate, order) = (gate.clone(), order.clone());
            handles.push(std::thread::spawn(move || {
                let _guard = worker_gate.acquire();
                order.lock().unwrap().push(n);
            }));
            // Make arrival order deterministic: wait until the thread holds a ticket.
            while gate.state.lock().unwrap().0 < (n + 2) as u64 {
                std::thread::yield_now();
            }
        }
        drop(first);
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn socket_paths_stay_short_and_directories_private() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths_in(temp.path(), Path::new("/some/where/db.sqlite3"));
        assert!(paths.socket.as_os_str().len() < 104);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&paths.dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    fn paths_in(dir: &Path, db: &Path) -> Paths {
        let key = crate::db::hash_bytes(db.to_string_lossy().as_bytes());
        let dir = dir.join("run");
        private_dir(&dir).unwrap();
        Paths {
            socket: dir.join(format!("d-{}.sock", &key[..8])),
            lock: dir.join("l"),
            info: dir.join("i"),
            log: dir.join("g"),
            dir,
        }
    }
}
