//! Rewind: retroactive, cooperative trace collection.
//!
//! Every node (one service instance + one agent) keeps its spans in a
//! fixed-size in-memory buffer and sends nothing by default. When a node
//! sees a problem (an error or a slow span) it *triggers* the trace: it
//! reports its own spans to the collector and asks the neighbouring nodes
//! that handled the same request (the "breadcrumbs") to do the same.

pub mod agent;
pub mod buffer;
pub mod collector;
pub mod model;

/// Current wall-clock time in microseconds since the Unix epoch.
pub fn now_unix_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}
