use rewind::collector::{router, Collector};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let listen = std::env::var("REWIND_LISTEN").unwrap_or_else(|_| "0.0.0.0:9100".into());
    tracing::info!(%listen, "rewind collector starting");

    // Agents shown in the viewer, e.g. "frontend:9000,orders:9000,payments:9000".
    let agents = std::env::var("REWIND_AGENTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(String::from)
        .collect();

    let listener = tokio::net::TcpListener::bind(&listen).await.expect("bind");
    axum::serve(listener, router(Collector::with_agents(agents)))
        .await
        .expect("serve");
}
