use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::app::AppState;
use crate::auth::KeyRecord;
use crate::shared::http::{HttpRequest, HttpResponse};
use crate::shared::log;

use super::models::route_plan::RoutePlan;
use super::probe::{probe_worker_json, probe_worker_version};

enum ProxyEndpoint {
    Generate,
    Chat,
    Embed,
    Tags,
    Ps,
    Show,
    Create,
    Copy,
    Pull,
    Push,
    Delete,
    Version,

    OpenAiChatCompletions,
    OpenAiCompletions,
    OpenAiEmbeddings,
    OpenAiModels,
    OpenAiModel { model: String },

    // LoadModel { model: String },
    // UnloadModel { model: String },
    Health,
    Unknown,
}

pub fn plan_request(
    state: &AppState,
    request: &HttpRequest,
    visible_key: Option<&KeyRecord>,
) -> RoutePlan {
    if request.protocol == "HIVE" {
        return RoutePlan::Reject {
            status: 405,
            reason: "Method Not Allowed",
            message: "HIVE requests are not accepted on the proxy listener",
        };
    }

    if let Some(worker) = request.header("node").map(str::to_string) {
        return RoutePlan::QueueByNode(worker);
    }

    match classify_endpoint(request) {
        ProxyEndpoint::Generate | ProxyEndpoint::Chat | ProxyEndpoint::Embed => {
            route_by_required_model(request)
        }

        ProxyEndpoint::OpenAiChatCompletions => route_openai_chat_request(request),
        ProxyEndpoint::OpenAiCompletions => route_openai_completion_request(request),
        ProxyEndpoint::OpenAiEmbeddings => route_openai_embedding_request(request),
        ProxyEndpoint::OpenAiModels => local_openai_models_response(state, visible_key),
        ProxyEndpoint::OpenAiModel { model } => {
            local_openai_model_response(state, visible_key, &model)
        }

        // ProxyEndpoint::LoadModel { model } => route_load_model_request(state, visible_key, &model),
        // ProxyEndpoint::UnloadModel { model } => {
        //     route_unload_model_request(state, visible_key, &model)
        // }
        ProxyEndpoint::Tags => local_tags_response(state, visible_key),
        ProxyEndpoint::Ps => local_ps_response(state, visible_key),
        ProxyEndpoint::Version => local_version_response(state),
        ProxyEndpoint::Health => local_health_response(state),
        ProxyEndpoint::Show => route_show_request(state, request, visible_key),
        ProxyEndpoint::Create => route_create_request(state, request, visible_key),
        ProxyEndpoint::Copy => route_copy_request(state, request, visible_key),
        ProxyEndpoint::Pull => route_pull_request(state),
        ProxyEndpoint::Push => route_push_request(state, request, visible_key),
        ProxyEndpoint::Delete => route_delete_request(state, request, visible_key),

        ProxyEndpoint::Unknown => RoutePlan::Reject {
            status: 404,
            reason: "Not Found",
            message: "",
        },
    }
}

