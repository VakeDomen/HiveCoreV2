use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crate::servers::proxy::models::client_task::ClientTask;
use crate::servers::proxy::models::model_route_kind::ModelRouteKind;
use crate::servers::proxy::models::queue_snapshot::QueueSnapshot;
use crate::servers::worker::models::worker_backend::WorkerBackend;
use crate::shared::log;

/// Each `(model, kind)` model queue and each worker's node queue is an
/// independently-locked `Arc<Mutex<VecDeque>>`. The outer `RwLock<HashMap>`
/// only guards *membership* (which keys exist and their `Arc`s); it is held
/// briefly to fetch/create/remove a key and is never held while the deque
/// itself is locked. Two threads touching different keys therefore never
/// contend, so dispatch is not serialized on a single global lock.
pub struct RequestQueue {
    model_queue: RwLock<HashMap<ModelQueueKey, Arc<Mutex<VecDeque<ClientTask>>>>>,
    node_queue: RwLock<HashMap<String, Arc<Mutex<VecDeque<ClientTask>>>>>,
    default_timeout: Duration,
}

impl Default for RequestQueue {
    fn default() -> Self {
        Self {
            model_queue: RwLock::new(HashMap::new()),
            node_queue: RwLock::new(HashMap::new()),
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
        let queued_model = model.clone();
        let deque = self.model_deque_for(&ModelQueueKey { model, kind })?;
        let request_id = task.id;
        let method = task.request.method.clone();
        let uri = task.request.uri.clone();
        let user = task
            .context
            .key_name
            .clone()
            .unwrap_or_else(|| "Unauthenticated".to_string());
        let mut guard = deque.lock().map_err(|_| "model queue poisoned")?;
        guard.push_back(task);
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
        let deque = self.node_deque_for(&node)?;
        let mut guard = deque.lock().map_err(|_| "node queue poisoned")?;
        guard.push_back(task);
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
        // Node-targeted work first: this worker's own queue is preferred.
        if let Some(task) = self.dequeue_node(worker_name) {
            return Some(task);
        }

        // Otherwise scan the model queues for a compatible (tag, kind).
        for tag in tags {
            for kind in [
                ModelRouteKind::OpenAiCompatible,
                ModelRouteKind::OllamaNative,
                ModelRouteKind::VllmSpecific,
                ModelRouteKind::SystemOne,
            ] {
                if !kind.compatible_with(backend) {
                    continue;
                }
                let key = ModelQueueKey {
                    model: tag.clone(),
                    kind,
                };
                if let Some(selected) = self.dequeue_model_key(&key, worker_name, backend, tag) {
                    return Some(selected);
                }
            }
        }
        None
    }

    /// Pop one task from this worker's targeted queue, or `None` if it has no
    /// dedicated queue (or its queue is empty). Expires stale tasks and
    /// removes the key when empty — the same hygiene `dequeue_for_worker`
    /// previously did under one lock, now scoped to this worker's queue only.
    fn dequeue_node(&self, worker_name: &str) -> Option<ClientTask> {
        let deque = {
            let read = self.node_queue.read().ok()?;
            read.get(worker_name).map(Arc::clone)
        }?;
        let mut guard = deque.lock().ok()?;
        expire_queue_tasks(
            &mut guard,
            self.default_timeout,
            &mut Vec::new(),
            &format!("worker:{worker_name}"),
            true,
        );
        let task = guard.pop_front();
        if let Some(task) = &task {
            let wait = log::format_duration(task.queue_wait());
            log::info(format!(
                "dispatch request id={} worker={} route=targeted wait={}",
                log::bold(task.id.to_string()),
                log::bold(worker_name),
                log::bold(wait)
            ));
        }
        let remove = guard.is_empty();
        drop(guard);
        if remove {
            self.remove_node_key_if_empty(worker_name);
        }
        task
    }

    /// Pop one task from a specific `(model, kind)` queue if it is backend
    /// compatible and non-empty. Expires stale tasks and removes the key when
    /// empty. Returns `None` when nothing was decoratable from this key.
    fn dequeue_model_key(
        &self,
        key: &ModelQueueKey,
        worker_name: &str,
        backend: WorkerBackend,
        tag: &str,
    ) -> Option<ClientTask> {
        let deque = {
            let read = self.model_queue.read().ok()?;
            read.get(key).map(Arc::clone)
        }?;
        let mut guard = deque.lock().ok()?;
        expire_queue_tasks(
            &mut guard,
            self.default_timeout,
            &mut Vec::new(),
            &format!("model:{} kind={}", key.model, key.kind.as_str()),
            true,
        );
        let task = guard.pop_front()?;
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
        let remove = guard.is_empty();
        drop(guard);
        if remove {
            self.remove_model_key_if_empty(key);
        }
        Some(task)
    }

    /// Get the deque for a `(model, kind)` key, creating it if absent.
    fn model_deque_for(
        &self,
        key: &ModelQueueKey,
    ) -> Result<Arc<Mutex<VecDeque<ClientTask>>>, &'static str> {
        let existing = self
            .model_queue
            .read()
            .map_err(|_| "model queue poisoned")?
            .get(key)
            .map(Arc::clone);
        match existing {
            Some(deque) => Ok(deque),
            None => {
                let mut write = self
                    .model_queue
                    .write()
                    .map_err(|_| "model queue poisoned")?;
                Ok(write
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(VecDeque::new())))
                    .clone())
            }
        }
    }

    /// Get the deque for a worker's node queue, creating it if absent.
    fn node_deque_for(
        &self,
        worker_name: &str,
    ) -> Result<Arc<Mutex<VecDeque<ClientTask>>>, &'static str> {
        let existing = self
            .node_queue
            .read()
            .map_err(|_| "node queue poisoned")?
            .get(worker_name)
            .map(Arc::clone);
        match existing {
            Some(deque) => Ok(deque),
            None => {
                let mut write = self
                    .node_queue
                    .write()
                    .map_err(|_| "node queue poisoned")?;
                Ok(write
                    .entry(worker_name.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(VecDeque::new())))
                    .clone())
            }
        }
    }

    /// Remove a worker's node key from the index if its deque is still empty.
    fn remove_node_key_if_empty(&self, worker_name: &str) {
        if let Ok(mut write) = self.node_queue.write() {
            if let Some(deque) = write.get(worker_name) {
                if deque.lock().map(|guard| guard.is_empty()).unwrap_or(false) {
                    write.remove(worker_name);
                }
            }
        }
    }

    /// Remove a `(model, kind)` key from the index if its deque is still empty.
    fn remove_model_key_if_empty(&self, key: &ModelQueueKey) {
        if let Ok(mut write) = self.model_queue.write() {
            if let Some(deque) = write.get(key) {
                if deque.lock().map(|guard| guard.is_empty()).unwrap_or(false) {
                    write.remove(key);
                }
            }
        }
    }

    pub fn enqueue_to_node(
        &self,
        worker_name: String,
        task: ClientTask,
    ) -> Result<(), &'static str> {
        self.enqueue_node(worker_name, task)
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        // Snapshot the Arcs under a brief read lock, then read each deque's
        // length under its own lock so the index lock is never held while a
        // deque is locked.
        let model_entries = self
            .model_queue
            .read()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(k, deque)| (k.display(), Arc::clone(deque)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let model_queue = model_entries
            .into_iter()
            .map(|(key, deque)| {
                let len = deque.lock().map(|guard| guard.len()).unwrap_or(0);
                (key, len)
            })
            .collect::<HashMap<_, _>>();

        let node_entries = self
            .node_queue
            .read()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(k, deque)| (k.clone(), Arc::clone(deque)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let node_queue = node_entries
            .into_iter()
            .map(|(key, deque)| {
                let len = deque.lock().map(|guard| guard.len()).unwrap_or(0);
                (key, len)
            })
            .collect::<HashMap<_, _>>();

        QueueSnapshot {
            model_queue,
            node_queue,
        }
    }

    pub fn expire_older_than(&self, max_age: Duration) -> Vec<ClientTask> {
        let mut expired = Vec::new();

        let model_entries = self
            .model_queue
            .read()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(k, deque)| (k.clone(), Arc::clone(deque)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (key, deque) in model_entries {
            let became_empty = {
                let mut guard = deque.lock().ok();
                match guard.as_mut() {
                    Some(guard) => {
                        expire_queue_tasks(
                            guard,
                            max_age,
                            &mut expired,
                            &format!("model:{} kind={}", key.model, key.kind.as_str()),
                            false,
                        );
                        guard.is_empty()
                    }
                    None => false,
                }
            };
            if became_empty {
                self.remove_model_key_if_empty(&key);
            }
        }

        let node_entries = self
            .node_queue
            .read()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(k, deque)| (k.clone(), Arc::clone(deque)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (worker, deque) in node_entries {
            let became_empty = {
                let mut guard = deque.lock().ok();
                match guard.as_mut() {
                    Some(guard) => {
                        expire_queue_tasks(
                            guard,
                            max_age,
                            &mut expired,
                            &format!("worker:{worker}"),
                            false,
                        );
                        guard.is_empty()
                    }
                    None => false,
                }
            };
            if became_empty {
                self.remove_node_key_if_empty(&worker);
            }
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
            HttpRequest::new("POST".to_string(), uri.to_string(), "HTTP/1.1".to_string(), Default::default(), Vec::new()),
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

    #[test]
    fn systemone_work_is_not_dequeued_by_ollama_workers() {
        let queue = RequestQueue::default();
        queue
            .enqueue_model(
                "systemone/diy-jev-0.1.0".to_string(),
                ModelRouteKind::SystemOne,
                task("/"),
            )
            .expect("enqueue systemone");

        assert!(
            queue
                .dequeue_for_worker(
                    "ollama",
                    WorkerBackend::Ollama,
                    &["systemone/diy-jev-0.1.0".to_string()]
                )
                .is_none()
        );
        assert!(
            queue
                .dequeue_for_worker(
                    "systemone",
                    WorkerBackend::SystemOne,
                    &["systemone/diy-jev-0.1.0".to_string()]
                )
                .is_some()
        );
    }

    #[test]
    fn concurrent_enqueue_dequeue_across_distinct_keys_lose_no_tasks() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;

        let queue = Arc::new(RequestQueue::default());
        const KEYS: usize = 8;
        const PER_KEY: usize = 50;

        // Enqueue many tasks to KEYS distinct model keys from one thread each.
        let producers: Vec<_> = (0..KEYS)
            .map(|i| {
                let queue = Arc::clone(&queue);
                thread::spawn(move || {
                    for n in 0..PER_KEY {
                        let uri = format!("/key{i}/task{n}");
                        queue
                            .enqueue_model(
                                format!("model-{i}"),
                                ModelRouteKind::OpenAiCompatible,
                                task(&uri),
                            )
                            .expect("enqueue");
                    }
                })
            })
            .collect();
        for p in producers {
            p.join().unwrap();
        }

        // Dequeue from KEYS distinct worker names concurrently; each worker can
        // drain any OpenAiCompatible model queue, so all KEYS*PER_KEY tasks must
        // come back exactly once across all consumers.
        let total = Arc::new(AtomicUsize::new(0));
        let consumers: Vec<_> = (0..KEYS)
            .map(|i| {
                let queue = Arc::clone(&queue);
                let total = Arc::clone(&total);
                thread::spawn(move || {
                    let tags: Vec<String> =
                        (0..KEYS).map(|m| format!("model-{m}")).collect();
                    let mut drained = 0;
                    loop {
                        match queue.dequeue_for_worker(
                            &format!("worker-{i}"),
                            WorkerBackend::OllamaLegacy,
                            &tags,
                        ) {
                            Some(_) => drained += 1,
                            None => break,
                        }
                    }
                    total.fetch_add(drained, Ordering::SeqCst);
                })
            })
            .collect();
        for c in consumers {
            c.join().unwrap();
        }

        assert_eq!(total.load(Ordering::SeqCst), KEYS * PER_KEY);
        assert!(queue.snapshot().model_queue.is_empty());
    }
}
