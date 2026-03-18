use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::TcpStream;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::auth::{KeyStore, Role};
use crate::config::Config;
use crate::http::{HttpRequest, HttpResponse, extract_json_value};
use crate::log;

pub struct AppState {
    pub config: Config,
    pub keys: KeyStore,
    pub request_queue: RequestQueue,
    pub workers: RwLock<HashMap<String, WorkerStatus>>,
}

impl AppState {
    pub fn new(config: Config) -> io::Result<Self> {
        Ok(Self {
            keys: KeyStore::new(&config.database_url)?,
            config,
            request_queue: RequestQueue::default(),
            workers: RwLock::new(HashMap::new()),
        })
    }
}

pub struct ClientTask {
    pub request: HttpRequest,
    pub client_stream: Option<TcpStream>,
}

#[derive(Default)]
pub struct RequestQueue {
    model_queue: Mutex<HashMap<String, VecDeque<ClientTask>>>,
    node_queue: Mutex<HashMap<String, VecDeque<ClientTask>>>,
}

impl RequestQueue {
    pub fn enqueue(&self, task: ClientTask) -> Result<(), &'static str> {
        if task.request.protocol == "HIVE" {
            return Err("HIVE requests are not accepted on the proxy listener");
        }

        if let Some(node) = task.request.header("node").map(str::to_string) {
            let mut guard = self.node_queue.lock().map_err(|_| "node queue poisoned")?;
            let method = task.request.method.clone();
            let uri = task.request.uri.clone();
            let node_name = node.clone();
            guard.entry(node).or_default().push_back(task);
            log::info(format!(
                "queued targeted request worker={} method={} uri={}",
                node_name, method, uri
            ));
            return Ok(());
        }

        let Some(model) = extract_json_value(&task.request.body, "model") else {
            return Err("request body is missing the model field");
        };

        let mut guard = self
            .model_queue
            .lock()
            .map_err(|_| "model queue poisoned")?;
        let queued_model = model.clone();
        let method = task.request.method.clone();
        let uri = task.request.uri.clone();
        guard.entry(model).or_default().push_back(task);
        log::info(format!(
            "queued model request model={} method={} uri={}",
            queued_model, method, uri
        ));
        Ok(())
    }

    pub fn dequeue_for_worker(&self, worker_name: &str, tags: &[String]) -> Option<ClientTask> {
        if let Ok(mut node_guard) = self.node_queue.lock() {
            if let Some(queue) = node_guard.get_mut(worker_name) {
                if let Some(task) = queue.pop_front() {
                    log::info(format!("dispatching targeted task to worker={worker_name}"));
                    return Some(task);
                }
            }
        }

        let Ok(mut model_guard) = self.model_queue.lock() else {
            return None;
        };
        for tag in tags {
            if let Some(queue) = model_guard.get_mut(tag) {
                if let Some(task) = queue.pop_front() {
                    log::info(format!(
                        "dispatching model task to worker={} model={}",
                        worker_name, tag
                    ));
                    return Some(task);
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
        let mut guard = self.node_queue.lock().map_err(|_| "node queue poisoned")?;
        let worker = worker_name.clone();
        guard.entry(worker_name).or_default().push_back(task);
        log::info(format!("queued worker command target={worker}"));
        Ok(())
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        let model_queue = self
            .model_queue
            .lock()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(k, v)| (k.clone(), v.len()))
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
}

pub struct QueueSnapshot {
    pub model_queue: HashMap<String, usize>,
    pub node_queue: HashMap<String, usize>,
}

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

pub fn authorize_admin(state: &Arc<AppState>, request: &HttpRequest) -> bool {
    request
        .bearer_token()
        .and_then(|token| state.keys.verify(token, &[Role::Admin]))
        .is_some()
}

pub fn reject_request(mut stream: TcpStream, status: u16, reason: &'static str) -> io::Result<()> {
    HttpResponse::new(status, reason, Vec::new()).write_to(&mut stream)
}

pub fn run_overseer(state: Arc<AppState>) -> io::Result<()> {
    loop {
        thread::sleep(Duration::from_millis(500));
        let now = Instant::now();
        let polling_timeout = Duration::from_secs(state.config.polling_node_connection_timeout);
        let working_timeout = Duration::from_secs(state.config.working_node_connection_timeout);

        let stale_workers = state
            .workers
            .read()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .filter_map(|(name, worker)| {
                        let timeout = match worker.state {
                            WorkerPhase::Working => working_timeout,
                            _ => polling_timeout,
                        };
                        (now.duration_since(worker.last_ping) > timeout
                            && now.duration_since(worker.last_poll) > timeout)
                            .then(|| name.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if stale_workers.is_empty() {
            continue;
        }

        if let Ok(mut guard) = state.workers.write() {
            for name in stale_workers {
                guard.remove(&name);
                log::warn(format!("removed stale worker={name}"));
            }
        }
    }
}
