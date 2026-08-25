use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use crate::servers::proxy::models::client_task::ClientTask;
use crate::servers::proxy::models::model_route_kind::ModelRouteKind;
use crate::servers::proxy::models::queue_snapshot::QueueSnapshot;
use crate::servers::worker::models::worker_backend::WorkerBackend;
use crate::shared::log;

pub struct RequestQueue {
    model_queue: Mutex<HashMap<ModelQueueKey, VecDeque<ClientTask>>>,
    node_queue: Mutex<HashMap<String, VecDeque<ClientTask>>>,
    default_timeout: Duration,
}

impl Default for RequestQueue {
    fn default() -> Self {
        Self {
            model_queue: Mutex::new(HashMap::new()),
            node_queue: Mutex::new(HashMap::new()),
            default_timeout: Duration::from_secs(60),
        }
    }
}

impl RequestQueue {
    pub fn enqueue_model(
        &self,
        model: String,
        kind: ModelRouteKind,
        task: ClientTask,
    ) -> Result<(), &'static str> {
        let mut guard = self
            .model_queue
            .lock()
            .map_err(|_| "model queue poisoned")?;
        let queued_model = model.clone();
        let request_id = task.id;
        let method = task.request.method.clone();
        let uri = task.request.uri.clone();
        let user = task
            .context
            .key_name
            .clone()
            .unwrap_or_else(|| "Unauthenticated".to_string());
        guard
            .entry(ModelQueueKey { model, kind })
            .or_default()
            .push_back(task);
        log::info(format!(
            "queued request id={} user={} route=model:{} kind={} method={} uri={}",
            log::bold(request_id.to_string()),
            log::bold(&user),
            log::bold(&queued_model),
            log::bold(kind.as_str()),
            method,
            uri
        ));
        Ok(())
    }

    pub fn enqueue_node(&self, node: String, task: ClientTask) -> Result<(), &'static str> {
        let request_id = task.id;
        let method = task.request.method.clone();
        let uri = task.request.uri.clone();
        let user = task
            .context
            .key_name
            .clone()
            .unwrap_or_else(|| "Unauthenticated".to_string());
        let node_name = node.clone();
        let mut guard = self.node_queue.lock().map_err(|_| "node queue poisoned")?;
        guard.entry(node).or_default().push_back(task);
        log::info(format!(
            "queued request id={} user={} route=worker:{} method={} uri={}",
            log::bold(request_id.to_string()),
            log::bold(&user),
            log::bold(&node_name),
            method,
            uri
        ));
        Ok(())
    }

    pub fn dequeue_for_worker(
        &self,
        worker_name: &str,
        backend: WorkerBackend,
        tags: &[String],
    ) -> Option<ClientTask> {
        if let Ok(mut node_guard) = self.node_queue.lock() {
            let mut remove_node_queue = false;
            let mut selected_task = None;
            if let Some(queue) = node_guard.get_mut(worker_name) {
                expire_queue_tasks(
                    queue,
                    self.default_timeout,
                    &mut Vec::new(),
                    &format!("worker:{worker_name}"),
                    true,
                );
                remove_node_queue = queue.is_empty();
                if let Some(task) = queue.pop_front() {
                    remove_node_queue = queue.is_empty();
                    let wait = log::format_duration(task.queue_wait());
                    log::info(format!(
                        "dispatch request id={} worker={} route=targeted wait={}",
                        log::bold(task.id.to_string()),
                        log::bold(worker_name),
                        log::bold(wait)
                    ));
                    selected_task = Some(task);
                }
            }
            if remove_node_queue {
                node_guard.remove(worker_name);
            }
            if selected_task.is_some() {
                return selected_task;
            }
        }

        let Ok(mut model_guard) = self.model_queue.lock() else {
            return None;
        };
        for tag in tags {
            let Some(key) = next_compatible_key(&model_guard, backend, tag) else {
                continue;
            };
            if let Some(queue) = model_guard.get_mut(&key) {
                let mut selected_task = None;
                expire_queue_tasks(
                    queue,
                    self.default_timeout,
                    &mut Vec::new(),
                    &format!("model:{} kind={}", key.model, key.kind.as_str()),
                    true,
                );
                let mut remove_model_queue = queue.is_empty();
                if let Some(task) = queue.pop_front() {
                    remove_model_queue = queue.is_empty();
                    let wait = log::format_duration(task.queue_wait());
                    log::info(format!(
                        "dispatch request id={} worker={} backend={} route=model:{} kind={} wait={}",
                        log::bold(task.id.to_string()),
                        log::bold(worker_name),
                        log::bold(backend.as_str()),
                        log::bold(tag),
                        log::bold(key.kind.as_str()),
                        log::bold(wait)
                    ));
                    selected_task = Some(task);
                }
                if remove_model_queue {
                    model_guard.remove(&key);
                }
                if selected_task.is_some() {
                    return selected_task;
                }
            }
        }
        None
    }

    pub fn enqueue_to_node(
        &self,
        worker_name: String,
        task: ClientTask,
    ) -> Result<(), &'static str> {
        self.enqueue_node(worker_name, task)
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        let model_queue = self
            .model_queue
            .lock()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(k, v)| (k.display(), v.len()))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let node_queue = self
            .node_queue
            .lock()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(k, v)| (k.clone(), v.len()))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();

        QueueSnapshot {
            model_queue,
            node_queue,
        }
    }

    pub fn expire_older_than(&self, max_age: Duration) -> Vec<ClientTask> {
        let mut expired = Vec::new();

        if let Ok(mut guard) = self.model_queue.lock() {
            guard.retain(|key, queue| {
                expire_queue_tasks(
                    queue,
                    max_age,
                    &mut expired,
                    &format!("model:{} kind={}", key.model, key.kind.as_str()),
                    false,
                );
                !queue.is_empty()
            });
        }

        if let Ok(mut guard) = self.node_queue.lock() {
            guard.retain(|worker, queue| {
                expire_queue_tasks(
                    queue,
                    max_age,
                    &mut expired,
                    &format!("worker:{worker}"),
                    false,
                );
                !queue.is_empty()
            });
        }

        expired
    }
}

