//! `midnight`: the discoverable name of the same program as `aether-midnight`.
//! Both binaries share one implementation (`src/main.rs`) so behaviour cannot drift.
#[path = "../main.rs"]
mod app;

fn main() -> anyhow::Result<()> {
    app::main()
}