fn classify_endpoint(request: &HttpRequest) -> ProxyEndpoint {
    let method = request.method.as_str();
    let uri = request.uri.as_str();

    match (method, uri) {
        ("POST", "/api/generate") => return ProxyEndpoint::Generate,
        ("POST", "/api/chat") => return ProxyEndpoint::Chat,
        ("POST", "/api/embed") | ("POST", "/api/embeddings") => return ProxyEndpoint::Embed,
        ("GET", "/api/tags") => return ProxyEndpoint::Tags,
        ("GET", "/api/ps") => return ProxyEndpoint::Ps,
        ("POST", "/api/show") => return ProxyEndpoint::Show,
        ("POST", "/api/create") => return ProxyEndpoint::Create,
        ("POST", "/api/copy") => return ProxyEndpoint::Copy,
        ("POST", "/api/pull") => return ProxyEndpoint::Pull,
        ("POST", "/api/push") => return ProxyEndpoint::Push,
        ("DELETE", "/api/delete") => return ProxyEndpoint::Delete,
        ("GET", "/api/version") => return ProxyEndpoint::Version,
        ("GET", "/health") => return ProxyEndpoint::Health,

        ("POST", "/v1/chat/completions") => return ProxyEndpoint::OpenAiChatCompletions,
        ("POST", "/v1/completions") => return ProxyEndpoint::OpenAiCompletions,
        ("POST", "/v1/embeddings") => return ProxyEndpoint::OpenAiEmbeddings,
        ("GET", "/v1/models") => return ProxyEndpoint::OpenAiModels,
        _ => {}
    }

    if method == "GET" {
        if let Some(model) = strip_prefix_segment(uri, "/v1/models/") {
            return ProxyEndpoint::OpenAiModel { model };
        }
    }

    // if method == "POST" {
    //     if let Some(model) = strip_suffix_segment(uri, "/api/models/", "/load") {
    //         return ProxyEndpoint::LoadModel { model };
    //     }
    //     if let Some(model) = strip_suffix_segment(uri, "/api/models/", "/unload") {
    //         return ProxyEndpoint::UnloadModel { model };
    //     }
    // }

    ProxyEndpoint::Unknown
}

fn route_openai_chat_request(request: &HttpRequest) -> RoutePlan {
    match json_string_field(&request.body, "model") {
        Some(model) => RoutePlan::QueueByModel(model),
        None => missing_field("model"),
    }
}

fn route_openai_completion_request(request: &HttpRequest) -> RoutePlan {
    match json_string_field(&request.body, "model") {
        Some(model) => RoutePlan::QueueByModel(model),
        None => missing_field("model"),
    }
}

fn route_openai_embedding_request(request: &HttpRequest) -> RoutePlan {
    match json_string_field(&request.body, "model") {
        Some(model) => RoutePlan::QueueByModel(model),
        None => missing_field("model"),
    }
}

fn route_load_model_request(
    state: &AppState,
    visible_key: Option<&KeyRecord>,
    model: &str,
) -> RoutePlan {
    if !model_visible(visible_key, model) {
        return not_found_for_masked_model();
    }

    let owners = model_owners(state, model);
    if !owners.is_empty() {
        return RoutePlan::QueueByNode(owners[0].clone());
    }

    let workers = connected_workers(state);
    if workers.is_empty() {
        return no_workers_available();
    }

    RoutePlan::QueueByNode(workers[0].clone())
}

fn route_unload_model_request(
    state: &AppState,
    visible_key: Option<&KeyRecord>,
    model: &str,
) -> RoutePlan {
    if !model_visible(visible_key, model) {
        return not_found_for_masked_model();
    }

    let owners = model_owners(state, model);
    if owners.is_empty() {
        return no_workers_available();
    }

    RoutePlan::QueueByNode(owners[0].clone())
}

fn local_health_response(state: &AppState) -> RoutePlan {
    let workers = connected_workers(state);
    RoutePlan::Local(json_response(json!({
        "status": "ok",
        "workers": {
            "connected": workers.len()
        },
        "queue": {
            "healthy": true
        },
        "kv_cache": {
            "supported": false
        }
    })))
}

fn strip_prefix_segment(uri: &str, prefix: &str) -> Option<String> {
    let rest = uri.strip_prefix(prefix)?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    Some(rest.to_string())
}

fn strip_suffix_segment(uri: &str, prefix: &str, suffix: &str) -> Option<String> {
    let rest = uri.strip_prefix(prefix)?;
    let model = rest.strip_suffix(suffix)?;
    if model.is_empty() || model.contains('/') {
        return None;
    }
    Some(model.to_string())
}

fn local_openai_models_response(state: &AppState, visible_key: Option<&KeyRecord>) -> RoutePlan {
    let workers = connected_workers(state);
    if workers.is_empty() {
        return RoutePlan::Local(json_response(json!({
            "object": "list",
            "data": []
        })));
    }

    let mut by_name = BTreeMap::new();
    for value in parallel_probe_json(state, workers, "/api/tags", None) {
        if let Some(models) = value.get("models").and_then(Value::as_array) {
            for model in models {
                let Some(name) = model.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if !model_visible(visible_key, name) {
                    continue;
                }

                by_name.entry(name.to_string()).or_insert_with(|| {
                    json!({
                        "id": name,
                        "object": "model",
                        "created": 0,
                        "owned_by": "ollama"
                    })
                });
            }
        }
    }

    RoutePlan::Local(json_response(json!({
        "object": "list",
        "data": by_name.into_values().collect::<Vec<_>>()
    })))
}

