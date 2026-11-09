//! Loopback-only MCP Streamable HTTP transport (stateless JSON responses).
//!
//! Dependency-free (std::net). Every request needs a bearer token from the
//! registry in `auth.rs`. Binds only to 127.0.0.1 / ::1, rejects foreign
//! Host/Origin headers (DNS-rebinding defence), bounds every read, and never
//! follows or issues network requests.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crate::auth::Authenticator;
use crate::mcp::handle_message;

const MAX_BODY: usize = 1024 * 1024;
const MAX_HEADER_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_CONNECTIONS: usize = 16;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn serve(db_path: &Path, bind: SocketAddr, cancellation: Arc<AtomicBool>) -> Result<()> {
    if !bind.ip().is_loopback() {
        bail!("MCP HTTP transport only binds loopback addresses (127.0.0.1 or ::1)");
    }
    // Finish and close bootstrap initialization before reporting readiness.
    // Request connections retain their existing close/checkpoint behavior; no
    // idle keeper changes WAL lifetime or normal checkpoint frequency.
    drop(crate::db::open_db(db_path).context("initialize HTTP database before readiness")?);
    let listener = TcpListener::bind(bind).with_context(|| format!("bind {bind}"))?;
    listener.set_nonblocking(true)?;
    let local = listener.local_addr()?;
    // Scripts parse this single line to discover an ephemeral port.
    println!(
        "listening http://{}/mcp",
        match local.ip() {
            IpAddr::V6(ip) => format!("[{ip}]:{}", local.port()),
            IpAddr::V4(ip) => format!("{ip}:{}", local.port()),
        }
    );
    std::io::stdout().flush()?;
    let active = Arc::new(AtomicUsize::new(0));
    let auth = Arc::new(Authenticator::new(db_path));
    while !cancellation.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    active.fetch_sub(1, Ordering::SeqCst);
                    let _ =
                        write_plain(&stream, 503, "Service Unavailable", "too many connections");
                    continue;
                }
                let (db, auth, cancel, active) = (
                    db_path.to_path_buf(),
                    auth.clone(),
                    cancellation.clone(),
                    active.clone(),
                );
                let port = local.port();
                std::thread::spawn(move || {
                    let _ = connection(stream, port, &db, &auth, &cancel);
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                wait_for_connection(&listener);
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Sleeps until a connection is pending or the cancellation poll interval
/// elapses. Polling the listener (rather than sleeping a fixed interval)
/// removes up to 15 ms of accept latency from every request while still
/// observing cancellation promptly.
fn wait_for_connection(listener: &TcpListener) {
    use std::os::fd::AsRawFd;
    let mut fd = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `fd` is a valid pollfd for the duration of the call.
    let ready = unsafe { libc::poll(&mut fd, 1, 100) };
    if ready < 0 {
        // EINTR or a transient failure: back off briefly instead of spinning.
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn connection(
    stream: TcpStream,
    port: u16,
    db_path: &Path,
    auth: &Authenticator,
    cancel: &AtomicBool,
) -> Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut memory_session = None;
    let mut session_principal = None;
    while !cancel.load(Ordering::Relaxed) {
        let Some(req) = read_request(&mut reader)? else {
            return Ok(());
        };
        let reject = |code: u16, text: &str, msg: &str| write_plain(&stream, code, text, msg);
        if !host_ok(req.header("host"), port) {
            reject(403, "Forbidden", "invalid Host header")?;
            return Ok(());
        }
        if let Some(origin) = req.header("origin")
            && !origin_ok(origin)
        {
            reject(403, "Forbidden", "invalid Origin header")?;
            return Ok(());
        }
        // `/mcp/<label>` lets a client keep one endpoint record per token; the
        // label carries no authority (the bearer token alone selects the policy).
        if req.path != "/mcp" && !req.path.starts_with("/mcp/") {
            reject(404, "Not Found", "endpoint is /mcp")?;
            return Ok(());
        }
        // Reachability on loopback is not authority: every request, including
        // initialize and tools/list, needs a live token. The policy is read
        // from the token record on each request, so revocation is immediate.
        let record = req
            .header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .and_then(|token| auth.authenticate(token.trim()));
        let Some(record) = record else {
            write_unauthorized(&stream)?;
            return Ok(());
        };
        let policy = &record.policy;
        if session_principal.as_ref() != Some(&record.id) {
            memory_session = None;
            session_principal = Some(record.id.clone());
        }
        match req.method.as_str() {
            "DELETE" => write_response(&stream, 200, "OK", None, req.keep_alive)?,
            "POST" => {
                let message: Value = match serde_json::from_slice(&req.body) {
                    Ok(v) => v,
                    Err(e) => {
                        let body = json!({"jsonrpc":"2.0","id":null,
                            "error":{"code":-32700,"message":format!("parse error: {e}")}});
                        write_response(&stream, 400, "Bad Request", Some(&body), req.keep_alive)?;
                        if !req.keep_alive {
                            return Ok(());
                        }
                        continue;
                    }
                };
                if !message.is_object() {
                    let body = json!({"jsonrpc":"2.0","id":null,
                        "error":{"code":-32600,"message":"batches are not supported"}});
                    write_response(&stream, 400, "Bad Request", Some(&body), req.keep_alive)?;
                    if !req.keep_alive {
                        return Ok(());
                    }
                    continue;
                }
                match handle_message(db_path, policy, &mut memory_session, &message, cancel) {
                    Some(resp) => write_response(&stream, 200, "OK", Some(&resp), req.keep_alive)?,
                    None => write_response(&stream, 202, "Accepted", None, req.keep_alive)?,
                }
            }
            // No server-initiated stream is offered.
            _ => {
                write_plain(&stream, 405, "Method Not Allowed", "use POST")?;
                return Ok(());
            }
        }
        if !req.keep_alive {
            return Ok(());
        }
    }
    Ok(())
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    keep_alive: bool,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn read_request<R: BufRead>(reader: &mut R) -> Result<Option<Request>> {
    let Some(line) = read_line(reader)? else {
        return Ok(None);
    };
    let mut parts = line.split_whitespace();
    let (method, path, version) = (
        parts.next().unwrap_or("").to_owned(),
        parts.next().unwrap_or("").to_owned(),
        parts.next().unwrap_or(""),
    );
    if method.is_empty() || !version.starts_with("HTTP/1.") {
        bail!("malformed request line");
    }
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader)?.context("truncated headers")?;
        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            bail!("too many headers");
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
    }
    let find = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    if find("transfer-encoding").is_some() {
        bail!("chunked request bodies are not supported");
    }
    let length: usize = find("content-length")
        .map(|v| v.parse().context("bad Content-Length"))
        .transpose()?
        .unwrap_or(0);
    if length > MAX_BODY {
        bail!("request body exceeds the 1 MiB limit");
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let keep_alive = version == "HTTP/1.1"
        && !find("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
    Ok(Some(Request {
        method,
        path,
        headers,
        body,
        keep_alive,
    }))
}

fn read_line<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut buf = Vec::new();
    let n = reader
        .by_ref()
        .take(MAX_HEADER_LINE as u64 + 1)
        .read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > MAX_HEADER_LINE {
        bail!("header line too long");
    }
    Ok(Some(
        String::from_utf8(buf)?
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
    ))
}

fn host_ok(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else { return false };
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
    .iter()
    .any(|h| h.eq_ignore_ascii_case(host))
}

fn origin_ok(origin: &str) -> bool {
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .unwrap_or("");
    let host = rest.rsplit_once(':').map_or(rest, |(h, p)| {
        if p.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            rest
        }
    });
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

fn write_response(
    mut stream: &TcpStream,
    code: u16,
    text: &str,
    body: Option<&Value>,
    keep_alive: bool,
) -> Result<()> {
    let payload = body
        .map(serde_json::to_vec)
        .transpose()?
        .unwrap_or_default();
    let mut head = format!(
        "HTTP/1.1 {code} {text}\r\nContent-Length: {}\r\nConnection: {}\r\nCache-Control: no-store\r\n",
        payload.len(),
        if keep_alive { "keep-alive" } else { "close" }
    );
    if body.is_some() {
        head.push_str("Content-Type: application/json\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()?;
    Ok(())
}

fn write_unauthorized(mut stream: &TcpStream) -> Result<()> {
    let msg = "authentication required";
    let head = format!(
        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"aether-midnight\"\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{msg}",
        msg.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn write_plain(mut stream: &TcpStream, code: u16, text: &str, msg: &str) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {code} {text}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{msg}",
        msg.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{host_ok, origin_ok};

    #[test]
    fn host_and_origin_checks_reject_rebinding() {
        assert!(host_ok(Some("127.0.0.1:9000"), 9000));
        assert!(host_ok(Some("localhost:9000"), 9000));
        assert!(!host_ok(Some("evil.example:9000"), 9000));
        assert!(!host_ok(Some("127.0.0.1:9001"), 9000));
        assert!(!host_ok(None, 9000));
        assert!(origin_ok("http://localhost:3000"));
        assert!(!origin_ok("https://evil.example"));
        assert!(!origin_ok("http://127.0.0.1.evil.example"));
    }
}
