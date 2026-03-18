use std::io;

use uuid::Uuid;

use crate::app::AppState;
use crate::auth::{KeyRecord, Role};
use crate::shared::http::{
    HttpRequest, HttpResponse, escape_json, extract_json_array_strings, extract_json_value,
};
use crate::shared::log;

pub fn get_keys(state: &AppState) -> HttpResponse {
    match render_keys(state) {
        Ok(body) => HttpResponse::json(200, "OK", body),
        Err(err) => {
            log::error(format!("failed to render keys: {err}"));
            HttpResponse::new(500, "Internal Server Error", Vec::new())
        }
    }
}

pub fn post_key(state: &AppState, request: &HttpRequest) -> HttpResponse {
    let role = extract_json_value(&request.body, "role");
    let name = extract_json_value(&request.body, "name").unwrap_or_else(|| "generated".to_string());
    let whitelist_models =
        extract_json_array_strings(&request.body, "whitelist_models").unwrap_or_default();
    let blacklist_models =
        extract_json_array_strings(&request.body, "blacklist_models").unwrap_or_default();
    let generated_token = Uuid::new_v4().to_string();

    match role.as_deref() {
        Some("Admin") => render_key_create_response(
            state.keys.insert(
                generated_token.clone(),
                Role::Admin,
                name.clone(),
                whitelist_models.clone(),
                blacklist_models.clone(),
            ),
            &name,
            &generated_token,
            "admin",
        ),
        Some("Client") => render_key_create_response(
            state.keys.insert(
                generated_token.clone(),
                Role::Client,
                name.clone(),
                whitelist_models.clone(),
                blacklist_models.clone(),
            ),
            &name,
            &generated_token,
            "client",
        ),
        Some("Worker") => render_key_create_response(
            state.keys.insert(
                generated_token.clone(),
                Role::Worker,
                name.clone(),
                whitelist_models.clone(),
                blacklist_models.clone(),
            ),
            &name,
            &generated_token,
            "worker",
        ),
        _ => HttpResponse::new(400, "Bad Request", b"Expected role".to_vec()),
    }
}

fn render_keys(state: &AppState) -> io::Result<String> {
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
) -> HttpResponse {
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
            HttpResponse::new(status.0, status.1, Vec::new())
        }
    }
}
