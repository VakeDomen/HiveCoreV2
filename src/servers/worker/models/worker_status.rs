use std::time::Instant;

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
}
