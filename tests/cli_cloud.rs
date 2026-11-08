//! CLI-level cloud-embedding tests against the deterministic loopback mock. No real provider is
//! contacted. Covers provider selection, credentials, space isolation, reindexing, degradation.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

mod common {
    pub mod mock;
}
use common::mock::{Mock, Reply, ok_response};

const KEY: &str = "sk-cli-SECRET-abcdef";
const DIMS: usize = 16;

struct Env {
    _temp: TempDir,
    db: std::path::PathBuf,
    models: std::path::PathBuf,
}

impl Env {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let db = temp.path().join("m.sqlite3");
        let models = temp.path().join("models");
        Env {
            _temp: temp,
            db,
            models,
        }
    }

    fn run(&self, key_env: Option<(&str, &str)>, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aether-midnight"));
        cmd.arg("--db")
            .arg(&self.db)
            .args(args)
            .env("AETHER_MIDNIGHT_MODELS_DIR", &self.models)
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("RAINY_API_KEY");
        if let Some((k, v)) = key_env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn ok(&self, key_env: Option<(&str, &str)>, args: &[&str]) -> Value {
        let mut all = args.to_vec();
        all.push("--json");
        let out = self.run(key_env, &all);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?} not JSON: {e}"))
    }
}

fn remote<'a>(url: &'a str, model: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let dims: &'static str = Box::leak(DIMS.to_string().into_boxed_str());
    let mut v = vec![
        "--provider",
        "openrouter",
        "--model",
        model,
        "--dimensions",
        dims,
        "--endpoint",
        url,
        "--timeout-secs",
        "5",
    ];
    v.extend_from_slice(extra);
    v
}

fn with<'a>(base: &[&'a str], tail: &[&'a str]) -> Vec<&'a str> {
    base.iter().chain(tail).copied().collect()
}

fn seed(env: &Env) {
    for (ns, text) in [
        ("proj:a", "the invoice payment gateway uses retry logic"),
        ("proj:a", "database backups run nightly at midnight"),
        ("proj:b", "invoice archive belongs to another project"),
    ] {
        env.ok(None, &["memory", "add", "--namespace", ns, text]);
    }
}

fn texts(v: &Value) -> Vec<String> {
    v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["content"]
                .as_str()
                .or(r["memory"]["content"].as_str())
                .unwrap()
                .to_string()
        })
        .collect()
}

