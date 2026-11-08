//! Runtime-distribution tests: checksum enforcement, safe extraction, diagnostics.
//! Install tests serve a tiny synthetic archive from a loopback server; nothing is downloaded from
//! the real ONNX Runtime release here (that path is exercised by the manual release smoke test).

use super::*;
use std::io::Write;
use std::net::TcpListener;
use tempfile::TempDir;

fn tar_entry(name: &str, data: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..107].copy_from_slice(b"0000644");
    let size = format!("{:011o}", data.len());
    header[124..135].copy_from_slice(size.as_bytes());
    header[156] = b'0';
    header[257..262].copy_from_slice(b"ustar");
    let mut out = header.to_vec();
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out
}

fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (n, d) in entries {
        tar.extend(tar_entry(n, d));
    }
    tar.extend([0u8; 1024]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

fn serve_once(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/ort.tgz", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let mut stream = stream.unwrap();
            let mut buf = [0u8; 2048];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    url
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn pin_for(archive: &[u8], lib: &[u8]) -> PinnedRuntime {
    PinnedRuntime {
        platform: "test-platform",
        version: "1.23.2",
        url: "http://unused.invalid/x.tgz",
        archive_size: archive.len() as u64,
        archive_sha256: leak(sha256_hex(archive)),
        library_member: "ort/lib/libonnxruntime.1.23.2.dylib",
        library_sha256: leak(sha256_hex(lib)),
        notice_members: &["ort/LICENSE"],
    }
}

#[test]
fn install_verifies_checksums_and_extracts_only_named_members() {
    let lib = b"fake-library-bytes".to_vec();
    let gz = archive(&[
        ("ort/include/huge.h", b"ignored"),
        ("ort/lib/libonnxruntime.1.23.2.dylib", &lib),
        ("ort/LICENSE", b"MIT License"),
        ("../evil", b"never extracted"),
    ]);
    let pin = pin_for(&gz, &lib);
    let url = serve_once(gz);
    let models = TempDir::new().unwrap();
    let report = install_pinned(models.path(), &pin, &url).unwrap();
    assert!(!report.already_installed);
    let dir = managed_dir(models.path());
    assert_eq!(std::fs::read(dir.join(library_file_name())).unwrap(), lib);
    assert_eq!(std::fs::read(dir.join("LICENSE")).unwrap(), b"MIT License");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["archive_sha256"], pin.archive_sha256);
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, vec!["LICENSE", library_file_name(), "manifest.json"]);
    assert!(!models.path().join("evil").exists());
    // Second run is a no-op that needs no network (the one-shot server is gone).
    let again = install_pinned(models.path(), &pin, "http://127.0.0.1:1/none").unwrap();
    assert!(again.already_installed);
}

#[test]
fn install_refuses_tampered_or_wrong_sized_archives_and_writes_nothing() {
    let lib = b"fake-library-bytes".to_vec();
    let gz = archive(&[
        ("ort/lib/libonnxruntime.1.23.2.dylib", &lib),
        ("ort/LICENSE", b"MIT"),
    ]);
    // Same size, different content: checksum mismatch.
    let mut tampered = gz.clone();
    let last = tampered.len() / 2;
    tampered[last] ^= 0xff;
    let pin = pin_for(&gz, &lib);
    let models = TempDir::new().unwrap();
    let err = install_pinned(models.path(), &pin, &serve_once(tampered)).unwrap_err();
    assert!(err.to_string().contains("SHA-256 mismatch"), "{err:#}");
    // Wrong size.
    let mut longer = gz.clone();
    longer.push(0);
    let err = install_pinned(models.path(), &pin, &serve_once(longer)).unwrap_err();
    assert!(err.to_string().contains("pinned size"), "{err:#}");
    // Archive hash right but library hash pin wrong.
    let mut bad_lib_pin = pin_for(&gz, &lib);
    bad_lib_pin.library_sha256 = leak(sha256_hex(b"something else"));
    let err = install_pinned(models.path(), &bad_lib_pin, &serve_once(gz)).unwrap_err();
    assert!(err.to_string().contains("library SHA-256"), "{err:#}");
    assert!(
        !managed_dir(models.path()).exists(),
        "failed installs must leave nothing behind"
    );
}

#[test]
fn install_is_refused_for_platforms_without_a_pin() {
    // Only runs the refusal path on hosts that really have no pin; elsewhere the table is checked.
    assert!(PINNED.iter().any(|p| p.platform == "macos-x86_64"));
    for p in PINNED {
        assert_eq!(p.archive_sha256.len(), 64);
        assert_eq!(p.library_sha256.len(), 64);
        assert!(
            p.url
                .starts_with("https://github.com/microsoft/onnxruntime/releases/download/")
        );
        assert!(version_ok(p.version));
    }
    if pinned_for_host().is_none() {
        let models = TempDir::new().unwrap();
        let err = install(models.path()).unwrap_err().to_string();
        assert!(err.contains("no pinned ONNX Runtime"), "{err}");
    }
}

#[test]
fn version_policy_accepts_1_22_and_newer_1_x_only() {
    for ok in ["1.22.0", "1.23.2", "1.30.1"] {
        assert!(version_ok(ok), "{ok}");
    }
    for bad in ["1.21.9", "0.9.0", "2.0.0", "garbage", ""] {
        assert!(!version_ok(bad), "{bad}");
    }
}

#[test]
fn probe_and_inspect_give_actionable_diagnostics_for_bad_libraries() {
    let dir = TempDir::new().unwrap();
    // Missing file.
    let err = probe_version(&dir.path().join("nope.dylib")).unwrap_err();
    assert!(err.contains("does not exist"), "{err}");
    // A file that is not a loadable library.
    let junk = dir.path().join(library_file_name());
    std::fs::write(&junk, b"not a library").unwrap();
    let err = probe_version(&junk).unwrap_err();
    assert!(err.contains("cannot load library"), "{err}");
    // A real library without the ONNX Runtime entry point.
    let libc_path = if cfg!(target_os = "macos") {
        "/usr/lib/libSystem.B.dylib"
    } else {
        "/lib/x86_64-linux-gnu/libc.so.6"
    };
    if std::path::Path::new(libc_path).exists() {
        let err = probe_version(std::path::Path::new(libc_path)).unwrap_err();
        assert!(err.contains("OrtGetApiBase"), "{err}");
    }
    // A managed junk library is reported with a remedy, and ensure_loadable refuses it.
    let models = TempDir::new().unwrap();
    std::fs::create_dir_all(managed_dir(models.path())).unwrap();
    std::fs::write(
        managed_dir(models.path()).join(library_file_name()),
        b"junk",
    )
    .unwrap();
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        let status = inspect(models.path());
        assert!(!status.compatible);
        assert_eq!(status.source, Some(Source::Managed));
        assert!(!status.problems.is_empty() && !status.remedies.is_empty());
        let err = ensure_loadable(models.path()).unwrap_err().to_string();
        assert!(err.contains("doctor"), "{err}");
    }
}
