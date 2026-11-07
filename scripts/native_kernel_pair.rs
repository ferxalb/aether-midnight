//! Native macOS cross-process kernel comparison. Build as an example in the
//! reference worktree so fixtures use the oldest supported schema. Arguments:
//! baseline binary, candidate binary, private output directory. Run twice into
//! separate directories, sequentially, with no compilation or other benchmark.
//! All decoded responses must match. CPU uses PROC_PIDTASKINFO; peak RSS uses
//! wait4 (bytes on macOS). Sampled resident size is separately labeled. Setup,
//! migration, warmup, and allocator diagnostics are outside acceptance timing.
//! Percentiles retain the repository's ceil((n - 1) * p) definition.
use aether_midnight::{code, memory};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
impl Client {
    fn start(binary: &str, db: &std::path::Path) -> Self {
        let mut child = Command::new(binary)
            .arg("--db")
            .arg(db)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut c = Self {
            child,
            input,
            output,
        };
        c.request("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"native-pair","version":"1"}}));
        c
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
        let mut line = String::new();
        assert!(self.output.read_line(&mut line).unwrap() > 0);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("error").is_none(), "{v}");
        v["result"].clone()
    }
    fn recall(&mut self, scope: &str, query: &str) -> Value {
        let v = self.request(
            "tools/call",
            json!({"name":"recall","arguments":{"scope":scope,"query":query,"limit":10}}),
        );
        assert_eq!(v["isError"], false, "{v}");
        serde_json::from_str(v["content"][0]["text"].as_str().unwrap()).unwrap()
    }
    fn usage(&self) -> (f64, u64) {
        // PROC_PIDTASKINFO: cumulative user/system nanoseconds and resident bytes.
        unsafe extern "C" {
            fn proc_pidinfo(
                pid: i32,
                flavor: i32,
                arg: u64,
                buffer: *mut libc::c_void,
                size: i32,
            ) -> i32;
        }
        let mut data = [0u64; 12];
        let n = unsafe { proc_pidinfo(self.child.id() as i32, 4, 0, data.as_mut_ptr().cast(), 96) };
        assert_eq!(n, 96);
        ((data[2] + data[3]) as f64 / 1e6, data[1])
    }
}
fn dist(v: &[f64]) -> Value {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    json!({"p50":s[((s.len()-1) as f64*0.5).ceil() as usize],"p95":s[((s.len()-1) as f64*0.95).ceil() as usize],"p99":s[((s.len()-1) as f64*0.99).ceil() as usize],"samples":v})
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = std::path::Path::new(&args[3]);
    std::fs::create_dir_all(root).unwrap();
    if args.get(4).is_some_and(|value| value == "--parse") {
        let source = std::fs::read(&args[5]).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let start = Instant::now();
        let tree = parser.parse(&source, None).unwrap();
        let mut errors = Vec::new();
        fn visit(node: tree_sitter::Node<'_>, errors: &mut Vec<Value>) {
            if node.is_error() || node.is_missing() {
                errors.push(json!({"kind":node.kind(),"start_row":node.start_position().row + 1,
                    "start_column":node.start_position().column + 1,"end_row":node.end_position().row + 1}));
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                visit(child, errors);
            }
        }
        visit(tree.root_node(), &mut errors);
        std::fs::write(root.join("parse.json"), serde_json::to_vec_pretty(&json!({
            "has_error":tree.root_node().has_error(),"elapsed_ms":start.elapsed().as_secs_f64()*1000.,"errors":errors
        })).unwrap()).unwrap();
        return;
    }
    if args.get(4).is_some_and(|value| value == "--code") {
        frozen_code(&args, root);
        return;
    }
    let scopes = ["bench:alpha", "bench:beta", "bench:gamma", "bench:delta"];
    let mut reports = Vec::new();
    for size in [10000usize, 100000] {
        for namespaces in [1usize, 4] {
            for tied in [false, true] {
                let temp = tempfile::tempdir_in(root).unwrap();
                let db = temp.path().join("fixture.db");
                let mut cumulative = Vec::new();
                let mut total = 0.;
                for rank in 0..3000 {
                    total += 1. / (rank as f64 + 1.);
                    cumulative.push(total);
                }
                let mut state = 0x9E37_79B9_7F4A_7C15u64;
                let mut next = || {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state
                };
                let mut batch = Vec::new();
                let mut planted = String::new();
                for i in 0..size {
                    let length = 12 + (next() % 29) as usize;
                    let words = (0..length)
                        .map(|_| {
                            let target = (next() >> 11) as f64 / (1u64 << 53) as f64 * total;
                            format!("w{}", cumulative.partition_point(|v| *v < target).min(2999))
                        })
                        .collect::<Vec<_>>();
                    if i == size / 2 {
                        planted = words[2..5].join(" ");
                    }
                    batch.push(memory::MemoryInput {
                        scope: scopes[i % namespaces].into(),
                        kind: "source".into(),
                        content: if tied {
                            format!("memoryload{i:06} namespace workload record {i}")
                        } else {
                            words.join(" ")
                        },
                        source_uri: Some("benchmark://zipf".into()),
                        source_id: None,
                        supersedes_id: None,
                        observed_at: None,
                        valid_from: None,
                        valid_to: None,
                    });
                    if batch.len() == 500 || i + 1 == size {
                        memory::remember_batch(&db, &batch).unwrap();
                        batch.clear();
                    }
                }
                code::wal_checkpoint(&db).unwrap();
                let mut clients = Vec::new();
                for (i, binary) in [&args[1], &args[1], &args[2]].iter().enumerate() {
                    let copy = temp.path().join(format!("{i}.db"));
                    std::fs::copy(&db, &copy).unwrap();
                    clients.push(Client::start(binary, &copy));
                }
                let queries = if tied {
                    vec!["namespace".to_string()]
                } else {
                    vec![
                        "w0".into(),
                        "w10".into(),
                        "w2000".into(),
                        "w3 w5".into(),
                        "w10 w2000".into(),
                        "w50 w300 w2000".into(),
                        planted.clone(),
                        format!("what is the {planted} and w2000"),
                    ]
                };
                for query in queries {
                    for c in &mut clients {
                        c.recall(scopes[0], &query);
                    }
                    let before = clients.iter().map(Client::usage).collect::<Vec<_>>();
                    let mut wall = [Vec::new(), Vec::new(), Vec::new()];
                    let mut rss = [0u64; 3];
                    for sample in 0..200 {
                        let scope = scopes[sample % namespaces];
                        let mut results = [Value::Null, Value::Null, Value::Null];
                        for offset in 0..3 {
                            let i = (sample + offset) % 3;
                            let start = Instant::now();
                            results[i] = clients[i].recall(scope, &query);
                            wall[i].push(start.elapsed().as_secs_f64() * 1000.);
                            rss[i] = rss[i].max(clients[i].usage().1);
                        }
                        assert_eq!(results[0], results[1]);
                        assert_eq!(results[0], results[2]);
                    }
                    let cpu = clients
                        .iter()
                        .enumerate()
                        .map(|(i, c)| (c.usage().0 - before[i].0) / 200.)
                        .collect::<Vec<_>>();
                    reports.push(json!({"size":size,"namespaces":namespaces,"tied":tied,"query":query,"samples":200,"baseline":dist(&wall[0]),"control":dist(&wall[1]),"candidate":dist(&wall[2]),"cpu_ms_query":cpu,"sampled_rss_bytes":rss}));
                    std::fs::write(
                        root.join("paired.json"),
                        serde_json::to_vec_pretty(&reports).unwrap(),
                    )
                    .unwrap();
                }
                let mut peaks = Vec::new();
                for c in clients {
                    let Client { child, input, .. } = c;
                    drop(input);
                    let mut status = 0;
                    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
                    let pid = unsafe { libc::wait4(child.id() as i32, &mut status, 0, &mut usage) };
                    assert_eq!(pid, child.id() as i32);
                    assert_eq!(status, 0);
                    peaks.push(usage.ru_maxrss);
                }
                std::fs::write(
                    root.join(format!("peak-{size}-{namespaces}-{tied}.json")),
                    serde_json::to_vec(&json!({"peak_rss_bytes":peaks})).unwrap(),
                )
                .unwrap();
            }
        }
    }
}