#[test]
fn remote_embed_then_hybrid_search_works_with_isolation_and_provider_identity() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let env = Env::new();
    seed(&env);
    let key = Some(("OPENROUTER_API_KEY", KEY));
    let args = with(
        &["embed", "--namespace", "proj:a"],
        &remote(&mock.url, "vendor/m1", &[]),
    );
    let report = env.ok(key, &args);
    assert_eq!(report[0]["embedded"], 2);
    assert_eq!(report[0]["model"], "openrouter:vendor/m1");
    // Only proj:a's two memories were sent; proj:b's text never left.
    let sent: Vec<String> = mock
        .recorded()
        .iter()
        .flat_map(|r| {
            r.body["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|t| !t.contains("another project")));
    // Re-run is incremental: nothing re-sent.
    let n = mock.count();
    let again = env.ok(key, &args);
    assert_eq!(
        (
            again[0]["embedded"].as_u64(),
            again[0]["unchanged"].as_u64()
        ),
        (Some(0), Some(2))
    );
    assert_eq!(mock.count(), n);

    let search = with(
        &[
            "memory",
            "search",
            "--namespace",
            "proj:a",
            "invoice",
            "--mode",
            "hybrid",
        ],
        &remote(&mock.url, "vendor/m1", &[]),
    );
    let found = env.ok(key, &search);
    assert_eq!(found["mode_used"], "hybrid");
    assert_eq!(found["degraded"], false);
    assert_eq!(found["model"]["name"], "openrouter:vendor/m1");
    assert_eq!(found["model"]["dimensions"], DIMS);
    assert_eq!(found["semantic_coverage"]["embedded"], 2);
    assert!(texts(&found).iter().all(|t| !t.contains("another project")));
    assert!(texts(&found)[0].contains("invoice"));
    assert_eq!(mock.count(), n + 1, "exactly one query embedding request");
    let last = mock.recorded().last().unwrap().clone();
    assert_eq!(last.body["input"], serde_json::json!(["invoice"]));
    assert_eq!(last.headers["authorization"], format!("Bearer {KEY}"));
}

#[test]
fn other_model_or_provider_is_isolated_and_never_receives_the_query() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let env = Env::new();
    seed(&env);
    let key = Some(("OPENROUTER_API_KEY", KEY));
    env.ok(
        key,
        &with(
            &["embed", "--namespace", "proj:a"],
            &remote(&mock.url, "vendor/m1", &[]),
        ),
    );
    let before = mock.count();
    // Same provider, different model: degraded lexical, query not sent.
    let found = env.ok(
        key,
        &with(
            &[
                "memory",
                "search",
                "--namespace",
                "proj:a",
                "invoice",
                "--mode",
                "hybrid",
            ],
            &remote(&mock.url, "vendor/m2", &[]),
        ),
    );
    assert_eq!(found["degraded"], true);
    assert_eq!(found["mode_used"], "lexical");
    let notice = found["notice"].as_str().unwrap();
    assert!(
        notice.contains("different embedding space") && notice.contains("openrouter:vendor/m1"),
        "{notice}"
    );
    assert!(!texts(&found).is_empty(), "lexical results must survive");
    assert_eq!(
        mock.count(),
        before,
        "query must not be sent to an incompatible space"
    );
    // Different dimensions declared: also a different space.
    let mut dims_args = vec![
        "memory",
        "search",
        "--namespace",
        "proj:a",
        "invoice",
        "--mode",
        "semantic",
    ];
    dims_args.extend([
        "--provider",
        "openrouter",
        "--model",
        "vendor/m1",
        "--dimensions",
        "32",
        "--endpoint",
        &mock.url,
    ]);
    let found = env.ok(key, &dims_args);
    assert_eq!(found["degraded"], true);
    assert_eq!(mock.count(), before);
    // Different provider (rainy, same model name and endpoint): isolated by name.
    let mut rainy = vec![
        "memory",
        "search",
        "--namespace",
        "proj:a",
        "invoice",
        "--mode",
        "hybrid",
    ];
    rainy.extend([
        "--provider",
        "rainy",
        "--model",
        "vendor/m1",
        "--dimensions",
        "16",
        "--endpoint",
        &mock.url,
    ]);
    let found = env.ok(Some(("RAINY_API_KEY", KEY)), &rainy);
    assert_eq!(found["degraded"], true);
    assert_eq!(mock.count(), before);
    // The local model is a different space as well (and no model is installed here).
    let found = env.ok(
        None,
        &[
            "memory",
            "search",
            "--namespace",
            "proj:a",
            "invoice",
            "--mode",
            "hybrid",
        ],
    );
    assert_eq!(found["degraded"], true);
    assert_eq!(mock.count(), before);
}

#[test]
fn changing_model_requires_explicit_embed_which_replaces_the_old_space() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let env = Env::new();
    seed(&env);
    let key = Some(("OPENROUTER_API_KEY", KEY));
    env.ok(
        key,
        &with(
            &["embed", "--namespace", "proj:a"],
            &remote(&mock.url, "vendor/m1", &[]),
        ),
    );
    let report = env.ok(
        key,
        &with(
            &["embed", "--namespace", "proj:a"],
            &remote(&mock.url, "vendor/m2", &[]),
        ),
    );
    assert_eq!(report[0]["replaced_other_model"], 2);
    assert_eq!(report[0]["embedded"], 2);
    let found = env.ok(
        key,
        &with(
            &[
                "memory",
                "search",
                "--namespace",
                "proj:a",
                "invoice",
                "--mode",
                "hybrid",
            ],
            &remote(&mock.url, "vendor/m2", &[]),
        ),
    );
    assert_eq!(found["degraded"], false);
    assert_eq!(found["model"]["name"], "openrouter:vendor/m2");
    // Changing a request parameter changes the space too.
    let report = env.ok(
        key,
        &with(
            &["embed", "--namespace", "proj:a"],
            &remote(
                &mock.url,
                "vendor/m2",
                &["--param", "input_type=search_document"],
            ),
        ),
    );
    assert_eq!(report[0]["replaced_other_model"], 2);
}

