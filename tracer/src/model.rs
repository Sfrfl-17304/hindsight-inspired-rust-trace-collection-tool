use serde::{Deserialize, Serialize};

/// One unit of work done by one node for one request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Name of the node that produced the span.
    pub node: String,
    pub name: String,
    pub start_unix_us: u64,
    pub duration_us: u64,
    #[serde(default)]
    pub error: bool,
    /// Breadcrumbs: agent addresses (`host:port`) of the neighbouring nodes
    /// this span talked to (caller and callees). Used to follow the request
    /// one hop at a time when the trace is triggered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<String>,
}

impl Span {
    /// Approximate memory footprint, used to enforce the buffer budget.
    pub fn approx_bytes(&self) -> usize {
        let s = std::mem::size_of::<String>();
        std::mem::size_of::<Span>()
            + self.trace_id.len()
            + self.span_id.len()
            + self.parent_id.as_ref().map_or(0, |p| p.len())
            + self.node.len()
            + self.name.len()
            + self.peers.iter().map(|p| p.len() + s).sum::<usize>()
    }
}

/// Sent agent -> agent: "collect your part of this trace".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerRequest {
    pub trace_id: String,
    pub reason: String,
    /// Agent address of the node that forwarded the trigger.
    #[serde(default)]
    pub from: Option<String>,
}

/// Sent agent -> collector: spans a node is handing over.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub node: String,
    pub reason: String,
    pub spans: Vec<Span>,
}
