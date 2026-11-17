//! The per-node agent. It runs next to one service instance (same network
//! namespace) and exposes:
//!
//! * `POST /v1/spans`   – the local service hands over finished spans
//! * `POST /v1/trigger` – a neighbouring agent asks for its part of a trace
//! * `GET  /v1/stats`   – buffer and network counters (for the experiments)
//!
//! Modes (so the baselines run on the same code path):
//! * `retro`        – keep locally, collect cooperatively on trigger (Rewind)
//! * `all`          – send every span to the collector
//! * `sample:<p>`   – head sampling, keep a trace with probability p

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::buffer::{BufferStats, Insert, TraceBuffer};
use crate::model::{Report, Span, TriggerRequest};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Retro,
    All,
    HeadSample(f64),
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "retro" => Ok(Mode::Retro),
            "all" => Ok(Mode::All),
            _ => match s.strip_prefix("sample:").map(str::parse::<f64>) {
                Some(Ok(p)) if (0.0..=1.0).contains(&p) => Ok(Mode::HeadSample(p)),
                _ => Err(format!(
                    "invalid mode {s:?} (expected retro | all | sample:<0..1>)"
                )),
            },
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Retro => write!(f, "retro"),
            Mode::All => write!(f, "all"),
            Mode::HeadSample(p) => write!(f, "sample:{p}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Node name, e.g. "orders".
    pub node: String,
    /// Address other agents use to reach this one, e.g. "orders:9000".
    pub self_addr: String,
    /// Base URL of the collector, e.g. "http://collector:9100".
    pub collector_url: String,
    pub buffer_bytes: usize,
    /// Spans slower than this trigger collection.
    pub latency_threshold_us: u64,
    pub mode: Mode,
}

