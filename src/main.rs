use std::path::Path;
use tracing_subscriber::EnvFilter;
use xbridge::config::Config;
use xbridge::router::app;
use xbridge::state::AppState;
use xbridge::webhook_client::WebhookClient;

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
    let webhook = WebhookClient::new(&config.webhook);

    let (ended_tx, ended_rx) = tokio::sync::mpsc::channel(32);
    let (dtmf_tx, dtmf_rx) = tokio::sync::mpsc::channel(32);
    let (state_tx, state_rx) = tokio::sync::mpsc::channel(32);

    let state = AppState::new(config.clone(), webhook, ended_tx, dtmf_tx, state_tx);

    // Start SIP bridge in background
    let bridge_state = state.clone();
    let bridge_config = config.clone();
    tokio::spawn(async move {
        if let Err(e) =
            xbridge::bridge::run(&bridge_config, bridge_state, ended_rx, dtmf_rx, state_rx).await
        {
            tracing::error!("SIP bridge error: {e}");
        }
    });

    tracing::info!("xbridge listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app(state)).await.unwrap();
}
