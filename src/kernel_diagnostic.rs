//! Controlled retrieval ablations; never selected by the production interfaces.
use serde::Serialize;
use std::cell::{Cell, RefCell};
thread_local! {
    static FLAGS: Cell<u8> = const { Cell::new(0) };
    static TRACE: RefCell<Option<Trace>> = const { RefCell::new(None) };
}
#[derive(Default, Serialize)]
pub(crate) struct Trace {
    pub fetch_limits: Vec<usize>,
    pub stages: usize,
    pub filtered_rows: usize,
    pub classified: usize,
    pub vm_steps: u64,
    pub sorts: u64,
    pub rows: usize,
    pub rejected: usize,
    pub hydrated: usize,
    pub content_bytes: usize,
    pub record_bytes: usize,
}
pub(crate) fn flags() -> u8 {
    FLAGS.get()
}
pub(crate) fn select(flags: u8) {
    FLAGS.set(flags);
}
pub(crate) fn begin() {
    TRACE.set(Some(Trace::default()));
}
pub(crate) fn finish() -> Trace {
    TRACE.take().unwrap_or_default()
}
pub(crate) fn work(f: impl FnOnce(&mut Trace)) {
    TRACE.with_borrow_mut(|t| {
        if let Some(t) = t {
            f(t);
        }
    });
}
