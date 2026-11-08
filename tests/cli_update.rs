//! `update` and `uninstall` through real processes. Releases are served by a loopback mock that
//! stands in for the GitHub API; the "installed" program is a copy of the test binary in a
//! temporary directory, so nothing outside the sandbox is read or changed. Behaviour against the
//! real GitHub Releases is not covered here.
#![cfg(unix)]
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A release archive whose "binary" is a shell script reporting `reported` as its version.
fn make_archive(work: &Path, version: &str, reported: &str, sums_lie: bool) -> Vec<u8> {
    let name = format!("aether-midnight-{version}-macos-universal2");
    let root = work.join(&name);
    fs::create_dir_all(root.join("bin")).unwrap();
    let script = format!("#!/bin/sh\necho \"aether-midnight {reported}\"\n");
    fs::write(root.join("bin/aether-midnight"), &script).unwrap();
    let recorded = if sums_lie {
        "0".repeat(64)
    } else {
        sha256(script.as_bytes())
    };
    fs::write(
        root.join("SHA256SUMS"),
        format!("{recorded}  ./bin/aether-midnight\n"),
    )
    .unwrap();
    let out = work.join(format!("{name}.tar.gz"));
    let status = Command::new("tar")
        .arg("--format=ustar")
        .arg("-czf")
        .arg(&out)
        .arg("-C")
        .arg(work)
        .arg(&name)
        .env("COPYFILE_DISABLE", "1")
        .status()
        .unwrap();
    assert!(status.success());
    fs::read(out).unwrap()
}

struct Server {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
}

/// Serves `files` by path and the release list at the GitHub API path.
fn serve(
    files: HashMap<String, Vec<u8>>,
    releases: impl Fn(&str) -> Value + Send + 'static,
) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&hits);
    let origin = base.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                request.push(byte[0]);
            }
            let line = String::from_utf8_lossy(&request);
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            log.lock().unwrap().push(path.clone());
            let (status, body) = if path.starts_with("/repos/ferxalbs/aether-midnight/releases") {
                (200, serde_json::to_vec(&releases(&origin)).unwrap())
            } else if let Some(bytes) = files.get(&path) {
                (200, bytes.clone())
            } else {
                (404, b"not found".to_vec())
            };
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    Server { base, hits }
}

fn release(origin: &str, version: &str, pre: bool) -> Value {
    let name = format!("aether-midnight-{version}-macos-universal2.tar.gz");
    json!({
        "tag_name": format!("v{version}"), "prerelease": pre && version.contains('-'), "draft": false,
        "assets": [
            {"name": name, "browser_download_url": format!("{origin}/dl/{name}")},
            {"name": format!("{name}.sha256"), "browser_download_url": format!("{origin}/dl/{name}.sha256")},
        ]
    })
}

struct Install {
    _temp: TempDir,
    root: PathBuf,
    bin: PathBuf,
    link: PathBuf,
}

impl Install {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let bindir = root.join("prefix/bin");
        fs::create_dir_all(&bindir).unwrap();
        let bin = bindir.join("aether-midnight");
        fs::copy(env!("CARGO_BIN_EXE_aether-midnight"), &bin).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let link = bindir.join("midnight");
        std::os::unix::fs::symlink("aether-midnight", &link).unwrap();
        Install {
            _temp: temp,
            root,
            bin,
            link,
        }
    }
    fn run(&self, program: &Path, api: Option<&str>, args: &[&str]) -> Output {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("XDG_DATA_HOME", self.root.join("home/.local/share"))
            .env("AETHER_MIDNIGHT_CONFIG_DIR", self.root.join("cfg"))
            .env_remove("CODEX_HOME");
        if let Some(api) = api {
            cmd.env("AETHER_MIDNIGHT_UPDATE_API", api);
        }
        fs::create_dir_all(self.root.join("home")).unwrap();
        cmd.output().unwrap()
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn files_for(archive: &[u8], version: &str) -> HashMap<String, Vec<u8>> {
    let name = format!("aether-midnight-{version}-macos-universal2.tar.gz");
    let mut files = HashMap::new();
    files.insert(
        format!("/dl/{name}.sha256"),
        format!("{}  {name}\n", sha256(archive)).into_bytes(),
    );
    files.insert(format!("/dl/{name}"), archive.to_vec());
    files
}

#[test]
fn check_reports_a_newer_prerelease_without_downloading_or_changing_anything() {
    let install = Install::new();
    let work = TempDir::new().unwrap();
    let archive = make_archive(work.path(), "99.0.0", "99.0.0", false);
    let server = serve(files_for(&archive, "99.0.0"), |o| {
        json!([release(o, "99.0.0", true), release(o, "0.1.0-rc.1", true)])
    });
    let before = fs::read(&install.bin).unwrap();

    let out = install.run(
        &install.link,
        Some(&server.base),
        &["update", "--check", "--json"],
    );
    assert!(out.status.success(), "{}", text(&out));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["update_available"], true);
    assert_eq!(report["latest"], "99.0.0");
    assert_eq!(report["current"], CURRENT);
    assert_eq!(fs::read(&install.bin).unwrap(), before);
    assert!(
        server
            .hits
            .lock()
            .unwrap()
            .iter()
            .all(|p| p.starts_with("/repos/")),
        "--check must only read the release list: {:?}",
        server.hits.lock().unwrap()
    );
}

