use tracing_subscriber::EnvFilter;
use xbridge::config::Config;
use xbridge::router::app;
use xbridge::state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // TODO: load config from file / env
    let config = Config::default();
    let state = AppState::new(config.clone());

    let addr = &config.listen.http;
    tracing::info!("xbridge listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app(state)).await.unwrap();
}