fn local_openai_model_response(
    state: &AppState,
    visible_key: Option<&KeyRecord>,
    model: &str,
) -> RoutePlan {
    if !model_visible(visible_key, model) {
        return not_found_for_masked_model();
    }

    let owners = model_owners(state, model);
    if owners.is_empty() {
        return RoutePlan::Reject {
            status: 404,
            reason: "Not Found",
            message: "model not found",
        };
    }

    RoutePlan::Local(json_response(json!({
        "id": model,
        "object": "model",
        "created": 0,
        "owned_by": "ollama"
    })))
}

fn route_by_required_model(request: &HttpRequest) -> RoutePlan {
    match json_string_field(&request.body, "model") {
        Some(model) => RoutePlan::QueueByModel(model),
        None => missing_field("model"),
    }
}

fn route_show_request(
    state: &AppState,
    request: &HttpRequest,
    visible_key: Option<&KeyRecord>,
) -> RoutePlan {
    let Some(model) = json_string_field(&request.body, "model") else {
        return missing_field("model");
    };
    if !model_visible(visible_key, &model) {
        return not_found_for_masked_model();
    }
    route_to_owner(state, &model)
}

fn route_create_request(
    state: &AppState,
    request: &HttpRequest,
    visible_key: Option<&KeyRecord>,
) -> RoutePlan {
    let Some(from) = json_string_field(&request.body, "from") else {
        return RoutePlan::Reject {
            status: 400,
            reason: "Bad Request",
            message: "create without a source model requires a Node header",
        };
    };
    if !model_visible(visible_key, &from) {
        return not_found_for_masked_model();
    }
    route_to_owner(state, &from)
}

fn route_copy_request(
    state: &AppState,
    request: &HttpRequest,
    visible_key: Option<&KeyRecord>,
) -> RoutePlan {
    let Some(source) = json_string_field(&request.body, "source") else {
        return missing_field("source");
    };
    if !model_visible(visible_key, &source) {
        return not_found_for_masked_model();
    }
    route_to_owner(state, &source)
}

fn route_pull_request(state: &AppState) -> RoutePlan {
    let workers = connected_workers(state);
    if workers.is_empty() {
        return no_workers_available();
    }
    RoutePlan::QueueByNode(workers[0].clone())
}

fn route_push_request(
    state: &AppState,
    request: &HttpRequest,
    visible_key: Option<&KeyRecord>,
) -> RoutePlan {
    let Some(model) = json_string_field(&request.body, "model") else {
        return missing_field("model");
    };
    if !model_visible(visible_key, &model) {
        return not_found_for_masked_model();
    }
    route_to_owner(state, &model)
}

fn route_delete_request(
    state: &AppState,
    request: &HttpRequest,
    visible_key: Option<&KeyRecord>,
) -> RoutePlan {
    let Some(model) = json_string_field(&request.body, "model") else {
        return missing_field("model");
    };
    if !model_visible(visible_key, &model) {
        return not_found_for_masked_model();
    }
    let owners = model_owners(state, &model);
    if owners.is_empty() {
        return no_workers_available();
    }
    RoutePlan::QueueByNode(owners[0].clone())
}

fn route_to_owner(state: &AppState, model: &str) -> RoutePlan {
    let owners = model_owners(state, model);
    if owners.is_empty() {
        return RoutePlan::Reject {
            status: 404,
            reason: "Not Found",
            message: "no connected worker advertises the requested model",
        };
    }
    RoutePlan::QueueByNode(owners[0].clone())
}

