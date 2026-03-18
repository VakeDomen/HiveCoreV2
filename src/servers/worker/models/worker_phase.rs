#[derive(Clone, Copy)]
pub enum WorkerPhase {
    Authenticating,
    Polling,
    Working,
}

impl WorkerPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkerPhase::Authenticating => "Authenticating",
            WorkerPhase::Polling => "Polling",
            WorkerPhase::Working => "Working",
        }
    }
}
