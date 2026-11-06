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

fn raw(port: u16, host: &str, method: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "{method} /mcp HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
    (status, body)
}

fn rpc(server: &Server, id: u64, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
    let (status, text) = raw(
        server.port,
        &format!("127.0.0.1:{}", server.port),
        "POST",
        &body,
    );
    assert_eq!(status, 200, "{text}");
    serde_json::from_str(&text).unwrap()
}

fn call(server: &Server, name: &str, args: Value) -> (bool, String) {
    let r = rpc(
        server,
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
    let args = ["--memory-scope", "project:*"];
    let first = start(&db, &args);
    let init = rpc(
        &first,
        1,
        "initialize",
        json!({"protocolVersion":"2025-11-25"}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    let (err, _) = call(
        &first,
        "remember",
        json!({"scope":"project:demo","kind":"source","content":"The launch codename is Tangerine."}),
    );
    assert!(!err);
    let (denied, msg) = call(
        &first,
        "recall",
        json!({"scope":"user:alice","query":"launch"}),
    );
    assert!(denied && msg.contains("permission denied"), "{msg}");
    drop(first);

    let second = start(&db, &args);
    let (err, text) = call(
        &second,
        "recall",
        json!({"scope":"project:demo","query":"codename"}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("Tangerine"), "{text}");
}

#[test]
fn http_read_only_hides_write_tools_and_rejects_foreign_hosts() {
    let temp = TempDir::new().unwrap();
    let server = start(&temp.path().join("m.sqlite3"), &["--read-only"]);
    let list = rpc(&server, 1, "tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"recall") && names.contains(&"search_code"));
    assert!(!names.contains(&"remember") && !names.contains(&"forget"));
    let (denied, _) = call(&server, "forget", json!({"scope":"project:x","id":1}));
    assert!(denied);
    let (status, _) = raw(server.port, "evil.example", "POST", "{}");
    assert_eq!(status, 403);
    let (status, _) = raw(
        server.port,
        &format!("127.0.0.1:{}", server.port),
        "POST",
        "{bad",
    );
    assert_eq!(status, 400);
    let (status, _) = raw(
        server.port,
        &format!("127.0.0.1:{}", server.port),
        "GET",
        "",
    );
    assert_eq!(status, 405);
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
