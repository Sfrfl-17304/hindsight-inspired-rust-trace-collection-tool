use rewind::agent::{router, Agent, AgentConfig};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = AgentConfig::from_env().unwrap_or_else(|e| {
        eprintln!("config error: {e}");
        std::process::exit(2);
    });
    let listen = std::env::var("REWIND_LISTEN").unwrap_or_else(|_| "0.0.0.0:9000".into());
    tracing::info!(?cfg, %listen, "rewind agent starting");

    let listener = tokio::net::TcpListener::bind(&listen).await.expect("bind");
    axum::serve(listener, router(Agent::new(cfg)))
        .await
        .expect("serve");
}