fn local_tags_response(state: &AppState, visible_key: Option<&KeyRecord>) -> RoutePlan {
    let workers = connected_workers(state);
    if workers.is_empty() {
        return RoutePlan::Local(json_response(json!({ "models": [] })));
    }

    let mut by_name = BTreeMap::new();
    for value in parallel_probe_json(state, workers, "/api/tags", None) {
        if let Some(models) = value.get("models").and_then(Value::as_array) {
            for model in models {
                if let Some(name) = model.get("name").and_then(Value::as_str) {
                    if !model_visible(visible_key, name) {
                        continue;
                    }
                    by_name
                        .entry(name.to_string())
                        .or_insert_with(|| model.clone());
                }
            }
        }
    }

    RoutePlan::Local(json_response(json!({
        "models": by_name.into_values().collect::<Vec<_>>()
    })))
}

fn local_ps_response(state: &AppState, visible_key: Option<&KeyRecord>) -> RoutePlan {
    let workers = connected_workers(state);
    if workers.is_empty() {
        return RoutePlan::Local(json_response(json!({ "models": [] })));
    }

    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for value in parallel_probe_json(state, workers, "/api/ps", None) {
        if let Some(entries) = value.get("models").and_then(Value::as_array) {
            for model in entries {
                let Some(name) = model.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if !model_visible(visible_key, name) {
                    continue;
                }
                let key = model
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| model.to_string());
                if seen.insert(key) {
                    models.push(model.clone());
                }
            }
        }
    }

    RoutePlan::Local(json_response(json!({ "models": models })))
}

fn local_version_response(state: &AppState) -> RoutePlan {
    let workers = connected_workers(state);
    if workers.is_empty() {
        return RoutePlan::Local(json_response(json!({ "version": "unknown" })));
    }

    let mut version = None;
    for value in parallel_probe_versions(state, workers) {
        if !value.is_empty() {
            version = Some(value);
            break;
        }
    }

    RoutePlan::Local(json_response(json!({
        "version": version.unwrap_or_else(|| "unknown".to_string())
    })))
}

