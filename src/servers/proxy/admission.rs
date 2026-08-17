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

    if request.header("node").is_some() {
        let Some(key) = verified_key.as_ref() else {
            log::warn(format!(
                "rejected node-targeted request without admin key method={} uri={}",
                request.method, request.uri
            ));
            return Err(401);
        };
        if key.role != Role::Admin {
            log::warn(format!(
                "rejected node-targeted request for non-admin key={} method={} uri={}",
                key.name, request.method, request.uri
            ));
            return Err(403);
        }
    }

    if admin_only_proxy_route(request) {
        let Some(key) = verified_key.as_ref() else {
            log::warn(format!(
                "rejected admin-only proxy request without admin key method={} uri={}",
                request.method, request.uri
            ));
            return Err(401);
        };
        if key.role != Role::Admin {
            log::warn(format!(
                "rejected admin-only proxy request for non-admin key={} method={} uri={}",
                key.name, request.method, request.uri
            ));
            return Err(403);
        }
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

fn admin_only_proxy_route(request: &HttpRequest) -> bool {
    matches!(
        (request.method.as_str(), request.uri.as_str()),
        ("POST", "/api/create")
            | ("POST", "/api/copy")
            | ("POST", "/api/pull")
            | ("POST", "/api/push")
            | ("DELETE", "/api/delete")
            | ("GET", "/load")
            | ("GET", "/metrics")
            | ("POST", "/v1/load_lora_adapter")
            | ("POST", "/v1/unload_lora_adapter")
            | ("POST", "/v1/lora_adapters")
            | ("POST", "/start_profile")
            | ("POST", "/stop_profile")
            | ("POST", "/sleep")
            | ("POST", "/wake_up")
    )
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
        | ("POST", "/v1/chat/completions")
        | ("POST", "/v1/chat/completions/batch")
        | ("POST", "/v1/completions")
        | ("POST", "/v1/responses")
        | ("POST", "/v1/embeddings")
        | ("POST", "/v2/embed")
        | ("POST", "/score")
        | ("POST", "/v1/score")
        | ("POST", "/rerank")
        | ("POST", "/v1/rerank")
        | ("POST", "/v2/rerank")
        | ("POST", "/tokenize")
        | ("POST", "/detokenize")
        | ("POST", "/api/pull")
        | ("POST", "/api/push")
        | ("DELETE", "/api/delete") => {
            if let Some(model) = extract_json_value(&request.body, "model") {
                models.push(model);
            }
        }
        ("POST", "/api/show") => {
            if let Some(model) = extract_json_value(&request.body, "model")
                .or_else(|| extract_json_value(&request.body, "name"))
            {
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
