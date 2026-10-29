use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
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
    idle_gate: IdleGate,
}

impl Default for RequestQueue {
    fn default() -> Self {
        Self {
            model_queue: RwLock::new(HashMap::new()),
            node_queue: RwLock::new(HashMap::new()),
            default_timeout: Duration::from_secs(60),
            idle_gate: IdleGate::default(),
        }
    }
}

/// Wakes pollers parked in [`RequestQueue::wait_for_work`].
///
/// `generation` is bumped by every enqueue. A poller observes it *before* its
/// dequeue attempt, so an enqueue that lands in the gap between the failed
/// dequeue and the wait cannot be missed: [`RequestQueue::wait_for_work`]
/// returns immediately whenever the generation has moved past what the caller
/// observed.
#[derive(Default)]
struct IdleGate {
    generation: Mutex<u64>,
    woken: Condvar,
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
        // Wake parked pollers only once the task is visible and the enqueue is
        // logged; the deque lock is released first so a woken poller can take
        // it immediately.
        drop(guard);
        self.notify_work_available();
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
        // See `enqueue_model`: notify after the task is visible and logged.
        drop(guard);
        self.notify_work_available();
        Ok(())
    }

    /// Current idle generation.
    ///
    /// Take this *before* attempting a dequeue and hand it to
    /// [`Self::wait_for_work`]: an enqueue that races the attempt then still
    /// wakes the caller instead of stranding the new task until the next poll.
    fn idle_mark(&self) -> u64 {
        *self.lock_generation()
    }

    /// Park the calling thread until work is enqueued, or until `timeout`
    /// elapses — whichever comes first.
    ///
    /// Returns immediately if the generation has moved past `seen`, which is
    /// what makes the mark → dequeue → wait sequence race-free rather than a
    /// blind sleep that can miss a wakeup.
    fn wait_for_work(&self, seen: u64, timeout: Duration) {
        let generation = self.lock_generation();
        let (generation, _) = self
            .idle_gate
            .woken
            .wait_timeout_while(generation, timeout, |generation| *generation == seen)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        drop(generation);
    }

    fn lock_generation(&self) -> MutexGuard<'_, u64> {
        self.idle_gate
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Bump the generation and wake every parked poller.
    ///
    /// Called once the task is visible in its queue, so a woken poller either
    /// takes it or correctly finds the queue empty again (another poller won
    /// the race). Waking every poller rather than one is deliberate: the
    /// woken thread may poll for a backend or tag set that cannot serve the
    /// new task, and the task must not wait a whole idle window for a
    /// compatible worker's timeout.
    fn notify_work_available(&self) {
        {
            let mut generation = self.lock_generation();
            *generation = generation.wrapping_add(1);
        }
        self.idle_gate.woken.notify_all();
    }

    /// Dequeue work for one poll; if the queue is empty, park for at most
    /// `idle_wait` and check once more before giving up.
    ///
    /// A poller answers work immediately, but an empty poll is held open for a
    /// short randomised window rather than answered at once: a request that
    /// lands just after the poll then rides the same frame out. The mark is
    /// taken *before* the first attempt (see [`Self::idle_mark`]), so work
    /// enqueued in the gap still wakes the wait instead of being stranded
    /// until the next poll, and an enqueue wakes it early.
    ///
    /// Returns `None` only when both attempts came up empty — the caller
    /// answers `PONG`.
    pub fn next_for_worker(
        &self,
        worker_name: &str,
        backend: WorkerBackend,
        tags: &[String],
        idle_wait: Duration,
    ) -> Option<ClientTask> {
        let idle_mark = self.idle_mark();
        if let Some(task) = self.dequeue_for_worker(worker_name, backend, tags) {
            return Some(task);
        }
        self.wait_for_work(idle_mark, idle_wait);
        self.dequeue_for_worker(worker_name, backend, tags)
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
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn task(uri: &str) -> ClientTask {
        ClientTask::new(
            HttpRequest::new("POST".to_string(), uri.to_string(), "HTTP/1.1".to_string(), Default::default(), Vec::new()),
            ResponseTarget::Ignore,
            RequestContext::default(),
        )
    }

    fn enqueue_openai_work(queue: &RequestQueue, uri: &str) {
        queue
            .enqueue_model(
                "llama3".to_string(),
                ModelRouteKind::OpenAiCompatible,
                task(uri),
            )
            .expect("enqueue");
    }

    #[test]
    fn idle_mark_taken_before_the_dequeue_still_sees_a_racing_enqueue() {
        // This is exactly the ordering `handle_poll` uses: observe the
        // generation, attempt a dequeue, and only then park. Work enqueued
        // after the mark but before the wait must return the wait at once —
        // otherwise the task would sit until the next poll.
        let queue = RequestQueue::default();
        let mark = queue.idle_mark();
        enqueue_openai_work(&queue, "/v1/chat/completions");

        let started = Instant::now();
        queue.wait_for_work(mark, Duration::from_secs(5));

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "wait ignored an enqueue that beat it to the gate"
        );
        assert!(
            queue
                .dequeue_for_worker(
                    "worker-a",
                    WorkerBackend::OllamaLegacy,
                    &["llama3".to_string()]
                )
                .is_some(),
            "the task that woke the poller must be dispatchable"
        );
    }

    #[test]
    fn idle_wait_wakes_a_parked_poller_when_work_arrives() {
        let queue = Arc::new(RequestQueue::default());
        let mark = queue.idle_mark();
        let waiter = {
            let queue = Arc::clone(&queue);
            thread::spawn(move || {
                let started = Instant::now();
                queue.wait_for_work(mark, Duration::from_secs(5));
                started.elapsed()
            })
        };

        // Let the poller park before the enqueue that must wake it.
        thread::sleep(Duration::from_millis(20));
        enqueue_openai_work(&queue, "/v1/chat/completions");

        let elapsed = waiter.join().expect("waiter thread");
        assert!(
            elapsed < Duration::from_secs(2),
            "parked poller was not woken by the enqueue (waited {elapsed:?})"
        );
    }

    #[test]
    fn idle_wait_honors_its_timeout_when_nothing_arrives() {
        let queue = RequestQueue::default();
        let mark = queue.idle_mark();

        let started = Instant::now();
        queue.wait_for_work(mark, Duration::from_millis(20));
        let elapsed = started.elapsed();

        assert!(
            elapsed >= Duration::from_millis(10),
            "wait returned before its timeout (elapsed {elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "wait overshot its timeout (elapsed {elapsed:?})"
        );
    }

    fn timed_task(uri: &str, queue_timeout: Duration, age: Duration) -> ClientTask {
        let mut task = task(uri);
        task.context.queue_timeout = Some(queue_timeout);
        task.queued_at = Instant::now() - age;
        task
    }

    #[test]
    fn next_for_worker_answers_queued_work_without_waiting() {
        let queue = RequestQueue::default();
        enqueue_openai_work(&queue, "/v1/chat/completions");

        let started = Instant::now();
        let task = queue.next_for_worker(
            "worker-a",
            WorkerBackend::OllamaLegacy,
            &["llama3".to_string()],
            Duration::from_secs(5),
        );

        assert!(task.is_some(), "queued work was not dispatched");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a poll with work available must not park first"
        );
    }

    #[test]
    fn next_for_worker_rechecks_and_takes_work_enqueued_during_the_window() {
        let queue = Arc::new(RequestQueue::default());
        let enqueuer = {
            let queue = Arc::clone(&queue);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                enqueue_openai_work(&queue, "/v1/chat/completions");
            })
        };

        let started = Instant::now();
        let task = queue.next_for_worker(
            "worker-a",
            WorkerBackend::OllamaLegacy,
            &["llama3".to_string()],
            Duration::from_secs(5),
        );
        let elapsed = started.elapsed();

        enqueuer.join().expect("enqueuer thread");
        let task = task.expect("work enqueued during the idle window must be dispatched");
        assert_eq!(task.request.uri, "/v1/chat/completions");
        assert!(
            elapsed < Duration::from_secs(2),
            "mid-window enqueue was not picked up early (waited {elapsed:?})"
        );
    }

    #[test]
    fn next_for_worker_gives_up_after_the_idle_window() {
        let queue = RequestQueue::default();

        let started = Instant::now();
        let task = queue.next_for_worker(
            "worker-a",
            WorkerBackend::OllamaLegacy,
            &["llama3".to_string()],
            Duration::from_millis(20),
        );
        let elapsed = started.elapsed();

        assert!(task.is_none(), "an idle poll must not invent work");
        assert!(
            elapsed >= Duration::from_millis(10),
            "idle poll skipped its re-check window (elapsed {elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "idle poll overshot its re-check window (elapsed {elapsed:?})"
        );
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
        use std::sync::atomic::{AtomicUsize, Ordering};

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
