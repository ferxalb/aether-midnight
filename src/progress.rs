//! Live progress for long CLI operations, written to stderr only.
//!
//! Output is shown only when stderr is a terminal; pipes, CI logs and `--json` runs stay clean
//! (callers print plain start/finish lines for those). Nothing here touches stdout, so data and
//! progress never mix.

use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub fn stderr_is_tty() -> bool {
    std::io::stderr().is_terminal()
}

/// Erase the current terminal line.
fn clear_line() {
    let mut err = std::io::stderr();
    let _ = write!(err, "\r\x1b[2K");
    let _ = err.flush();
}

fn elapsed_text(since: Instant) -> String {
    let s = since.elapsed().as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// `1234567` -> `1,234,567`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A spinner with elapsed time and a live counter, drawn by a background thread. Does nothing
/// unless stderr is a terminal. The line is erased when it is dropped.
pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Spinner {
    pub fn start(label: String, counter: fn() -> u64, unit: &'static str) -> Spinner {
        let stop = Arc::new(AtomicBool::new(false));
        if !stderr_is_tty() {
            return Spinner { stop, handle: None };
        }
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let frames = ['|', '/', '-', '\\'];
            let mut tick = 0usize;
            while !flag.load(Ordering::Relaxed) {
                let mut err = std::io::stderr();
                let _ = write!(
                    err,
                    "\r\x1b[2K{} {label} · {} {unit} · {}",
                    frames[tick % frames.len()],
                    thousands(counter()),
                    elapsed_text(started)
                );
                let _ = err.flush();
                tick += 1;
                std::thread::sleep(Duration::from_millis(120));
            }
            clear_line();
        });
        Spinner {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A `done/total` line redrawn in place, at most ten times a second. Does nothing unless stderr
/// is a terminal.
pub struct Counter {
    label: String,
    enabled: bool,
    started: Instant,
    last_draw: Option<Instant>,
}

impl Counter {
    pub fn new(label: impl Into<String>) -> Counter {
        Counter {
            label: label.into(),
            enabled: stderr_is_tty(),
            started: Instant::now(),
            last_draw: None,
        }
    }

    pub fn update(&mut self, done: u64, total: u64, detail: &str) {
        if !self.enabled {
            return;
        }
        let due = self
            .last_draw
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(100));
        if !due && done < total {
            return;
        }
        self.last_draw = Some(Instant::now());
        let percent = (done * 100).checked_div(total).unwrap_or(100);
        let mut err = std::io::stderr();
        let _ = write!(
            err,
            "\r\x1b[2K{} {}/{} files ({percent}%){}{} · {}",
            self.label,
            thousands(done),
            thousands(total),
            if detail.is_empty() { "" } else { " · " },
            detail,
            elapsed_text(self.started)
        );
        let _ = err.flush();
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        if self.enabled && self.last_draw.is_some() {
            clear_line();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::thousands;

    #[test]
    fn thousands_separates_groups() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