#[test]
fn missing_credentials_or_flags_fail_closed_and_send_nothing() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let env = Env::new();
    seed(&env);
    let out = env.run(
        None,
        &with(
            &["embed", "--namespace", "proj:a"],
            &remote(&mock.url, "vendor/m1", &[]),
        ),
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("OPENROUTER_API_KEY is not set"), "{stderr}");
    // The other provider's credential is not substituted.
    let out = env.run(
        Some(("RAINY_API_KEY", KEY)),
        &with(
            &["embed", "--namespace", "proj:a"],
            &remote(&mock.url, "vendor/m1", &[]),
        ),
    );
    assert!(!out.status.success());
    // Missing model / dimensions / bad param / local-incompatible flags.
    for bad in [
        vec![
            "embed",
            "--namespace",
            "proj:a",
            "--provider",
            "openrouter",
            "--dimensions",
            "16",
        ],
        vec![
            "embed",
            "--namespace",
            "proj:a",
            "--provider",
            "openrouter",
            "--model",
            "m",
        ],
        vec![
            "embed",
            "--namespace",
            "proj:a",
            "--provider",
            "openrouter",
            "--model",
            "m",
            "--dimensions",
            "16",
            "--param",
            "dimensions=3",
        ],
        vec!["embed", "--namespace", "proj:a", "--model", "other-model"],
        vec![
            "embed",
            "--namespace",
            "proj:a",
            "--endpoint",
            "http://127.0.0.1:1/v1",
        ],
    ] {
        let out = env.run(Some(("OPENROUTER_API_KEY", KEY)), &bad);
        assert!(!out.status.success(), "{bad:?} should fail");
        assert!(!String::from_utf8_lossy(&out.stderr).contains(KEY));
    }
    assert_eq!(mock.count(), 0);
}

#[test]
fn lexical_and_default_modes_never_contact_a_selected_provider() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let env = Env::new();
    seed(&env);
    let key = Some(("OPENROUTER_API_KEY", KEY));
    let lexical = env.ok(
        key,
        &with(
            &["memory", "search", "--namespace", "proj:a", "invoice"],
            &remote(&mock.url, "vendor/m1", &[]),
        ),
    );
    assert!(lexical.is_array());
    // Hybrid with no vectors stored: no embedding request is made for the query either.
    let hybrid = env.ok(
        key,
        &with(
            &[
                "memory",
                "search",
                "--namespace",
                "proj:a",
                "invoice",
                "--mode",
                "hybrid",
            ],
            &remote(&mock.url, "vendor/m1", &[]),
        ),
    );
    assert_eq!(hybrid["degraded"], true);
    assert!(
        hybrid["notice"]
            .as_str()
            .unwrap()
            .contains("no embeddings stored")
    );
    assert_eq!(mock.count(), 0);
}

