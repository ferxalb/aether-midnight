//! First-run journey through the real `midnight` / `aether-midnight` processes, starting from a
//! clean configuration directory. Cloud flows use the deterministic loopback mock only: nothing
//! here proves behaviour against a live Rainy or OpenRouter account.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

mod common {
    pub mod mock;
}
use common::mock::{Mock, Reply, ok_response};

const KEY: &str = "sk-journey-SECRET-123456";
const DIMS: usize = 16;

struct Sandbox {
    _temp: TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let repo = root.join("project");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("src/billing.rs"),
            "pub fn compute_invoice_total(items: &[u32]) -> u32 {\n    items.iter().sum()\n}\n",
        )
        .unwrap();
        fs::write(
            repo.join("src/notes.rs"),
            "pub fn archive_old_records() {\n    // nightly retention sweep\n}\n",
        )
        .unwrap();
        Sandbox {
            _temp: temp,
            root,
            repo,
        }
    }
    fn db(&self) -> PathBuf {
        self.root.join("data/midnight.sqlite3")
    }
    fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    fn cmd(&self, bin: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(bin);
        cmd.arg("--db")
            .arg(self.db())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("AETHER_MIDNIGHT_CONFIG_DIR", self.config_dir())
            .env("AETHER_MIDNIGHT_MODELS_DIR", self.root.join("models"))
            .env_remove("ORT_DYLIB_PATH")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("RAINY_API_KEY")
            .env_remove("MIDNIGHT_NO_INPUT");
        cmd
    }

    fn run_with(&self, bin: &str, stdin: &str, env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut cmd = self.cmd(bin, args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn mn(&self, stdin: &str, args: &[&str]) -> Output {
        self.run_with(env!("CARGO_BIN_EXE_midnight"), stdin, &[], args)
    }
}

fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_ok(out: &Output, what: &str) -> (String, String) {
    let (o, e) = text(out);
    assert!(
        out.status.success(),
        "{what} failed\nstdout:{o}\nstderr:{e}"
    );
    (o, e)
}

fn assert_no_secret(label: &str, s: &str) {
    assert!(!s.contains(KEY), "{label} leaked the API key");
}

fn mock() -> Mock {
    Mock::start(|_, r| {
        if r.path.ends_with("/embeddings/models") {
            Reply::Json(
                200,
                r#"{"data":[{"id":"test/embed-small"},{"id":"test/embed-large"}]}"#.into(),
            )
        } else {
            ok_response(r, DIMS)
        }
    })
}

fn embed_inputs(m: &Mock) -> Vec<String> {
    m.recorded()
        .iter()
        .filter(|r| r.path.ends_with("/embeddings"))
        .flat_map(|r| {
            r.body["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn both_binaries_are_discoverable_and_share_one_help() {
    let s = Sandbox::new();
    let mn = s.mn("", &["--help"]);
    let (o, _) = assert_ok(&mn, "midnight --help");
    for word in ["setup", "index", "search", "status", "Get started"] {
        assert!(o.contains(word), "help should mention {word}:\n{o}");
    }
    assert!(o.contains("Usage: midnight"), "{o}");
    let legacy = s.run_with(env!("CARGO_BIN_EXE_aether-midnight"), "", &[], &["--help"]);
    let (lo, _) = assert_ok(&legacy, "aether-midnight --help");
    assert!(lo.contains("Usage: aether-midnight"), "{lo}");
}

#[test]
fn status_on_a_clean_machine_explains_next_steps_and_creates_nothing() {
    let s = Sandbox::new();
    let out = s.mn("", &["status"]);
    let (o, _) = assert_ok(&out, "status");
    assert!(o.contains("not configured"), "{o}");
    assert!(o.contains("midnight setup"), "{o}");
    assert!(o.contains("not indexed"), "{o}");
    assert!(!s.db().exists(), "status must not create the database");
    assert!(!s.config_dir().exists(), "status must not write config");
    let j: Value = serde_json::from_slice(&s.mn("", &["status", "--json"]).stdout).unwrap();
    assert_eq!(j["ok"], true);
    assert_eq!(j["provider"]["configured"], false);
}

#[test]
fn lexical_first_run_needs_no_network_onnx_or_model() {
    let s = Sandbox::new();
    // Interactive wizard fed by a pipe: choose option 4 (lexical).
    let setup = s.mn("4\n", &["setup"]);
    let (o, e) = assert_ok(&setup, "setup lexical");
    assert!(e.contains("How should Midnight search"), "{e}");
    assert!(o.contains("provider   lexical"), "{o}");
    assert!(s.config_dir().join("config.json").exists());
    assert!(
        !s.config_dir().join("credentials.json").exists(),
        "no key for lexical"
    );

    let index = s.mn("", &["index", ".", "--human"]);
    let (o, _) = assert_ok(&index, "index");
    assert!(o.contains("Indexed"), "{o}");
    assert!(o.contains("embeddings: off"), "{o}");

    let search = s.mn("", &["search", "compute_invoice_total", "--human"]);
    let (o, _) = assert_ok(&search, "search");
    assert!(o.contains("src/billing.rs"), "{o}");
    assert!(o.contains("lexical"), "{o}");
    assert!(o.contains("compute_invoice_total"), "{o}");

    let status = s.mn("", &["status", "--json"]);
    let j: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(j["provider"]["provider"], "lexical");
    assert_eq!(j["repository"]["indexed"], true);
    assert_eq!(j["ok"], true);
    assert!(!s.root.join("models").exists(), "no model was fetched");
}

#[test]
fn interactive_cloud_wizard_then_index_search_status_end_to_end() {
    let s = Sandbox::new();
    let m = mock();
    // 2=OpenRouter, hidden-key line, model #1 from discovery, 16 dims, allow uploads.
    let input = format!("2\n{KEY}\n1\n{DIMS}\ny\n");
    let setup = s.mn(&input, &["setup", "--endpoint", &m.url]);
    let (o, e) = assert_ok(&setup, "interactive cloud setup");
    assert_no_secret("setup stdout", &o);
    assert_no_secret("setup stderr", &e);
    assert!(e.contains("test/embed-small"), "discovery listing:\n{e}");
    assert!(e.contains("ok: the provider accepted"), "{e}");
    assert!(o.contains("validated  yes"), "{o}");
    assert!(o.contains("uploads    allowed"), "{o}");

    // Secret handling: key only in the 0600 credential file, never in config or output.
    let cfg = fs::read_to_string(s.config_dir().join("config.json")).unwrap();
    assert!(!cfg.contains(KEY));
    assert!(cfg.contains("test/embed-large") && cfg.contains("openrouter"));
    let creds = s.config_dir().join("credentials.json");
    assert!(fs::read_to_string(&creds).unwrap().contains(KEY));
    assert_eq!(mode_of(&creds), 0o600);
    assert_eq!(mode_of(&s.config_dir()), 0o700);

    // The setup probe sent only the fixed test string, authenticated with the entered key.
    let sent = embed_inputs(&m);
    assert_eq!(sent, vec!["midnight setup check".to_owned()]);
    let probe = m
        .recorded()
        .into_iter()
        .find(|r| r.path.ends_with("/embeddings"))
        .unwrap();
    assert_eq!(probe.headers["authorization"], format!("Bearer {KEY}"));

    // `index .` embeds automatically with the saved provider.
    let index = s.mn("", &["index", ".", "--human"]);
    let (o, e) = assert_ok(&index, "index");
    assert_no_secret("index out", &o);
    assert_no_secret("index err", &e);
    // Progress for people goes to stderr (stdout stays the report); not a terminal here, so plain lines.
    assert!(
        e.contains("Indexing ") && e.contains("Indexed 2 files in"),
        "{e}"
    );
    assert!(
        e.contains("Embedding with openrouter (test/embed-large): 2 files")
            && e.contains("Embedded 2 files in"),
        "{e}"
    );
    assert!(
        !o.contains("Embedding with"),
        "progress must not leak into stdout:\n{o}"
    );
    assert!(
        o.contains("embeddings:") && o.contains("openrouter:test/embed-large"),
        "{o}"
    );
    let sent = embed_inputs(&m);
    assert!(sent.iter().any(|t| t.contains("compute_invoice_total")));
    assert!(
        sent.iter().all(|t| !t.contains(KEY)),
        "credentials never ride in content"
    );

    // Re-index is incremental: nothing is re-sent.
    let before = m.count();
    assert_ok(&s.mn("", &["index", ".", "--human"]), "re-index");
    assert_eq!(m.count(), before, "unchanged files are not re-uploaded");

    // `search` uses the configured retrieval mode with no flags at all.
    let search = s.mn("", &["search", "invoice total", "--human"]);
    let (o, _) = assert_ok(&search, "search");
    assert!(o.contains("hybrid"), "{o}");
    assert!(o.contains("src/billing.rs"), "{o}");
    assert!(o.contains("semantic coverage: 2/2"), "{o}");
    let sj: Value =
        serde_json::from_slice(&s.mn("", &["search", "invoice total", "--json"]).stdout).unwrap();
    assert_eq!(sj["mode_used"], "hybrid");
    assert_eq!(sj["degraded"], false);
    assert_eq!(sj["results"][0]["path"], "src/billing.rs");

    // `status` reports provider, model, credentials source (not value) and index records.
    let status = s.mn("", &["status"]);
    let (o, _) = assert_ok(&status, "status");
    assert_no_secret("status", &o);
    assert!(o.contains("openrouter · test/embed-large · 16d"), "{o}");
    assert!(o.contains("Credential  stored"), "{o}");
    assert!(o.contains("Uploads     allowed"), "{o}");
    assert!(o.contains("2 files indexed"), "{o}");
    assert!(o.contains("Embeddings  2/2"), "{o}");
}

#[test]
fn remote_upload_requires_consent_and_yes_authorises_one_run() {
    let s = Sandbox::new();
    let m = mock();
    let setup = s.run_with(
        env!("CARGO_BIN_EXE_midnight"),
        &format!("{KEY}\n"),
        &[],
        &[
            "setup",
            "--provider",
            "openrouter",
            "--model",
            "vendor/m",
            "--dimensions",
            "16",
            "--endpoint",
            &m.url,
            "--api-key-stdin",
        ],
    );
    let (o, _) = assert_ok(&setup, "scripted setup without consent");
    assert!(o.contains("not yet allowed"), "{o}");
    let after_probe = m.count();

    // Non-interactive, no consent: the keyword index completes, nothing is uploaded, exit != 0.
    let denied = s.mn("", &["index", "."]);
    assert!(!denied.status.success());
    let (o, e) = text(&denied);
    assert!(e.contains("not authorised") && e.contains("--yes"), "{e}");
    assert_no_secret("denied stderr", &e);
    assert_eq!(
        m.count(),
        after_probe,
        "no content may be sent without consent"
    );
    let v: Value = serde_json::from_str(&o).expect("JSON result when piped");
    assert!(v["embedding"]["error"].as_str().is_some());
    let st: Value = serde_json::from_slice(&s.mn("", &["status", "--json"]).stdout).unwrap();
    assert_eq!(st["repository"]["indexed"], true, "lexical index kept");
    assert!(
        st["next"].to_string().contains("not authorised"),
        "status explains how to fix: {st}"
    );

    // --yes authorises this run only; consent is not persisted.
    assert_ok(&s.mn("", &["index", ".", "--yes"]), "index --yes");
    assert!(m.count() > after_probe);
    let st: Value = serde_json::from_slice(&s.mn("", &["status", "--json"]).stdout).unwrap();
    assert_eq!(st["provider"]["upload_consent"], false);

    // --no-embed never touches the provider even with consent.
    let n = m.count();
    assert_ok(
        &s.mn("", &["index", ".", "--no-embed", "--yes"]),
        "no-embed",
    );
    assert_eq!(m.count(), n);
}

#[test]
fn scripted_setup_fails_clearly_and_saves_nothing_on_bad_input() {
    let s = Sandbox::new();
    let m = Mock::start(|_, _| Reply::Status(401, "{}"));

    // No key: names the variable, never prompts, saves nothing.
    let out = s.mn(
        "",
        &[
            "setup",
            "--provider",
            "rainy",
            "--model",
            "x",
            "--dimensions",
            "8",
        ],
    );
    assert!(!out.status.success());
    assert!(text(&out).1.contains("RAINY_API_KEY"), "{:?}", text(&out));
    assert!(!s.config_dir().join("config.json").exists());

    // Keys are never accepted on the command line.
    let out = s.mn("", &["setup", "--provider", "rainy", "--api-key", KEY]);
    assert!(!out.status.success());

    // --no-input without --provider refuses to guess.
    let out = s.mn("", &["setup", "--no-input"]);
    assert!(!out.status.success());
    assert!(text(&out).1.contains("--provider"));

    // Rejected key (HTTP 401): validation error, no secret echoed, nothing saved.
    let out = s.run_with(
        env!("CARGO_BIN_EXE_midnight"),
        "",
        &[("OPENROUTER_API_KEY", KEY)],
        &[
            "setup",
            "--provider",
            "openrouter",
            "--model",
            "vendor/m",
            "--dimensions",
            "16",
            "--endpoint",
            &m.url,
        ],
    );
    assert!(!out.status.success());
    let (o, e) = text(&out);
    assert!(e.contains("authentication failed"), "{e}");
    assert_no_secret("stdout", &o);
    assert_no_secret("stderr", &e);
    assert!(!s.config_dir().join("config.json").exists());

    // Interactive EOF yields guidance, not a hang or a panic.
    let out = s.mn("", &["setup"]);
    assert!(!out.status.success());
    assert!(text(&out).1.contains("input ended"));
}

#[test]
fn environment_key_is_used_without_being_copied_to_disk() {
    let s = Sandbox::new();
    let m = mock();
    let env = [("OPENROUTER_API_KEY", KEY)];
    let out = s.run_with(
        env!("CARGO_BIN_EXE_midnight"),
        "",
        &env,
        &[
            "setup",
            "--provider",
            "openrouter",
            "--model",
            "vendor/m",
            "--dimensions",
            "16",
            "--endpoint",
            &m.url,
            "--allow-remote-upload",
            "--json",
        ],
    );
    assert_ok(&out, "setup with env key");
    assert!(!s.config_dir().join("credentials.json").exists());
    let idx = s.run_with(env!("CARGO_BIN_EXE_midnight"), "", &env, &["index", "."]);
    let (o, _) = assert_ok(&idx, "index with env key");
    let v: Value = serde_json::from_str(&o).unwrap();
    assert!(v["embedding"]["embedded"].as_u64().unwrap() >= 2);
    // Without the variable the credential is reported missing, with the fix.
    let st = s.mn("", &["status"]);
    assert!(!st.status.success());
    let (o, _) = text(&st);
    assert!(o.contains("OPENROUTER_API_KEY"), "{o}");
}

#[test]
fn legacy_flags_and_json_contract_are_preserved() {
    let s = Sandbox::new();
    let repo = s.repo.to_str().unwrap().to_owned();
    for bin in [
        env!("CARGO_BIN_EXE_aether-midnight"),
        env!("CARGO_BIN_EXE_midnight"),
    ] {
        let out = s.run_with(bin, "", &[], &["index", "--repo", &repo]);
        let (o, _) = assert_ok(&out, "legacy index");
        let v: Value = serde_json::from_str(&o).expect("piped output stays JSON");
        assert_eq!(v["complete"], true);
        let out = s.run_with(
            bin,
            "",
            &[],
            &[
                "search",
                "--repo",
                &repo,
                "--query",
                "archive_old_records",
                "--mode",
                "lexical",
            ],
        );
        let (o, _) = assert_ok(&out, "legacy search");
        let v: Value = serde_json::from_str(&o).unwrap();
        assert_eq!(v["results"][0]["path"], "src/notes.rs");
        assert!(v.get("mode_used").is_none(), "lexical JSON shape unchanged");
    }
    // --json overrides --human-by-default contexts explicitly.
    let out = s.mn("", &["search", "archive_old_records", "--json"]);
    assert!(serde_json::from_slice::<Value>(&out.stdout).is_ok());
}
