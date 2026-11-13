//! Cross-commit persistent MCP comparison. Raw output is private by default.
use aether_midnight::{db, memory};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::Instant,
};
struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
impl Client {
    fn start(binary: &str, path: &Path) -> Result<Self> {
        let mut child = Command::new(binary)
            .arg("--db")
            .arg(path)
            .env("MIDNIGHT_DAEMON", "0")
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take().context("stdin")?;
        let output = BufReader::new(child.stdout.take().context("stdout")?);
        let mut client = Self {
            child,
            input,
            output,
        };
        client.call(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"paired","version":"1"}}}))?;
        Ok(client)
    }
    fn call(&mut self, value: Value) -> Result<Value> {
        serde_json::to_writer(&mut self.input, &value)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        let mut line = String::new();
        ensure!(self.output.read_line(&mut line)? > 0, "child exited");
        let response: Value = serde_json::from_str(&line)?;
        ensure!(
            response.get("error").is_none() && response["result"]["isError"] != true,
            "MCP error: {response}"
        );
        Ok(response)
    }
    fn recall(&mut self, scope: &str, query: &str) -> Result<Value> {
        let response=self.call(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"recall","arguments":{"scope":scope,"query":query,"limit":10,"at":"2026-11-06T00:00:00Z"}}}))?;
        Ok(serde_json::from_str(
            response["result"]["content"][0]["text"]
                .as_str()
                .context("result text")?,
        )?)
    }
}
fn child_cpu_seconds(child: &Child) -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", child.id())).ok()?;
        let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        // SAFETY: sysconf reads a constant process-independent clock frequency.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks <= 0 {
            return None;
        }
        Some(
            (fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?) as f64
                / ticks as f64,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = child;
        None
    }
}
fn child_peak_rss(child: &Child) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", child.id())).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmHWM:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|kb| kb * 1024)
    })
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        (6..=8).contains(&args.len()),
        "paired_kernel BASELINE CANDIDATE SIZE NAMESPACES SAMPLES [--fixed-scope] [--broad-only]"
    );
    ensure!(
        args[6..]
            .iter()
            .all(|v| v == "--fixed-scope" || v == "--broad-only"),
        "unknown diagnostic option"
    );
    let fixed = args[6..].iter().any(|v| v == "--fixed-scope");
    let broad_only = args[6..].iter().any(|v| v == "--broad-only");
    let size: usize = args[3].parse()?;
    let ns: usize = args[4].parse()?;
    let samples: usize = args[5].parse()?;
    ensure!(
        (1000..=100000).contains(&size) && [1, 4].contains(&ns) && (1..=2_000).contains(&samples),
        "bounded workload required"
    );
    let temp = tempfile::tempdir()?;
    let seed = temp.path().join("seed.db");
    let scopes = ["bench:alpha", "bench:beta", "bench:gamma", "bench:delta"];
    let mut cumulative = Vec::new();
    let mut total = 0.0;
    for rank in 0..3000 {
        total += 1.0 / (rank as f64 + 1.0);
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
    for index in 0..size {
        let length = 12 + (next() % 29) as usize;
        let words: Vec<_> = (0..length)
            .map(|_| {
                let target = (next() >> 11) as f64 / (1u64 << 53) as f64 * total;
                format!("w{}", cumulative.partition_point(|v| *v < target).min(2999))
            })
            .collect();
        if index == size / 2 {
            planted = words[2..5].join(" ");
        }
        batch.push(memory::MemoryInput {
            scope: scopes[index % ns].into(),
            kind: "source".into(),
            content: words.join(" "),
            source_uri: Some("benchmark://zipf".into()),
            source_id: None,
            supersedes_id: None,
            observed_at: None,
            valid_from: None,
            valid_to: None,
        });
        if batch.len() == 512 || index + 1 == size {
            memory::remember_batch(&seed, &batch)?;
            batch.clear();
        }
    }
    let conn = db::open_db(&seed)?;
    conn.execute_batch("DROP TABLE memory_operations; PRAGMA user_version=4; UPDATE metadata SET value='4' WHERE key='schema_version'; PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(conn);
    let mut clients = Vec::new();
    for (i, binary) in [&args[1], &args[1], &args[2]].iter().enumerate() {
        let path = temp.path().join(format!("copy{i}.db"));
        std::fs::copy(&seed, &path)?;
        clients.push(Client::start(binary, &path)?);
    }
    let mut queries = vec![
        "w0".to_string(),
        "w10".into(),
        "w2000".into(),
        "w3 w5".into(),
        "w10 w2000".into(),
        "w50 w300 w2000".into(),
        planted.clone(),
        format!("what is the {planted} and w2000"),
    ];
    if broad_only {
        queries.truncate(1);
    }
    let mut reports = Vec::new();
    for query in queries {
        for c in &mut clients {
            for _ in 0..3 {
                c.recall(scopes[0], &query)?;
            }
        }
        let cpu_before: Vec<_> = clients
            .iter()
            .map(|c| child_cpu_seconds(&c.child))
            .collect();
        let mut times = vec![Vec::new(); 3];
        for sample in 0..samples {
            let mut results = vec![Value::Null; 3];
            for offset in 0..3 {
                let i = (sample + offset) % 3;
                let start = Instant::now();
                results[i] = clients[i].recall(
                    scopes[if fixed { (size / 2) % ns } else { sample % ns }],
                    &query,
                )?;
                times[i].push(start.elapsed().as_secs_f64() * 1000.0);
            }
            ensure!(
                results[0] == results[1] && results[0] == results[2],
                "full result mismatch"
            );
        }
        let cpu: Vec<_> = clients
            .iter()
            .zip(cpu_before)
            .map(|(c, b)| {
                b.zip(child_cpu_seconds(&c.child))
                    .map(|(a, z)| (z - a) * 1000.0 / samples as f64)
            })
            .collect();
        reports.push(
            json!({"query":query,"wall_ms_in_sample_order":times,"child_cpu_ms_per_query":cpu}),
        );
    }
    let rss: Vec<_> = clients.iter().map(|c| child_peak_rss(&c.child)).collect();
    println!(
        "{}",
        json!({"fixed_namespace":fixed,"peak_rss_bytes":rss,"size":size,"namespaces":ns,"samples":samples,"paths":["baseline","baseline_control","candidate"],"classes":reports,"cpu":"thread CPU measured separately by bench-retrieval --kernel-ab; cross-process wall includes JSON and pipes"})
    );
    Ok(())
}
