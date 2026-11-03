// Reuse the frozen workload model/oracles; Backend adapters are unused by this read-only probe.
#[allow(dead_code)]
#[path = "../common.rs"]
mod common;
#[path = "../memory_probe.rs"]
mod memory_probe;
#[allow(dead_code)]
#[path = "../oracle.rs"]
mod oracle;
#[path = "../process.rs"]
mod process;
#[global_allocator]
static ALLOC: std::alloc::System = std::alloc::System;
fn main() -> common::Result<()> {
    memory_probe::run(false)
}
