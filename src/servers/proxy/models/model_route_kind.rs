use crate::servers::worker::models::worker_backend::WorkerBackend;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelRouteKind {
    OllamaNative,
    OpenAiCompatible,
    VllmSpecific,
}

impl ModelRouteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OllamaNative => "ollama",
            Self::OpenAiCompatible => "openai",
            Self::VllmSpecific => "vllm",
        }
    }

    pub fn compatible_with(self, backend: WorkerBackend) -> bool {
        match self {
            Self::OllamaNative => {
                matches!(backend, WorkerBackend::OllamaLegacy | WorkerBackend::Ollama)
            }
            Self::OpenAiCompatible => true,
            Self::VllmSpecific => matches!(backend, WorkerBackend::Vllm),
        }
    }
}