impl AgentConfig {
    pub fn from_env() -> Result<Self, String> {
        fn var(k: &str, default: Option<&str>) -> Result<String, String> {
            std::env::var(k)
                .ok()
                .or(default.map(String::from))
                .ok_or_else(|| format!("missing env var {k}"))
        }
        fn num<T: FromStr>(k: &str, default: &str) -> Result<T, String> {
            var(k, Some(default))?
                .parse()
                .map_err(|_| format!("env var {k} is not a number"))
        }
        Ok(Self {
            node: var("REWIND_NODE", None)?,
            self_addr: var("REWIND_SELF_ADDR", None)?,
            collector_url: var("REWIND_COLLECTOR_URL", Some("http://collector:9100"))?,
            buffer_bytes: num("REWIND_BUFFER_BYTES", "16777216")?,
            latency_threshold_us: num("REWIND_LATENCY_THRESHOLD_US", "500000")?,
            mode: var("REWIND_MODE", Some("retro"))?.parse()?,
        })
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct NetStats {
    pub reports_sent: u64,
    pub report_bytes_sent: u64,
    pub report_failures: u64,
    pub triggers_sent: u64,
    pub trigger_failures: u64,
    pub triggers_received: u64,
}

#[derive(Serialize)]
struct StatsResponse {
    node: String,
    mode: String,
    buffer: BufferStats,
    net: NetStats,
}

pub struct Agent {
    cfg: AgentConfig,
    buffer: Mutex<TraceBuffer>,
    net: Mutex<NetStats>,
    http: reqwest::Client,
}

pub type SharedAgent = Arc<Agent>;

impl Agent {
    pub fn new(cfg: AgentConfig) -> SharedAgent {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("http client");
        Arc::new(Self {
            buffer: Mutex::new(TraceBuffer::new(cfg.buffer_bytes)),
            net: Mutex::new(NetStats::default()),
            cfg,
            http,
        })
    }

    fn trigger_reason(&self, s: &Span) -> Option<String> {
        if s.error {
            Some(format!("error in {} ({})", s.node, s.name))
        } else if s.duration_us > self.cfg.latency_threshold_us {
            Some(format!("slow {} ({} us)", s.name, s.duration_us))
        } else {
            None
        }
    }

    async fn send_report(&self, spans: Vec<Span>, reason: &str) {
        let report = Report {
            node: self.cfg.node.clone(),
            reason: reason.to_string(),
            spans,
        };
        let body = serde_json::to_vec(&report).expect("serialize report");
        let url = format!("{}/v1/report", self.cfg.collector_url);
        for attempt in 0..3u64 {
            let res = self
                .http
                .post(&url)
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .await;
            let ok = matches!(&res, Ok(r) if r.status().is_success());
            {
                let mut net = self.net.lock().unwrap();
                if ok {
                    net.reports_sent += 1;
                    net.report_bytes_sent += body.len() as u64;
                } else {
                    net.report_failures += 1;
                }
            }
            if ok {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await;
        }
        tracing::warn!(node = %self.cfg.node, "giving up on report to collector");
    }

    async fn send_trigger(&self, peer: String, req: TriggerRequest) {
        let res = self
            .http
            .post(format!("http://{peer}/v1/trigger"))
            .json(&req)
            .send()
            .await;
        let mut net = self.net.lock().unwrap();
        match res {
            Ok(r) if r.status().is_success() => net.triggers_sent += 1,
            _ => {
                net.trigger_failures += 1;
                tracing::warn!(node = %self.cfg.node, %peer, "trigger to peer failed");
            }
        }
    }

    /// Ask every peer in `peers` to collect `trace_id`.
    fn fan_out(self: &Arc<Self>, trace_id: &str, reason: &str, peers: BTreeSet<String>) {
        for peer in peers {
            let req = TriggerRequest {
                trace_id: trace_id.to_string(),
                reason: reason.to_string(),
                from: Some(self.cfg.self_addr.clone()),
            };
            let me = self.clone();
            tokio::spawn(async move { me.send_trigger(peer, req).await });
        }
    }
}

/// Breadcrumbs found in `spans`, excluding this node itself.
fn peers_of(spans: &[Span], self_addr: &str) -> BTreeSet<String> {
    spans
        .iter()
        .flat_map(|s| s.peers.iter())
        .filter(|p| p.as_str() != self_addr)
        .cloned()
        .collect()
}

/// Deterministic per-trace coin flip, identical on every node (FNV-1a).
fn head_sampled(trace_id: &str, p: f64) -> bool {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in trace_id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h as f64 / u64::MAX as f64) < p
}

/// Collect this node's part of `trace_id` and pass the trigger on.
pub async fn run_trigger(agent: SharedAgent, trace_id: String, reason: String) {
    let taken = agent.buffer.lock().unwrap().take_for_trigger(&trace_id);
    let Some(spans) = taken else { return }; // already collected here
    tracing::info!(node = %agent.cfg.node, %trace_id, %reason, spans = spans.len(), "trigger");
    let peers = peers_of(&spans, &agent.cfg.self_addr);
    agent.fan_out(&trace_id, &reason, peers);
    if !spans.is_empty() {
        agent.send_report(spans, &reason).await;
    }
}

async fn ingest(State(agent): State<SharedAgent>, Json(spans): Json<Vec<Span>>) -> StatusCode {
    match agent.cfg.mode {
        Mode::All => {
            tokio::spawn(async move { agent.send_report(spans, "all").await });
        }
        Mode::HeadSample(p) => {
            let kept: Vec<Span> = spans
                .into_iter()
                .filter(|s| head_sampled(&s.trace_id, p))
                .collect();
            if !kept.is_empty() {
                tokio::spawn(async move { agent.send_report(kept, "head-sample").await });
            }
        }
        Mode::Retro => ingest_retro(agent, spans),
    }
    StatusCode::ACCEPTED
}

/// Rewind proper: buffer spans locally, trigger collection when one looks
/// bad, and forward spans whose trace was already triggered.
fn ingest_retro(agent: SharedAgent, spans: Vec<Span>) {
    let mut late = Vec::new(); // spans of already-triggered traces
    let mut fire = Vec::new(); // (trace_id, reason) to trigger now
    {
        let mut buf = agent.buffer.lock().unwrap();
        for s in spans {
            let reason = agent.trigger_reason(&s);
            let trace_id = s.trace_id.clone();
            match buf.insert(s) {
                Insert::AlreadyTriggered(s) => late.push(s),
                Insert::Buffered | Insert::Dropped => {
                    if let Some(r) = reason {
                        fire.push((trace_id, r));
                    }
                }
            }
        }
    }
    forward_late_spans(&agent, late);
    for (trace_id, reason) in fire {
        tokio::spawn(run_trigger(agent.clone(), trace_id, reason));
    }
}

/// Spans that finish after their trace was triggered — typical for callers,
/// whose span ends after the callee's error — go to the collector at once,
/// and their breadcrumbs carry the trigger further upstream.
fn forward_late_spans(agent: &SharedAgent, late: Vec<Span>) {
    let mut by_trace: std::collections::BTreeMap<String, Vec<Span>> = Default::default();
    for s in late {
        by_trace.entry(s.trace_id.clone()).or_default().push(s);
    }
    for (trace_id, spans) in by_trace {
        let peers = peers_of(&spans, &agent.cfg.self_addr);
        agent.fan_out(&trace_id, "late span of triggered trace", peers);
        let a = agent.clone();
        tokio::spawn(async move { a.send_report(spans, "late").await });
    }
}

async fn trigger(State(agent): State<SharedAgent>, Json(req): Json<TriggerRequest>) -> StatusCode {
    agent.net.lock().unwrap().triggers_received += 1;
    tokio::spawn(run_trigger(agent, req.trace_id, req.reason));
    StatusCode::ACCEPTED
}

async fn stats(State(agent): State<SharedAgent>) -> Json<serde_json::Value> {
    let resp = StatsResponse {
        node: agent.cfg.node.clone(),
        mode: agent.cfg.mode.to_string(),
        buffer: agent.buffer.lock().unwrap().stats(),
        net: agent.net.lock().unwrap().clone(),
    };
    Json(serde_json::to_value(resp).expect("serialize stats"))
}

pub fn router(agent: SharedAgent) -> Router {
    Router::new()
        .route("/v1/spans", post(ingest))
        .route("/v1/trigger", post(trigger))
        .route("/v1/stats", get(stats))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modes() {
        assert_eq!("retro".parse::<Mode>().unwrap(), Mode::Retro);
        assert_eq!("all".parse::<Mode>().unwrap(), Mode::All);
        assert_eq!(
            "sample:0.01".parse::<Mode>().unwrap(),
            Mode::HeadSample(0.01)
        );
        assert!("sample:2".parse::<Mode>().is_err());
    }

    #[test]
    fn head_sampling_rate_is_roughly_right() {
        let kept = (0..100_000)
            .filter(|i| head_sampled(&format!("trace-{i}"), 0.01))
            .count();
        assert!((700..1300).contains(&kept), "kept {kept}");
    }
}