// Correctness-only companion: --code CLEAN_ARCHIVE. Run from repository root.
// Use one identical corpus for both implementations, excluding benchmark labels
// from the held-out archive. No latency from this mode is an acceptance sample.
fn frozen_code(args: &[String], root: &std::path::Path) {
    let mut rows = Vec::new();
    for (suite, corpus) in [
        ("benchmark/suites/dev.json", "benchmark/retrieval_fixture"),
        ("benchmark/suites/heldout.json", args[5].as_str()),
        (
            "benchmark/retrieval_queries.json",
            "benchmark/retrieval_fixture",
        ),
    ] {
        let corpus = std::fs::canonicalize(corpus).unwrap();
        let temp = tempfile::tempdir_in(root).unwrap();
        let db = temp.path().join("code.db");
        let report = code::index_repository(
            &db,
            &corpus,
            false,
            true,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        // Preserve structural failures explicitly; do not turn an equality
        // result on an incomplete index into a structural acceptance pass.
        assert!(report.catalog_complete, "{report:?}");
        rows.push(json!({"suite":suite,"index_complete":report.complete,
            "structural_complete":report.structural_complete,"skipped":report.skipped}));
        code::wal_checkpoint(&db).unwrap();
        let mut clients = Vec::new();
        for (index, binary) in [&args[1], &args[1], &args[2]].iter().enumerate() {
            let copy = temp.path().join(format!("{index}.db"));
            std::fs::copy(&db, &copy).unwrap();
            clients.push(Client::start(binary, &copy));
        }
        let fixture: Value = serde_json::from_slice(&std::fs::read(suite).unwrap()).unwrap();
        for query in fixture["queries"].as_array().unwrap() {
            let mut responses = Vec::new();
            for client in &mut clients {
                let response = client.request(
                    "tools/call",
                    json!({"name":"search_code","arguments":{
                        "repo_path":corpus,"query":query["query"],"limit":10}}),
                );
                assert_eq!(response["isError"], false, "{response}");
                responses.push(
                    serde_json::from_str::<Value>(response["content"][0]["text"].as_str().unwrap())
                        .unwrap(),
                );
            }
            assert_eq!(responses[0], responses[1], "{suite}: {}", query["id"]);
            assert_eq!(responses[0], responses[2], "{suite}: {}", query["id"]);
            rows.push(json!({"suite":suite,"query_id":query["id"],
                "response":responses[0],"identical":true}));
        }
        for client in clients {
            let Client {
                mut child, input, ..
            } = client;
            drop(input);
            assert!(child.wait().unwrap().success());
        }
    }
    std::fs::write(
        root.join("code-frozen.json"),
        serde_json::to_vec_pretty(&rows).unwrap(),
    )
    .unwrap();
}
