// E14 standalone System probe; common harness modules are reused without runtime adapters.
#[allow(dead_code)]
#[path = "../common.rs"]
mod common;
#[allow(dead_code)]
#[path = "../engine.rs"]
mod engine;
#[path = "../index_policy_probe.rs"]
mod index_policy_probe;
#[allow(dead_code)]
#[path = "../oracle.rs"]
mod oracle;
#[path = "../process.rs"]
mod process;
#[global_allocator]
static ALLOC: std::alloc::System = std::alloc::System;
fn main() -> common::Result<()> {
    index_policy_probe::run()
}
