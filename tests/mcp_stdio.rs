use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
fn mcp_stdio_handshake_lists_tools_and_returns_grounded_result() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn stdio_grounded_marker() {}\n",
    )
    .unwrap();
    let database = temp.path().join("mcp.sqlite3");
    let mut child = Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .arg("--db")
        .arg(&database)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}
        }),
    );
    let initialized = read_response(&mut stdout);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    write_request(
        &mut stdin,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    write_request(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    let tools = read_response(&mut stdout);
    let names = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"search_code"));
    assert!(names.contains(&"remember"));
    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"search_code","arguments":{"repo_path":repo,"query":"stdio_grounded_marker","limit":5}}
        }),
    );
    let result = read_response(&mut stdout);
    let text = result["result"]["content"][0]["text"].as_str().unwrap();
    let rows: Value = serde_json::from_str(text).unwrap();
    assert_eq!(rows["results"][0]["path"], "src/lib.rs");
    assert_eq!(rows["results"][0]["freshness"], "fresh_unindexed");
    assert!(rows["estimated_tokens"].as_u64().unwrap() <= rows["token_budget"].as_u64().unwrap());
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

fn write_request(stdin: &mut impl Write, value: &Value) {
    serde_json::to_writer(&mut *stdin, value).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
}

fn read_response(stdout: &mut impl BufRead) -> Value {
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
