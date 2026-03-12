use axum::extract::ws::Message;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::call::CallInfo;
use crate::call_control::CallControl;
use crate::config::Config;
use crate::metrics::Metrics;
use crate::trunk::dialog::{SipOutgoing, TrunkDialog};
use crate::webhook_client::WebhookClient;

pub type CallRegistry = Arc<RwLock<HashMap<String, CallInfo>>>;
pub type XphoneCallRegistry = Arc<RwLock<HashMap<String, Arc<dyn CallControl>>>>;
pub type PhoneRegistry = Arc<RwLock<HashMap<String, xphone::Phone>>>;

/// Registry of active WebSocket senders, keyed by call_id.
/// Uses std::sync::RwLock because it's accessed from sync xphone callbacks.
pub type WsSenderRegistry = Arc<std::sync::RwLock<HashMap<String, mpsc::Sender<Message>>>>;

/// Registry of active playback handles, keyed by call_id.
pub type PlayRegistry = Arc<RwLock<HashMap<String, PlayHandle>>>;

/// Entry in the trunk dialog map (SIP Call-ID → active dialog state).
pub(crate) struct TrunkDialogEntry {
    /// xbridge call_id for reverse lookup during cleanup.
    pub xbridge_call_id: Option<String>,
    /// xphone::Call reference (for simulate_bye on remote hangup).
    pub xphone_call: Option<Arc<xphone::Call>>,
    /// TrunkDialog reference (for updating dialog state from SIP responses).
    pub trunk_dialog: Option<Arc<TrunkDialog>>,
}

/// Registry of active trunk SIP dialogs, keyed by SIP Call-ID.
pub(crate) type TrunkDialogMap = Arc<RwLock<HashMap<String, TrunkDialogEntry>>>;

pub struct PlayHandle {
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    pub task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
pub struct AppState {
    pub calls: CallRegistry,
    pub(crate) xphone_calls: XphoneCallRegistry,
    pub phones: PhoneRegistry,
    pub(crate) ended_tx: mpsc::Sender<(String, xphone::EndReason, std::time::Duration)>,
    pub(crate) dtmf_tx: mpsc::Sender<(String, String)>,
    pub(crate) state_tx: mpsc::Sender<(String, xphone::CallState)>,
    pub webhook: WebhookClient,
    pub(crate) config: Arc<Config>,
    pub(crate) metrics: Metrics,
    pub(crate) ws_senders: WsSenderRegistry,
    pub(crate) plays: PlayRegistry,
    pub(crate) play_counter: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) trunk_dialogs: TrunkDialogMap,
    /// Trunk server SIP send channel (set when trunk host server starts).
    pub(crate) trunk_sip_tx: Arc<RwLock<Option<mpsc::Sender<SipOutgoing>>>>,
    /// Trunk server local address (set when trunk host server starts).
    pub(crate) trunk_local_addr: Arc<RwLock<Option<SocketAddr>>>,
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
            ws_senders: Arc::new(std::sync::RwLock::new(HashMap::new())),
            plays: Arc::new(RwLock::new(HashMap::new())),
            play_counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            trunk_dialogs: Arc::new(RwLock::new(HashMap::new())),
            trunk_sip_tx: Arc::new(RwLock::new(None)),
            trunk_local_addr: Arc::new(RwLock::new(None)),
        }
    }
}
