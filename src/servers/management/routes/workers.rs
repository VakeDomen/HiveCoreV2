use serde_json::{Value, json};

use crate::app::AppState;
use crate::shared::http::HttpResponse;

pub fn get_status(state: &AppState) -> HttpResponse {
    json_response(render_workers(state))
}

pub fn get_connections(state: &AppState) -> HttpResponse {
    json_response(render_worker_connections(state))
}

pub fn get_pings(state: &AppState) -> HttpResponse {
    json_response(render_worker_pings(state))
}

pub fn get_tags(state: &AppState) -> HttpResponse {
    json_response(render_worker_tags(state))
}

pub fn get_versions(state: &AppState) -> HttpResponse {
    json_response(render_worker_versions(state))
}

fn render_workers(state: &AppState) -> Value {
    let entries = state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .map(|worker| {
                    (
                        worker.name.clone(),
                        json!({
                            "state": worker.state,
                            "backend": worker.backend.as_str(),
                            "states": worker.connection_states(),
                            "connections": worker.connections.len()
                        }),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!(
        entries
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
    )
}

fn render_worker_pings(state: &AppState) -> Value {
    let entries = state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .map(|worker| {
                    (
                        worker.name.clone(),
                        json!({
                            "last_ping_ms": worker.last_ping.elapsed().as_millis(),
                            "last_poll_ms": worker.last_poll.elapsed().as_millis()
                        }),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!(
        entries
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
    )
}

fn render_worker_connections(state: &AppState) -> Value {
    let entries = state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .map(|worker| {
                    let (
                        connections,
                        authenticating_connections,
                        polling_connections,
                        working_connections,
                    ) = worker.connection_counts();
                    (
                        worker.name.clone(),
                        json!({
                            "connections": connections,
                            "working_connections": working_connections,
                            "polling_connections": polling_connections,
                            "authenticating_connections": authenticating_connections,
                            "backend": worker.backend.as_str(),
                            "state": worker.state,
                            "states": worker.connection_states(),
                        }),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!(
        entries
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
    )
}

fn render_worker_tags(state: &AppState) -> Value {
    let entries = state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .map(|worker| (worker.name.clone(), json!(worker.tags)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!(
        entries
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
    )
}

fn render_worker_versions(state: &AppState) -> Value {
    let entries = state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .map(|worker| {
                    (
                        worker.name.clone(),
                        json!({
                            "hive_version": worker.hive_version,
                            "ollama_version": worker.ollama_version,
                            "backend": worker.backend.as_str()
                        }),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!(
        entries
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
    )
}

fn json_response(value: Value) -> HttpResponse {
    match serde_json::to_string(&value) {
        Ok(body) => HttpResponse::json(200, "OK", body),
        Err(_) => HttpResponse::new(500, "Internal Server Error", Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::time::Instant;

    use serde_json::Value;
    use uuid::Uuid;

    use crate::app::{AppState, Config};
    use crate::servers::worker::models::worker_backend::WorkerBackend;
    use crate::servers::worker::models::worker_phase::WorkerPhase;
    use crate::servers::worker::models::worker_status::{WorkerConnectionStatus, WorkerStatus};

    use super::render_worker_connections;

    fn temp_db_path(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hive_core_v2_workers_{test_name}_{}.db",
            Uuid::new_v4()
        ))
    }

    #[test]
    fn worker_connections_include_total_and_working_counts() -> io::Result<()> {
        let db_path = temp_db_path("connections");
        let (stats_tx, _stats_rx) = std::sync::mpsc::channel();
        let (capture_tx, _capture_rx) = std::sync::mpsc::channel();
        let state = AppState::new(
            Config {
                database_url: db_path.to_string_lossy().into_owned(),
                ..Config::default()
            },
            stats_tx,
            capture_tx,
        )?;

        state
            .workers
            .write()
            .map_err(|_| io::Error::other("worker registry poisoned"))?
            .insert(
                "worker-a".to_string(),
                WorkerStatus {
                    name: "worker-a".to_string(),
                    hive_version: "core".to_string(),
                    ollama_version: "backend".to_string(),
                    backend: WorkerBackend::Vllm,
                    tags: Vec::new(),
                    state: WorkerPhase::Working,
                    last_ping: Instant::now(),
                    last_poll: Instant::now(),
                    connections: vec![WorkerConnectionStatus {
                        index: 0,
                        nonce: "nonce".to_string(),
                        tags: Vec::new(),
                        state: WorkerPhase::Working,
                        last_ping: Instant::now(),
                        last_poll: Instant::now(),
                    }],
                    next_connection_index: 1,
                    model_catalog: None,
                    running_models: None,
                    version_payload: None,
                },
            );

        let rendered = render_worker_connections(&state);
        let worker = rendered
            .get("worker-a")
            .and_then(Value::as_object)
            .expect("worker entry");
        assert_eq!(worker.get("connections").and_then(Value::as_u64), Some(1));
        assert_eq!(
            worker.get("working_connections").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(worker.get("backend").and_then(Value::as_str), Some("vllm"));

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }
}
