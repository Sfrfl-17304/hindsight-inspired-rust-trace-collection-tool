//! Three nodes (frontend -> orders -> payments) and a collector, all in one
//! process on localhost. Checks that a failing request is collected in full
//! and that a healthy request never leaves its nodes.

use std::time::Duration;

use rewind::agent::{self, Agent, AgentConfig, Mode};
use rewind::collector::{self, Collector};
use rewind::model::Span;

struct Node {
    addr: String,
}

async fn start_collector() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(l, collector::router(Collector::new())).await });
    format!("http://{addr}")
}

async fn start_agent(name: &str, collector_url: &str) -> Node {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let cfg = AgentConfig {
        node: name.into(),
        self_addr: addr.clone(),
        collector_url: collector_url.into(),
        buffer_bytes: 1 << 20,
        latency_threshold_us: 500_000,
        mode: Mode::Retro,
    };
    tokio::spawn(async move { axum::serve(l, agent::router(Agent::new(cfg))).await });
    Node { addr }
}

fn span(
    trace: &str,
    id: &str,
    parent: Option<&str>,
    node: &str,
    error: bool,
    peers: &[&Node],
) -> Span {
    Span {
        trace_id: trace.into(),
        span_id: id.into(),
        parent_id: parent.map(String::from),
        node: node.into(),
        name: format!("{node}-handler"),
        start_unix_us: rewind::now_unix_us(),
        duration_us: 1_000,
        error,
        peers: peers.iter().map(|n| n.addr.clone()).collect(),
    }
}

async fn send(http: &reqwest::Client, node: &Node, s: Span) {
    let r = http
        .post(format!("http://{}/v1/spans", node.addr))
        .json(&vec![s])
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
}

async fn wait_for_spans(
    http: &reqwest::Client,
    collector: &str,
    trace: &str,
    want: usize,
) -> usize {
    let mut got = 0;
    for _ in 0..50 {
        let r = http
            .get(format!("{collector}/v1/traces/{trace}"))
            .send()
            .await
            .unwrap();
        if r.status().is_success() {
            let v: serde_json::Value = r.json().await.unwrap();
            got = v["spans"].as_array().unwrap().len();
            if got >= want {
                return got;
            }
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    got
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_request_is_collected_from_every_node() {
    let collector = start_collector().await;
    let fe = start_agent("frontend", &collector).await;
    let orders = start_agent("orders", &collector).await;
    let pay = start_agent("payments", &collector).await;
    let http = reqwest::Client::new();

    // Healthy request: spans stay on their nodes.
    send(
        &http,
        &pay,
        span("ok-1", "p", Some("o"), "payments", false, &[&orders]),
    )
    .await;
    send(
        &http,
        &orders,
        span("ok-1", "o", Some("f"), "orders", false, &[&fe, &pay]),
    )
    .await;
    send(
        &http,
        &fe,
        span("ok-1", "f", None, "frontend", false, &[&orders]),
    )
    .await;

    // Failing request, in the realistic order: the callee finishes first,
    // so the callers' spans arrive *after* the trigger has started.
    send(
        &http,
        &pay,
        span("bad-1", "p", Some("o"), "payments", true, &[&orders]),
    )
    .await;
    send(
        &http,
        &orders,
        span("bad-1", "o", Some("f"), "orders", true, &[&fe, &pay]),
    )
    .await;
    send(
        &http,
        &fe,
        span("bad-1", "f", None, "frontend", true, &[&orders]),
    )
    .await;

    assert_eq!(wait_for_spans(&http, &collector, "bad-1", 3).await, 3);

    tokio::time::sleep(Duration::from_millis(200)).await;
    let r = http
        .get(format!("{collector}/v1/traces/ok-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "healthy trace must not be shipped");
}

#[tokio::test(flavor = "multi_thread")]
async fn trigger_walks_breadcrumbs_when_spans_are_already_buffered() {
    let collector = start_collector().await;
    let fe = start_agent("frontend", &collector).await;
    let orders = start_agent("orders", &collector).await;
    let pay = start_agent("payments", &collector).await;
    let http = reqwest::Client::new();

    // Frontend and orders were fine and buffered their spans; only later
    // does payments report a (slow, asynchronous) failure.
    send(
        &http,
        &fe,
        span("bad-2", "f", None, "frontend", false, &[&orders]),
    )
    .await;
    send(
        &http,
        &orders,
        span("bad-2", "o", Some("f"), "orders", false, &[&fe, &pay]),
    )
    .await;
    send(
        &http,
        &pay,
        span("bad-2", "p", Some("o"), "payments", true, &[&orders]),
    )
    .await;

    // payments -> orders (1 hop) -> frontend (2 hops)
    assert_eq!(wait_for_spans(&http, &collector, "bad-2", 3).await, 3);
}
