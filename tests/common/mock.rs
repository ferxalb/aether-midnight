//! Deterministic loopback HTTP mock for OpenAI-compatible `/embeddings` endpoints.
//! One request per connection (`Connection: close`); every request is recorded.
#![allow(dead_code)]

use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Recorded {
    pub path: String,
    /// Lower-cased header names.
    pub headers: HashMap<String, String>,
    pub body: Value,
}

pub enum Reply {
    Json(u16, String),
    /// Wait, then answer (used to trip client timeouts).
    Delayed(Duration, Box<Reply>),
    /// Raw bytes after the status line, for malformed-HTTP cases.
    Status(u16, &'static str),
}

pub struct Mock {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
}

type Handler = dyn Fn(usize, &Recorded) -> Reply + Send + Sync;

impl Mock {
    pub fn start(handler: impl Fn(usize, &Recorded) -> Reply + Send + Sync + 'static) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let (reqs, stop2) = (requests.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (handler, reqs) = (handler.clone(), reqs.clone());
                        std::thread::spawn(move || serve(stream, handler, reqs));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Mock {
            url,
            requests,
            stop,
        }
    }

    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    pub fn recorded(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve(mut stream: TcpStream, handler: Arc<Handler>, reqs: Arc<Mutex<Vec<Recorded>>>) {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let path = lines
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < header_end + length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body = serde_json::from_slice(&buf[header_end..]).unwrap_or(Value::Null);
    let recorded = Recorded {
        path,
        headers,
        body,
    };
    let n = {
        let mut guard = reqs.lock().unwrap();
        guard.push(recorded.clone());
        guard.len() - 1
    };
    write_reply(&mut stream, handler(n, &recorded));
}

fn write_reply(stream: &mut TcpStream, reply: Reply) {
    match reply {
        Reply::Delayed(d, inner) => {
            std::thread::sleep(d);
            write_reply(stream, *inner);
        }
        Reply::Status(code, body) => {
            let _ = write!(
                stream,
                "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
        Reply::Json(code, body) => {
            let _ = write!(
                stream,
                "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    }
}

/// Deterministic bag-of-words embedding of `text` into `dims` buckets (not normalised).
pub fn fake_vector(text: &str, dims: usize) -> Vec<f32> {
    let mut v = vec![0f32; dims];
    for word in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let h = word
            .bytes()
            .fold(2166136261u32, |a, b| (a ^ b as u32).wrapping_mul(16777619));
        v[h as usize % dims] += 1.0;
    }
    if v.iter().all(|x| *x == 0.0) {
        v[0] = 1.0;
    }
    v
}

/// A well-formed embeddings response for the request's inputs.
pub fn ok_response(req: &Recorded, dims: usize) -> Reply {
    let inputs = req.body["input"].as_array().cloned().unwrap_or_default();
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            serde_json::json!({
                "object": "embedding",
                "index": i,
                "embedding": fake_vector(t.as_str().unwrap_or(""), dims),
            })
        })
        .collect();
    Reply::Json(
        200,
        serde_json::json!({
            "object": "list",
            "data": data,
            "model": req.body["model"],
            "usage": {"prompt_tokens": 1, "total_tokens": 1},
        })
        .to_string(),
    )
}
