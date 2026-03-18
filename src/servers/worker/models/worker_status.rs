use std::time::Instant;

use serde_json::Value;

use crate::servers::worker::models::worker_phase::WorkerPhase;

#[derive(Clone)]
pub struct WorkerStatus {
    pub name: String,
    pub nonce: String,
    pub hive_version: String,
    pub ollama_version: String,
    pub tags: Vec<String>,
    pub state: WorkerPhase,
    pub last_ping: Instant,
    pub last_poll: Instant,
    pub model_catalog: Option<Value>,
    pub running_models: Option<Value>,
    pub version_payload: Option<Value>,
}
