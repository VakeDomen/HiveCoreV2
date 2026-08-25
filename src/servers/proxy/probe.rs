use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

use crate::app::AppState;
use crate::servers::proxy::models::client_task::{ClientTask, RequestContext};
use crate::servers::proxy::models::response_target::ResponseTarget;
use crate::servers::proxy::models::worker_http_response::WorkerHttpResponse;
use crate::servers::worker::models::worker_phase::WorkerPhase;
use crate::shared::http::HttpRequest;
use crate::shared::log;

pub fn probe_worker_json(
    state: &AppState,
    worker: &str,
    request: HttpRequest,
    timeout: Duration,
) -> Option<Value> {
    let cache_key = request.uri.clone();
    if !worker_has_polling_connection(state, worker) {
        return cached_value(state, worker, &cache_key);
    }

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

pub fn probe_worker_version(state: &AppState, worker: &str, timeout: Duration) -> Option<String> {
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
    let task = ClientTask::new(
        request,
        ResponseTarget::Capture(tx),
        RequestContext {
            queue_timeout: Some(timeout),
            ..RequestContext::default()
        },
    );
    if let Err(err) = state
        .request_queue
        .enqueue_to_node(worker.to_string(), task)
    {
        log::warn(format!("failed to enqueue probe to worker={worker}: {err}"));
        return None;
    }
    rx.recv_timeout(timeout).ok()
}

fn worker_has_polling_connection(state: &AppState, worker: &str) -> bool {
    state
        .workers
        .read()
        .ok()
        .and_then(|guard| {
            guard.get(worker).map(|status| {
                status
                    .connections
                    .iter()
                    .any(|connection| matches!(connection.state, WorkerPhase::Polling))
            })
        })
        .unwrap_or(false)
}

fn update_worker_cache(state: &AppState, worker: &str, uri: &str, value: &Value) {
    if let Ok(mut guard) = state.workers.write() {
        if let Some(status) = guard.get_mut(worker) {
            match uri {
                "/api/tags" => status.model_catalog = Some(value.clone()),
                "/v1/models" => status.model_catalog = Some(value.clone()),
                "/api/ps" => status.running_models = Some(value.clone()),
                "/api/version" => status.version_payload = Some(value.clone()),
                "/version" => status.version_payload = Some(value.clone()),
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
            "/v1/models" => status.model_catalog.clone(),
            "/api/ps" => status.running_models.clone(),
            "/api/version" => status.version_payload.clone(),
            "/version" => status.version_payload.clone(),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::RwLock;
    use std::time::{Duration, Instant};

    use serde_json::json;
    use uuid::Uuid;

    use crate::app::{AppState, Config};
    use crate::servers::worker::models::worker_backend::WorkerBackend;
    use crate::servers::worker::models::worker_phase::WorkerPhase;
    use crate::servers::worker::models::worker_status::{WorkerConnectionStatus, WorkerStatus};
    use crate::shared::http::HttpRequest;

    use super::probe_worker_json;

    fn temp_db_path(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hive_core_v2_probe_{test_name}_{}.db",
            Uuid::new_v4()
        ))
    }

    fn test_state(test_name: &str) -> io::Result<(AppState, PathBuf)> {
        let db_path = temp_db_path(test_name);
        let config = Config {
            database_url: db_path.to_string_lossy().into_owned(),
            proxy_timeout_ms: 5,
            ..Config::default()
        };
        let (stats_tx, _stats_rx) = std::sync::mpsc::channel();
        let (capture_tx, _capture_rx) = std::sync::mpsc::channel();
        Ok((AppState::new(config, stats_tx, capture_tx)?, db_path))
    }

    fn worker_status(
        name: &str,
        phase: WorkerPhase,
        model_catalog: Option<serde_json::Value>,
    ) -> WorkerStatus {
        let now = Instant::now();
        WorkerStatus {
            name: name.to_string(),
            hive_version: "0.1.0".to_string(),
            ollama_version: "0.1.0".to_string(),
            backend: WorkerBackend::OllamaLegacy,
            tags: vec!["llama3".to_string()],
            state: phase,
            last_ping: now,
            last_poll: now,
            connections: vec![WorkerConnectionStatus {
                index: 0,
                nonce: "nonce".to_string(),
                tags: vec!["llama3".to_string()],
                state: phase,
                last_ping: now,
                last_poll: now,
            }],
            next_connection_index: 1,
            model_catalog,
            running_models: None,
            version_payload: None,
        }
    }

    fn request(uri: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".to_string(),
            uri: uri.to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
        }
    }

    fn cleanup(path: &PathBuf) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn busy_worker_probe_returns_cached_catalog_without_queueing() -> io::Result<()> {
        let (mut state, db) = test_state("busy_cached")?;
        let cached = json!({"models":[{"name":"llama3"}]});
        state.workers = RwLock::new(HashMap::from([(
            "worker-a".to_string(),
            worker_status("worker-a", WorkerPhase::Working, Some(cached.clone())),
        )]));

        let value = probe_worker_json(
            &state,
            "worker-a",
            request("/api/tags"),
            Duration::from_millis(5),
        );

        assert_eq!(value, Some(cached));
        assert!(state.request_queue.snapshot().node_queue.is_empty());
        cleanup(&db);
        Ok(())
    }

    #[test]
    fn busy_worker_probe_without_cache_does_not_queue() -> io::Result<()> {
        let (mut state, db) = test_state("busy_uncached")?;
        state.workers = RwLock::new(HashMap::from([(
            "worker-a".to_string(),
            worker_status("worker-a", WorkerPhase::Working, None),
        )]));

        let value = probe_worker_json(
            &state,
            "worker-a",
            request("/api/tags"),
            Duration::from_millis(5),
        );

        assert_eq!(value, None);
        assert!(state.request_queue.snapshot().node_queue.is_empty());
        cleanup(&db);
        Ok(())
    }
}
