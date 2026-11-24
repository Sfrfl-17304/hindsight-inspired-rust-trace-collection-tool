//! The collector: receives the pieces of triggered traces from the nodes
//! and puts them back together. It never asks nodes for data itself.
//!
//! * `GET    /`               – trace viewer (HTML, for demos)
//! * `POST   /v1/report`      – a node hands over spans
//! * `GET    /v1/traces`      – summary of every trace received
//! * `GET    /v1/traces/{id}` – one full trace, spans ordered by start time
//! * `DELETE /v1/traces`      – reset between experiment runs
//! * `GET    /v1/stats`       – bytes and reports received
//! * `GET    /v1/nodes`       – stats of each agent in `REWIND_AGENTS` (viewer only)
//! * `GET    /v1/nodes/{addr}/buffer` – peek into one agent's buffer (viewer only)

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
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
    /// Earliest span start and total duration of the spans received.
    start_unix_us: u64,
    duration_us: u64,
    /// Node of the deepest failing span: where the error originated.
    error_origin: Option<String>,
}

impl TraceSummary {
    fn of(r: &TraceRecord) -> Self {
        let start = r.spans.iter().map(|s| s.start_unix_us).min().unwrap_or(0);
        let end = r
            .spans
            .iter()
            .map(|s| s.start_unix_us + s.duration_us)
            .max()
            .unwrap_or(0);
        let error_origin = r
            .spans
            .iter()
            .filter(|s| s.error)
            .max_by_key(|s| s.start_unix_us)
            .map(|s| s.node.clone());
        Self {
            trace_id: r.trace_id.clone(),
            span_count: r.spans.len(),
            nodes: r.nodes.clone(),
            reasons: r.reasons.clone(),
            has_error: r.has_error,
            start_unix_us: start,
            duration_us: end.saturating_sub(start),
            error_origin,
        }
    }
}

#[derive(Default)]
pub struct Collector {
    traces: Mutex<HashMap<String, TraceRecord>>,
    reports_received: AtomicU64,
    bytes_received: AtomicU64,
    /// Agent addresses polled by `/v1/nodes` for the viewer. Display only:
    /// the collector never pulls spans from agents.
    agents: Vec<String>,
    http: reqwest::Client,
}

pub type SharedCollector = Arc<Collector>;

impl Collector {
    pub fn new() -> SharedCollector {
        Arc::new(Self::default())
    }

    pub fn with_agents(agents: Vec<String>) -> SharedCollector {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .expect("http client");
        Arc::new(Self {
            agents,
            http,
            ..Self::default()
        })
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
    let mut out: Vec<TraceSummary> = traces.values().map(TraceSummary::of).collect();
    out.sort_by(|a, b| b.start_unix_us.cmp(&a.start_unix_us)); // newest first
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

/// Current stats of every known agent, or `up: false` if it does not answer.
async fn nodes(State(c): State<SharedCollector>) -> Json<serde_json::Value> {
    let polls: Vec<_> = c
        .agents
        .iter()
        .map(|addr| {
            let (http, addr) = (c.http.clone(), addr.clone());
            tokio::spawn(async move {
                let stats = match http.get(format!("http://{addr}/v1/stats")).send().await {
                    Ok(r) => r.json::<serde_json::Value>().await.ok(),
                    Err(_) => None,
                };
                serde_json::json!({ "addr": addr, "up": stats.is_some(), "stats": stats })
            })
        })
        .collect();
    let mut out = Vec::new();
    for p in polls {
        out.push(p.await.unwrap_or(serde_json::Value::Null));
    }
    Json(serde_json::Value::Array(out))
}

/// What one node's agent currently holds in its buffer (viewer only).
async fn node_buffer(
    State(c): State<SharedCollector>,
    Path(addr): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !c.agents.contains(&addr) {
        return Err(StatusCode::NOT_FOUND);
    }
    let resp = c
        .http
        .get(format!("http://{addr}/v1/buffer"))
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    resp.json()
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_GATEWAY)
}

/// A small trace viewer for demos: lists the collected traces and draws one
/// as a timeline. Static page, it only calls the JSON endpoints above.
async fn viewer() -> Html<&'static str> {
    Html(include_str!("viewer.html"))
}

pub fn router(c: SharedCollector) -> Router {
    Router::new()
        .route("/", get(viewer))
        .route("/v1/report", post(report))
        .route("/v1/traces", get(list).delete(reset))
        .route("/v1/traces/{id}", get(one))
        .route("/v1/stats", get(stats))
        .route("/v1/nodes", get(nodes))
        .route("/v1/nodes/{addr}/buffer", get(node_buffer))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(c)
}
