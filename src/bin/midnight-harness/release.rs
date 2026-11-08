//! Release-only measurements through the product CLI, never a real provider.
use super::*;
#[path = "../../../tests/common/mock.rs"]
mod mock;

pub(super) fn measure(bin: &Path, models: &Path) -> Result<Value> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("measure.sqlite3");
    let dbs = db.to_str().context("db path")?;
    let mock = mock::Mock::start(|_, r| mock::ok_response(r, 384));
    let flags = [
        "--provider",
        "openrouter",
        "--model",
        "release/mock",
        "--dimensions",
        "384",
        "--endpoint",
        &mock.url,
    ];
    let mut report = Vec::new();
    for size in [10_000, 100_000] {
        let scope = format!("release:n{size}");
        let mut mcp = Mcp::start(bin, &db)?;
        let mut oracle = Vec::new();
        let mut query = mock::fake_vector("payment retry", 384);
        aether_midnight::embed::l2_normalize(&mut query);
        for start in (0..size).step_by(100) {
            let memories: Vec<Value> = (start..start+100).map(|i| json!({"scope":scope,"kind":"source",
                "content":format!("record {i} {} token{}", if i%7==0 {"payment retry"} else {"cache backup"}, i%101)})).collect();
            mcp.call("remember_batch", json!({"memories":memories}))?;
        }
        mcp.close()?;
        let mut args = vec!["--db", dbs, "embed", "--namespace", &scope];
        args.extend(flags);
        run_cli(bin, &args, &[("OPENROUTER_API_KEY", "release-mock-key")])?;
        // Independent exhaustive scoring over product-stored vectors; no production KNN helper.
        let conn = rusqlite::Connection::open(&db)?;
        let mut stmt = conn.prepare("SELECT owner_id,vector FROM embeddings WHERE scope=?1")?;
        let mut rows = stmt.query([&scope])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            let score: f32 = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .zip(&query)
                .map(|(v, q)| f32::from_le_bytes(*v) * q)
                .sum();
            oracle.push((id, score));
        }
        if oracle.len() != size {
            bail!("expected {size} stored vectors, found {}", oracle.len());
        }
        oracle.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let expected: Vec<i64> = oracle.iter().take(10).map(|x| x.0).collect();
        let mut runs = Vec::new();
        for _ in 0..5 {
            let mut args = vec![
                "--db",
                dbs,
                "memory",
                "search",
                "--namespace",
                &scope,
                "payment retry",
                "--mode",
                "semantic",
                "--limit",
                "10",
                "--json",
            ];
            args.extend(flags);
            let (out, usage) = run_cli(bin, &args, &[("OPENROUTER_API_KEY", "release-mock-key")])?;
            let got: Vec<i64> = out["results"]
                .as_array()
                .context("results")?
                .iter()
                .map(|r| r["memory"]["id"].as_i64().unwrap())
                .collect();
            if got != expected || out["degraded"] != false {
                bail!("KNN oracle disagreement");
            }
            runs.push(usage.json());
        }
        report
            .push(json!({"vectors":size,"dimensions":384,"ordered_oracle_match":true,"runs":runs}));
    }
    let localdb = temp.path().join("local.sqlite3");
    let ld = localdb.to_str().unwrap();
    let envs = [
        ("AETHER_MIDNIGHT_MODELS_DIR", models.to_str().unwrap()),
        ("HF_HUB_OFFLINE", "1"),
        ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ("HTTP_PROXY", "http://127.0.0.1:9"),
    ];
    run_cli(
        bin,
        &[
            "--db",
            ld,
            "memory",
            "add",
            "--namespace",
            "release:local",
            "La política de pagos permite tres reintentos.",
            "--json",
        ],
        &envs,
    )?;
    run_cli(
        bin,
        &["--db", ld, "embed", "--namespace", "release:local"],
        &envs,
    )?;
    let mut fresh = Vec::new();
    for _ in 0..5 {
        let (out, usage) = run_cli(
            bin,
            &[
                "--db",
                ld,
                "memory",
                "search",
                "--namespace",
                "release:local",
                "payment retry policy",
                "--mode",
                "semantic",
                "--json",
            ],
            &envs,
        )?;
        if out["degraded"] != false || out["results"][0]["memory"]["id"] != 1 {
            bail!("local inference result mismatch");
        }
        fresh.push(usage.json());
    }
    // Warmed inference measured by a single product embed invocation: model remains loaded across
    // 16-text batches. Total includes load, so incremental throughput is a conservative estimate.
    let mut mcp = Mcp::start(bin, &localdb)?;
    for start in (0..512).step_by(128) {
        let records: Vec<Value> = (start..start+128).map(|i| json!({"scope":"release:warm","kind":"source","content":format!("payment policy retry record {i}")})).collect();
        mcp.call("remember_batch", json!({"memories":records}))?;
    }
    let records: Vec<Value> = (0..16).map(|i| json!({"scope":"release:warm16","kind":"source","content":format!("payment policy retry record {i}")})).collect();
    mcp.call("remember_batch", json!({"memories":records}))?;
    mcp.close()?;
    let (_, warm16) = run_cli(
        bin,
        &["--db", ld, "embed", "--namespace", "release:warm16"],
        &envs,
    )?;
    let (_, warm) = run_cli(
        bin,
        &["--db", ld, "embed", "--namespace", "release:warm"],
        &envs,
    )?;
    Ok(
        json!({"platform":"macOS x86_64","knn":report,"fresh_process_local_semantic":fresh,
        "warm_batch_16_including_load":warm16.json(),"warm_batch_512_including_load":warm.json(),"incremental_warm_ms_per_text_estimate":(warm.wall_s-warm16.wall_s)*1000.0/496.0,"method":"CLI child wait4; seed and embed excluded from query measurements; one request in flight"}),
    )
}

