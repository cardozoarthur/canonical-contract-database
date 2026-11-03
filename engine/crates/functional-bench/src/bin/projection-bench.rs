// E13 read-only probe. No production/global allocator is installed by libraries.
#[allow(dead_code)]
#[path = "../codec_log.rs"]
mod codec_log;
#[allow(dead_code)]
#[path = "../common.rs"]
mod common;
#[allow(dead_code)]
#[path = "../engine.rs"]
mod engine;
#[allow(dead_code)]
#[path = "../oracle.rs"]
mod oracle;
#[path = "../process.rs"]
mod process;
#[allow(dead_code)]
#[path = "../projection_probe.rs"]
mod projection_probe;
#[allow(dead_code)]
#[path = "../sqlite.rs"]
mod sqlite;
#[global_allocator]
static ALLOC: std::alloc::System = std::alloc::System;
fn main() -> common::Result<()> {
    projection_probe::run()
}
