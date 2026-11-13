//! Native measurement and correctness harness. It replaces the former Python
//! tooling. Every measurement drives the compiled release `aether-midnight`
//! binary through a real entry point (`--db DB mcp` or a CLI subcommand) and
//! reports wall time, CPU time (user+system) and peak RSS of Midnight itself
//! from `wait4`. Fixture generation, builds and client work are not measured.
#[path = "midnight-harness/release.rs"]
mod release;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

#[derive(Default, Clone, Copy)]
struct Usage {
    wall_s: f64,
    cpu_user_s: f64,
    cpu_sys_s: f64,
    peak_rss_bytes: u64,
}
impl Usage {
    fn json(&self) -> Value {
        json!({"wall_seconds":self.wall_s,"cpu_user_seconds":self.cpu_user_s,
            "cpu_system_seconds":self.cpu_sys_s,"cpu_total_seconds":self.cpu_user_s+self.cpu_sys_s,
            "peak_rss_bytes":self.peak_rss_bytes})
    }
}

fn reap(pid: u32) -> Result<(i32, Usage)> {
    let mut status = 0;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::wait4(pid as i32, &mut status, 0, &mut ru) } < 0 {
        bail!("wait4 failed: {}", std::io::Error::last_os_error());
    }
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    let rss = ru.ru_maxrss as u64 * if cfg!(target_os = "macos") { 1 } else { 1024 };
    Ok((
        libc::WEXITSTATUS(status),
        Usage {
            wall_s: 0.0,
            cpu_user_s: tv(ru.ru_utime),
            cpu_sys_s: tv(ru.ru_stime),
            peak_rss_bytes: rss,
        },
    ))
}

fn binary_path(arg: Option<&str>) -> Result<PathBuf> {
    let p = arg
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(ROOT).join("target/release/aether-midnight"));
    if !p.is_file() {
        bail!(
            "binary {} missing; run `cargo build --release --locked` first",
            p.display()
        );
    }
    Ok(p)
}

/// One-shot CLI invocation, measured as a whole process.
fn run_cli(bin: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<(Value, Usage)> {
    let started = Instant::now();
    let mut child = Command::new(bin)
        .args(args)
        .envs(envs.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t = std::thread::spawn(move || {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut err, &mut s).ok();
        s
    });
    let mut text = String::new();
    std::io::Read::read_to_string(&mut out, &mut text)?;
    let (code, mut usage) = reap(child.id())?;
    usage.wall_s = started.elapsed().as_secs_f64();
    let stderr = t.join().unwrap_or_default();
    if code != 0 {
        bail!("{args:?} exited {code}: {}", stderr.trim());
    }
    Ok((
        serde_json::from_str(&text).with_context(|| format!("{args:?} output"))?,
        usage,
    ))
}

/// Persistent `--db DB mcp` session with exactly one request in flight.
struct Mcp {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    started: Instant,
    next_id: u64,
    requests: u64,
}
impl Mcp {
    fn start(bin: &Path, db: &Path) -> Result<Self> {
        let started = Instant::now();
        let mut child = Command::new(bin)
            .arg("--db")
            .arg(db)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut m = Self {
            child,
            input,
            output,
            started,
            next_id: 0,
            requests: 0,
        };
        m.rpc(
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{},
            "clientInfo":{"name":"midnight-harness","version":"1"}}),
        )?;
        Ok(m)
    }
    fn rpc(&mut self, method: &str, params: Value) -> Result<(f64, Value)> {
        self.next_id += 1;
        self.requests += 1;
        let line = json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params});
        let t = Instant::now();
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{line}")?;
        input.flush()?;
        let mut resp = String::new();
        if self.output.read_line(&mut resp)? == 0 {
            bail!("MCP server exited during {method}");
        }
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let v: Value = serde_json::from_str(&resp)?;
        if v.get("error").is_some() {
            bail!("MCP {method} error: {}", v["error"]);
        }
        Ok((ms, v["result"].clone()))
    }
    fn call(&mut self, name: &str, args: Value) -> Result<(f64, Value)> {
        let (ms, r) = self.rpc("tools/call", json!({"name":name,"arguments":args}))?;
        let text = r["content"][0]["text"].as_str().unwrap_or("");
        if r["isError"].as_bool().unwrap_or(false) {
            bail!("MCP {name} failed: {text}");
        }
        Ok((ms, serde_json::from_str(text)?))
    }
    fn close(mut self) -> Result<Usage> {
        if let Some(mut i) = self.input.take() {
            let _ = writeln!(i, r#"{{"jsonrpc":"2.0","method":"exit"}}"#);
        }
        let (code, mut usage) = reap(self.child.id())?;
        usage.wall_s = self.started.elapsed().as_secs_f64();
        if code != 0 {
            bail!("MCP server returned {code}");
        }
        Ok(usage)
    }
}

fn percentile(values: &[f64], p: f64) -> Option<f64> {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    (!v.is_empty()).then(|| v[(((v.len() - 1) as f64 * p).ceil() as usize).min(v.len() - 1)])
}
fn distribution(v: &[f64]) -> Value {
    json!({"sample_count":v.len(),"p50_ms":percentile(v,0.5),"p95_ms":percentile(v,0.95),
        "p99_ms":percentile(v,0.99),"samples_ms":v})
}

fn read_json(p: &Path) -> Result<Value> {
    Ok(serde_json::from_str(
        &std::fs::read_to_string(p).with_context(|| p.display().to_string())?,
    )?)
}

