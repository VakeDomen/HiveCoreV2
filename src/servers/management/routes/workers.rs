use std::time::Duration;

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
    let stale_after = Duration::from_secs(state.config.latency_report_timeout);
    let entries = state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .map(|worker| {
                    // Age of the last latency report in ms; null when the node
                    // never reported a latency (no opt-in LATENCY support).
                    let latency_age_ms = worker
                        .latency_reported_at
                        .map(|reported_at| reported_at.elapsed().as_millis());
                    let latency_stale = latency_age_ms
                        .map(|age_ms| Duration::from_millis(age_ms as u64) > stale_after)
                        .unwrap_or(false);
                    (
                        worker.name.clone(),
                        json!({
                            "last_ping_ms": worker.last_ping.elapsed().as_millis(),
                            "last_poll_ms": worker.last_poll.elapsed().as_millis(),
                            // Node-reported core<->node round-trip in ms.
                            // null when the node does not support the opt-in latency echo.
                            "core_to_node_rtt_latency": worker.core_to_node_rtt_latency,
                            // Node-reported node->backend round-trip in ms.
                            // null when the node reports only the core<->node leg.
                            "node_to_backend_rtt_latency": worker.node_to_backend_rtt_latency,
                            // How long ago the latency was reported, in ms.
                            "latency_age_ms": latency_age_ms,
                            // True when the latency report is older than LATENCY_REPORT_TIMEOUT.
                            "latency_stale": latency_stale,
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
                            "backend_version": worker.backend_version,
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

    use super::{render_worker_connections, render_worker_pings};

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
                    backend_version: "backend".to_string(),
                    backend: WorkerBackend::Vllm,
                    tags: Vec::new(),
                    state: WorkerPhase::Working,
                    last_ping: Instant::now(),
                    last_poll: Instant::now(),
                    core_to_node_rtt_latency: Some(12),
                    node_to_backend_rtt_latency: Some(3),
                    latency_reported_at: Some(Instant::now()),
                    connections: vec![WorkerConnectionStatus {
                        index: 0,
                        nonce: "nonce".to_string(),
                        tags: Vec::new(),
                        state: WorkerPhase::Working,
                        last_ping: Instant::now(),
                        last_poll: Instant::now(),
                        core_to_node_rtt_latency: Some(12),
                        node_to_backend_rtt_latency: Some(3),
                        latency_reported_at: Some(Instant::now()),
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

    #[test]
    fn pings_report_latency_age_and_staleness() -> io::Result<()> {
        let db_path = temp_db_path("pings_stale");
        let (stats_tx, _stats_rx) = std::sync::mpsc::channel();
        let (capture_tx, _capture_rx) = std::sync::mpsc::channel();
        let state = AppState::new(
            Config {
                database_url: db_path.to_string_lossy().into_owned(),
                latency_report_timeout: 1, // 1s stale threshold
                ..Config::default()
            },
            stats_tx,
            capture_tx,
        )?;

        let reported_far_past = Instant::now() - std::time::Duration::from_secs(60);
        state
            .workers
            .write()
            .map_err(|_| io::Error::other("worker registry poisoned"))?
            .insert(
                "worker-a".to_string(),
                WorkerStatus {
                    name: "worker-a".to_string(),
                    hive_version: "core".to_string(),
                    backend_version: "backend".to_string(),
                    backend: WorkerBackend::Ollama,
                    tags: Vec::new(),
                    state: WorkerPhase::Polling,
                    last_ping: Instant::now(),
                    last_poll: Instant::now(),
                    core_to_node_rtt_latency: Some(12),
                    node_to_backend_rtt_latency: Some(4),
                    latency_reported_at: Some(reported_far_past),
                    connections: vec![],
                    next_connection_index: 1,
                    model_catalog: None,
                    running_models: None,
                    version_payload: None,
                },
            );

        let rendered = render_worker_pings(&state);
        let worker = rendered
            .get("worker-a")
            .and_then(Value::as_object)
            .expect("worker entry");

        assert_eq!(
            worker.get("core_to_node_rtt_latency").and_then(Value::as_u64),
            Some(12)
        );
        assert_eq!(
            worker.get("node_to_backend_rtt_latency").and_then(Value::as_u64),
            Some(4)
        );
        let age_ms = worker
            .get("latency_age_ms")
            .and_then(Value::as_u64)
            .expect("latency_age_ms should be present");
        assert!(age_ms >= 1000, "age should be >= the 1s stale threshold, got {age_ms}ms");
        assert_eq!(worker.get("latency_stale").and_then(Value::as_bool), Some(true));

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }
}
