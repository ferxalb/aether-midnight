//! Real MCP process + deterministic loopback provider. Never uses user configuration or keys.
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::AtomicBool;

use aether_midnight::{cloud, code, config, embed, memory};
use serde_json::{Value, json};
use tempfile::TempDir;

mod common {
    pub mod mock;
}
use common::mock::{Mock, Reply, ok_response};

const KEY: &str = "sk-mcp-test-never-real";
const DIMS: usize = 16;

struct Fixture {
    temp: TempDir,
    db: PathBuf,
    repo: PathBuf,
    config: PathBuf,
    settings: config::Settings,
}

impl Fixture {
    fn new(mock: &Mock) -> Self {
        let temp = TempDir::new().unwrap();
        let db = temp.path().join("m.sqlite3");
        let repo = temp.path().join("repo");
        let config = temp.path().join("config");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(
            repo.join("Products.tsx"),
            "const description = 'Privacy-first personal intelligence';\n",
        )
        .unwrap();
        let mut settings = config::Settings::lexical();
        settings.provider = config::Choice::Openrouter;
        settings.model = Some("mock-embedding".into());
        settings.dimensions = Some(DIMS);
        settings.endpoint = Some(mock.url.clone());
        settings.upload_consent = Some(settings.consent_token().unwrap());
        let f = Self {
            temp,
            db,
            repo,
            config,
            settings,
        };
        f.save();
        code::index_repository(&f.db, &f.repo, false, false, &AtomicBool::new(false)).unwrap();
        f
    }

    fn save(&self) {
        fs::write(
            self.config.join("config.json"),
            serde_json::to_vec(&self.settings).unwrap(),
        )
        .unwrap();
    }

    fn seed_vectors(&self) {
        let mut e = cloud::CloudEmbedder::new(
            self.settings.cloud_config().unwrap(),
            KEY.into(),
            "OPENROUTER_API_KEY",
        )
        .unwrap();
        embed::embed_code(&self.db, &self.repo, &mut e, &AtomicBool::new(false)).unwrap();
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aether-midnight"));
        cmd.args(["--db", self.db.to_str().unwrap()])
            .env("AETHER_MIDNIGHT_CONFIG_DIR", &self.config)
            .env(
                "AETHER_MIDNIGHT_MODELS_DIR",
                self.temp.path().join("no-models"),
            )
            .env("OPENROUTER_API_KEY", KEY)
            .env_remove("RAINY_API_KEY");
        cmd
    }

    fn mcp(&self, extra: &[&str]) -> Mcp {
        let mut child = self
            .command()
            .arg("mcp")
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Mcp {
            child,
            input,
            output,
            id: 0,
        }
    }
}

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    id: u64,
}
impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Mcp {
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], self.id);
        response
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.rpc("tools/call", json!({"name":name,"arguments":arguments}));
        assert_eq!(response["result"]["isError"], false, "{response}");
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }
}

#[test]
fn configured_mcp_defaults_to_hybrid_caches_queries_and_preserves_explicit_lexical() {
    let mock = Mock::start(|_, req| ok_response(req, DIMS));
    let f = Fixture::new(&mock);
    f.seed_vectors();
    let seeded = mock.count();
    let mut mcp = f.mcp(&[]);
    let tools = mcp.rpc("tools/list", json!({}));
    let tool = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "search_code")
        .unwrap();
    assert_eq!(
        tool["inputSchema"]["properties"]["mode"]["enum"],
        json!(["lexical", "semantic", "hybrid"])
    );
    assert_eq!(tool["annotations"]["openWorldHint"], true);
    let args = json!({"repo_path":f.repo,"query":"Privacy-first personal intelligence"});
    let hybrid = mcp.call("search_code", args.clone());
    assert_eq!(hybrid["mode_used"], "hybrid");
    assert_eq!(hybrid["degraded"], false);
    assert_eq!(hybrid["results"][0]["path"], "Products.tsx");
    assert_eq!(mock.count(), seeded + 1);
    let repeat = mcp.call("search_code", args.clone());
    assert_eq!(repeat["results"], hybrid["results"]);
    assert_eq!(mock.count(), seeded + 1);
    let mut explicit = args.clone();
    explicit["mode"] = json!("lexical");
    let lexical = mcp.call("search_code", explicit.clone());
    assert!(lexical.get("mode_used").is_none());
    assert_eq!(mock.count(), seeded + 1);
    explicit["mode"] = json!("semantic");
    assert_eq!(mcp.call("search_code", explicit)["mode_used"], "semantic");
    assert_eq!(mock.count(), seeded + 1);
    // Cache is an embedding cache, not a result cache: current-source freshness still applies.
    fs::write(f.repo.join("Products.tsx"), "// changed\n").unwrap();
    let changed = mcp.call("search_code", args);
    assert!(
        !changed["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["freshness"] == "fresh")
    );
    assert_eq!(mock.count(), seeded + 1);
}

