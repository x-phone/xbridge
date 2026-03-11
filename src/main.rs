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

    let config_path = std::env::args().skip_while(|a| a != "--config").nth(1);

    let config = Config::load(config_path.as_deref().map(Path::new)).unwrap_or_else(|e| {
        eprintln!("failed to load config: {e}");
        std::process::exit(1);
    });

    let addr = config.listen.http.clone();
    let webhook = WebhookClient::new(&config.webhook);

    let (ended_tx, ended_rx) = tokio::sync::mpsc::channel(256);
    let (dtmf_tx, dtmf_rx) = tokio::sync::mpsc::channel(256);
    let (state_tx, state_rx) = tokio::sync::mpsc::channel(256);

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
    serve(&config, app(state), &addr).await;
    tracing::info!("xbridge shut down");
}

async fn serve(config: &Config, router: axum::Router, addr: &str) {
    #[cfg(feature = "tls")]
    if let (Some(cert), Some(key)) = (&config.tls.cert, &config.tls.key) {
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
            .await
            .unwrap_or_else(|e| {
                eprintln!("failed to load TLS certs: {e}");
                std::process::exit(1);
            });
        tracing::info!("TLS enabled");
        let addr: std::net::SocketAddr = addr.parse().unwrap();
        axum_server::bind_rustls(addr, tls_config)
            .serve(router.into_make_service())
            .await
            .unwrap();
        return;
    }

    let _ = config;
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT, shutting down"),
        _ = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}
