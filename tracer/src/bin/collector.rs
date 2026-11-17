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

    let listener = tokio::net::TcpListener::bind(&listen).await.expect("bind");
    axum::serve(listener, router(Collector::new()))
        .await
        .expect("serve");
}