fn connected_workers(state: &AppState) -> Vec<String> {
    state
        .workers
        .read()
        .ok()
        .map(|guard| guard.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
}

fn model_owners(state: &AppState, model: &str) -> Vec<String> {
    state
        .workers
        .read()
        .ok()
        .map(|guard| {
            guard
                .values()
                .filter(|worker| worker.tags.iter().any(|tag| tag == model))
                .map(|worker| worker.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn missing_field(field: &'static str) -> RoutePlan {
    let message = match field {
        _ => "request body is missing a required field",
    };
    RoutePlan::Reject {
        status: 400,
        reason: "Bad Request",
        message,
    }
}

fn no_workers_available() -> RoutePlan {
    RoutePlan::Reject {
        status: 503,
        reason: "Service Unavailable",
        message: "no connected workers are available",
    }
}

fn not_found_for_masked_model() -> RoutePlan {
    RoutePlan::Reject {
        status: 404,
        reason: "Not Found",
        message: "no connected worker advertises the requested model",
    }
}

fn model_visible(key: Option<&KeyRecord>, model: &str) -> bool {
    key.map(|key| key.allows_model(model)).unwrap_or(true)
}

fn json_string_field(body: &[u8], field: &str) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get(field).and_then(Value::as_str).map(str::to_string))
}

fn json_response(value: Value) -> HttpResponse {
    match serde_json::to_string(&value) {
        Ok(body) => HttpResponse::json(200, "OK", body),
        Err(err) => {
            log::error(format!(
                "failed to serialize proxy aggregate response: {err}"
            ));
            HttpResponse::new(500, "Internal Server Error", Vec::new())
        }
    }
}

fn parallel_probe_json(
    state: &AppState,
    workers: Vec<String>,
    uri: &'static str,
    body: Option<Vec<u8>>,
) -> Vec<Value> {
    let timeout = Duration::from_millis(state.config.proxy_timeout_ms);
    let (tx, rx) = mpsc::channel();

    thread::scope(|scope| {
        for worker in workers {
            let tx = tx.clone();
            let body = body.clone();
            scope.spawn(move || {
                let request = HttpRequest {
                    method: if body.is_some() { "POST" } else { "GET" }.to_string(),
                    uri: uri.to_string(),
                    protocol: "HTTP/1.1".to_string(),
                    headers: Default::default(),
                    body: body.unwrap_or_default(),
                };
                let _ = tx.send(probe_worker_json(state, &worker, request, timeout));
            });
        }
    });
    drop(tx);

    rx.into_iter().flatten().collect()
}

fn parallel_probe_versions(state: &AppState, workers: Vec<String>) -> Vec<String> {
    let timeout = Duration::from_millis(state.config.proxy_timeout_ms);
    let (tx, rx) = mpsc::channel();

    thread::scope(|scope| {
        for worker in workers {
            let tx = tx.clone();
            scope.spawn(move || {
                let _ = tx.send(probe_worker_version(state, &worker, timeout));
            });
        }
    });
    drop(tx);

    rx.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::RwLock;
    use std::time::Instant;

    use serde_json::{Value, json};
    use uuid::Uuid;

    use crate::app::{AppState, Config};
    use crate::servers::proxy::models::route_plan::RoutePlan;
    use crate::servers::worker::models::worker_phase::WorkerPhase;
    use crate::servers::worker::models::worker_status::WorkerStatus;
    use crate::shared::http::HttpRequest;

    use super::plan_request;

    fn temp_db_path(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hive_core_v2_planner_{test_name}_{}.db",
            Uuid::new_v4()
        ))
    }

    fn request(method: &str, uri: &str, body: &[u8]) -> HttpRequest {
        HttpRequest {
            method: method.to_string(),
            uri: uri.to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers: HashMap::new(),
            body: body.to_vec(),
        }
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

    fn worker_status(name: &str, tags: Vec<&str>) -> WorkerStatus {
        WorkerStatus {
            name: name.to_string(),
            nonce: "nonce".to_string(),
            hive_version: "0.1.0".to_string(),
            ollama_version: "0.1.0".to_string(),
            tags: tags.into_iter().map(str::to_string).collect(),
            state: WorkerPhase::Polling,
            last_ping: Instant::now(),
            last_poll: Instant::now(),
            model_catalog: Some(json!({"models":[{"name":"llama3"}]})),
            running_models: Some(json!({"models":[{"name":"llama3"}]})),
            version_payload: Some(json!({"version":"0.1.0"})),
        }
    }

    fn cleanup(path: &PathBuf) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn generate_routes_by_model() -> io::Result<()> {
        let (state, db) = test_state("generate")?;
        let req = request("POST", "/api/generate", br#"{"model":"llama3"}"#);
        match plan_request(&state, &req, None) {
            RoutePlan::QueueByModel(model) => assert_eq!(model, "llama3"),
            _ => panic!("expected model route"),
        }
        cleanup(&db);
        Ok(())
    }

    #[test]
    fn tags_is_local_aggregate() -> io::Result<()> {
        let (mut state, db) = test_state("tags")?;
        let mut workers = HashMap::new();
        workers.insert(
            "worker-a".to_string(),
            worker_status("worker-a", vec!["llama3"]),
        );
        state.workers = RwLock::new(workers);
        let req = request("GET", "/api/tags", b"");
        match plan_request(&state, &req, None) {
            RoutePlan::Local(response) => assert_eq!(response.status_code, 200),
            _ => panic!("expected local aggregate"),
        }
        cleanup(&db);
        Ok(())
    }

    #[test]
    fn copy_routes_by_source_owner() -> io::Result<()> {
        let (mut state, db) = test_state("copy")?;
        let mut workers = HashMap::new();
        workers.insert(
            "worker-a".to_string(),
            worker_status("worker-a", vec!["llama3"]),
        );
        state.workers = RwLock::new(workers);
        let req = request(
            "POST",
            "/api/copy",
            br#"{"source":"llama3","destination":"copy"}"#,
        );
        match plan_request(&state, &req, None) {
            RoutePlan::QueueByNode(worker) => assert_eq!(worker, "worker-a"),
            _ => panic!("expected node route"),
        }
        cleanup(&db);
        Ok(())
    }

    #[test]
    fn pull_targets_single_connected_worker() -> io::Result<()> {
        let (mut state, db) = test_state("pull")?;
        let mut workers = HashMap::new();
        workers.insert("a".to_string(), worker_status("a", vec![]));
        workers.insert("b".to_string(), worker_status("b", vec![]));
        state.workers = RwLock::new(workers);
        let req = request("POST", "/api/pull", br#"{"model":"llama3"}"#);
        match plan_request(&state, &req, None) {
            RoutePlan::QueueByNode(worker) => assert!(worker == "a" || worker == "b"),
            _ => panic!("expected single-node route"),
        }
        cleanup(&db);
        Ok(())
    }

    #[test]
    fn delete_targets_single_owner() -> io::Result<()> {
        let (mut state, db) = test_state("delete")?;
        let mut workers = HashMap::new();
        workers.insert(
            "worker-a".to_string(),
            worker_status("worker-a", vec!["llama3"]),
        );
        workers.insert(
            "worker-b".to_string(),
            worker_status("worker-b", vec!["llama3"]),
        );
        state.workers = RwLock::new(workers);
        let req = request("DELETE", "/api/delete", br#"{"model":"llama3"}"#);
        match plan_request(&state, &req, None) {
            RoutePlan::QueueByNode(worker) => {
                assert!(worker == "worker-a" || worker == "worker-b")
            }
            _ => panic!("expected single-owner route"),
        }
        cleanup(&db);
        Ok(())
    }

    #[test]
    fn tags_masks_models_for_restricted_key() -> io::Result<()> {
        let (mut state, db) = test_state("masked_tags")?;
        let token = "token".to_string();
        let key = state.keys.insert(
            token,
            crate::auth::Role::Client,
            "alice".to_string(),
            false,
            vec!["llama3".to_string()],
            Vec::new(),
        )?;
        let mut workers = HashMap::new();
        workers.insert(
            "worker-a".to_string(),
            WorkerStatus {
                model_catalog: Some(json!({"models":[{"name":"llama3"},{"name":"mistral"}]})),
                ..worker_status("worker-a", vec!["llama3", "mistral"])
            },
        );
        state.workers = RwLock::new(workers);
        let req = request("GET", "/api/tags", b"");
        match plan_request(&state, &req, Some(&key)) {
            RoutePlan::Local(response) => {
                let value: Value = serde_json::from_slice(&response.body).expect("json");
                let models = value["models"].as_array().expect("models");
                assert_eq!(models.len(), 1);
                assert_eq!(models[0]["name"], "llama3");
            }
            _ => panic!("expected local aggregate"),
        }
        cleanup(&db);
        Ok(())
    }

    #[test]
    fn tags_keeps_latest_alias_visible_for_bare_whitelist() -> io::Result<()> {
        let (mut state, db) = test_state("masked_tags_latest_alias")?;
        let token = "token".to_string();
        let key = state.keys.insert(
            token,
            crate::auth::Role::Client,
            "alice".to_string(),
            false,
            vec!["bge-m3".to_string()],
            Vec::new(),
        )?;
        let mut workers = HashMap::new();
        workers.insert(
            "worker-a".to_string(),
            WorkerStatus {
                model_catalog: Some(json!({"models":[{"name":"bge-m3:latest"}]})),
                ..worker_status("worker-a", vec!["bge-m3"])
            },
        );
        state.workers = RwLock::new(workers);
        let req = request("GET", "/api/tags", b"");
        match plan_request(&state, &req, Some(&key)) {
            RoutePlan::Local(response) => {
                let value: Value = serde_json::from_slice(&response.body).expect("json");
                let models = value["models"].as_array().expect("models");
                assert_eq!(models.len(), 1);
                assert_eq!(models[0]["name"], "bge-m3:latest");
            }
            _ => panic!("expected local aggregate"),
        }
        cleanup(&db);
        Ok(())
    }
}
