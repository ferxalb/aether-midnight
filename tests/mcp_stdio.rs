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
    assert!(names.contains(&"refresh_indexed_file"));
    assert!(names.contains(&"remember"));
    assert!(names.contains(&"remember_batch"));
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

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"remember_batch","arguments":{"memories":[
                {"scope":"project:mcp","kind":"source","content":"persistent session atlas alpha token","source_uri":"mcp://alpha"},
                {"scope":"project:mcp","kind":"source","content":"persistent session atlas beta token","source_uri":"mcp://beta"}
            ]}}
        }),
    );
    let batch = read_response(&mut stdout);
    assert_eq!(batch["result"]["isError"], false);
    let batch_rows: Value =
        serde_json::from_str(batch["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(batch_rows.as_array().unwrap().len(), 2);
    let first_memory_id = batch_rows[0]["id"].as_i64().unwrap();

    for request_id in [5, 6] {
        write_request(
            &mut stdin,
            &json!({
                "jsonrpc":"2.0","id":request_id,"method":"tools/call",
                "params":{"name":"recall","arguments":{"scope":"project:mcp","query":"persistent session atlas alpha token","limit":10}}
            }),
        );
        let recalled = read_response(&mut stdout);
        let recalled_rows: Value =
            serde_json::from_str(recalled["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(recalled_rows[0]["memory"]["id"], first_memory_id);
        assert_eq!(recalled_rows[0]["match_reason"], "memory_exact_phrase");
    }

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":7,"method":"tools/call",
            "params":{"name":"list_memories","arguments":{"scope":"project:mcp","limit":10}}
        }),
    );
    let listed = read_response(&mut stdout);
    let listed_rows: Value =
        serde_json::from_str(listed["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(listed_rows.as_array().unwrap().len(), 2);

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":8,"method":"tools/call",
            "params":{"name":"forget","arguments":{"scope":"project:mcp","id":first_memory_id}}
        }),
    );
    let forgotten = read_response(&mut stdout);
    assert_eq!(
        forgotten["result"]["content"][0]["text"],
        "{\"forgotten\":true}"
    );

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":9,"method":"tools/call",
            "params":{"name":"recall","arguments":{"scope":"project:mcp","query":"persistent session atlas alpha token","limit":10}}
        }),
    );
    let after_forget = read_response(&mut stdout);
    let after_forget_rows: Value = serde_json::from_str(
        after_forget["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(
        after_forget_rows
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["memory"]["id"] != first_memory_id)
    );

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":10,"method":"tools/call",
            "params":{"name":"remember_batch","arguments":{"memories":[
                {"scope":"project:mcp","kind":"source","content":"rollback atomic marker first","source_uri":"mcp://rollback"},
                {"scope":"project:mcp","kind":"derived","content":"rollback atomic marker second","source_id":999}
            ]}}
        }),
    );
    let failed_batch = read_response(&mut stdout);
    assert_eq!(failed_batch["result"]["isError"], true);

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":11,"method":"tools/call",
            "params":{"name":"recall","arguments":{"scope":"project:mcp","query":"rollback atomic marker","limit":10}}
        }),
    );
    let rolled_back = read_response(&mut stdout);
    let rolled_back_rows: Value = serde_json::from_str(
        rolled_back["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(rolled_back_rows.as_array().unwrap().is_empty());

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":12,"method":"tools/call",
            "params":{"name":"recall","arguments":{"scope":"project:mcp","query":"!!!","limit":10}}
        }),
    );
    let no_tokens = read_response(&mut stdout);
    assert_eq!(no_tokens["result"]["isError"], false);
    let no_tokens_rows: Value =
        serde_json::from_str(no_tokens["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(no_tokens_rows.as_array().unwrap().is_empty());

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":13,"method":"tools/call",
            "params":{"name":"index_repository","arguments":{"repo_path":repo}}
        }),
    );
    let indexed = read_response(&mut stdout);
    assert_eq!(indexed["result"]["isError"], false);
    let indexed_report: Value =
        serde_json::from_str(indexed["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(indexed_report["complete"], true);

    fs::write(
        repo.join("src/lib.rs"),
        "pub fn mcp_refreshed_file_marker() {}\n",
    )
    .unwrap();
    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":14,"method":"tools/call",
            "params":{"name":"refresh_indexed_file","arguments":{"repo_path":repo,"path":"src/lib.rs"}}
        }),
    );
    let refreshed = read_response(&mut stdout);
    assert_eq!(refreshed["result"]["isError"], false);
    let refresh_report: Value =
        serde_json::from_str(refreshed["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(refresh_report["operation"], "updated");
    assert_eq!(refresh_report["catalog_complete"], false);
    assert_eq!(refresh_report["reconciliation_required"], true);

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":15,"method":"tools/call",
            "params":{"name":"search_code","arguments":{"repo_path":repo,"query":"mcp_refreshed_file_marker","limit":5}}
        }),
    );
    let refreshed_search = read_response(&mut stdout);
    let refreshed_results: Value = serde_json::from_str(
        refreshed_search["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(refreshed_results["results"][0]["freshness"], "fresh");

    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn mcp_recall_finds_middle_exact_phrase_and_reports_candidate_coverage() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("mcp-middle-recall.sqlite3");
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
    read_response(&mut stdout);
    write_request(
        &mut stdin,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );

    let memories = (0..100)
        .map(|index| {
            json!({
                "scope":"project:mcp-middle",
                "kind":"source",
                "content":if index == 50 { "where is nebula".to_string() } else { format!("nebula low value background entry {index:03}") },
                "source_uri":format!("benchmark://mcp-middle/{index:03}")
            })
        })
        .collect::<Vec<_>>();
    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"remember_batch","arguments":{"memories":memories}}
        }),
    );
    let remembered = read_response(&mut stdout);
    assert_eq!(remembered["result"]["isError"], false);

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"recall","arguments":{"scope":"project:mcp-middle","query":"where is nebula","limit":10}}
        }),
    );
    let recalled = read_response(&mut stdout);
    let rows: Value =
        serde_json::from_str(recalled["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(rows[0]["memory"]["content"], "where is nebula");
    assert_eq!(rows[0]["match_reason"], "memory_exact_phrase");
    assert_eq!(rows[0]["candidate_coverage"], "exact_top_k");

    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn dots_persistent_memory_lifecycle_preserves_evidence() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("dots.sqlite3");
    let mut child = Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .args(["--db", database.to_str().unwrap(), "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    write_request(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"dots-test","version":"1"}}}),
    );
    read_response(&mut stdout);
    let mut sequence = 1;
    let mut timings = Vec::new();
    let mut call = |name: &str, arguments: Value| {
        sequence += 1;
        let started = std::time::Instant::now();
        write_request(
            &mut stdin,
            &json!({"jsonrpc":"2.0","id":sequence,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}}),
        );
        let response = read_response(&mut stdout);
        timings.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(response["result"]["isError"], false, "{response}");
        serde_json::from_str::<Value>(response["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap()
    };
    let original = call(
        "remember",
        json!({"scope":"project:dots","kind":"derived",
        "content":"orbital launch window monday","source_uri":"dots://decision/1",
        "valid_from":"2025-01-01T00:00:00Z"}),
    );
    let original_id = original["id"].as_i64().unwrap();
    let recalled = call(
        "recall",
        json!({"scope":"project:dots","query":"orbital launch window"}),
    );
    assert_eq!(recalled[0]["memory"]["id"], original_id);
    assert_eq!(recalled[0]["memory"]["source_uri"], "dots://decision/1");
    let updated = call(
        "remember",
        json!({"scope":"project:dots","kind":"derived",
        "content":"orbital launch window friday","source_uri":"dots://decision/2",
        "supersedes_id":original_id,"valid_from":"2025-02-01T00:00:00Z"}),
    );
    let updated_id = updated["id"].as_i64().unwrap();
    let current = call(
        "recall",
        json!({"scope":"project:dots","query":"orbital launch window"}),
    );
    assert_eq!(current.as_array().unwrap().len(), 1);
    assert_eq!(current[0]["memory"]["id"], updated_id);
    assert_eq!(current[0]["memory"]["source_uri"], "dots://decision/2");
    assert_eq!(current[0]["memory"]["supersedes_id"], original_id);
    let isolated = call(
        "recall",
        json!({"scope":"project:other","query":"orbital launch window"}),
    );
    assert!(isolated.as_array().unwrap().is_empty());
    call("forget", json!({"scope":"project:dots","id":updated_id}));
    let forgotten = call(
        "recall",
        json!({"scope":"project:dots","query":"orbital launch window"}),
    );
    assert!(forgotten.as_array().unwrap().is_empty());
    eprintln!("Dots lifecycle round-trip milliseconds: {timings:?}");
    drop(stdin);
    assert!(child.wait().unwrap().success());
    let conn = aether_midnight::db::open_db(&database).unwrap();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
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

#[test]
fn killed_writer_rolls_back_memory_counters_and_schema() {
    const CHILD_DB: &str = "MIDNIGHT_CRASH_TEST_DB";
    if let Some(path) = std::env::var_os(CHILD_DB) {
        let conn = aether_midnight::db::open_db(std::path::Path::new(&path)).unwrap();
        conn.execute_batch(
            "BEGIN IMMEDIATE;
            UPDATE memories SET deleted_at='2025-01-01T00:00:00Z';
            CREATE TABLE interrupted_schema(value TEXT);",
        )
        .unwrap();
        println!("transaction-ready");
        std::io::stdout().flush().unwrap();
        std::thread::park();
        unreachable!();
    }
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("crash.sqlite3");
    aether_midnight::memory::remember(
        &database,
        "project:crash",
        "source",
        "durable provenance marker",
        Some("dots://durable"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "killed_writer_rolls_back_memory_counters_and_schema",
            "--nocapture",
        ])
        .env(CHILD_DB, &database)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert!(stdout.read_line(&mut line).unwrap() > 0);
        if line.trim() == "transaction-ready" {
            break;
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let conn = aether_midnight::db::open_db(&database).unwrap();
    let state: (String, i64, bool, String) = conn
        .query_row(
            "SELECT (SELECT source_uri FROM memories WHERE deleted_at IS NULL),
         (SELECT active FROM memory_scope_counts WHERE scope=''),
         EXISTS(SELECT 1 FROM sqlite_master WHERE name='interrupted_schema'),
         (SELECT integrity_check FROM pragma_integrity_check)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(state, ("dots://durable".into(), 1, false, "ok".into()));
    assert_eq!(
        aether_midnight::memory::recall(&database, "project:crash", "durable provenance", None, 10)
            .unwrap()
            .len(),
        1
    );
}
