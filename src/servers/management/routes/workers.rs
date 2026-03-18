use crate::app::AppState;
use crate::shared::http::{HttpResponse, escape_json};

pub fn get_status(state: &AppState) -> HttpResponse {
    HttpResponse::json(200, "OK", render_workers(state))
}

pub fn get_connections(state: &AppState) -> HttpResponse {
    HttpResponse::json(200, "OK", render_workers(state))
}

pub fn get_pings(state: &AppState) -> HttpResponse {
    HttpResponse::json(200, "OK", render_worker_pings(state))
}

pub fn get_tags(state: &AppState) -> HttpResponse {
    HttpResponse::json(200, "OK", render_worker_tags(state))
}

pub fn get_versions(state: &AppState) -> HttpResponse {
    HttpResponse::json(200, "OK", render_worker_versions(state))
}

fn render_workers(state: &AppState) -> String {
    let entries = state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .map(|worker| {
                    format!(
                        "\"{}\":[\"{}\"]",
                        escape_json(&worker.name),
                        worker.state.as_str()
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    format!("{{{}}}", entries.join(","))
}

fn render_worker_pings(state: &AppState) -> String {
    let entries = state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .map(|worker| {
                    format!(
                        "\"{}\":{{\"last_ping_ms\":{},\"last_poll_ms\":{}}}",
                        escape_json(&worker.name),
                        worker.last_ping.elapsed().as_millis(),
                        worker.last_poll.elapsed().as_millis()
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    format!("{{{}}}", entries.join(","))
}

fn render_worker_tags(state: &AppState) -> String {
    let entries = state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .map(|worker| {
                    let tags = worker
                        .tags
                        .iter()
                        .map(|tag| format!("\"{}\"", escape_json(tag)))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("\"{}\":[{}]", escape_json(&worker.name), tags)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    format!("{{{}}}", entries.join(","))
}

fn render_worker_versions(state: &AppState) -> String {
    let entries = state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .map(|worker| {
                    format!(
                        "\"{}\":{{\"hive_version\":\"{}\",\"ollama_version\":\"{}\"}}",
                        escape_json(&worker.name),
                        escape_json(&worker.hive_version),
                        escape_json(&worker.ollama_version)
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    format!("{{{}}}", entries.join(","))
}
