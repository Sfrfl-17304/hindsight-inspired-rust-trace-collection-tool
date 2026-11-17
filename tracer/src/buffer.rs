//! The per-node trace buffer: a fixed memory budget that overwrites the
//! oldest traces first (the "dashcam loop").
//!
//! A trace is *locked* the moment it is triggered: its spans are taken out
//! of the buffer, so they can no longer be overwritten, and the trace id is
//! remembered so that spans arriving later are handed back for immediate
//! forwarding instead of being buffered.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::Serialize;

use crate::model::Span;

/// Outcome of inserting a span.
#[derive(Debug)]
pub enum Insert {
    /// Stored in the buffer.
    Buffered,
    /// The trace was already triggered: forward this span right away.
    AlreadyTriggered(Span),
    /// Larger than the whole buffer; dropped.
    Dropped,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct BufferStats {
    pub capacity_bytes: usize,
    pub used_bytes: usize,
    pub traces_buffered: usize,
    pub spans_received: u64,
    pub spans_evicted: u64,
    pub traces_evicted: u64,
    pub spans_dropped_oversize: u64,
    pub traces_triggered: u64,
}

struct Entry {
    spans: Vec<Span>,
    bytes: usize,
    /// Distinguishes this entry from an older entry with the same trace id.
    seq: u64,
}

pub struct TraceBuffer {
    capacity_bytes: usize,
    used_bytes: usize,
    traces: HashMap<String, Entry>,
    /// Oldest-first order of trace entries. May contain stale ids (traces
    /// already taken); those are skipped at eviction time.
    order: VecDeque<(String, u64)>,
    next_seq: u64,
    triggered: HashSet<String>,
    triggered_order: VecDeque<String>,
    triggered_limit: usize,
    stats: BufferStats,
}

impl TraceBuffer {
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            capacity_bytes,
            used_bytes: 0,
            traces: HashMap::new(),
            order: VecDeque::new(),
            next_seq: 0,
            triggered: HashSet::new(),
            triggered_order: VecDeque::new(),
            triggered_limit: 100_000,
            stats: BufferStats::default(),
        }
    }

    pub fn insert(&mut self, span: Span) -> Insert {
        self.stats.spans_received += 1;
        if self.triggered.contains(&span.trace_id) {
            return Insert::AlreadyTriggered(span);
        }
        let size = span.approx_bytes();
        if size > self.capacity_bytes {
            self.stats.spans_dropped_oversize += 1;
            return Insert::Dropped;
        }
        while self.used_bytes + size > self.capacity_bytes {
            if !self.evict_oldest() {
                break;
            }
        }
        self.used_bytes += size;
        if let Some(entry) = self.traces.get_mut(&span.trace_id) {
            entry.bytes += size;
            entry.spans.push(span);
        } else {
            let seq = self.next_seq;
            self.next_seq += 1;
            self.order.push_back((span.trace_id.clone(), seq));
            self.traces.insert(
                span.trace_id.clone(),
                Entry {
                    spans: vec![span],
                    bytes: size,
                    seq,
                },
            );
        }
        Insert::Buffered
    }

    /// Lock the trace and take its spans out of the buffer.
    /// Returns `None` if the trace was already triggered on this node
    /// (so a node collects each trace at most once).
    pub fn take_for_trigger(&mut self, trace_id: &str) -> Option<Vec<Span>> {
        if self.triggered.contains(trace_id) {
            return None;
        }
        self.mark_triggered(trace_id);
        self.stats.traces_triggered += 1;
        Some(match self.traces.remove(trace_id) {
            Some(e) => {
                self.used_bytes -= e.bytes;
                e.spans
            }
            None => Vec::new(),
        })
    }

    pub fn is_triggered(&self, trace_id: &str) -> bool {
        self.triggered.contains(trace_id)
    }

    pub fn stats(&self) -> BufferStats {
        BufferStats {
            capacity_bytes: self.capacity_bytes,
            used_bytes: self.used_bytes,
            traces_buffered: self.traces.len(),
            ..self.stats.clone()
        }
    }

    fn evict_oldest(&mut self) -> bool {
        while let Some((id, seq)) = self.order.pop_front() {
            if self.traces.get(&id).is_some_and(|e| e.seq == seq) {
                let e = self.traces.remove(&id).expect("checked above");
                self.used_bytes -= e.bytes;
                self.stats.spans_evicted += e.spans.len() as u64;
                self.stats.traces_evicted += 1;
                return true;
            }
        }
        false
    }

    fn mark_triggered(&mut self, trace_id: &str) {
        self.triggered.insert(trace_id.to_string());
        self.triggered_order.push_back(trace_id.to_string());
        while self.triggered_order.len() > self.triggered_limit {
            if let Some(old) = self.triggered_order.pop_front() {
                self.triggered.remove(&old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(trace: &str, id: &str) -> Span {
        Span {
            trace_id: trace.into(),
            span_id: id.into(),
            parent_id: None,
            node: "n".into(),
            name: "op".into(),
            start_unix_us: 0,
            duration_us: 1,
            error: false,
            peers: vec![],
        }
    }

    #[test]
    fn evicts_oldest_trace_first() {
        let one = span("t1", "a").approx_bytes();
        let mut b = TraceBuffer::new(one * 2);
        b.insert(span("t1", "a"));
        b.insert(span("t2", "a"));
        b.insert(span("t3", "a")); // must push out t1
        assert!(b.take_for_trigger("t1").unwrap().is_empty());
        assert_eq!(b.take_for_trigger("t2").unwrap().len(), 1);
        assert_eq!(b.take_for_trigger("t3").unwrap().len(), 1);
        assert_eq!(b.stats().traces_evicted, 1);
    }

    #[test]
    fn triggered_trace_is_locked_and_collected_once() {
        let mut b = TraceBuffer::new(10_000);
        b.insert(span("t1", "a"));
        assert_eq!(b.take_for_trigger("t1").unwrap().len(), 1);
        assert!(
            b.take_for_trigger("t1").is_none(),
            "second trigger is a no-op"
        );
        assert_eq!(b.stats().used_bytes, 0);
    }

    #[test]
    fn late_spans_of_triggered_trace_are_handed_back() {
        let mut b = TraceBuffer::new(10_000);
        b.take_for_trigger("t1");
        match b.insert(span("t1", "late")) {
            Insert::AlreadyTriggered(s) => assert_eq!(s.span_id, "late"),
            other => panic!("expected AlreadyTriggered, got {other:?}"),
        }
    }

    #[test]
    fn oversize_span_is_dropped() {
        let mut b = TraceBuffer::new(8);
        assert!(matches!(b.insert(span("t1", "a")), Insert::Dropped));
        assert_eq!(b.stats().spans_dropped_oversize, 1);
    }
}
