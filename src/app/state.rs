use std::collections::HashMap;
use std::io;
use std::sync::{Arc, RwLock};

use crate::app::Config;
use crate::auth::{KeyStore, Role};
use crate::servers::proxy::queue::RequestQueue;
use crate::servers::worker::models::worker_status::WorkerStatus;
use crate::shared::http::HttpRequest;

pub struct AppState {
    pub config: Config,
    pub keys: KeyStore,
    pub request_queue: RequestQueue,
    pub workers: RwLock<HashMap<String, WorkerStatus>>,
}

impl AppState {
    pub fn new(config: Config) -> io::Result<Self> {
        Ok(Self {
            keys: KeyStore::new(&config.database_url)?,
            config,
            request_queue: RequestQueue::default(),
            workers: RwLock::new(HashMap::new()),
        })
    }
}

pub fn authorize_admin(state: &Arc<AppState>, request: &HttpRequest) -> bool {
    request
        .bearer_token()
        .and_then(|token| state.keys.verify(token, &[Role::Admin]))
        .is_some()
}