#[test]
fn mcp_revoked_consent_and_wrong_space_send_nothing_even_with_cached_queries() {
    let mock = Mock::start(|_, req| ok_response(req, DIMS));
    let mut f = Fixture::new(&mock);
    f.seed_vectors();
    let mut mcp = f.mcp(&[]);
    let args =
        json!({"repo_path":f.repo,"query":"Privacy-first personal intelligence","mode":"hybrid"});
    assert_eq!(mcp.call("search_code", args.clone())["degraded"], false);
    let count = mock.count();
    f.settings.upload_consent = None;
    f.save();
    let denied = mcp.call("search_code", args.clone());
    assert_eq!(denied["mode_used"], "lexical");
    assert_eq!(denied["degraded"], true);
    assert!(denied["notice"].as_str().unwrap().contains("consent"));
    assert_eq!(mock.count(), count);
    f.settings.model = Some("other-model".into());
    f.settings.upload_consent = Some(f.settings.consent_token().unwrap());
    f.save();
    let wrong = mcp.call("search_code", args);
    assert_eq!(wrong["degraded"], true);
    assert!(
        wrong["notice"]
            .as_str()
            .unwrap()
            .contains("different embedding space")
    );
    assert_eq!(mock.count(), count);
}

#[test]
fn mcp_index_updates_embeddings_incrementally_and_provider_failure_preserves_lexical() {
    let mock = Mock::start(|n, req| {
        if n >= 1 {
            Reply::Json(401, json!({"error":{"message":"rejected"}}).to_string())
        } else {
            ok_response(req, DIMS)
        }
    });
    let f = Fixture::new(&mock);
    let mut mcp = f.mcp(&[]);
    let indexed = mcp.call("index_repository", json!({"repo_path":f.repo}));
    assert_eq!(indexed["embedding"]["embedded"], 1);
    let count = mock.count();
    let repeated = mcp.call("index_repository", json!({"repo_path":f.repo}));
    assert_eq!(repeated["embedding"]["unchanged"], 1);
    assert_eq!(mock.count(), count);
    let result = mcp.call(
        "search_code",
        json!({"repo_path":f.repo,"query":"Privacy-first personal intelligence"}),
    );
    assert_eq!(result["degraded"], true);
    assert_eq!(result["results"][0]["path"], "Products.tsx");
    assert!(!result.to_string().contains(KEY));
    fs::write(f.repo.join("new.txt"), "new searchable evidence\n").unwrap();
    let failed_index = mcp.call("index_repository", json!({"repo_path":f.repo}));
    assert_eq!(failed_index["complete"], true);
    assert!(failed_index["embedding_notice"].is_string());
    let lexical = mcp.call(
        "search_code",
        json!({"repo_path":f.repo,"query":"searchable evidence","mode":"lexical"}),
    );
    assert_eq!(lexical["results"][0]["path"], "new.txt");
}

#[test]
fn unconfigured_mcp_stays_lexical_and_invalid_modes_fail_without_requests() {
    let mock = Mock::start(|_, req| ok_response(req, DIMS));
    let f = Fixture::new(&mock);
    fs::remove_file(f.config.join("config.json")).unwrap();
    let mut mcp = f.mcp(&[]);
    let result = mcp.call(
        "search_code",
        json!({"repo_path":f.repo,"query":"Privacy-first personal intelligence"}),
    );
    assert!(result.get("mode_used").is_none());
    for mode in [json!("typo"), json!(null), json!(7)] {
        let r = mcp.rpc("tools/call", json!({"name":"search_code","arguments":{"repo_path":f.repo,"query":"marker","mode":mode}}));
        assert_eq!(r["result"]["isError"], true);
    }
    assert_eq!(mock.count(), 0);
}

