use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::call::CallInfo;
use crate::config::Config;

pub type CallRegistry = Arc<RwLock<HashMap<String, CallInfo>>>;
pub type XphoneCallRegistry = Arc<RwLock<HashMap<String, Arc<xphone::Call>>>>;

#[derive(Clone)]
pub struct AppState {
    pub(crate) calls: CallRegistry,
    pub(crate) xphone_calls: XphoneCallRegistry,
    pub(crate) config: Arc<Config>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            calls: Arc::new(RwLock::new(HashMap::new())),
            xphone_calls: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(config),
        }
    }
}
