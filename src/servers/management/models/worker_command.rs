use serde::Deserialize;

#[derive(Deserialize)]
pub struct WorkerCommandRequest {
    pub worker: String,
    pub command: WorkerCommand,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum WorkerCommand {
    Update,
    Reboot,
    Shutdown,
}

impl WorkerCommand {
    pub fn hive_method(self) -> &'static str {
        match self {
            WorkerCommand::Update => "UPDATE_OLLAMA",
            WorkerCommand::Reboot => "REBOOT",
            WorkerCommand::Shutdown => "SHUTDOWN",
        }
    }
}
