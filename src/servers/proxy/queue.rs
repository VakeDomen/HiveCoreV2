use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::servers::proxy::models::client_task::ClientTask;
use crate::servers::proxy::models::queue_snapshot::QueueSnapshot;
use crate::shared::log;

#[derive(Default)]
pub struct RequestQueue {
    model_queue: Mutex<HashMap<String, VecDeque<ClientTask>>>,
    node_queue: Mutex<HashMap<String, VecDeque<ClientTask>>>,
}

impl RequestQueue {
    pub fn enqueue_model(&self, model: String, task: ClientTask) -> Result<(), &'static str> {
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

    pub fn enqueue_node(&self, node: String, task: ClientTask) -> Result<(), &'static str> {
        let mut guard = self.node_queue.lock().map_err(|_| "node queue poisoned")?;
        let method = task.request.method.clone();
        let uri = task.request.uri.clone();
        let node_name = node.clone();
        guard.entry(node).or_default().push_back(task);
        log::info(format!(
            "queued targeted request worker={} method={} uri={}",
            node_name, method, uri
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
