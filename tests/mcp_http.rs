use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};
use tempfile::TempDir;

struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(db: &std::path::Path, extra: &[&str]) -> Server {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .arg("--db")
        .arg(db)
        .arg("serve-http")
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line
        .trim()
        .rsplit_once(':')
        .unwrap()
        .1
        .trim_end_matches("/mcp")
        .parse()
        .unwrap();
    Server { child, port }
}

fn mint(db: &std::path::Path, name: &str, extra: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .arg("--db")
        .arg(db)
        .args(["token", "create", "--name", name])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn raw(port: u16, host: &str, token: Option<&str>, method: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    write!(
        s,
        "{method} /mcp HTTP/1.1\r\nHost: {host}\r\n{auth}Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
    (status, body)
}

fn rpc(server: &Server, token: &str, id: u64, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
    let (status, text) = raw(
        server.port,
        &format!("127.0.0.1:{}", server.port),
        Some(token),
        "POST",
        &body,
    );
    assert_eq!(status, 200, "{text}");
    serde_json::from_str(&text).unwrap()
}

fn call(server: &Server, token: &str, name: &str, args: Value) -> (bool, String) {
    let r = rpc(
        server,
        token,
        9,
        "tools/call",
        json!({"name":name,"arguments":args}),
    );
    (
        r["result"]["isError"].as_bool().unwrap(),
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
}

#[test]
fn http_memory_survives_restart_and_enforces_scope_policy() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");
    let token = mint(&db, "demo", &["--memory-scope", "project:demo"]);
    let first = start(&db, &[]);
    let init = rpc(
        &first,
        &token,
        1,
        "initialize",
        json!({"protocolVersion":"2025-11-25"}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    let (err, _) = call(
        &first,
        &token,
        "remember",
        json!({"scope":"project:demo","kind":"source","content":"The launch codename is Tangerine."}),
    );
    assert!(!err);
    let (denied, msg) = call(
        &first,
        &token,
        "recall",
        json!({"scope":"user:alice","query":"launch"}),
    );
    assert!(denied && msg.contains("permission denied"), "{msg}");
    drop(first);

    // Tokens live in the registry, so they survive a server restart.
    let second = start(&db, &[]);
    let (err, text) = call(
        &second,
        &token,
        "recall",
        json!({"scope":"project:demo","query":"codename"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("Tangerine"), "{text}");
}

#[test]
fn http_read_only_token_hides_write_tools_and_rejects_foreign_hosts() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");
    let token = mint(&db, "ro", &["--all-scopes", "--read-only"]);
    let server = start(&db, &[]);
    let list = rpc(&server, &token, 1, "tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"recall"));
    assert!(!names.contains(&"remember") && !names.contains(&"forget"));
    let (denied, _) = call(
        &server,
        &token,
        "forget",
        json!({"scope":"project:x","id":1}),
    );
    assert!(denied);
    let (denied, _) = call(
        &server,
        &token,
        "remember",
        json!({"scope":"project:x","kind":"source","content":"x"}),
    );
    assert!(denied, "a read-only token cannot write");
    let (status, _) = raw(server.port, "evil.example", Some(&token), "POST", "{}");
    assert_eq!(status, 403);
    let host = format!("127.0.0.1:{}", server.port);
    assert_eq!(raw(server.port, &host, Some(&token), "POST", "{bad").0, 400);
    assert_eq!(raw(server.port, &host, Some(&token), "GET", "").0, 405);
}

#[test]
fn http_rejects_unauthenticated_invalid_revoked_and_expired_tokens() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");
    let token = mint(&db, "victim", &["--memory-scope", "dot:v"]);
    let server = start(&db, &[]);
    let host = format!("127.0.0.1:{}", server.port);
    let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string();
    for bad in [
        None,
        Some(""),
        Some("mdn_0000"),
        Some("not-a-token"),
        Some(&token[..token.len() - 1]),
    ] {
        let (status, text) = raw(server.port, &host, bad, "POST", &body);
        assert_eq!(status, 401, "{bad:?}");
        assert!(!text.contains("recall"), "no tool names leak before auth");
    }
    // A wrong scheme with the right secret is still unauthenticated.
    let mut s = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nAuthorization: Basic {token}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 401"), "{out}");

    // The good token works, then revocation bites on the very next request.
    assert!(!call(&server, &token, "list_memories", json!({})).0);
    let status = Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .arg("--db")
        .arg(&db)
        .args(["token", "revoke", "victim"])
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(raw(server.port, &host, Some(&token), "POST", &body).0, 401);

    // Expired tokens are refused; the registry never holds plaintext.
    let short = mint(&db, "short", &["--all-scopes", "--ttl-secs", "0"]);
    assert_eq!(raw(server.port, &host, Some(&short), "POST", &body).0, 401);
    let registry = std::fs::read_to_string(temp.path().join("m.sqlite3.tokens.json")).unwrap();
    assert!(!registry.contains(&token) && !registry.contains(&short));
    // Error bodies never echo the presented credential.
    let (_, text) = raw(server.port, &host, Some(&token), "POST", &body);
    assert!(!text.contains(&token));
}

#[test]
fn http_tokens_isolate_dots_even_when_the_model_forges_scopes() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");
    let a = mint(
        &db,
        "dot-a",
        &[
            "--memory-scope",
            "dot:a",
            "--tools",
            "recall,remember,list_memories,forget",
        ],
    );
    let b = mint(
        &db,
        "dot-b",
        &[
            "--memory-scope",
            "dot:b",
            "--tools",
            "recall,remember,list_memories,forget",
        ],
    );
    let server = start(&db, &[]);
    let secret =
        json!({"scope":"dot:a","kind":"source","content":"Dot A secret passphrase is plumtree."});
    assert!(!call(&server, &a, "remember", secret).0);

    // B cannot read A by forging the scope, and omitting it reads only B.
    let (denied, msg) = call(
        &server,
        &b,
        "recall",
        json!({"scope":"dot:a","query":"plumtree"}),
    );
    assert!(denied && msg.contains("permission denied"), "{msg}");
    let (err, text) = call(&server, &b, "recall", json!({"query":"plumtree"}));
    assert!(!err && !text.contains("plumtree"), "{text}");
    let (err, text) = call(&server, &b, "list_memories", json!({}));
    assert!(!err && text.trim() == "[]", "{text}");
    // B cannot write into, or batch-write across, A's scope.
    let (denied, _) = call(&server, &b, "remember", secret_for("dot:a"));
    assert!(denied);
    let (denied, _) = call(&server, &b, "forget", json!({"scope":"dot:a","id":1}));
    assert!(denied);
    // Forgetting A's row id through B's own scope removes nothing.
    let (err, text) = call(&server, &b, "forget", json!({"id":1}));
    assert!(!err && text.contains("false"), "{text}");
    let (_, text) = call(&server, &a, "recall", json!({"query":"plumtree"}));
    assert!(
        text.contains("plumtree"),
        "A's memory must be untouched: {text}"
    );
    // Scope wildcards are not reachable through argument tricks.
    for forged in ["dot:a ", "DOT:A", "dot:*", "dot:b\u{0}dot:a"] {
        let (denied, _) = call(
            &server,
            &b,
            "recall",
            json!({"scope":forged,"query":"plumtree"}),
        );
        assert!(denied, "{forged:?}");
    }
    // An unlisted tool is invisible and uncallable.
    let (denied, _) = call(&server, &b, "index_repository", json!({"repo_path":"/"}));
    assert!(denied);
}

fn secret_for(scope: &str) -> Value {
    json!({"scope":scope,"kind":"source","content":"injected"})
}

#[test]
fn http_refuses_non_loopback_bind() {
    let temp = TempDir::new().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .arg("--db")
        .arg(temp.path().join("m.sqlite3"))
        .args(["serve-http", "--bind", "0.0.0.0:0"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn token_creation_requires_explicit_scope_and_roots() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");
    for extra in [vec![], vec!["--tools", "search_code", "--all-scopes"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
            .arg("--db")
            .arg(&db)
            .args(["token", "create", "--name", "x"])
            .args(&extra)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{extra:?} must be refused");
    }
}

#[test]
fn two_server_processes_share_one_database_without_losing_acknowledged_writes() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");
    let token = std::sync::Arc::new(mint(&db, "w", &["--memory-scope", "project:shared"]));
    assert!(
        !db.exists(),
        "token creation must not initialize the database"
    );
    let a = std::sync::Arc::new(start(&db, &[]));
    let initialized = rusqlite::Connection::open(&db).unwrap();
    let mode: String = initialized
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal", "readiness must follow WAL initialization");
    let version: i64 = initialized
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 5);
    drop(initialized);
    let b = std::sync::Arc::new(start(&db, &[]));
    let mut threads = Vec::new();
    for t in 0..4u64 {
        let server = if t % 2 == 0 { a.clone() } else { b.clone() };
        let token = token.clone();
        threads.push(std::thread::spawn(move || {
            for i in 0..25 {
                let (err, text) = call(
                    &server,
                    &token,
                    "remember",
                    json!({"scope":"project:shared","kind":"source","content":format!("writer {t} note {i}")}),
                );
                assert!(!err, "{text}");
            }
        }));
    }
    for t in threads {
        t.join().unwrap();
    }
    let (err, text) = call(&a, &token, "list_memories", json!({"limit":500}));
    assert!(!err);
    let rows: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(rows.len(), 100);
}

#[test]
fn http_scoped_replays_survive_concurrent_reads_maintenance_and_restart() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");
    let token_a = mint(&db, "a", &["--memory-scope", "project:a"]);
    let token_b = mint(&db, "b", &["--memory-scope", "project:b"]);
    let mut server = start(&db, &[]);
    let payload = |scope: &str| {
        json!({"operation_key":"shared-key","scope":scope,"kind":"source",
            "content":"persistent scoped replay marker","source_uri":"test://native-validation"})
    };
    let (err, original) = call(&server, &token_a, "remember", payload("project:a"));
    assert!(!err, "{original}");
    let original: Value = serde_json::from_str(&original).unwrap();
    let (err, other) = call(&server, &token_b, "remember", payload("project:b"));
    assert!(!err, "{other}");
    let other: Value = serde_json::from_str(&other).unwrap();
    assert_ne!(original["id"], other["id"]);
    let mut conflict = payload("project:a");
    conflict["content"] = json!("conflicting payload");
    let (err, text) = call(&server, &token_a, "remember", conflict);
    assert!(err && text.contains("different payload"), "{text}");

    let barrier = std::sync::Barrier::new(3);
    std::thread::scope(|threads| {
        threads.spawn(|| {
            barrier.wait();
            for index in 0..30 {
                let (err, text) = call(&server, &token_a, "remember", payload("project:a"));
                assert!(!err, "{text}");
                assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), original);
                let (err, text) = call(
                    &server,
                    &token_a,
                    "remember",
                    json!({"scope":"project:a","kind":"source",
                        "operation_key":format!("writer-{index}"),
                        "content":format!("concurrent mutation {index}")}),
                );
                assert!(!err, "{text}");
            }
        });
        threads.spawn(|| {
            barrier.wait();
            for _ in 0..30 {
                let (err, text) = call(
                    &server,
                    &token_a,
                    "recall",
                    json!({"scope":"project:a","query":"persistent scoped replay marker"}),
                );
                assert!(!err, "{text}");
                let result: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(result[0]["memory"]["id"], original["id"]);
                assert!(
                    result
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|row| { row["memory"]["scope"] == "project:a" })
                );
            }
        });
        threads.spawn(|| {
            barrier.wait();
            for _ in 0..30 {
                aether_midnight::memory::maintain(&db, 10).unwrap();
            }
        });
    });
    assert_eq!(
        unsafe { libc::kill(server.child.id() as i32, libc::SIGINT) },
        0
    );
    assert!(server.child.wait().unwrap().success());
    drop(server);
    let restarted = start(&db, &[]);
    let (err, text) = call(&restarted, &token_a, "remember", payload("project:a"));
    assert!(!err, "{text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), original);
    let (err, text) = call(
        &restarted,
        &token_a,
        "list_memories",
        json!({"scope":"project:a","limit":500}),
    );
    assert!(!err, "{text}");
    assert_eq!(serde_json::from_str::<Vec<Value>>(&text).unwrap().len(), 31);
}
