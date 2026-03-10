use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::call::CallInfo;
use crate::config::Config;
use crate::metrics::Metrics;
use crate::webhook_client::WebhookClient;

pub type CallRegistry = Arc<RwLock<HashMap<String, CallInfo>>>;
pub type XphoneCallRegistry = Arc<RwLock<HashMap<String, Arc<xphone::Call>>>>;
pub type PhoneRegistry = Arc<RwLock<HashMap<String, xphone::Phone>>>;

#[derive(Clone)]
pub struct AppState {
    pub(crate) calls: CallRegistry,
    pub(crate) xphone_calls: XphoneCallRegistry,
    pub(crate) phones: PhoneRegistry,
    pub(crate) ended_tx: mpsc::Sender<(String, xphone::EndReason, std::time::Duration)>,
    pub(crate) dtmf_tx: mpsc::Sender<(String, String)>,
    pub(crate) state_tx: mpsc::Sender<(String, xphone::CallState)>,
    pub(crate) webhook: WebhookClient,
    pub(crate) config: Arc<Config>,
    pub(crate) metrics: Metrics,
}

impl AppState {
    pub fn new(
        config: Config,
        webhook: WebhookClient,
        ended_tx: mpsc::Sender<(String, xphone::EndReason, std::time::Duration)>,
        dtmf_tx: mpsc::Sender<(String, String)>,
        state_tx: mpsc::Sender<(String, xphone::CallState)>,
    ) -> Self {
        Self {
            calls: Arc::new(RwLock::new(HashMap::new())),
            xphone_calls: Arc::new(RwLock::new(HashMap::new())),
            phones: Arc::new(RwLock::new(HashMap::new())),
            ended_tx,
            dtmf_tx,
            state_tx,
            webhook,
            config: Arc::new(config),
            metrics: Metrics::new(),
        }
    }
}