#[test]
fn update_installs_the_verified_release_and_keeps_the_alias() {
    let install = Install::new();
    let work = TempDir::new().unwrap();
    let archive = make_archive(work.path(), "99.0.0", "99.0.0", false);
    let server = serve(files_for(&archive, "99.0.0"), |o| {
        json!([release(o, "99.0.0", true)])
    });

    let out = install.run(&install.link, Some(&server.base), &["upgrade"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("99.0.0"), "{}", text(&out));

    // The installed file is now the new program; the alias still resolves to it.
    let version = Command::new(&install.link)
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "aether-midnight 99.0.0"
    );
    assert_eq!(
        fs::read_link(&install.link).unwrap(),
        Path::new("aether-midnight")
    );
    assert_eq!(
        fs::metadata(&install.bin).unwrap().permissions().mode() & 0o111,
        0o111
    );
    let leftovers: Vec<_> = fs::read_dir(install.bin.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".aether-midnight.update"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_bad_download_never_replaces_the_installed_program() {
    let cases: [(&str, bool, &str, bool); 3] = [
        // (label, sums lie, reported version, corrupt the .sha256)
        ("internal SHA256SUMS mismatch", true, "99.0.0", false),
        ("wrong reported version", false, "9.9.9", false),
        ("archive checksum mismatch", false, "99.0.0", true),
    ];
    for (label, sums_lie, reported, corrupt) in cases {
        let install = Install::new();
        let work = TempDir::new().unwrap();
        let archive = make_archive(work.path(), "99.0.0", reported, sums_lie);
        let mut files = files_for(&archive, "99.0.0");
        if corrupt {
            files.insert(
                "/dl/aether-midnight-99.0.0-macos-universal2.tar.gz.sha256".into(),
                format!("{}\n", "b".repeat(64)).into_bytes(),
            );
        }
        let server = serve(files, |o| json!([release(o, "99.0.0", true)]));
        let before = fs::read(&install.bin).unwrap();
        let out = install.run(&install.link, Some(&server.base), &["update"]);
        assert!(!out.status.success(), "{label}: {}", text(&out));
        assert_eq!(
            fs::read(&install.bin).unwrap(),
            before,
            "{label}: binary must be untouched"
        );
        assert!(
            fs::read_dir(install.bin.parent().unwrap())
                .unwrap()
                .all(|e| !e
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".aether-midnight.update")),
            "{label}: staged file must be cleaned up"
        );
    }
}

#[test]
fn asset_urls_pointing_at_other_hosts_are_refused() {
    let install = Install::new();
    let server = serve(HashMap::new(), |_| {
        let name = "aether-midnight-99.0.0-macos-universal2.tar.gz";
        json!([{"tag_name": "v99.0.0", "prerelease": true, "draft": false, "assets": [
            {"name": name, "browser_download_url": format!("https://evil.example/{name}")},
            {"name": format!("{name}.sha256"), "browser_download_url": format!("https://evil.example/{name}.sha256")},
        ]}])
    });
    let out = install.run(&install.link, Some(&server.base), &["update"]);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("not an allowed release host"),
        "{}",
        text(&out)
    );
}