fn expire_queue_tasks(
    queue: &mut VecDeque<ClientTask>,
    default_timeout: Duration,
    expired: &mut Vec<ClientTask>,
    route: &str,
    custom_timeout_only: bool,
) {
    let mut kept = VecDeque::new();
    while let Some(task) = queue.pop_front() {
        if custom_timeout_only && task.context.queue_timeout.is_none() {
            kept.push_back(task);
            continue;
        }
        if task.is_expired(default_timeout) {
            log_expired_task(&task, route);
            expired.push(task);
        } else {
            kept.push_back(task);
        }
    }
    *queue = kept;
}

fn log_expired_task(task: &ClientTask, route: &str) {
    let user = task
        .context
        .key_name
        .as_deref()
        .unwrap_or("Unauthenticated");
    log::warn(format!(
        "expired queued request id={} user={} route={} method={} uri={} wait={}",
        log::bold(task.id.to_string()),
        log::bold(user),
        route,
        task.request.method,
        task.request.uri,
        log::bold(log::format_duration(task.queue_wait()))
    ));
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ModelQueueKey {
    model: String,
    kind: ModelRouteKind,
}

impl ModelQueueKey {
    fn display(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.model)
    }
}

fn next_compatible_key(
    queues: &HashMap<ModelQueueKey, VecDeque<ClientTask>>,
    backend: WorkerBackend,
    model: &str,
) -> Option<ModelQueueKey> {
    [
        ModelRouteKind::OpenAiCompatible,
        ModelRouteKind::OllamaNative,
        ModelRouteKind::VllmSpecific,
    ]
    .into_iter()
    .find_map(|kind| {
        if !kind.compatible_with(backend) {
            return None;
        }
        let key = ModelQueueKey {
            model: model.to_string(),
            kind,
        };
        queues
            .get(&key)
            .and_then(|queue| (!queue.is_empty()).then_some(key))
    })
}

#[cfg(test)]
mod tests {
    use super::RequestQueue;
    use crate::servers::proxy::models::client_task::{ClientTask, RequestContext};
    use crate::servers::proxy::models::model_route_kind::ModelRouteKind;
    use crate::servers::proxy::models::response_target::ResponseTarget;
    use crate::servers::worker::models::worker_backend::WorkerBackend;
    use crate::shared::http::HttpRequest;
    use std::time::{Duration, Instant};

    fn task(uri: &str) -> ClientTask {
        ClientTask::new(
            HttpRequest {
                method: "POST".to_string(),
                uri: uri.to_string(),
                protocol: "HTTP/1.1".to_string(),
                headers: Default::default(),
                body: Vec::new(),
            },
            ResponseTarget::Ignore,
            RequestContext::default(),
        )
    }

    fn timed_task(uri: &str, queue_timeout: Duration, age: Duration) -> ClientTask {
        let mut task = task(uri);
        task.context.queue_timeout = Some(queue_timeout);
        task.queued_at = Instant::now() - age;
        task
    }

