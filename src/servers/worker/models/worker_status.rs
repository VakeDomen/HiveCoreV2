use std::time::Instant;

use serde_json::Value;

use crate::servers::worker::models::worker_backend::WorkerBackend;
use crate::servers::worker::models::worker_phase::WorkerPhase;

#[derive(Clone)]
pub struct WorkerConnectionStatus {
    pub index: u64,
    pub nonce: String,
    pub tags: Vec<String>,
    pub state: WorkerPhase,
    pub last_ping: Instant,
    pub last_poll: Instant,
}

#[derive(Clone)]
pub struct WorkerStatus {
    pub name: String,
    pub hive_version: String,
    pub ollama_version: String,
    pub backend: WorkerBackend,
    pub tags: Vec<String>,
    pub state: WorkerPhase,
    pub last_ping: Instant,
    pub last_poll: Instant,
    pub connections: Vec<WorkerConnectionStatus>,
    pub next_connection_index: u64,
    pub model_catalog: Option<Value>,
    pub running_models: Option<Value>,
    pub version_payload: Option<Value>,
}

impl WorkerStatus {
    pub fn register_connection(&mut self, nonce: String) -> u64 {
        let index = self.next_connection_index;
        self.next_connection_index = self.next_connection_index.saturating_add(1);
        self.connections.push(WorkerConnectionStatus {
            index,
            nonce,
            tags: self.tags.clone(),
            state: WorkerPhase::Authenticating,
            last_ping: Instant::now(),
            last_poll: Instant::now(),
        });
        self.sync_summary();
        index
    }

    pub fn connection_mut(&mut self, index: u64) -> Option<&mut WorkerConnectionStatus> {
        self.connections
            .iter_mut()
            .find(|connection| connection.index == index)
    }

    pub fn connection(&self, index: u64) -> Option<&WorkerConnectionStatus> {
        self.connections
            .iter()
            .find(|connection| connection.index == index)
    }

    pub fn remove_connection(&mut self, index: u64) -> bool {
        let before = self.connections.len();
        self.connections
            .retain(|connection| connection.index != index);
        self.sync_summary();
        before != self.connections.len()
    }

    pub fn connection_states(&self) -> Vec<WorkerPhase> {
        self.connections
            .iter()
            .map(|connection| connection.state)
            .collect()
    }

    pub fn connection_counts(&self) -> (usize, usize, usize, usize) {
        let mut authenticating = 0;
        let mut polling = 0;
        let mut working = 0;
        for connection in &self.connections {
            match connection.state {
                WorkerPhase::Authenticating => authenticating += 1,
                WorkerPhase::Polling => polling += 1,
                WorkerPhase::Working => working += 1,
            }
        }
        (self.connections.len(), authenticating, polling, working)
    }

    pub fn accepts_nonce(&self, nonce: &str) -> bool {
        self.connections.is_empty()
            || self
                .connections
                .iter()
                .all(|connection| connection.nonce == nonce)
    }

    pub fn sync_summary(&mut self) {
        self.state = self
            .connections
            .iter()
            .map(|connection| connection.state)
            .max_by_key(priority)
            .unwrap_or(WorkerPhase::Polling);
        if let Some(connection) = self
            .connections
            .iter()
            .max_by_key(|connection| connection.last_poll)
        {
            self.tags = connection.tags.clone();
            self.last_ping = connection.last_ping;
            self.last_poll = connection.last_poll;
        }
    }
}

fn priority(phase: &WorkerPhase) -> u8 {
    match phase {
        WorkerPhase::Authenticating => 0,
        WorkerPhase::Polling => 1,
        WorkerPhase::Working => 2,
    }
}
