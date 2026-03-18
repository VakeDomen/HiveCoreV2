use crate::app::AppState;
use crate::auth::{KeyRecord, Role};
use crate::shared::http::{HttpRequest, extract_json_value};
use crate::shared::log;

pub fn authorize_request(state: &AppState, request: &HttpRequest) -> Result<(), u16> {
    let verified_key = authorized_key(state, request);

    if state.config.user_authentication && verified_key.is_none() {
        log::warn(format!(
            "rejected unauthorized client request method={} uri={}",
            request.method, request.uri
        ));
        return Err(401);
    }

    if let Some(key) = verified_key.as_ref() {
        for model in request_models(request) {
            if !key.allows_model(&model) {
                log::warn(format!(
                    "rejected model by key policy key={} model={}",
                    key.name, model
                ));
                return Err(403);
            }
        }
    }

    Ok(())
}

pub fn authorized_key(state: &AppState, request: &HttpRequest) -> Option<KeyRecord> {
    request
        .bearer_token()
        .and_then(|token| state.keys.verify(token, &[Role::Admin, Role::Client]))
}

fn request_models(request: &HttpRequest) -> Vec<String> {
    let mut models = Vec::new();
    match (request.method.as_str(), request.uri.as_str()) {
        ("POST", "/api/generate")
        | ("POST", "/api/chat")
        | ("POST", "/api/embed")
        | ("POST", "/api/embeddings")
        | ("POST", "/api/show")
        | ("POST", "/api/pull")
        | ("POST", "/api/push")
        | ("DELETE", "/api/delete") => {
            if let Some(model) = extract_json_value(&request.body, "model") {
                models.push(model);
            }
        }
        ("POST", "/api/copy") => {
            if let Some(source) = extract_json_value(&request.body, "source") {
                models.push(source);
            }
        }
        ("POST", "/api/create") => {
            if let Some(from) = extract_json_value(&request.body, "from") {
                models.push(from);
            }
        }
        _ => {}
    }
    models
}
