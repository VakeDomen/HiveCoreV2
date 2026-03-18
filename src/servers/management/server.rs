use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use crate::app::{authorize_admin, AppState};
use crate::auth::{KeyRecord, Role};
use crate::shared::http::{
    HttpResponse, escape_json, extract_json_array_strings, extract_json_value, read_request,
};
use crate::shared::log;
use uuid::Uuid;

pub fn run(state: Arc<AppState>) -> io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", state.config.management_connection_port))?;
    log::info(format!(
        "management listener bound on 0.0.0.0:{}",
        state.config.management_connection_port
    ));
    for connection in listener.incoming() {
        let state = Arc::clone(&state);
        match connection {
            Ok(stream) => {
                thread::spawn(move || {
                    let _ = handle_connection(state, stream);
                });
            }
            Err(err) => log::error(format!("management accept error: {err}")),
        }
    }
    Ok(())
}

fn handle_connection(state: Arc<AppState>, mut stream: TcpStream) -> io::Result<()> {
    let request = match read_request(&stream) {
        Ok(request) => request,
        Err(err) => {
            log::warn(format!("management parse error: {err}"));
            return Ok(());
        }
    };

    if !authorize_admin(&state, &request) {
        log::warn(format!(
            "rejected unauthorized management request method={} uri={}",
            request.method, request.uri
        ));
        return HttpResponse::new(403, "Unauthorized", Vec::new()).write_to(&mut stream);
    }

    log::info(format!(
        "management request method={} uri={}",
        request.method, request.uri
    ));

    match (request.method.as_str(), request.uri.as_str()) {
        ("GET", "/queue") => {
            let snapshot = state.request_queue.snapshot();
            let model_queue = render_count_map(&snapshot.model_queue);
            let node_queue = render_count_map(&snapshot.node_queue);
            let body = format!("{{\"model_queue\":{model_queue},\"node_queue\":{node_queue}}}");
            HttpResponse::json(200, "OK", body).write_to(&mut stream)
        }
        ("GET", "/worker/status") | ("GET", "/worker/connections") => {
            let body = render_workers(&state);
            HttpResponse::json(200, "OK", body).write_to(&mut stream)
        }
        ("GET", "/worker/pings") => {
            let body = render_worker_pings(&state);
            HttpResponse::json(200, "OK", body).write_to(&mut stream)
        }
        ("GET", "/worker/tags") => {
            let body = render_worker_tags(&state);
            HttpResponse::json(200, "OK", body).write_to(&mut stream)
        }
        ("GET", "/worker/versions") => {
            let body = render_worker_versions(&state);
            HttpResponse::json(200, "OK", body).write_to(&mut stream)
        }
        ("GET", "/key") => {
            match render_keys(&state) {
                Ok(body) => HttpResponse::json(200, "OK", body).write_to(&mut stream),
                Err(err) => {
                    log::error(format!("failed to render keys: {err}"));
                    HttpResponse::new(500, "Internal Server Error", Vec::new())
                        .write_to(&mut stream)
                }
            }
        }
        ("POST", "/key") => {
            let role = extract_json_value(&request.body, "role");
            let name = extract_json_value(&request.body, "name")
                .unwrap_or_else(|| "generated".to_string());
            let whitelist_models =
                extract_json_array_strings(&request.body, "whitelist_models").unwrap_or_default();
            let blacklist_models =
                extract_json_array_strings(&request.body, "blacklist_models").unwrap_or_default();
            let generated_token = Uuid::new_v4().to_string();
            match role.as_deref() {
                Some("Admin") => {
                    let result = state.keys.insert(
                        generated_token.clone(),
                        Role::Admin,
                        name.clone(),
                        whitelist_models.clone(),
                        blacklist_models.clone(),
                    );
                    render_key_create_response(result, &name, &generated_token, "admin", &mut stream)
                }
                Some("Client") => {
                    let result = state.keys.insert(
                        generated_token.clone(),
                        Role::Client,
                        name.clone(),
                        whitelist_models.clone(),
                        blacklist_models.clone(),
                    );
                    render_key_create_response(result, &name, &generated_token, "client", &mut stream)
                }
                Some("Worker") => {
                    let result = state.keys.insert(
                        generated_token.clone(),
                        Role::Worker,
                        name.clone(),
                        whitelist_models.clone(),
                        blacklist_models.clone(),
                    );
                    render_key_create_response(result, &name, &generated_token, "worker", &mut stream)
                }
                _ => HttpResponse::new(400, "Bad Request", b"Expected role".to_vec())
                    .write_to(&mut stream),
            }
        }
        ("POST", "/worker/command") => {
            let Some(worker) = extract_json_value(&request.body, "worker") else {
                return HttpResponse::new(400, "Bad Request", b"Missing worker".to_vec())
                    .write_to(&mut stream);
            };
            let Some(command) = extract_json_value(&request.body, "command") else {
                return HttpResponse::new(400, "Bad Request", b"Missing command".to_vec())
                    .write_to(&mut stream);
            };

            let synthetic = crate::shared::http::HttpRequest {
                method: match command.as_str() {
                    "UPDATE" => "UPDATE_OLLAMA".to_string(),
                    _ => command,
                },
                uri: "/".to_string(),
                protocol: "HIVE".to_string(),
                headers: Default::default(),
                body: Vec::new(),
            };

            let task = crate::servers::proxy::models::client_task::ClientTask {
                request: synthetic,
                client_stream: None,
            };
            if state.request_queue.enqueue_to_node(worker, task).is_ok() {
                log::info("accepted worker command");
                HttpResponse::new(202, "Accepted", Vec::new()).write_to(&mut stream)
            } else {
                HttpResponse::new(500, "Internal Server Error", Vec::new()).write_to(&mut stream)
            }
        }
        _ => HttpResponse::new(404, "Not Found", Vec::new()).write_to(&mut stream),
    }
}

