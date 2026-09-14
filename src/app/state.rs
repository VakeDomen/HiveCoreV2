use std::collections::HashMap;
use std::io;
use std::sync::mpsc;
use std::sync::{Arc, RwLock};

use crate::app::{Config, RateLimiter};
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
    pub rate_limiter: RateLimiter,
}

impl AppState {
    pub fn new(
        config: Config,
        stats_tx: mpsc::Sender<UsageEvent>,
        capture_tx: mpsc::Sender<CaptureEvent>,
    ) -> io::Result<Self> {
        let keys = KeyStore::new(&config.database_url)?;
        let rate_limiter = RateLimiter::new(
            config.rate_limit_tiers.clone(),
            config.default_rate_limit_tier.clone(),
        );
        Ok(Self {
            keys,
            rate_limiter,
            config,
            request_queue: RequestQueue::default(),
            workers: RwLock::new(HashMap::new()),
            stats_tx,
            capture_tx,
        })
    }
}

pub fn authorize_management(
    state: &Arc<AppState>,
    request: &HttpRequest,
    allowed_roles: &[Role],
) -> Option<Role> {
    request
        .bearer_token()
        .and_then(|token| state.keys.verify(token, allowed_roles))
        .map(|record| record.role)
}
