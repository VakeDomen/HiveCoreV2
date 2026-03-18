use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
pub enum WorkerPhase {
    Authenticating,
    Polling,
    Working,
}
