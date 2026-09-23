use std::sync::Arc;

use crate::app::AppState;
use crate::auth::{KeyRecord, Role};
use crate::shared::http::{HttpRequest, extract_json_value};
use crate::shared::log;

/// Result of an authorization check that failed due to a rate limit.
#[derive(Debug, PartialEq)]
pub struct RateLimitDenial {
    pub reason: &'static str,
    pub retry_after_secs: Option<u64>,
}

/// Authorization error — either a status code for auth/model failures,
/// or a rate limit denial with a descriptive reason.
#[derive(Debug, PartialEq)]
pub enum AuthError {
    Status(u16),
    RateLimited(RateLimitDenial),
}

pub fn authorize_request(state: &AppState, request: &HttpRequest) -> Result<(), AuthError> {
    let verified_key = authorized_key(state, request);

    if state.config.user_authentication && verified_key.is_none() {
        log::warn(format!(
            "rejected unauthorized client request method={} uri={}",
            request.method, request.uri
        ));
        return Err(AuthError::Status(401));
    }

    if request.header("node").is_some() {
        let Some(key) = verified_key.as_ref() else {
            log::warn(format!(
                "rejected node-targeted request without admin key method={} uri={}",
                request.method, request.uri
            ));
            return Err(AuthError::Status(401));
        };
        if key.role != Role::Admin {
            log::warn(format!(
                "rejected node-targeted request for non-admin key={} method={} uri={}",
                key.name, request.method, request.uri
            ));
            return Err(AuthError::Status(403));
        }
    }

    if admin_only_proxy_route(request) {
        let Some(key) = verified_key.as_ref() else {
            log::warn(format!(
                "rejected admin-only proxy request without admin key method={} uri={}",
                request.method, request.uri
            ));
            return Err(AuthError::Status(401));
        };
        if key.role != Role::Admin {
            log::warn(format!(
                "rejected admin-only proxy request for non-admin key={} method={} uri={}",
                key.name, request.method, request.uri
            ));
            return Err(AuthError::Status(403));
        }
    }

    if let Some(key) = verified_key.as_ref() {
        for model in request_models(request) {
            if !key.allows_model(&model) {
                log::warn(format!(
                    "rejected model by key policy key={} model={}",
                    key.name, model
                ));
                return Err(AuthError::Status(403));
            }
        }

        // Skip rate limiting for lightweight discovery / info routes.
        if !is_discovery_route(request) {
            // Rate limit check — reserve a concurrent slot on success
            let tier = state
                .rate_limiter
                .resolve_tier(&key.rate_limit_tier);
            let result = state.rate_limiter.check(key.id, &tier);
            if !result.allowed {
                log::warn(format!(
                    "rate limited key={} method={} uri={} reason={:?} retry_after={:?}",
                    key.name, request.method, request.uri, result.reason, result.retry_after_secs
                ));
                return Err(AuthError::RateLimited(RateLimitDenial {
                    reason: result.reason.unwrap_or("rate limit"),
                    retry_after_secs: result.retry_after_secs,
                }));
            }
            // Concurrent slot reserved at admission time so queued requests also count.
            state.rate_limiter.start_request(key.id);
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

pub fn authorized_key(state: &AppState, request: &HttpRequest) -> Option<Arc<KeyRecord>> {
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
        | ("POST", "/generative_scoring")
        | ("POST", "/api/pull")
        | ("POST", "/api/push")
        | ("DELETE", "/api/delete")
        | ("POST", "/")
        | ("POST", "/v1/evaluate")
        | ("POST", "/v1/systemone")
        | ("POST", "/ai/run") => {
            if let Some(model) = extract_json_value(request, "model") {
                models.push(model);
            }
        }
        ("POST", "/api/show") => {
            if let Some(model) = extract_json_value(request, "model")
                .or_else(|| extract_json_value(request, "name"))
            {
                models.push(model);
            }
        }
        ("POST", "/api/copy") => {
            if let Some(source) = extract_json_value(request, "source") {
                models.push(source);
            }
        }
        ("POST", "/api/create") => {
            if let Some(from) = extract_json_value(request, "from") {
                models.push(from);
            }
        }
        _ => {}
    }
    models
}

/// Returns true for lightweight discovery / info routes that should not be rate limited.
fn is_discovery_route(request: &HttpRequest) -> bool {
    matches!(
        (request.method.as_str(), request.uri.as_str()),
        ("GET", "/api/tags")
            | ("GET", "/api/ps")
            | ("GET", "/api/version")
            | ("GET", "/health")
            | ("GET", "/v1/models")
            | ("GET", "/tokenizer_info")
            | ("GET", "/version")
            | ("GET", "/is_sleeping")
    ) || (request.method == "GET" && request.uri.as_str().starts_with("/v1/models/"))
}

#[cfg(test)]
mod tests {
    use crate::shared::http::HttpRequest;

    use super::{admin_only_proxy_route, request_models};

    #[test]
    fn systemone_evaluate_route_extracts_model() {
        let req = HttpRequest::new("POST".to_string(), "/".to_string(), "HTTP/1.1".to_string(), Default::default(), br#"{"model":"systemone/diy-jev-0.1.0"}"#.to_vec());
        let models = request_models(&req);
        assert_eq!(models, vec!["systemone/diy-jev-0.1.0"]);
    }

    #[test]
    fn systemone_evaluate_v1_route_extracts_model() {
        let req = HttpRequest::new("POST".to_string(), "/v1/evaluate".to_string(), "HTTP/1.1".to_string(), Default::default(), br#"{"model":"systemone/diy-jev-0.1.0"}"#.to_vec());
        let models = request_models(&req);
        assert_eq!(models, vec!["systemone/diy-jev-0.1.0"]);
    }

    #[test]
    fn systemone_systemone_route_extracts_model() {
        let req = HttpRequest::new("POST".to_string(), "/v1/systemone".to_string(), "HTTP/1.1".to_string(), Default::default(), br#"{"model":"systemone/diy-jev-0.1.0"}"#.to_vec());
        let models = request_models(&req);
        assert_eq!(models, vec!["systemone/diy-jev-0.1.0"]);
    }

    #[test]
    fn systemone_ai_run_route_extracts_model() {
        let req = HttpRequest::new("POST".to_string(), "/ai/run".to_string(), "HTTP/1.1".to_string(), Default::default(), br#"{"model":"systemone/diy-jev-0.1.0"}"#.to_vec());
        let models = request_models(&req);
        assert_eq!(models, vec!["systemone/diy-jev-0.1.0"]);
    }

    #[test]
    fn systemone_routes_are_not_admin_only() {
        let req = HttpRequest::new("POST".to_string(), "/v1/evaluate".to_string(), "HTTP/1.1".to_string(), Default::default(), br#"{"model":"systemone/diy-jev-0.1.0"}"#.to_vec());
        assert!(!admin_only_proxy_route(&req));
    }
}
