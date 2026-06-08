use serde_json::{json, Value};

use crate::app::AppState;
use crate::shared::http::HttpResponse;

pub fn get_status(state: &AppState) -> HttpResponse {
    json_response(render_workers(state))
}

pub fn get_connections(state: &AppState) -> HttpResponse {
    json_response(render_workers(state))
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
                            "backend": worker.backend.as_str()
                        }),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!(entries
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>())
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
    json!(entries
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>())
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
    json!(entries
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>())
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
    json!(entries
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>())
}

fn json_response(value: Value) -> HttpResponse {
    match serde_json::to_string(&value) {
        Ok(body) => HttpResponse::json(200, "OK", body),
        Err(_) => HttpResponse::new(500, "Internal Server Error", Vec::new()),
    }
}
