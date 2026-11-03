#[allow(dead_code)]
#[path = "../codec_log.rs"]
mod codec_log;
#[allow(dead_code)]
#[path = "../common.rs"]
mod common;
#[path = "../e15_probe.rs"]
mod e15_probe;
#[allow(dead_code)]
#[path = "../engine.rs"]
mod engine;
#[allow(dead_code)]
#[path = "../index_policy_probe.rs"]
mod index_policy_probe;
#[allow(dead_code)]
#[path = "../oracle.rs"]
mod oracle;
#[path = "../process.rs"]
mod process;
#[allow(dead_code)]
#[path = "../sqlite.rs"]
mod sqlite;
#[global_allocator]
static ALLOC: canonical_memory::TrackingAllocator =
    canonical_memory::TrackingAllocator::new(std::alloc::System);
fn main() -> common::Result<()> {
    e15_probe::run()
}
