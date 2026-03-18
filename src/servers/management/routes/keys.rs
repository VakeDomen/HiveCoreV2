use std::io;

use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::{KeyRecord, Role};
use crate::servers::management::models::key_request::KeyRequest;
use crate::shared::http::{HttpRequest, HttpResponse};
use crate::shared::log;

pub fn get_keys(state: &AppState) -> HttpResponse {
    match render_keys(state) {
        Ok(body) => json_response(200, "OK", body),
        Err(err) => {
            log::error(format!("failed to render keys: {err}"));
            HttpResponse::new(500, "Internal Server Error", Vec::new())
        }
    }
}

pub fn post_key(state: &AppState, request: &HttpRequest) -> HttpResponse {
    let payload = match serde_json::from_slice::<KeyRequest>(&request.body) {
        Ok(payload) => payload,
        Err(err) => {
            log::warn(format!("invalid key request json: {err}"));
            return HttpResponse::new(400, "Bad Request", b"Invalid JSON body".to_vec());
        }
    };
    let generated_token = Uuid::new_v4().to_string();

    match payload.role {
        Role::Admin => render_key_create_response(
            state.keys.insert(
                generated_token.clone(),
                Role::Admin,
                payload.name.clone(),
                payload.whitelist_models.clone(),
                payload.blacklist_models.clone(),
            ),
            &payload.name,
            &generated_token,
            "admin",
        ),
        Role::Client => render_key_create_response(
            state.keys.insert(
                generated_token.clone(),
                Role::Client,
                payload.name.clone(),
                payload.whitelist_models.clone(),
                payload.blacklist_models.clone(),
            ),
            &payload.name,
            &generated_token,
            "client",
        ),
        Role::Worker => render_key_create_response(
            state.keys.insert(
                generated_token.clone(),
                Role::Worker,
                payload.name.clone(),
                payload.whitelist_models.clone(),
                payload.blacklist_models.clone(),
            ),
            &payload.name,
            &generated_token,
            "worker",
        ),
    }
}

fn render_keys(state: &AppState) -> io::Result<Value> {
    let entries = state
        .keys
        .list()?
        .into_iter()
        .map(KeyResponse::from)
        .collect::<Vec<_>>();
    Ok(json!(entries))
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
            json_response(201, "Created", json!({ "token": token }))
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

fn json_response(status: u16, reason: &'static str, value: Value) -> HttpResponse {
    match serde_json::to_string(&value) {
        Ok(body) => HttpResponse::json(status, reason, body),
        Err(_) => HttpResponse::new(500, "Internal Server Error", Vec::new()),
    }
}

#[derive(Serialize)]
struct KeyResponse {
    id: i64,
    token: String,
    role: Role,
    name: String,
    whitelist_models: Vec<String>,
    blacklist_models: Vec<String>,
}

impl From<KeyRecord> for KeyResponse {
    fn from(value: KeyRecord) -> Self {
        Self {
            id: value.id,
            token: value.token.clone(),
            role: value.role,
            name: value.name,
            whitelist_models: value.whitelist_models,
            blacklist_models: value.blacklist_models,
        }
    }
}