// ---------- fixture generation ----------
fn build_file(index: usize, size: usize, seed: u64) -> Result<Vec<u8>> {
    let mut lines = vec![format!("pub fn filler_{index:05}() -> usize {{ {index} }}")];
    if index == 0 {
        lines.push("pub fn benchmark_target_00000() -> usize { 0 }".into());
    }
    if index == 4 || index == 5 {
        lines.push("pub fn common_retry_budget() -> usize { 3 }".into());
    }
    lines.push(format!("// fixture_path=src/{index:05}.rs"));
    if index == 2 {
        lines.push("// delta cache invalidation token".into());
    }
    if index == 3 {
        lines.push("// stale revision consistency".into());
    }
    let mut content = (lines.join("\n") + "\n").into_bytes();
    content.extend_from_slice(b"// payload ");
    if content.len() >= size {
        bail!("fixture prefix exceeds file target ({index})");
    }
    let pattern = format!("seed{seed:08}-file{index:05}-").into_bytes();
    let padding = size - content.len() - 1;
    content.extend(pattern.iter().cycle().take(padding));
    content.push(b'\n');
    assert_eq!(content.len(), size);
    Ok(content)
}

fn generate_fixture(output: &Path) -> Result<Value> {
    let spec = read_json(&Path::new(ROOT).join("benchmark/manifest.json"))?;
    let f = &spec["fixture"];
    let (count, total, seed) = (
        f["eligible_files"].as_u64().unwrap() as usize,
        f["eligible_bytes"].as_u64().unwrap() as usize,
        f["seed"].as_u64().unwrap(),
    );
    if output.exists() {
        bail!(
            "refusing to overwrite existing fixture directory: {}",
            output.display()
        );
    }
    std::fs::create_dir_all(output.join("src"))?;
    let (base, rem) = (total / count, total % count);
    let mut bytes = 0;
    for i in 0..count {
        let c = build_file(i, base + usize::from(i < rem), seed)?;
        bytes += c.len();
        std::fs::write(output.join(format!("src/{i:05}.rs")), c)?;
    }
    if bytes != total {
        bail!("fixture size mismatch");
    }
    Ok(json!({"root":output,"eligible_files":count,"eligible_bytes":bytes,"seed":seed}))
}

// ---------- license inventory ----------
/// Components that are not Cargo packages: the runtime and model that Midnight fetches on explicit
/// user command (`runtime install`, `models pull`). Midnight's own release artifacts redistribute
/// none of them; the pins below are the values the binary verifies at install/load time.
fn non_crate_components() -> Value {
    json!([
        {
            "kind": "native-runtime",
            "name": "ONNX Runtime",
            "version": "1.23.2",
            "platform": "macos-x86_64",
            "license": "MIT",
            "license_text": "LICENSE in the release archive (copied beside the library by `runtime install`)",
            "third_party_notices": "ThirdPartyNotices.txt in the release archive (copied beside the library); it lists the runtime's own bundled third-party licences",
            "source": "https://github.com/microsoft/onnxruntime/releases/download/v1.23.2/onnxruntime-osx-x86_64-1.23.2.tgz",
            "archive_sha256": "d10359e16347b57d9959f7e80a225a5b4a66ed7d7e007274a15cae86836485a6",
            "library_sha256": "8c9c78de65ea3786f987c0d980e9c1b13a3a5fbc6b3e2965ba05b450e6e4c054",
            "obtained_by": "`aether-midnight runtime install` (explicit, SHA-256 verified); not bundled in Midnight binaries",
            "linked": "dynamically at run time (ort-load-dynamic), only for local embeddings"
        },
        {
            "kind": "model-weights",
            "name": "intfloat/multilingual-e5-small",
            "revision": "614241f622f53c4eeff9890bdc4f31cfecc418b3",
            "license": "MIT",
            "license_evidence": "Hugging Face model card metadata `license: mit` at the pinned revision",
            "source": "https://huggingface.co/intfloat/multilingual-e5-small",
            "files": [
                {"path":"onnx/model.onnx","bytes":470268510,"sha256":"ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665"},
                {"path":"tokenizer.json","bytes":17082730,"sha256":"0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39"},
                {"path":"config.json","bytes":655,"sha256":"69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959"},
                {"path":"special_tokens_map.json","bytes":167,"sha256":"d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7"},
                {"path":"tokenizer_config.json","bytes":443,"sha256":"a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b"}
            ],
            "obtained_by": "`aether-midnight models pull` (explicit, pinned revision, SHA-256 verified); not bundled",
            "citation": "Wang et al., Multilingual E5 Text Embeddings: A Technical Report, arXiv:2402.05672"
        },
        {
            "kind": "remote-service",
            "name": "OpenRouter embeddings API",
            "license": "service terms (no code or weights are redistributed)",
            "source": "https://openrouter.ai/api/v1/embeddings",
            "note": "used only when `--provider openrouter` is selected; text is sent to the provider under the user's own account"
        },
        {
            "kind": "remote-service",
            "name": "Rainy-compatible embeddings API",
            "license": "service terms (no code or weights are redistributed)",
            "source": "endpoint chosen by the user (`--endpoint`) or the rainy-sdk default",
            "note": "used only when `--provider rainy` is selected; transport is the Apache-2.0 `rainy-sdk` crate listed under packages"
        }
    ])
}