#[test]
fn recall_modes_preserve_lexical_default_scope_and_time_contracts() {
    let mock = Mock::start(|_, req| ok_response(req, DIMS));
    let f = Fixture::new(&mock);
    memory::remember(
        &f.db,
        "project:demo",
        "source",
        "privacy intelligence evidence",
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let mut e = cloud::CloudEmbedder::new(
        f.settings.cloud_config().unwrap(),
        KEY.into(),
        "OPENROUTER_API_KEY",
    )
    .unwrap();
    embed::embed_memories(&f.db, "project:demo", &mut e, &AtomicBool::new(false)).unwrap();
    let seeded = mock.count();
    let mut mcp = f.mcp(&["--memory-scope", "project:demo"]);
    assert!(mcp.call("recall", json!({"query":"privacy"})).is_array());
    assert_eq!(mock.count(), seeded);
    assert_eq!(
        mcp.call("recall", json!({"query":"privacy","mode":"hybrid"}))["mode_used"],
        "hybrid"
    );
    let count = mock.count();
    for extra in [
        json!({"at":"2025-01-01T00:00:00Z"}),
        json!({"broad_terms":"recent"}),
        json!({"scope":"project:other"}),
    ] {
        let mut args = json!({"query":"privacy","mode":"semantic"});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let r = mcp.rpc("tools/call", json!({"name":"recall","arguments":args}));
        assert_eq!(r["result"]["isError"], true);
    }
    assert_eq!(mock.count(), count);
}

#[test]
fn http_tokens_require_their_own_remote_grant_in_addition_to_saved_consent() {
    use std::io::Read;
    use std::net::TcpStream;
    let mock = Mock::start(|_, req| ok_response(req, DIMS));
    let f = Fixture::new(&mock);
    f.seed_vectors();
    let seeded = mock.count();
    let mint = |remote: bool| {
        let mut cmd = f.command();
        cmd.args([
            "token",
            "create",
            "--name",
            if remote { "remote" } else { "local" },
            "--tools",
            "search_code,index_repository",
            "--repo-root",
            f.repo.to_str().unwrap(),
        ]);
        if remote {
            cmd.arg("--allow-remote-embeddings");
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let local = mint(false);
    let remote = mint(true);
    let mut child = f
        .command()
        .arg("serve-http")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let address = line
        .trim()
        .strip_prefix("listening http://")
        .unwrap()
        .trim_end_matches("/mcp")
        .to_string();
    let call = |token: &str, name: &str, args: Value| {
        let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}).to_string();
        let mut s = TcpStream::connect(&address).unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        write!(s,"POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let response: Value = serde_json::from_str(out.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(response["result"]["isError"], false, "{response}");
        serde_json::from_str::<Value>(response["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap()
    };
    let args = json!({"repo_path":f.repo,"query":"Privacy-first personal intelligence"});
    assert!(
        call(&local, "search_code", args.clone())
            .get("mode_used")
            .is_none()
    );
    let mut explicit = args.clone();
    explicit["mode"] = json!("hybrid");
    let denied = call(&local, "search_code", explicit);
    assert_eq!(denied["degraded"], true);
    assert!(denied["notice"].as_str().unwrap().contains("not permitted"));
    let indexed = call(&local, "index_repository", json!({"repo_path":f.repo}));
    assert!(indexed["embedding_notice"].is_string());
    assert_eq!(mock.count(), seeded);
    assert_eq!(call(&remote, "search_code", args)["mode_used"], "hybrid");
    assert_eq!(mock.count(), seeded + 1);
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn project_opt_out_blocks_implicit_and_explicit_remote_calls() {
    let mock = Mock::start(|_, req| ok_response(req, DIMS));
    let f = Fixture::new(&mock);
    f.seed_vectors();
    let count = mock.count();
    // Adopt project storage through the product entry point; this also records trust in the
    // fixture's isolated configuration directory, never in the developer's trust registry.
    let out = Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .env("AETHER_MIDNIGHT_CONFIG_DIR", &f.config)
        .args(["init", "--storage", "project", f.repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::write(
        f.repo.join(".midnight/midnight.json"),
        r#"{"version":1,"embeddings":false}"#,
    )
    .unwrap();
    let mut mcp = f.mcp(&[]);
    let mut args = json!({"repo_path":f.repo,"query":"Privacy-first personal intelligence"});
    assert!(
        mcp.call("search_code", args.clone())
            .get("mode_used")
            .is_none()
    );
    args["mode"] = json!("hybrid");
    assert_eq!(mcp.call("search_code", args)["degraded"], true);
    assert_eq!(mock.count(), count);
}

#[test]
fn local_only_stdio_and_transient_provider_outages_do_not_upload_or_retry() {
    let mock = Mock::start(|n, req| {
        if n == 0 {
            ok_response(req, DIMS)
        } else {
            Reply::Json(
                503,
                json!({"error":{"message":"temporarily unavailable"}}).to_string(),
            )
        }
    });
    let f = Fixture::new(&mock);
    f.seed_vectors();
    let seeded = mock.count();
    let mut offline = f.mcp(&["--local-only"]);
    let mut args = json!({"repo_path":f.repo,"query":"Privacy-first personal intelligence"});
    assert!(
        offline
            .call("search_code", args.clone())
            .get("mode_used")
            .is_none()
    );
    args["mode"] = json!("hybrid");
    assert_eq!(offline.call("search_code", args.clone())["degraded"], true);
    assert_eq!(mock.count(), seeded);
    let mut online = f.mcp(&[]);
    let degraded = online.call("search_code", args);
    assert_eq!(degraded["degraded"], true);
    assert_eq!(degraded["results"][0]["path"], "Products.tsx");
    assert_eq!(
        mock.count(),
        seeded + 1,
        "interactive queries must not retry a 503"
    );
}

#[test]
fn query_cache_evicts_oldest_at_32_entries_and_does_not_cross_connections() {
    let mock = Mock::start(|_, req| ok_response(req, DIMS));
    let f = Fixture::new(&mock);
    f.seed_vectors();
    let seeded = mock.count();
    let mut mcp = f.mcp(&[]);
    for i in 0..33 {
        assert_eq!(
            mcp.call(
                "search_code",
                json!({"repo_path":f.repo,"query":format!("query {i}"),"mode":"semantic"})
            )["degraded"],
            false
        );
    }
    assert_eq!(mock.count(), seeded + 33);
    let first = json!({"repo_path":f.repo,"query":"query 0","mode":"semantic"});
    mcp.call("search_code", first.clone());
    assert_eq!(mock.count(), seeded + 34);
    mcp.call("search_code", first.clone());
    assert_eq!(mock.count(), seeded + 34);
    f.mcp(&[]).call("search_code", first);
    assert_eq!(mock.count(), seeded + 35);
}
