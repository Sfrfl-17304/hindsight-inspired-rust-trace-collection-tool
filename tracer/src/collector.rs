//! The collector: receives the pieces of triggered traces from the nodes
//! and puts them back together. It never asks nodes for data itself.
//!
//! * `POST   /v1/report`      – a node hands over spans
//! * `GET    /v1/traces`      – summary of every trace received
//! * `GET    /v1/traces/{id}` – one full trace, spans ordered by start time
//! * `DELETE /v1/traces`      – reset between experiment runs
//! * `GET    /v1/stats`       – bytes and reports received

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::model::{Report, Span};
use crate::now_unix_us;

#[derive(Debug, Clone, Serialize)]
pub struct TraceRecord {
    pub trace_id: String,
    pub nodes: BTreeSet<String>,
    pub reasons: BTreeSet<String>,
    pub has_error: bool,
    pub first_report_unix_us: u64,
    pub last_report_unix_us: u64,
    pub spans: Vec<Span>,
}

#[derive(Serialize)]
struct TraceSummary {
    trace_id: String,
    span_count: usize,
    nodes: BTreeSet<String>,
    reasons: BTreeSet<String>,
    has_error: bool,
}

#[derive(Default)]
pub struct Collector {
    traces: Mutex<HashMap<String, TraceRecord>>,
    reports_received: AtomicU64,
    bytes_received: AtomicU64,
}

pub type SharedCollector = Arc<Collector>;

impl Collector {
    pub fn new() -> SharedCollector {
        Arc::new(Self::default())
    }

    pub fn ingest(&self, report: Report) {
        let now = now_unix_us();
        let mut traces = self.traces.lock().unwrap();
        for span in report.spans {
            let rec = traces
                .entry(span.trace_id.clone())
                .or_insert_with(|| TraceRecord {
                    trace_id: span.trace_id.clone(),
                    nodes: BTreeSet::new(),
                    reasons: BTreeSet::new(),
                    has_error: false,
                    first_report_unix_us: now,
                    last_report_unix_us: now,
                    spans: Vec::new(),
                });
            rec.last_report_unix_us = now;
            rec.nodes.insert(report.node.clone());
            rec.reasons.insert(report.reason.clone());
            // Two nodes may report the same span (e.g. retries); keep one copy.
            if !rec.spans.iter().any(|s| s.span_id == span.span_id) {
                rec.has_error |= span.error;
                rec.spans.push(span);
            }
        }
    }

    pub fn trace(&self, id: &str) -> Option<TraceRecord> {
        let mut rec = self.traces.lock().unwrap().get(id).cloned()?;
        rec.spans.sort_by_key(|s| s.start_unix_us);
        Some(rec)
    }
}

async fn report(State(c): State<SharedCollector>, body: Bytes) -> StatusCode {
    c.bytes_received
        .fetch_add(body.len() as u64, Ordering::Relaxed);
    c.reports_received.fetch_add(1, Ordering::Relaxed);
    match serde_json::from_slice::<Report>(&body) {
        Ok(r) => {
            c.ingest(r);
            StatusCode::ACCEPTED
        }
        Err(_) => StatusCode::BAD_REQUEST,
    }
}

async fn list(State(c): State<SharedCollector>) -> Json<serde_json::Value> {
    let traces = c.traces.lock().unwrap();
    let mut out: Vec<TraceSummary> = traces
        .values()
        .map(|r| TraceSummary {
            trace_id: r.trace_id.clone(),
            span_count: r.spans.len(),
            nodes: r.nodes.clone(),
            reasons: r.reasons.clone(),
            has_error: r.has_error,
        })
        .collect();
    out.sort_by(|a, b| a.trace_id.cmp(&b.trace_id));
    Json(serde_json::to_value(out).expect("serialize"))
}

async fn one(
    State(c): State<SharedCollector>,
    Path(id): Path<String>,
) -> Result<Json<TraceRecord>, StatusCode> {
    c.trace(&id).map(Json).ok_or(StatusCode::NOT_FOUND)
}

async fn reset(State(c): State<SharedCollector>) -> StatusCode {
    c.traces.lock().unwrap().clear();
    c.reports_received.store(0, Ordering::Relaxed);
    c.bytes_received.store(0, Ordering::Relaxed);
    StatusCode::NO_CONTENT
}

async fn stats(State(c): State<SharedCollector>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "traces": c.traces.lock().unwrap().len(),
        "reports_received": c.reports_received.load(Ordering::Relaxed),
        "bytes_received": c.bytes_received.load(Ordering::Relaxed),
    }))
}

pub fn router(c: SharedCollector) -> Router {
    Router::new()
        .route("/v1/report", post(report))
        .route("/v1/traces", get(list).delete(reset))
        .route("/v1/traces/{id}", get(one))
        .route("/v1/stats", get(stats))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(c)
}