fn license_inventory(output: &Path, force: bool) -> Result<Value> {
    if output.exists() && !force {
        bail!(
            "refusing to overwrite existing inventory: {}",
            output.display()
        );
    }
    let out = Command::new("cargo")
        .args(["metadata", "--locked", "--format-version", "1"])
        .current_dir(ROOT)
        .output()?;
    if !out.status.success() {
        bail!("cargo metadata failed");
    }
    let m: Value = serde_json::from_slice(&out.stdout)?;
    let root = m["resolve"]["root"].as_str().unwrap();
    let node = m["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == root)
        .unwrap();
    let direct: BTreeSet<&str> = node["deps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["pkg"].as_str())
        .collect();
    let mut entries: Vec<Value> = m["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| !p["source"].is_null())
        .map(|p| {
            json!({"name":p["name"],"version":p["version"],
            "license":if p["license"].is_null(){json!("UNDECLARED")}else{p["license"].clone()},
            "license_file":p["license_file"],"source":p["source"],
            "direct_dependency":direct.contains(p["id"].as_str().unwrap())})
        })
        .collect();
    entries.sort_by(|a, b| {
        (a["name"].as_str(), a["version"].as_str())
            .cmp(&(b["name"].as_str(), b["version"].as_str()))
    });
    let doc = json!({"schema_version":2,"generated_from":"Cargo.lock via cargo metadata --locked",
        "package_count":entries.len(),"packages":entries,
        "non_crate_components":non_crate_components()});
    std::fs::write(output, serde_json::to_string_pretty(&doc)? + "\n")?;
    Ok(json!({"wrote_packages":entries.len(),"output":output}))
}

// ---------- memory-quality oracle (persistent MCP) ----------
fn fixture_records(fx: &Value, reverse: bool) -> Result<Vec<Value>> {
    let mut recs: Vec<Value> = fx["records"].as_array().cloned().unwrap_or_default();
    if let Some(bg) = fx.get("background_collection").filter(|v| v.is_object()) {
        for i in 0..bg["count"].as_u64().unwrap() {
            recs.push(
                json!({"key":format!("background-{i:03}"),"scope":bg["scope"],"kind":"source",
                "content":bg["template"].as_str().unwrap().replace("{index}",&i.to_string()),
                "source_uri":format!("benchmark://background/{i:03}")}),
            );
        }
    }
    for c in fx["generated_collections"].as_array().into_iter().flatten() {
        let id = c["id"].as_str().unwrap();
        let term = c["query_term"].as_str().unwrap();
        for i in 0..c["count"].as_u64().unwrap() {
            let target = i == c["target_position"].as_u64().unwrap();
            let content = if target {
                c["target_template"]
                    .as_str()
                    .unwrap()
                    .replace("{term}", term)
            } else {
                c["background_template"]
                    .as_str()
                    .unwrap()
                    .replace("{term}", term)
                    .replace("{index}", &i.to_string())
            };
            recs.push(json!({"key":if target{format!("{id}-target")}else{format!("{id}-background-{i:04}")},
                "scope":c["scope"],"kind":c.get("kind").cloned().unwrap_or(json!("source")),
                "content":content,"source_uri":format!("benchmark://{id}/{i:04}")}));
        }
    }
    if reverse {
        recs.reverse();
    }
    order_fixture_dependencies(recs)
}

/// Respect source/supersession foreign keys while retaining the requested order among ready
/// records. A raw reverse is not insertable when a derived record depends on an earlier source.
fn order_fixture_dependencies(records: Vec<Value>) -> Result<Vec<Value>> {
    let mut by_key = HashMap::new();
    for (index, record) in records.iter().enumerate() {
        let key = record["key"]
            .as_str()
            .context("fixture record needs a string key")?;
        if by_key.insert(key, index).is_some() {
            bail!("duplicate fixture record key {key:?}");
        }
    }
    let mut pending = vec![0usize; records.len()];
    let mut dependents = vec![Vec::new(); records.len()];
    for (index, record) in records.iter().enumerate() {
        for field in ["source_key", "supersedes_key"] {
            if let Some(key) = record.get(field).and_then(Value::as_str) {
                let parent = *by_key
                    .get(key)
                    .with_context(|| format!("fixture {field} refers to missing key {key:?}"))?;
                pending[index] += 1;
                dependents[parent].push(index);
            }
        }
    }
    let mut ready: BTreeSet<usize> = pending
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (*n == 0).then_some(i))
        .collect();
    let mut order = Vec::with_capacity(records.len());
    while let Some(index) = ready.pop_first() {
        order.push(index);
        for child in &dependents[index] {
            pending[*child] -= 1;
            if pending[*child] == 0 {
                ready.insert(*child);
            }
        }
    }
    if order.len() != records.len() {
        bail!("fixture source/supersession dependencies contain a cycle");
    }
    drop(by_key);
    let mut records: Vec<Option<Value>> = records.into_iter().map(Some).collect();
    Ok(order
        .into_iter()
        .map(|i| records[i].take().expect("each index is emitted once"))
        .collect())
}

fn valid_at(m: &Value, at: Option<&str>) -> bool {
    let Some(at) = at else { return true };
    m["valid_from"].as_str().is_none_or(|f| f <= at)
        && m["valid_to"].as_str().is_none_or(|t| t > at)
}

fn memory_quality(
    bin: &Path,
    fixture: &Path,
    split: &str,
    reverse: bool,
    repeats: usize,
    db: &Path,
) -> Result<Value> {
    let fx = read_json(fixture)?;
    let records = fixture_records(&fx, reverse)?;
    let queries: Vec<&Value> = fx["queries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|q| split == "all" || q["split"] == split)
        .collect();
    let mut mcp = Mcp::start(bin, db)?;
    let (mut key_to_id, mut id_to_key) = (
        HashMap::<String, Value>::new(),
        HashMap::<String, String>::new(),
    );
    let mut remember_ms = vec![];
    for r in records {
        let mut args = serde_json::Map::new();
        for k in [
            "scope",
            "kind",
            "content",
            "source_uri",
            "observed_at",
            "valid_from",
            "valid_to",
        ] {
            if let Some(v) = r.get(k) {
                args.insert(k.into(), v.clone());
            }
        }
        if let Some(k) = r.get("source_key").and_then(Value::as_str) {
            args.insert("source_id".into(), key_to_id[k].clone());
        }
        if let Some(k) = r.get("supersedes_key").and_then(Value::as_str) {
            args.insert("supersedes_id".into(), key_to_id[k].clone());
        }
        let (ms, mem) = mcp.call("remember", Value::Object(args))?;
        remember_ms.push(ms);
        let id = mem["id"].to_string();
        key_to_id.insert(r["key"].as_str().unwrap().to_string(), mem["id"].clone());
        id_to_key.insert(id, r["key"].as_str().unwrap().to_string());
    }
    for key in fx["forget_keys"].as_array().into_iter().flatten() {
        let key = key.as_str().unwrap();
        let scope = fx["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap()["scope"]
            .clone();
        let (_, f) = mcp.call("forget", json!({"scope":scope,"id":key_to_id[key]}))?;
        if f["forgotten"] != true {
            bail!("fixture forget did not suppress {key}");
        }
    }
    let (mut rows, mut recall_ms) = (vec![], vec![]);
    for q in &queries {
        let mut last = Value::Null;
        for _ in 0..repeats {
            let (ms, res) = mcp.call(
                "recall",
                json!({"scope":q["scope"],"query":q["query"],"at":q.get("at"),"limit":10}),
            )?;
            recall_ms.push(ms);
            last = res;
        }
        let results = last.as_array().cloned().unwrap_or_default();
        let actual: Vec<String> = results
            .iter()
            .map(|i| {
                let id = i["memory"]["id"].to_string();
                id_to_key
                    .get(&id)
                    .cloned()
                    .unwrap_or(format!("unknown:{id}"))
            })
            .collect();
        let relevant: BTreeSet<String> = q["relevant"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().into())
            .collect();
        let ranks: Vec<usize> = actual
            .iter()
            .enumerate()
            .filter(|(_, k)| relevant.contains(*k))
            .map(|(i, _)| i + 1)
            .collect();
        let found = |n: usize| {
            actual
                .iter()
                .take(n)
                .collect::<BTreeSet<_>>()
                .iter()
                .filter(|k| relevant.contains(**k))
                .count()
        };
        let rel = relevant.len() as f64;
        let dcg: f64 = ranks
            .iter()
            .filter(|r| **r <= 10)
            .map(|r| 1.0 / ((*r + 1) as f64).log2())
            .sum();
        let ideal: f64 = (1..=relevant.len().min(10))
            .map(|r| 1.0 / ((r + 1) as f64).log2())
            .sum();
        let at = q.get("at").and_then(Value::as_str);
        let invalid: Vec<&str> = results
            .iter()
            .filter(|i| !valid_at(&i["memory"], at))
            .map(|_| "invalid")
            .collect();
        let leaks = results
            .iter()
            .filter(|i| i["memory"]["scope"] != q["scope"])
            .count();
        let temporal = [
            "temporal_history",
            "conflicting_and_superseded_facts",
            "long_supersession_chain",
            "temporal_validity",
        ]
        .contains(&q["category"].as_str().unwrap());
        let top10: Vec<&String> = actual.iter().take(10).collect();
        rows.push(json!({"id":q["id"],"split":q["split"],"category":q["category"],"top10":top10,
            "first_relevant_rank":ranks.iter().min(),"returned_count":results.len(),
            "recall_at_1":(!relevant.is_empty()).then(|| found(1) as f64 / rel),
            "recall_at_5":(!relevant.is_empty()).then(|| found(5) as f64 / rel),
            "recall_at_10":(!relevant.is_empty()).then(|| found(10) as f64 / rel),
            "precision_at_5":found(5) as f64/5.0,
            "reciprocal_rank":ranks.iter().min().map_or(0.0,|r| 1.0/ *r as f64),
            "ndcg_at_10":(ideal>0.0).then(|| dcg/ideal),
            "found_relevant_count_at_10":found(10),
            "false_positive":relevant.is_empty() && !results.is_empty(),
            "invalid_temporal_results":invalid.len(),
            "temporal_correct_at_10":temporal.then(|| (if relevant.is_empty(){actual.is_empty()}else{found(10)==relevant.len()}) && invalid.is_empty()),
            "namespace_leaks":leaks,"relevant_count":relevant.len()}));
    }
    let usage = mcp.requests;
    let proc_usage = mcp.close()?;
    let pos: Vec<&Value> = rows.iter().filter(|r| r["relevant_count"] != 0).collect();
    let neg: Vec<&Value> = rows.iter().filter(|r| r["relevant_count"] == 0).collect();
    let avg = |k: &str| {
        pos.iter()
            .map(|r| r[k].as_f64().unwrap_or(0.0))
            .sum::<f64>()
            / pos.len().max(1) as f64
    };
    let tem: Vec<&Value> = rows
        .iter()
        .filter(|r| !r["temporal_correct_at_10"].is_null())
        .collect();
    Ok(
        json!({"schema_version":2,"suite":split,"fixture":fixture,"insertion_order":if reverse{"reverse"}else{"forward"},
        "query_count":queries.len(),"memory_count":key_to_id.len(),
        "measurement":{"scope":"whole aether-midnight mcp process, one request in flight; setup remember calls included in the session",
            "mcp_request_count":usage,"process":proc_usage.json()},
        "latency":{"remember":distribution(&remember_ms),"warm_recall":distribution(&recall_ms)},
        "retrieval_quality":{"positive_query_count":pos.len(),"negative_query_count":neg.len(),
            "recall_at_1":avg("recall_at_1"),"recall_at_5":avg("recall_at_5"),"recall_at_10":avg("recall_at_10"),
            "precision_at_5":avg("precision_at_5"),"mrr":avg("reciprocal_rank"),"ndcg_at_10":avg("ndcg_at_10"),
            "false_positive_queries":neg.iter().filter(|r| r["false_positive"]==true).map(|r| r["id"].clone()).collect::<Vec<_>>(),
            "temporal_invalid_result_count":rows.iter().map(|r| r["invalid_temporal_results"].as_u64().unwrap()).sum::<u64>(),
            "temporal_accuracy_at_10":tem.iter().filter(|r| r["temporal_correct_at_10"]==true).count() as f64/tem.len().max(1) as f64,
            "namespace_leak_count":rows.iter().map(|r| r["namespace_leaks"].as_u64().unwrap()).sum::<u64>(),
            "queries":rows}}),
    )
}

// ---------- code-retrieval oracle (CLI) ----------
fn retrieval_quality(bin: &Path) -> Result<Value> {
    let queries = read_json(&Path::new(ROOT).join("benchmark/retrieval_queries.json"))?["queries"]
        .as_array()
        .unwrap()
        .clone();
    let tmp = tempfile::tempdir()?;
    let repo = tmp.path().join("repo");
    copy_dir(&Path::new(ROOT).join("benchmark/retrieval_fixture"), &repo)?;
    let mut strategies = serde_json::Map::new();
    for (name, decomposition) in [("unicode61", "0"), ("identifier_decomposition", "1")] {
        let db = tmp.path().join(format!("{name}.sqlite3"));
        let (db_s, repo_s) = (db.to_str().unwrap(), repo.to_str().unwrap());
        let envs = [
            ("MIDNIGHT_IDENTIFIER_DECOMPOSITION", decomposition),
            ("MIDNIGHT_INDEX_WORKERS", "1"),
        ];
        let (idx, _) = run_cli(
            bin,
            &["--db", db_s, "index", "--repo", repo_s, "--verify-hashes"],
            &envs,
        )?;
        if idx["complete"] != true {
            bail!("fixture index incomplete");
        }
        let (mut recall, mut rr, mut pos, mut rows) = (0.0, 0.0, 0, vec![]);
        let mut usage_total = Usage::default();
        for q in &queries {
            let (res, u) = run_cli(
                bin,
                &[
                    "--db",
                    db_s,
                    "search",
                    "--repo",
                    repo_s,
                    "--query",
                    q["query"].as_str().unwrap(),
                    "--limit",
                    "5",
                ],
                &envs,
            )?;
            usage_total.wall_s += u.wall_s;
            usage_total.cpu_user_s += u.cpu_user_s;
            usage_total.cpu_sys_s += u.cpu_sys_s;
            usage_total.peak_rss_bytes = usage_total.peak_rss_bytes.max(u.peak_rss_bytes);
            let paths: Vec<String> = res["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["path"].as_str().unwrap().into())
                .collect();
            let relevant: BTreeSet<&str> = q["relevant_paths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let ranks: Vec<usize> = paths
                .iter()
                .enumerate()
                .filter(|(_, p)| relevant.contains(p.as_str()))
                .map(|(i, _)| i + 1)
                .collect();
            let (r, m) = if relevant.is_empty() {
                (0.0, 0.0)
            } else {
                pos += 1;
                (
                    ranks.len() as f64 / relevant.len() as f64,
                    ranks.iter().min().map_or(0.0, |x| 1.0 / *x as f64),
                )
            };
            recall += r;
            rr += m;
            rows.push(json!({"id":q["id"],"kind":q["kind"],"top5":paths,"recall_at_5":r,"reciprocal_rank":m}));
        }
        let (diag, _) = run_cli(bin, &["--db", db_s, "diagnose"], &envs)?;
        strategies.insert(
            name.into(),
            json!({"query_count":queries.len(),"positive_query_count":pos,
            "recall_at_5":recall/pos.max(1) as f64,"mrr":rr/pos.max(1) as f64,
            "sqlite_version":diag["sqlite_version"],
            "measurement":{"scope":"sum of per-query `search` processes; peak_rss is the maximum",
                "process":usage_total.json()},"queries":rows}),
        );
    }
    Ok(json!({"schema_version":2,"strategies":strategies}))
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}

// ---------- measured MCP run ----------
/// Seed through Midnight once (untimed), then time an ordered, one-in-flight
/// session of tools/call requests against the existing database.
fn measure_mcp(bin: &Path, db: &Path, requests: &Path) -> Result<Value> {
    let reqs = read_json(requests)?;
    let mut mcp = Mcp::start(bin, db)?;
    let (mut results, mut request_ms) = (vec![], vec![]);
    for r in reqs
        .as_array()
        .context("requests file must be a JSON array of {name, arguments}")?
    {
        let (ms, v) = mcp.call(r["name"].as_str().unwrap(), r["arguments"].clone())?;
        request_ms.push(ms);
        results.push(v);
    }
    let n = mcp.requests;
    Ok(
        json!({"mcp_request_count":n,"process":mcp.close()?.json(),"request_ms":request_ms,"results":results}),
    )
}

/// Deterministic 64-bit LCG (Knuth MMIX constants): reproducible workloads without a RNG crate.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn words(&mut self, vocabulary: &[&str], count: usize) -> String {
        (0..count)
            .map(|_| vocabulary[self.next() as usize % vocabulary.len()])
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Write the `measure-mcp` request files of the rc.7 paired campaign (docs/performance.md,
/// 2026-11-11 and 2026-11-12) for a fixture repository at `repo`, so the same workloads can be
/// replayed on another host. Contents depend only on `repo` and the fixed seeds.
fn rc7_workloads(repo: &Path, output: &Path) -> Result<Value> {
    std::fs::create_dir_all(output)?;
    let repo = repo
        .canonicalize()
        .with_context(|| format!("resolve {}", repo.display()))?;
    let repo = repo.to_string_lossy();
    let vocabulary = [
        "alpha", "beta", "gamma", "delta", "retry", "budget", "invoice", "cache", "payment",
        "timeout", "session", "token", "schema", "index", "memory", "policy", "vector", "cluster",
        "region", "deploy", "rollback", "config", "secret", "audit", "metric", "latency", "queue",
        "worker", "shard", "replica", "backup", "restore",
    ];
    let mut files = serde_json::Map::new();
    let mut write = |name: &str, requests: Vec<Value>| -> Result<()> {
        let path = output.join(name);
        std::fs::write(&path, serde_json::to_string(&requests)? + "\n")?;
        files.insert(
            name.into(),
            json!({"path": path, "requests": requests.len()}),
        );
        Ok(())
    };

    let mut code = Vec::new();
    for _ in 0..30 {
        for query in [
            "filler_01095",
            "FILLER_04321",
            "filler_09999",
            "payload",
            "seed20261103",
            "usize",
            "missing_symbol_zz",
        ] {
            code.push(json!({"name":"search_code","arguments":{"repo_path":repo,"query":query,"limit":10,"mode":"lexical"}}));
        }
        code.push(json!({"name":"open_range","arguments":{"repo_path":repo,"path":"src/01095.rs","line_start":1,"line_end":3}}));
        code.push(json!({"name":"list_symbols","arguments":{"repo_path":repo,"name":"filler_0109","limit":20}}));
        code.push(json!({"name":"index_status","arguments":{"repo_path":repo}}));
        code.push(json!({"name":"repo_map","arguments":{"repo_path":repo}}));
    }
    write("code_lexical.json", code)?;

    let mut rng = Lcg(20261112);
    let mut seed = Vec::new();
    let mut n = 0;
    for _ in 0..20 {
        let memories = (0..500)
            .map(|_| {
                n += 1;
                json!({"scope":"project:knn","kind":"source","content":format!("note {n}: {}", rng.words(&vocabulary, 12))})
            })
            .collect::<Vec<_>>();
        seed.push(json!({"name":"remember_batch","arguments":{"memories":memories,"embed":false}}));
    }
    write("seed_memories.json", seed)?;
    let memory_knn = (0..60)
        .map(|i| json!({"name":"recall","arguments":{"scope":"project:knn","query":format!("distinct query {i} {}", rng.words(&vocabulary, 3)),"mode":"semantic","limit":10}}))
        .collect();
    write("knn_memory.json", memory_knn)?;
    let code_knn = (0..60)
        .map(|i| json!({"name":"search_code","arguments":{"repo_path":repo,"query":format!("semantic question {i} about filler payload"),"mode":"semantic","limit":10}}))
        .collect();
    write("knn_code.json", code_knn)?;
    let mut lexical = Vec::new();
    for i in 0..100 {
        lexical.push(json!({"name":"remember","arguments":{"scope":"project:knn","kind":"source","content":format!("lexical op {i} {}", rng.words(&vocabulary, 8)),"embed":false}}));
        lexical.push(json!({"name":"recall","arguments":{"scope":"project:knn","query":rng.words(&vocabulary, 2),"limit":10}}));
        lexical
            .push(json!({"name":"list_memories","arguments":{"scope":"project:knn","limit":20}}));
    }
    write("memory_lexical.json", lexical)?;
    let writes = |embed: bool, rng: &mut Lcg| -> Vec<Value> {
        (0..100)
            .map(|i| {
                let mut args = json!({"scope":"project:w","kind":"source","content":format!("write embed {i} {}", rng.words(&vocabulary, 8))});
                if !embed {
                    args["embed"] = json!(false);
                }
                json!({"name":"remember","arguments":args})
            })
            .collect()
    };
    let mut write_rng = Lcg(7);
    write("remember_embed.json", writes(true, &mut write_rng))?;
    let mut write_rng = Lcg(7);
    write("remember_no_embed.json", writes(false, &mut write_rng))?;
    Ok(json!({"repo": repo, "files": files}))
}

// ---------- token metrics (v0.2) ----------
/// Copy of `aether_midnight::code::estimate_tokens` (`midnight-heuristic-v1`). The harness applies
/// one estimator to the raw responses of any binary so before/after numbers are comparable even
/// when the base binary cannot report them. An estimate, not a model tokenizer count.
fn estimate_tokens(text: &str) -> usize {
    let mut tokens = 0usize;
    let mut identifier_bytes = 0usize;
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            identifier_bytes += 1;
        } else {
            tokens += identifier_bytes.div_ceil(4);
            identifier_bytes = 0;
            if !character.is_whitespace() {
                tokens += 1;
            }
        }
    }
    tokens + identifier_bytes.div_ceil(4)
}

fn pctl(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

/// Drive `search_code` (lexical, one request in flight, one fresh connection) over the frozen
/// retrieval queries and record paths, response bytes and estimated tokens per query.
fn token_report(bin: &Path, limit: usize, samples_output: Option<&Path>) -> Result<Value> {
    let queries = read_json(&Path::new(ROOT).join("benchmark/retrieval_queries.json"))?["queries"]
        .as_array()
        .context("queries")?
        .clone();
    let tmp = tempfile::tempdir()?;
    let repo = tmp.path().join("repo");
    copy_dir(&Path::new(ROOT).join("benchmark/retrieval_fixture"), &repo)?;
    let db = tmp.path().join("tokens.sqlite3");
    let (db_s, repo_s) = (db.to_str().unwrap(), repo.to_str().unwrap());
    let envs = [("MIDNIGHT_INDEX_WORKERS", "1")];
    let (idx, _) = run_cli(
        bin,
        &["--db", db_s, "index", "--repo", repo_s, "--verify-hashes"],
        &envs,
    )?;
    if idx["complete"] != true {
        bail!("fixture index incomplete");
    }
    let mut rows = vec![];
    let mut samples = vec![];
    let (mut bytes_all, mut tokens_all) = (vec![], vec![]);
    for q in &queries {
        // A fresh connection per query: no delivery history, so every response carries text.
        let mut mcp = Mcp::start(bin, &db)?;
        let (_, v) = mcp.call(
            "search_code",
            json!({"repo_path":repo_s,"query":q["query"],"limit":limit,"mode":"lexical"}),
        )?;
        mcp.close()?;
        let text = v.to_string();
        let paths: Vec<&str> = v["results"]
            .as_array()
            .map(|r| r.iter().filter_map(|x| x["path"].as_str()).collect())
            .unwrap_or_default();
        let (bytes, tokens) = (text.len(), estimate_tokens(&text));
        bytes_all.push(bytes as f64);
        tokens_all.push(tokens as f64);
        rows.push(json!({"id":q["id"],"kind":q["kind"],"paths":paths,"bytes":bytes,"estimated_tokens":tokens}));
        samples.push(Value::String(text));
    }
    if let Some(path) = samples_output {
        std::fs::write(path, serde_json::to_string_pretty(&samples)? + "\n")?;
    }
    let summarize = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        json!({"sum":v.iter().sum::<f64>(),"p50":pctl(&v,0.5),"p95":pctl(&v,0.95),"max":v.last()})
    };
    Ok(
        json!({"schema_version":1,"limit":limit,"estimator":"midnight-heuristic-v1 (estimate)",
        "query_count":rows.len(),"bytes":summarize(bytes_all),"estimated_tokens":summarize(tokens_all),"queries":rows}),
    )
}

/// Compare two `token-report` outputs: paths and order per query, then token and byte deltas.
fn token_compare(before: &Path, after: &Path) -> Result<Value> {
    let (b, a) = (read_json(before)?, read_json(after)?);
    let (bq, aq) = (
        b["queries"].as_array().context("before")?,
        a["queries"].as_array().context("after")?,
    );
    if bq.len() != aq.len() {
        bail!("reports have different query counts");
    }
    let (mut differing, mut worse, mut rows) = (vec![], 0, vec![]);
    for (x, y) in bq.iter().zip(aq) {
        let (px, py) = (
            x["paths"].as_array().unwrap(),
            y["paths"].as_array().unwrap(),
        );
        // The after side may add results the base cut; the shared prefix must be identical.
        let n = px.len().min(py.len());
        let same_order = px[..n] == py[..n];
        let same_paths = px == py;
        if !same_order {
            differing.push(x["id"].clone());
        }
        let (tb, ta) = (
            x["estimated_tokens"].as_u64().unwrap(),
            y["estimated_tokens"].as_u64().unwrap(),
        );
        if ta > tb {
            worse += 1;
        }
        rows.push(json!({"id":x["id"],"same_paths":same_paths,"same_order_on_shared_prefix":same_order,
            "tokens_before":tb,"tokens_after":ta,"bytes_before":x["bytes"],"bytes_after":y["bytes"]}));
    }
    Ok(
        json!({"queries":rows.len(),"order_differences":differing,"queries_with_more_tokens_after":worse,
        "tokens_before":b["estimated_tokens"],"tokens_after":a["estimated_tokens"],
        "bytes_before":b["bytes"],"bytes_after":a["bytes"],"rows":rows}),
    )
}

/// Offline estimator calibration: the ratio of the estimate to externally supplied reference
/// counts (one number per sample). No network call; "not measured" without a reference file.
fn calibrate_tokens(samples: &Path, reference: Option<&Path>) -> Result<Value> {
    let samples = read_json(samples)?;
    let samples = samples
        .as_array()
        .context("samples must be a JSON array of strings")?;
    let estimates: Vec<usize> = samples
        .iter()
        .map(|s| estimate_tokens(s.as_str().unwrap_or("")))
        .collect();
    let Some(reference) = reference else {
        return Ok(
            json!({"estimator":"midnight-heuristic-v1","sample_count":estimates.len(),
            "reference":"not measured","estimates":estimates}),
        );
    };
    let reference = read_json(reference)?;
    let reference: Vec<f64> = reference
        .as_array()
        .context("reference must be a JSON array of numbers")?
        .iter()
        .map(|v| v.as_f64().unwrap_or(0.0))
        .collect();
    if reference.len() != estimates.len() {
        bail!(
            "reference has {} counts for {} samples",
            reference.len(),
            estimates.len()
        );
    }
    let mut ratios: Vec<f64> = estimates
        .iter()
        .zip(&reference)
        .filter(|(_, r)| **r > 0.0)
        .map(|(e, r)| *e as f64 / r)
        .collect();
    ratios.sort_by(|a, b| a.total_cmp(b));
    let mean = ratios.iter().sum::<f64>() / ratios.len().max(1) as f64;
    Ok(
        json!({"estimator":"midnight-heuristic-v1","sample_count":ratios.len(),
        "ratio_estimate_over_reference":{"mean":mean,"p5":pctl(&ratios,0.05),"p50":pctl(&ratios,0.5),"p95":pctl(&ratios,0.95)},
        "note":"reference counts were supplied by the caller; the harness made no network call"}),
    )
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| {
        a.iter()
            .position(|x| x == n)
            .and_then(|i| a.get(i + 1))
            .map(String::as_str)
    };
    let need = |n: &str| flag(n).with_context(|| format!("missing {n}"));
    let cmd = a.first().map(String::as_str).unwrap_or("");
    let out = match cmd {
        "license-notices" => release::notices(Path::new(need("--output")?))?,
        "release-measure" => release::measure(
            &binary_path(flag("--binary"))?,
            Path::new(need("--models")?),
        )?,
        "generate-fixture" => generate_fixture(Path::new(need("--output")?))?,
        "license-inventory" => license_inventory(
            &flag("--output")
                .map(PathBuf::from)
                .unwrap_or_else(|| Path::new(ROOT).join("THIRD_PARTY_LICENSES.json")),
            a.iter().any(|x| x == "--force"),
        )?,
        "memory-quality" => {
            let fixture = flag("--fixture")
                .map(PathBuf::from)
                .unwrap_or_else(|| Path::new(ROOT).join("benchmark/memory_quality_v1.json"));
            let tmp = tempfile::tempdir()?;
            memory_quality(
                &binary_path(flag("--binary"))?,
                &fixture,
                flag("--split").unwrap_or("dev"),
                flag("--insertion-order") == Some("reverse"),
                flag("--repeats").map_or(3, |v| v.parse().unwrap()),
                &tmp.path().join("memory.sqlite3"),
            )?
        }
        "retrieval-quality" => retrieval_quality(&binary_path(flag("--binary"))?)?,
        "token-report" => token_report(
            &binary_path(flag("--binary"))?,
            flag("--limit").map_or(5, |v| v.parse().unwrap()),
            flag("--samples-output").map(Path::new),
        )?,
        "token-compare" => {
            token_compare(Path::new(need("--before")?), Path::new(need("--after")?))?
        }
        "calibrate-tokens" => calibrate_tokens(
            Path::new(need("--samples")?),
            flag("--reference").map(Path::new),
        )?,
        "rc7-workloads" => rc7_workloads(Path::new(need("--repo")?), Path::new(need("--output")?))?,
        "measure-mcp" => measure_mcp(
            &binary_path(flag("--binary"))?,
            Path::new(need("--db")?),
            Path::new(need("--requests")?),
        )?,
        _ => bail!(
            "usage: midnight-harness <release-measure --models DIR [--binary B] | license-notices --output DIR | generate-fixture --output DIR | license-inventory [--output F --force] | memory-quality [--fixture F --split dev|heldout|all --insertion-order forward|reverse --repeats N --binary B] | retrieval-quality [--binary B] | measure-mcp --db DB --requests FILE [--binary B] | rc7-workloads --repo FIXTURE --output DIR | token-report [--limit N --samples-output F --binary B --json-output F] | token-compare --before F --after F | calibrate-tokens --samples F [--reference F]>"
        ),
    };
    if let Some(p) = flag("--json-output") {
        std::fs::write(p, serde_json::to_string_pretty(&out)? + "\n")?;
    }
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

#[cfg(test)]
mod fixture_tests {
    use super::*;

    #[test]
    fn reverse_fixture_keeps_sources_and_superseded_records_before_dependents() {
        let fixture = json!({"records":[
            {"key":"source"},
            {"key":"old","source_key":"source"},
            {"key":"new","source_key":"source","supersedes_key":"old"},
            {"key":"independent"}
        ]});
        let forward = fixture_records(&fixture, false).unwrap();
        let reverse = fixture_records(&fixture, true).unwrap();
        let keys = |rs: &[Value]| {
            rs.iter()
                .map(|r| r["key"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&forward), ["source", "old", "new", "independent"]);
        assert_eq!(keys(&reverse), ["independent", "source", "old", "new"]);
    }

    #[test]
    fn invalid_fixture_dependencies_return_errors_before_starting_midnight() {
        for records in [
            json!([{"key":"a","source_key":"missing"}]),
            json!([{"key":"a","source_key":"b"},{"key":"b","source_key":"a"}]),
            json!([{"key":"a"},{"key":"a"}]),
        ] {
            assert!(order_fixture_dependencies(records.as_array().unwrap().clone()).is_err());
        }
    }
}
