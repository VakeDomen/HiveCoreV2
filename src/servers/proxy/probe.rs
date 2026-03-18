use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

use crate::app::AppState;
use crate::servers::proxy::models::client_task::ClientTask;
use crate::servers::proxy::models::response_target::ResponseTarget;
use crate::servers::proxy::models::worker_http_response::WorkerHttpResponse;
use crate::shared::http::HttpRequest;
use crate::shared::log;

pub fn probe_worker_json(
    state: &AppState,
    worker: &str,
    request: HttpRequest,
    timeout: Duration,
) -> Option<Value> {
    let cache_key = request.uri.clone();
    let response = match dispatch_capture(state, worker, request, timeout) {
        Some(response) => response,
        None => return cached_value(state, worker, &cache_key),
    };
    if response.status_code >= 400 {
        return cached_value(state, worker, &cache_key);
    }
    let value = serde_json::from_slice::<Value>(&response.body).ok()?;
    update_worker_cache(state, worker, &cache_key, &value);
    Some(value)
}

pub fn probe_worker_version(
    state: &AppState,
    worker: &str,
    timeout: Duration,
) -> Option<String> {
    let request = HttpRequest {
        method: "GET".to_string(),
        uri: "/api/version".to_string(),
        protocol: "HTTP/1.1".to_string(),
        headers: Default::default(),
        body: Vec::new(),
    };
    probe_worker_json(state, worker, request, timeout)?
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn dispatch_capture(
    state: &AppState,
    worker: &str,
    request: HttpRequest,
    timeout: Duration,
) -> Option<WorkerHttpResponse> {
    let (tx, rx) = mpsc::channel();
    let task = ClientTask::new(request, ResponseTarget::Capture(tx));
    if let Err(err) = state.request_queue.enqueue_to_node(worker.to_string(), task) {
        log::warn(format!("failed to enqueue probe to worker={worker}: {err}"));
        return None;
    }
    rx.recv_timeout(timeout).ok()
}

fn update_worker_cache(state: &AppState, worker: &str, uri: &str, value: &Value) {
    if let Ok(mut guard) = state.workers.write() {
        if let Some(status) = guard.get_mut(worker) {
            match uri {
                "/api/tags" => status.model_catalog = Some(value.clone()),
                "/api/ps" => status.running_models = Some(value.clone()),
                "/api/version" => status.version_payload = Some(value.clone()),
                _ => {}
            }
        }
    }
}

fn cached_value(state: &AppState, worker: &str, uri: &str) -> Option<Value> {
    state.workers.read().ok().and_then(|guard| {
        let status = guard.get(worker)?;
        match uri {
            "/api/tags" => status.model_catalog.clone(),
            "/api/ps" => status.running_models.clone(),
            "/api/version" => status.version_payload.clone(),
            _ => None,
        }
    })
}
