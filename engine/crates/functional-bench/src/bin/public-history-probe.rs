#[allow(dead_code)]
#[path = "../common.rs"]
mod common;
#[path = "../process.rs"]
mod process;
#[path = "../public_history_probe.rs"]
mod public_history_probe;
#[global_allocator]
static ALLOC: std::alloc::System = std::alloc::System;
fn main() -> common::Result<()> {
    public_history_probe::run()
}