fn render_count_map(map: &std::collections::HashMap<String, usize>) -> String {
    let entries: Vec<String> = map
        .iter()
        .map(|(key, value)| format!("\"{}\":{}", escape_json(key), value))
        .collect();
    format!("{{{}}}", entries.join(","))
}

fn render_workers(state: &Arc<AppState>) -> String {
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

fn render_worker_pings(state: &Arc<AppState>) -> String {
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

fn render_worker_tags(state: &Arc<AppState>) -> String {
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

fn render_worker_versions(state: &Arc<AppState>) -> String {
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

fn render_keys(state: &Arc<AppState>) -> io::Result<String> {
    let entries = state
        .keys
        .list()?
        .into_iter()
        .map(|key| {
            format!(
                "{{\"id\":{},\"token\":\"{}\",\"value\":\"{}\",\"role\":\"{}\",\"name\":\"{}\",\"whitelist_models\":{},\"blacklist_models\":{}}}",
                key.id,
                escape_json(&key.token),
                escape_json(&key.token),
                key.role.as_str(),
                escape_json(&key.name),
                render_string_list(&key.whitelist_models),
                render_string_list(&key.blacklist_models)
            )
        })
        .collect::<Vec<_>>();
    Ok(format!("[{}]", entries.join(",")))
}

fn render_string_list(values: &[String]) -> String {
    let entries = values
        .iter()
        .map(|value| format!("\"{}\"", escape_json(value)))
        .collect::<Vec<_>>();
    format!("[{}]", entries.join(","))
}

fn render_key_create_response(
    result: io::Result<KeyRecord>,
    name: &str,
    token: &str,
    role_label: &str,
    stream: &mut TcpStream,
) -> io::Result<()> {
    match result {
        Ok(_) => {
            log::info(format!(
                "generated {role_label} key name={} token={token}",
                name
            ));
            HttpResponse::json(
                201,
                "Created",
                format!("{{\"token\":\"{}\"}}", escape_json(token)),
            )
            .write_to(stream)
        }
        Err(err) => {
            log::warn(format!(
                "failed to generate {role_label} key name={} error={err}",
                name
            ));
            let status = if err.kind() == io::ErrorKind::AlreadyExists {
                (409, "Conflict")
            } else {
                (500, "Internal Server Error")
            };
            HttpResponse::new(status.0, status.1, Vec::new()).write_to(stream)
        }
    }
}
