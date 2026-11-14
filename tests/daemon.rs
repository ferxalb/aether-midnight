//! Single-writer daemon behavior (docs/design/v0.2-daemon.md). Written in Phase 3 of v0.2 and
//! first run in the final validation phase. Every test uses its own temporary run directory and
//! database, and stops the daemon it started.
#![cfg(unix)]
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use aether_midnight::daemon;
use aether_midnight::mcp::Policy;
use serde_json::{Value, json};

/// The daemon paths come from process environment; tests in this file run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Env {
    _temp: tempfile::TempDir,
    run_dir: PathBuf,
    db: PathBuf,
}

fn env() -> Env {
    let temp = tempfile::tempdir().unwrap();
    // Short path: Unix socket paths are limited to ~104 bytes.
    let run_dir = temp.path().join("run");
    let db = temp.path().join("db.sqlite3");
    // SAFETY: the SERIAL lock keeps this file's tests from reading the environment concurrently.
    unsafe {
        std::env::set_var("MIDNIGHT_RUN_DIR", &run_dir);
        std::env::set_var("MIDNIGHT_DAEMON_IDLE_SECS", "60");
        std::env::set_var("AETHER_MIDNIGHT_CONFIG_DIR", temp.path().join("config"));
        std::env::remove_var("MIDNIGHT_DAEMON");
    }
    Env {
        _temp: temp,
        run_dir,
        db,
    }
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_aether-midnight")
}

fn cli(env: &Env, args: &[&str]) -> Value {
    let out = Command::new(bin())
        .env("MIDNIGHT_RUN_DIR", &env.run_dir)
        .env("MIDNIGHT_DAEMON_IDLE_SECS", "60")
        .env(
            "AETHER_MIDNIGHT_CONFIG_DIR",
            env._temp.path().join("config"),
        )
        .arg("--db")
        .arg(&env.db)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Client {
    fn start(env: &Env, extra: &[&str], daemon_env: Option<&str>) -> Client {
        let mut command = Command::new(bin());
        command
            .env("MIDNIGHT_RUN_DIR", &env.run_dir)
            .env("MIDNIGHT_DAEMON_IDLE_SECS", "60")
            .env(
                "AETHER_MIDNIGHT_CONFIG_DIR",
                env._temp.path().join("config"),
            )
            .arg("--db")
            .arg(&env.db)
            .arg("mcp")
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(value) = daemon_env {
            command.env("MIDNIGHT_DAEMON", value);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Client {
            child,
            stdin,
            stdout,
            next: 0,
        };
        client.rpc("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}));
        client
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let line = json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params});
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
        let mut reply = String::new();
        assert!(
            self.stdout.read_line(&mut reply).unwrap() > 0,
            "server closed"
        );
        serde_json::from_str(&reply).unwrap()
    }

    /// Returns (is_error, parsed text).
    fn call(&mut self, name: &str, arguments: Value) -> (bool, Value) {
        let reply = self.rpc("tools/call", json!({"name":name,"arguments":arguments}));
        let result = &reply["result"];
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        (
            result["isError"] == true,
            serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
        )
    }

    fn finish(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

fn stop(env: &Env) {
    let status = cli(env, &["daemon", "stop"]);
    assert_eq!(status["running"], false);
}

#[test]
fn two_clients_write_concurrently_without_database_is_locked() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = env();
    let started = cli(&env, &["daemon", "start"]);
    assert_eq!(started["midnight"], "status");
    let mut threads = vec![];
    for client in 0..6 {
        let (run_dir, db) = (env.run_dir.clone(), env.db.clone());
        threads.push(std::thread::spawn(move || {
            let shim = Env {
                _temp: tempfile::tempdir().unwrap(),
                run_dir,
                db,
            };
            let mut c = Client::start(&shim, &[], None);
            let mut errors = vec![];
            for n in 0..15 {
                let (is_error, body) = c.call(
                    "remember",
                    json!({"scope":"project:load","kind":"source",
                        "content":format!("client {client} note {n} concurrent writer"),
                        "embed":false}),
                );
                if is_error {
                    errors.push(body.to_string());
                }
            }
            c.finish();
            errors
        }));
    }
    let errors: Vec<String> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    assert!(errors.is_empty(), "writer errors: {errors:?}");
    let status = cli(&env, &["daemon", "status"]);
    assert_eq!(status["running"], true);
    stop(&env);
}

#[test]
fn killing_the_daemon_mid_session_leaves_a_valid_database_and_the_next_client_restarts_it() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = env();
    let started = cli(&env, &["daemon", "start"]);
    let pid = started["pid"].as_u64().unwrap();
    let mut c = Client::start(&env, &[], None);
    let (is_error, _) = c.call(
        "remember",
        json!({"scope":"project:kill","kind":"source","content":"survives a crash","embed":false}),
    );
    assert!(!is_error);
    assert_eq!(
        Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap()
            .code(),
        Some(0)
    );
    c.finish();
    // Stale socket, info and lock files remain; the next client must still get a daemon.
    let mut next = Client::start(&env, &[], None);
    let (is_error, body) = next.call("recall", json!({"scope":"project:kill","query":"crash"}));
    assert!(!is_error, "{body}");
    assert!(body.to_string().contains("survives a crash"));
    next.finish();
    let conn = aether_midnight::db::open_db(&env.db).unwrap();
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    let fresh = cli(&env, &["daemon", "status"]);
    assert_ne!(
        fresh["pid"].as_u64().unwrap(),
        pid,
        "a new daemon replaced the killed one"
    );
    stop(&env);
}

#[test]
fn a_stale_lock_and_socket_do_not_block_start() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = env();
    let paths = daemon::paths(&env.db).unwrap();
    std::fs::write(&paths.lock, b"").unwrap(); // lock file nobody holds
    std::fs::write(&paths.socket, b"not a socket").unwrap();
    std::fs::write(&paths.info, b"{\"pid\":1}").unwrap();
    let status = cli(&env, &["daemon", "start"]);
    assert_eq!(status["midnight"], "status");
    stop(&env);
}

