use std::path::Path;
use tracing_subscriber::EnvFilter;
use xbridge::config::Config;
use xbridge::router::app;
use xbridge::state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config_path = std::env::args()
        .skip_while(|a| a != "--config")
        .nth(1);

    let config = Config::load(config_path.as_deref().map(Path::new)).unwrap_or_else(|e| {
        eprintln!("failed to load config: {e}");
        std::process::exit(1);
    });

    let addr = config.listen.http.clone();
    let state = AppState::new(config.clone());

    // Start SIP bridge in background
    let bridge_state = state.clone();
    let bridge_config = config.clone();
    tokio::spawn(async move {
        if let Err(e) = xbridge::bridge::run(&bridge_config, bridge_state).await {
            tracing::error!("SIP bridge error: {e}");
        }
    });

    tracing::info!("xbridge listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app(state)).await.unwrap();
}