    #[test]
    fn ollama_native_work_is_not_dequeued_by_vllm_workers() {
        let queue = RequestQueue::default();
        queue
            .enqueue_model(
                "llama3".to_string(),
                ModelRouteKind::OllamaNative,
                task("/api/generate"),
            )
            .expect("enqueue");

        assert!(
            queue
                .dequeue_for_worker("vllm", WorkerBackend::Vllm, &["llama3".to_string()])
                .is_none()
        );
        assert!(
            queue
                .dequeue_for_worker("ollama", WorkerBackend::Ollama, &["llama3".to_string()])
                .is_some()
        );
    }

    #[test]
    fn openai_compatible_work_can_be_dequeued_by_any_backend() {
        let queue = RequestQueue::default();
        queue
            .enqueue_model(
                "llama3".to_string(),
                ModelRouteKind::OpenAiCompatible,
                task("/v1/chat/completions"),
            )
            .expect("enqueue");

        assert!(
            queue
                .dequeue_for_worker(
                    "ollama",
                    WorkerBackend::OllamaLegacy,
                    &["llama3".to_string()]
                )
                .is_some()
        );

        queue
            .enqueue_model(
                "llama3".to_string(),
                ModelRouteKind::OpenAiCompatible,
                task("/v1/chat/completions"),
            )
            .expect("enqueue");
        assert!(
            queue
                .dequeue_for_worker("vllm", WorkerBackend::Vllm, &["llama3".to_string()])
                .is_some()
        );
    }

    #[test]
    fn vllm_specific_work_is_not_dequeued_by_ollama_workers() {
        let queue = RequestQueue::default();
        queue
            .enqueue_model(
                "reranker".to_string(),
                ModelRouteKind::VllmSpecific,
                task("/rerank"),
            )
            .expect("enqueue");

        assert!(
            queue
                .dequeue_for_worker("ollama", WorkerBackend::Ollama, &["reranker".to_string()])
                .is_none()
        );
        assert!(
            queue
                .dequeue_for_worker("vllm", WorkerBackend::Vllm, &["reranker".to_string()])
                .is_some()
        );
    }

    #[test]
    fn expire_older_than_removes_stale_model_tasks() {
        let queue = RequestQueue::default();
        queue
            .enqueue_model(
                "llama3".to_string(),
                ModelRouteKind::OpenAiCompatible,
                task("/v1/chat/completions"),
            )
            .expect("enqueue");

        let expired = queue.expire_older_than(Duration::from_millis(0));

        assert_eq!(expired.len(), 1);
        assert!(queue.snapshot().model_queue.is_empty());
    }

    #[test]
    fn expire_older_than_keeps_fresh_model_tasks() {
        let queue = RequestQueue::default();
        queue
            .enqueue_model(
                "llama3".to_string(),
                ModelRouteKind::OpenAiCompatible,
                task("/v1/chat/completions"),
            )
            .expect("enqueue");

        let expired = queue.expire_older_than(Duration::from_secs(60));

        assert!(expired.is_empty());
        assert!(
            queue
                .dequeue_for_worker("vllm", WorkerBackend::Vllm, &["llama3".to_string()])
                .is_some()
        );
    }

    #[test]
    fn expire_older_than_honors_short_task_timeout() {
        let queue = RequestQueue::default();
        queue
            .enqueue_to_node(
                "worker-a".to_string(),
                timed_task("/v1/models", Duration::from_millis(5), Duration::from_millis(10)),
            )
            .expect("enqueue");

        let expired = queue.expire_older_than(Duration::from_secs(60));

        assert_eq!(expired.len(), 1);
        assert!(queue.snapshot().node_queue.is_empty());
    }

    #[test]
    fn dispatch_prunes_stale_targeted_probe_before_model_work() {
        let queue = RequestQueue::default();
        queue
            .enqueue_to_node(
                "vllm".to_string(),
                timed_task("/v1/models", Duration::from_millis(5), Duration::from_millis(10)),
            )
            .expect("enqueue probe");
        queue
            .enqueue_model(
                "reranker".to_string(),
                ModelRouteKind::VllmSpecific,
                task("/rerank"),
            )
            .expect("enqueue rerank");

        let task = queue
            .dequeue_for_worker("vllm", WorkerBackend::Vllm, &["reranker".to_string()])
            .expect("rerank task");

        assert_eq!(task.request.uri, "/rerank");
        assert!(queue.snapshot().node_queue.is_empty());
    }
}
