use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::{mpsc, RwLock};

use crate::call::CallInfo;
use crate::config::Config;
use crate::webhook_client::WebhookClient;

pub type CallRegistry = Arc<RwLock<HashMap<String, CallInfo>>>;
pub type XphoneCallRegistry = Arc<RwLock<HashMap<String, Arc<xphone::Call>>>>;

#[derive(Clone)]
pub struct AppState {
    pub(crate) calls: CallRegistry,
    pub(crate) xphone_calls: XphoneCallRegistry,
    pub(crate) phone: Arc<OnceLock<xphone::Phone>>,
    pub(crate) ended_tx: mpsc::Sender<(String, xphone::EndReason, std::time::Duration)>,
    pub(crate) dtmf_tx: mpsc::Sender<(String, String)>,
    pub(crate) state_tx: mpsc::Sender<(String, xphone::CallState)>,
    pub(crate) webhook: WebhookClient,
    pub(crate) config: Arc<Config>,
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
            phone: Arc::new(OnceLock::new()),
            ended_tx,
            dtmf_tx,
            state_tx,
            webhook,
            config: Arc::new(config),
        }
    }
}