pub(super) fn notices(output: &Path) -> Result<Value> {
    if output.exists() {
        bail!("notice output already exists");
    }
    let out = Command::new("cargo")
        .args(["metadata", "--locked", "--format-version", "1"])
        .current_dir(ROOT)
        .output()?;
    if !out.status.success() {
        bail!("cargo metadata failed");
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    let mut count = 0;
    fn copy_notices(root: &Path, at: &Path, dest: &Path, count: &mut usize) -> Result<()> {
        for entry in std::fs::read_dir(at)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                copy_notices(root, &path, dest, count)?;
            } else if entry.file_type()?.is_file() {
                let name = entry.file_name().to_string_lossy().to_ascii_uppercase();
                if [
                    "LICENSE",
                    "LICENCE",
                    "COPYING",
                    "NOTICE",
                    "COPYRIGHT",
                    "UNLICENSE",
                ]
                .iter()
                .any(|p| name.starts_with(p))
                {
                    let to = dest.join(path.strip_prefix(root)?);
                    std::fs::create_dir_all(to.parent().unwrap())?;
                    std::fs::copy(path, to)?;
                    *count += 1;
                }
            }
        }
        Ok(())
    }
    for package in meta["packages"].as_array().context("packages")? {
        if package["source"].is_null() {
            continue;
        }
        let root = Path::new(package["manifest_path"].as_str().context("manifest")?)
            .parent()
            .unwrap();
        let dest = output.join(format!(
            "{}-{}",
            package["name"].as_str().unwrap(),
            package["version"].as_str().unwrap()
        ));
        copy_notices(root, root, &dest, &mut count)?;
        if let Some(file) = package["license_file"].as_str() {
            let from = root.join(file);
            std::fs::create_dir_all(&dest)?;
            std::fs::copy(
                &from,
                dest.join(from.file_name().context("license filename")?),
            )?;
        }
    }
    Ok(
        json!({"notice_files":count,"source":"locked dependency sources; all license/copyright/notice files, including vendored components"}),
    )
}