#[test]
fn provider_failures_degrade_search_to_lexical_and_never_leak_the_key() {
    for (status, body) in [
        (401, format!(r#"{{"error":{{"message":"bad key {KEY}"}}}}"#)),
        (
            402,
            r#"{"error":{"code":402,"message":"Insufficient credits"}}"#.to_string(),
        ),
        (
            429,
            r#"{"error":{"code":"RATE_LIMIT_EXCEEDED","message":"slow"}}"#.to_string(),
        ),
        (503, "unavailable".to_string()),
    ] {
        let healthy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let h = healthy.clone();
        let b = body.clone();
        let mock = Mock::start(move |_, r| {
            if h.load(std::sync::atomic::Ordering::Relaxed) {
                ok_response(r, DIMS)
            } else {
                Reply::Json(status, b.clone())
            }
        });
        let env = Env::new();
        seed(&env);
        let key = Some(("OPENROUTER_API_KEY", KEY));
        env.ok(
            key,
            &with(
                &["embed", "--namespace", "proj:a"],
                &remote(&mock.url, "vendor/m1", &[]),
            ),
        );
        healthy.store(false, std::sync::atomic::Ordering::Relaxed);
        let out = env.run(
            key,
            &with(
                &[
                    "memory",
                    "search",
                    "--namespace",
                    "proj:a",
                    "invoice",
                    "--mode",
                    "hybrid",
                    "--json",
                ],
                &remote(&mock.url, "vendor/m1", &[]),
            ),
        );
        assert!(out.status.success(), "HTTP {status} must degrade, not fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stderr.contains(KEY) && !stdout.contains(KEY),
            "secret leaked for HTTP {status}"
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["degraded"], true, "HTTP {status}");
        assert_eq!(v["mode_used"], "lexical");
        assert!(
            v["notice"]
                .as_str()
                .unwrap()
                .contains("query embedding failed"),
            "{v}"
        );
        assert!(stderr.contains("warning:"));
        assert!(
            texts(&v)[0].contains("invoice"),
            "lexical results must be preserved"
        );
    }
}

#[test]
fn embed_failure_midway_keeps_committed_vectors_and_lexical_search() {
    // 20 memories = two batches; the second request is rate limited.
    let mock = Mock::start(|n, r| {
        if n != 1 {
            ok_response(r, DIMS)
        } else {
            Reply::Json(429, r#"{"error":{"code":"RATE_LIMIT_EXCEEDED","message":"slow down","details":{"retry_after":3}}}"#.into())
        }
    });
    let env = Env::new();
    for i in 0..20 {
        env.ok(
            None,
            &[
                "memory",
                "add",
                "--namespace",
                "proj:a",
                &format!("note number {i} about invoice handling"),
            ],
        );
    }
    let key = Some(("OPENROUTER_API_KEY", KEY));
    let out = env.run(
        key,
        &with(
            &["embed", "--namespace", "proj:a"],
            &remote(&mock.url, "vendor/m1", &[]),
        ),
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rate limited") && stderr.contains("lexical search is unaffected"),
        "{stderr}"
    );
    assert!(!stderr.contains(KEY));
    // First batch (16) was committed; lexical search is intact.
    let lexical = env.ok(
        None,
        &[
            "memory",
            "search",
            "--namespace",
            "proj:a",
            "invoice",
            "--limit",
            "3",
        ],
    );
    assert_eq!(lexical.as_array().unwrap().len(), 3);
    let partial = env.ok(
        key,
        &with(
            &[
                "memory",
                "search",
                "--namespace",
                "proj:a",
                "invoice",
                "--mode",
                "semantic",
            ],
            &remote(&mock.url, "vendor/m1", &[]),
        ),
    );
    assert_eq!(partial["degraded"], false);
    assert!(
        partial["notice"]
            .as_str()
            .unwrap()
            .contains("partial semantic coverage: 16 of 20")
    );
    assert_eq!(partial["semantic_coverage"]["embedded"], 16);
}

#[test]
fn remote_code_embedding_and_hybrid_code_search() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let env = Env::new();
    let repo = TempDir::new().unwrap();
    write(
        repo.path(),
        "billing.rs",
        "fn charge_invoice() {\n    // retry payment gateway\n}\n",
    );
    write(repo.path(), "notes.md", "# Cooking\nrecipes for pasta\n");
    let repo_str = repo.path().to_str().unwrap();
    env.ok(None, &["index", "--repo", repo_str]);
    let key = Some(("OPENROUTER_API_KEY", KEY));
    let report = env.ok(
        key,
        &with(
            &["embed", "--repo", repo_str],
            &remote(&mock.url, "vendor/m1", &[]),
        ),
    );
    assert!(report[0]["embedded"].as_u64().unwrap() >= 2);
    let found = env.ok(
        key,
        &with(
            &[
                "search",
                "--repo",
                repo_str,
                "invoice payment",
                "--mode",
                "hybrid",
            ],
            &remote(&mock.url, "vendor/m1", &[]),
        ),
    );
    assert_eq!(found["mode_used"], "hybrid");
    assert_eq!(found["results"][0]["path"], "billing.rs");
    assert_eq!(found["model"]["name"], "openrouter:vendor/m1");
}

fn write(root: &Path, name: &str, text: &str) {
    fs::write(root.join(name), text).unwrap();
}

#[test]
fn doctor_reports_cloud_credentials_without_values_and_never_requests() {
    let mock = Mock::start(|_, r| ok_response(r, DIMS));
    let env = Env::new();
    let out = env.run(Some(("OPENROUTER_API_KEY", KEY)), &["doctor", "--json"]);
    assert!(
        out.status.success(),
        "optional capability checks must not fail doctor"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains(KEY));
    let v: Value = serde_json::from_str(&text).unwrap();
    let find = |name: &str| {
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(find("cloud_openrouter")["ok"], true);
    assert_eq!(find("cloud_rainy")["ok"], false);
    assert_eq!(find("cloud_rainy")["optional"], true);
    assert!(
        find("cloud_rainy")["remedy"][0]
            .as_str()
            .unwrap()
            .contains("RAINY_API_KEY")
    );
    assert_eq!(find("local_model")["ok"], false);
    assert!(
        find("local_model")["remedy"][0]
            .as_str()
            .unwrap()
            .contains("models pull")
    );
    assert_eq!(mock.count(), 0);
}

/// Optional live smoke test. Ignored by default so it can never be mistaken for a pass; run with
/// `cargo test --test cli_cloud -- --ignored live_openrouter` after exporting `OPENROUTER_API_KEY`,
/// `AETHER_MIDNIGHT_LIVE_MODEL` and `AETHER_MIDNIGHT_LIVE_DIMENSIONS`. It sends two short synthetic
/// sentences (no repository content) to the real OpenRouter endpoint and costs a fraction of a cent.
#[test]
#[ignore = "needs real credentials: see the doc comment"]
fn live_openrouter_embed_and_search_smoke() {
    let (Ok(key), Ok(model), Ok(dims)) = (
        std::env::var("OPENROUTER_API_KEY"),
        std::env::var("AETHER_MIDNIGHT_LIVE_MODEL"),
        std::env::var("AETHER_MIDNIGHT_LIVE_DIMENSIONS"),
    ) else {
        // An explicit `--ignored` run must never pass vacuously.
        panic!(
            "live smoke requested but OPENROUTER_API_KEY, AETHER_MIDNIGHT_LIVE_MODEL and AETHER_MIDNIGHT_LIVE_DIMENSIONS are not all set"
        );
    };
    let env = Env::new();
    env.ok(
        None,
        &[
            "memory",
            "add",
            "--namespace",
            "live:t",
            "The server keeps its cache on disk",
        ],
    );
    env.ok(
        None,
        &[
            "memory",
            "add",
            "--namespace",
            "live:t",
            "Quarterly invoices are archived",
        ],
    );
    let flags = [
        "--provider",
        "openrouter",
        "--model",
        &model,
        "--dimensions",
        &dims,
    ];
    let k = Some(("OPENROUTER_API_KEY", key.as_str()));
    let report = env.ok(k, &with(&["embed", "--namespace", "live:t"], &flags));
    assert_eq!(report[0]["embedded"], 2);
    let found = env.ok(
        k,
        &with(
            &[
                "memory",
                "search",
                "--namespace",
                "live:t",
                "disk cache",
                "--mode",
                "semantic",
            ],
            &flags,
        ),
    );
    assert_eq!(found["degraded"], false, "{found}");
    assert!(texts(&found)[0].contains("cache"), "{found}");
}