#[test]
fn a_daemon_with_another_protocol_is_reported_not_trusted() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = env();
    let paths = daemon::paths(&env.db).unwrap();
    let listener = UnixListener::bind(&paths.socket).unwrap();
    let fake = std::thread::spawn(move || {
        // Answer hello with a protocol mismatch; answer the shutdown_if_idle probe as busy.
        for stream in listener.incoming().take(3) {
            let mut stream = stream.unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let reply = if request["midnight"] == "hello" {
                json!({"midnight":"error","code":"version_mismatch","message":"fake daemon 0.0.1 (protocol 0)"})
            } else {
                json!({"midnight":"error","code":"busy","message":"busy"})
            };
            writeln!(stream, "{reply}").unwrap();
        }
    });
    let error = match daemon::open_session(&env.db, &Policy::default()) {
        Ok(_) => panic!("a mismatching daemon must not be used"),
        Err(error) => format!("{error:#}"),
    };
    assert!(error.contains("fake daemon 0.0.1"), "{error}");
    assert!(error.contains("midnight daemon stop"), "{error}");
    drop(fake);
}

#[test]
fn policy_is_per_connection_and_scopes_do_not_leak() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = env();
    cli(&env, &["daemon", "start"]);
    let policy = |scope: &str| Policy {
        memory_scopes: Some(vec![scope.to_string()]),
        ..Policy::default()
    };
    let mut a = rpc_session(&env, &policy("project:a"));
    let mut b = rpc_session(&env, &policy("project:b"));
    let (err, _) = a.call(
        "remember",
        json!({"scope":"project:a","kind":"source","content":"alpha secret","embed":false}),
    );
    assert!(!err);
    // B asks for A's scope explicitly: denied. B asking without scope gets its pinned scope only.
    let (err, body) = b.call("recall", json!({"scope":"project:a","query":"alpha"}));
    assert!(err, "{body}");
    assert!(body.to_string().contains("permission denied"));
    let (err, body) = b.call("recall", json!({"query":"alpha"}));
    assert!(!err, "{body}");
    assert!(!body.to_string().contains("alpha secret"));
    stop(&env);
}

#[test]
fn disabling_the_daemon_or_losing_it_falls_back_in_process_and_says_so() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = env();
    let mut c = Client::start(&env, &[], Some("0"));
    let init = c.rpc("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}));
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("Server mode: in-process"),
        "{instructions}"
    );
    let (err, _) = c.call(
        "remember",
        json!({"scope":"project:fb","kind":"source","content":"in process","embed":false}),
    );
    assert!(!err);
    c.finish();
    // Unusable run directory: a regular file where the directory should be.
    let blocker = env._temp.path().join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let mut child = Command::new(bin())
        .env("MIDNIGHT_RUN_DIR", blocker.join("sub"))
        .env(
            "AETHER_MIDNIGHT_CONFIG_DIR",
            env._temp.path().join("config"),
        )
        .arg("--db")
        .arg(&env.db)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":1,"method":"ping"})).unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("daemon unavailable"), "{stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"result\""));
}

struct Rpc {
    reader: BufReader<Box<dyn daemon::Stream>>,
    writer: Box<dyn daemon::Stream>,
    next: u64,
}

fn rpc_session(env: &Env, policy: &Policy) -> Rpc {
    let ready = daemon::open_session(&env.db, policy).unwrap();
    let writer = ready.stream.try_clone_stream().unwrap();
    Rpc {
        reader: BufReader::new(ready.stream),
        writer,
        next: 0,
    }
}

impl Rpc {
    fn call(&mut self, name: &str, arguments: Value) -> (bool, Value) {
        self.next += 1;
        let line = json!({"jsonrpc":"2.0","id":self.next,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}});
        writeln!(self.writer, "{line}").unwrap();
        self.writer.flush().unwrap();
        let mut reply = String::new();
        self.reader.read_line(&mut reply).unwrap();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        let text = reply["result"]["content"][0]["text"].as_str().unwrap_or("");
        (
            reply["result"]["isError"] == true,
            serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
        )
    }
}
