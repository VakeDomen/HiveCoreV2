use std::collections::HashMap;
use std::io;
use std::sync::mpsc;
use std::sync::{Arc, RwLock};

use crate::app::Config;
use crate::auth::{KeyStore, Role};
use crate::servers::proxy::queue::RequestQueue;
use crate::servers::worker::models::worker_status::WorkerStatus;
use crate::shared::capture::CaptureEvent;
use crate::shared::http::{HttpRequest, UsageEvent};

pub struct AppState {
    pub config: Config,
    pub keys: KeyStore,
    pub request_queue: RequestQueue,
    pub workers: RwLock<HashMap<String, WorkerStatus>>,
    pub stats_tx: mpsc::Sender<UsageEvent>,
    pub capture_tx: mpsc::Sender<CaptureEvent>,
}

impl AppState {
    pub fn new(
        config: Config,
        stats_tx: mpsc::Sender<UsageEvent>,
        capture_tx: mpsc::Sender<CaptureEvent>,
    ) -> io::Result<Self> {
        Ok(Self {
            keys: KeyStore::new(&config.database_url)?,
            config,
            request_queue: RequestQueue::default(),
            workers: RwLock::new(HashMap::new()),
            stats_tx,
            capture_tx,
        })
    }
}

pub fn authorize_admin(state: &Arc<AppState>, request: &HttpRequest) -> bool {
    request
        .bearer_token()
        .and_then(|token| state.keys.verify(token, &[Role::Admin]))
        .is_some()
}
