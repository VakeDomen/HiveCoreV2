#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerBackend {
    OllamaLegacy,
    Ollama,
    Vllm,
}

impl WorkerBackend {
    pub fn from_poll_method(method: &str) -> Option<Self> {
        match method {
            "POLL" => Some(Self::OllamaLegacy),
            "POLL-OLLAMA" => Some(Self::Ollama),
            "POLL-VLLM" => Some(Self::Vllm),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OllamaLegacy => "ollama_legacy",
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WorkerBackend;

    #[test]
    fn poll_methods_map_to_backend() {
        assert_eq!(
            WorkerBackend::from_poll_method("POLL"),
            Some(WorkerBackend::OllamaLegacy)
        );
        assert_eq!(
            WorkerBackend::from_poll_method("POLL-OLLAMA"),
            Some(WorkerBackend::Ollama)
        );
        assert_eq!(
            WorkerBackend::from_poll_method("POLL-VLLM"),
            Some(WorkerBackend::Vllm)
        );
        assert_eq!(WorkerBackend::from_poll_method("PING"), None);
    }
}