#[test]
fn update_refuses_a_development_build_and_non_loopback_http_overrides() {
    let install = Install::new();
    let work = TempDir::new().unwrap();
    let archive = make_archive(work.path(), "99.0.0", "99.0.0", false);
    let server = serve(files_for(&archive, "99.0.0"), |o| {
        json!([release(o, "99.0.0", true)])
    });
    // The cargo-built binary lives in target/debug: --check works, a real update is refused.
    let dev = Path::new(env!("CARGO_BIN_EXE_midnight"));
    let out = install.run(dev, Some(&server.base), &["update", "--check"]);
    assert!(out.status.success(), "{}", text(&out));
    let out = install.run(dev, Some(&server.base), &["update"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("development build"), "{}", text(&out));

    let out = install.run(
        &install.link,
        Some("http://example.com"),
        &["update", "--check"],
    );
    assert!(!out.status.success());
    assert!(text(&out).contains("loopback"), "{}", text(&out));
}

#[test]
fn update_to_an_unpublished_version_and_when_current_is_newest() {
    let install = Install::new();
    let server = serve(HashMap::new(), |o| json!([release(o, CURRENT, true)]));
    let out = install.run(
        &install.link,
        Some(&server.base),
        &["update", "--to", "9.9.9"],
    );
    assert!(!out.status.success());
    assert!(text(&out).contains("not published"), "{}", text(&out));
    let out = install.run(&install.link, Some(&server.base), &["update", "--check"]);
    assert!(out.status.success());
    assert!(text(&out).contains("up to date"), "{}", text(&out));
}

#[test]
fn uninstall_removes_only_the_program_unless_asked_and_never_touches_project_data() {
    let install = Install::new();
    let repo = install.root.join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    // Global data, configuration and a project database, all created by the installed program.
    assert!(install.run(&install.link, None, &["init"]).status.success());
    assert!(
        install
            .run(
                &install.link,
                None,
                &["setup", "--provider", "lexical", "--no-validate"]
            )
            .status
            .success()
    );
    let out = Command::new(&install.link)
        .args(["init", "--storage", "project"])
        .arg(&repo)
        .current_dir(&repo)
        .env("HOME", install.root.join("home"))
        .env("XDG_CONFIG_HOME", install.root.join("home/.config"))
        .env("XDG_DATA_HOME", install.root.join("home/.local/share"))
        .env("AETHER_MIDNIGHT_CONFIG_DIR", install.root.join("cfg"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        install
            .run(
                &install.link,
                None,
                &["connect", "claude-code", "--scope", "user"]
            )
            .status
            .success()
    );
    let claude = install.root.join("home/.claude.json");
    assert!(
        fs::read_to_string(&claude)
            .unwrap()
            .contains("aether-midnight")
    );

    // Dry run changes nothing and lists what stays.
    let out = install.run(&install.link, None, &["uninstall", "--dry-run", "--json"]);
    assert!(out.status.success(), "{}", text(&out));
    let plan: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(plan["plan"]["program"].as_array().unwrap().len() == 2);
    assert!(
        plan["plan"]["client_entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["client"] == "claude-code")
    );
    assert!(install.bin.exists() && install.link.exists());

    // Without --yes and without a terminal it refuses.
    let out = install.run(&install.link, None, &["uninstall"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("--yes"), "{}", text(&out));
    assert!(install.bin.exists());

    // Default: program and alias only.
    let out = install.run(&install.link, None, &["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!install.bin.exists() && fs::symlink_metadata(&install.link).is_err());
    assert!(
        install.root.join("cfg/config.json").exists(),
        "configuration stays"
    );
    assert!(
        fs::read_to_string(&claude)
            .unwrap()
            .contains("aether-midnight"),
        "client entry stays"
    );
    assert!(repo.join(".midnight/midnight.sqlite3").exists());

    // Reinstall and purge everything that was asked for.
    fs::copy(env!("CARGO_BIN_EXE_aether-midnight"), &install.bin).unwrap();
    std::os::unix::fs::symlink("aether-midnight", &install.link).unwrap();
    let out = install.run(
        &install.link,
        None,
        &[
            "uninstall",
            "--yes",
            "--purge-config",
            "--purge-data",
            "--remove-client-entries",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(!install.root.join("cfg/config.json").exists());
    assert!(!install.root.join("cfg/trusted_projects.json").exists());
    assert!(
        !fs::read_to_string(&claude)
            .unwrap()
            .contains("aether-midnight")
    );
    assert!(
        repo.join(".midnight/midnight.sqlite3").exists(),
        "project data is never deleted"
    );
    assert!(repo.join(".git").exists());
}

#[test]
fn uninstall_refuses_a_development_build() {
    let install = Install::new();
    let out = install.run(
        Path::new(env!("CARGO_BIN_EXE_midnight")),
        None,
        &["uninstall", "--yes"],
    );
    assert!(!out.status.success());
    assert!(text(&out).contains("development build"), "{}", text(&out));
    assert!(Path::new(env!("CARGO_BIN_EXE_midnight")).exists());
}

#[test]
fn force_reinstalls_the_current_version_and_without_it_nothing_happens() {
    let install = Install::new();
    let work = TempDir::new().unwrap();
    let archive = make_archive(work.path(), CURRENT, CURRENT, false);
    let server = serve(files_for(&archive, CURRENT), |o| {
        json!([release(o, CURRENT, true)])
    });
    let before = fs::read(&install.bin).unwrap();

    let out = install.run(&install.link, Some(&server.base), &["update"]);
    assert!(
        out.status.success() && text(&out).contains("up to date"),
        "{}",
        text(&out)
    );
    assert_eq!(fs::read(&install.bin).unwrap(), before);

    let out = install.run(&install.link, Some(&server.base), &["update", "--force"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_ne!(
        fs::read(&install.bin).unwrap(),
        before,
        "the repair installs the release's binary"
    );
}
