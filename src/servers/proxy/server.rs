use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::app::AppState;
use crate::servers::proxy::admission::authorize_request;
use crate::servers::proxy::admission::authorized_key;
use crate::servers::proxy::models::client_task::{ClientTask, RequestContext};
use crate::servers::proxy::models::model_route_kind::ModelRouteKind;
use crate::servers::proxy::models::response_target::ResponseTarget;
use crate::servers::proxy::models::route_plan::RoutePlan;
use crate::servers::proxy::planner::plan_request;
use crate::shared::http::{
    HttpRequest, HttpResponse, ensure_openai_stream_usage, read_request, request_usage_model_name,
};
use crate::shared::log;

pub fn run(state: Arc<AppState>) -> io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", state.config.proxy_port))?;
    log::info(format!(
        "client listener bound on 0.0.0.0:{}",
        state.config.proxy_port
    ));
    for connection in listener.incoming() {
        let state = Arc::clone(&state);
        match connection {
            Ok(stream) => {
                thread::spawn(move || {
                    let _ = handle_connection(state, stream);
                });
            }
            Err(err) => log::error(format!("client accept error: {err}")),
        }
    }
    Ok(())
}

fn handle_connection(state: Arc<AppState>, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(state.config.proxy_timeout_ms)))?;
    stream.set_write_timeout(Some(Duration::from_millis(state.config.proxy_timeout_ms)))?;
    let started_at = Instant::now();
    let request = match read_request(&stream) {
        Ok(request) => request,
        Err(err) => {
            log::warn(format!("client request parse error: {err}"));
            return Ok(());
        }
    };

    if let Err(status) = authorize_request(&state, &request) {
        let reason = match status {
            403 => "Forbidden",
            _ => "Unauthorized",
        };
        log::warn(format!(
            "rejected client request method={} uri={} status={} total={}",
            request.method,
            request.uri,
            log::bold(status.to_string()),
            log::bold(log::format_duration(started_at.elapsed()))
        ));
        return HttpResponse::new(status, reason, Vec::new()).write_to(&mut stream);
    }

    let request_method = request.method.clone();
    let request_uri = request.uri.clone();
    let visible_key = authorized_key(&state, &request);
    let user_name = visible_key
        .as_ref()
        .map(|key| key.name.as_str())
        .unwrap_or("Unauthenticated");

    match plan_request(&state, &request, visible_key.as_ref()) {
        RoutePlan::Local(response) => {
            let status_code = response.status_code;
            response.write_to(&mut stream)?;
            log::info(format!(
                "served local request user={} method={} uri={} status={} total={}",
                log::bold(user_name),
                request_method,
                request_uri,
                log::bold(status_code.to_string()),
                log::bold(log::format_duration(started_at.elapsed()))
            ));
            Ok(())
        }
        RoutePlan::Reject {
            status,
            reason,
            message,
        } => {
            log::warn(format!(
                "rejected client request user={} method={} uri={} status={} total={} reason={}",
                log::bold(user_name),
                request_method,
                request_uri,
                log::bold(status.to_string()),
                log::bold(log::format_duration(started_at.elapsed())),
                message
            ));
            HttpResponse::new(status, reason, message.as_bytes().to_vec()).write_to(&mut stream)
        }
        RoutePlan::QueueByModel { model, kind } => {
            let client_request = request.clone();
            let mut request = request;
            let mut proxy_mutations = Vec::new();
            if ensure_openai_stream_usage(&mut request) {
                proxy_mutations.push("openai_stream_include_usage".to_string());
                log::info(format!(
                    "enabled usage metadata for streaming OpenAI-compatible request user={} method={} uri={} model={}",
                    log::bold(user_name),
                    request_method,
                    request_uri,
                    log::bold(&model)
                ));
            }
            let task = client_task(
                request,
                &stream,
                RequestContext {
                    key_id: visible_key.as_ref().map(|key| key.id),
                    key_name: visible_key.as_ref().map(|key| key.name.clone()),
                    model: Some(model.clone()),
                    capture: visible_key.as_ref().map(|key| key.capture).unwrap_or(false),
                    client_request: Some(client_request),
                    proxy_mutations,
                },
            )?;
            enqueue_model(
                &state,
                model,
                kind,
                task,
                &request_method,
                &request_uri,
                &mut stream,
            )
        }
        RoutePlan::QueueByNode(worker) => {
            let client_request = request.clone();
            let mut request = request;
            let model = request_usage_model_name(&request);
            let mut proxy_mutations = Vec::new();
            if ensure_openai_stream_usage(&mut request) {
                proxy_mutations.push("openai_stream_include_usage".to_string());
                log::info(format!(
                    "enabled usage metadata for streaming OpenAI-compatible node request user={} method={} uri={} worker={}",
                    log::bold(user_name),
                    request_method,
                    request_uri,
                    log::bold(&worker)
                ));
            }
            let task = client_task(
                request,
                &stream,
                RequestContext {
                    key_id: visible_key.as_ref().map(|key| key.id),
                    key_name: visible_key.as_ref().map(|key| key.name.clone()),
                    model,
                    capture: visible_key.as_ref().map(|key| key.capture).unwrap_or(false),
                    client_request: Some(client_request),
                    proxy_mutations,
                },
            )?;
            enqueue_node(
                &state,
                worker,
                task,
                &request_method,
                &request_uri,
                &mut stream,
            )
        }
    }
}

fn client_task(
    request: HttpRequest,
    stream: &TcpStream,
    context: RequestContext,
) -> io::Result<ClientTask> {
    Ok(ClientTask::new(
        request,
        ResponseTarget::ProxyClient(stream.try_clone()?),
        context,
    ))
}

fn enqueue_model(
    state: &Arc<AppState>,
    model: String,
    kind: ModelRouteKind,
    task: ClientTask,
    request_method: &str,
    request_uri: &str,
    stream: &mut TcpStream,
) -> io::Result<()> {
    if let Err(message) = state.request_queue.enqueue_model(model, kind, task) {
        log::warn(format!(
            "rejecting client request method={} uri={} reason={}",
            request_method, request_uri, message
        ));
        return HttpResponse::new(500, "Internal Server Error", message.as_bytes().to_vec())
            .write_to(stream);
    }

    Ok(())
}

fn enqueue_node(
    state: &Arc<AppState>,
    worker: String,
    task: ClientTask,
    request_method: &str,
    request_uri: &str,
    stream: &mut TcpStream,
) -> io::Result<()> {
    if let Err(message) = state.request_queue.enqueue_node(worker, task) {
        log::warn(format!(
            "rejecting client request method={} uri={} reason={}",
            request_method, request_uri, message
        ));
        return HttpResponse::new(500, "Internal Server Error", message.as_bytes().to_vec())
            .write_to(stream);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::io;
    use std::path::PathBuf;

    use uuid::Uuid;

    use crate::app::{AppState, Config};
    use crate::auth::Role;
    use crate::servers::proxy::admission::authorize_request;
    use crate::shared::http::HttpRequest;

    fn temp_db_path(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hive_core_v2_client_{test_name}_{}.db",
            Uuid::new_v4()
        ))
    }

    fn test_state(test_name: &str, user_authentication: bool) -> io::Result<(AppState, PathBuf)> {
        let db_path = temp_db_path(test_name);
        let config = Config {
            user_authentication,
            database_url: db_path.to_string_lossy().into_owned(),
            ..Config::default()
        };
        let (_stats_tx, _stats_rx): (
            std::sync::mpsc::Sender<crate::shared::http::UsageEvent>,
            std::sync::mpsc::Receiver<crate::shared::http::UsageEvent>,
        ) = std::sync::mpsc::channel();
        let (_capture_tx, _capture_rx): (
            std::sync::mpsc::Sender<crate::shared::capture::CaptureEvent>,
            std::sync::mpsc::Receiver<crate::shared::capture::CaptureEvent>,
        ) = std::sync::mpsc::channel();
        Ok((AppState::new(config, _stats_tx, _capture_tx)?, db_path))
    }

    fn request_with_auth(token: Option<&str>, body: &[u8]) -> HttpRequest {
        let mut headers = HashMap::new();
        if let Some(token) = token {
            headers.insert("authorization".to_string(), format!("Bearer {token}"));
        }
        HttpRequest {
            method: "POST".to_string(),
            uri: "/api/generate".to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers,
            body: body.to_vec(),
        }
    }

    fn request_with_auth_to(
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: &[u8],
    ) -> HttpRequest {
        let mut request = request_with_auth(token, body);
        request.method = method.to_string();
        request.uri = uri.to_string();
        request
    }

    fn cleanup(path: &PathBuf) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_missing_client_credentials_when_auth_is_enabled() -> io::Result<()> {
        let (state, db_path) = test_state("missing_auth", true)?;
        let request = request_with_auth(None, br#"{"model":"llama3"}"#);

        assert_eq!(authorize_request(&state, &request), Err(401));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn rejects_local_proxy_route_without_auth_when_auth_is_enabled() -> io::Result<()> {
        let (state, db_path) = test_state("local_missing_auth", true)?;
        let request = request_with_auth_to("GET", "/api/tags", None, b"");

        assert_eq!(authorize_request(&state, &request), Err(401));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn accepts_request_with_api_key_header() -> io::Result<()> {
        let (state, db_path) = test_state("api_key_auth", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "foras".to_string(),
            false,
            vec!["llama3".to_string()],
            Vec::new(),
        )?;

        let mut headers = HashMap::new();
        headers.insert("api-key".to_string(), token);
        let request = HttpRequest {
            method: "POST".to_string(),
            uri: "/api/generate".to_string(),
            protocol: "HTTP/1.1".to_string(),
            headers,
            body: br#"{"model":"llama3"}"#.to_vec(),
        };

        assert_eq!(authorize_request(&state, &request), Ok(()));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn accepts_request_when_whitelist_allows_model() -> io::Result<()> {
        let (state, db_path) = test_state("whitelist_ok", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["llama3".to_string()],
            Vec::new(),
        )?;
        let request = request_with_auth(Some(&token), br#"{"model":"llama3"}"#);

        assert_eq!(authorize_request(&state, &request), Ok(()));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn accepts_request_when_bare_whitelist_allows_latest_alias() -> io::Result<()> {
        let (state, db_path) = test_state("whitelist_latest_alias", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["bge-m3".to_string()],
            Vec::new(),
        )?;
        let request = request_with_auth(Some(&token), br#"{"model":"bge-m3:latest"}"#);

        assert_eq!(authorize_request(&state, &request), Ok(()));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn rejects_request_when_model_not_in_whitelist() -> io::Result<()> {
        let (state, db_path) = test_state("whitelist_reject", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["llama3".to_string()],
            Vec::new(),
        )?;
        let request = request_with_auth(Some(&token), br#"{"model":"mistral"}"#);

        assert_eq!(authorize_request(&state, &request), Err(403));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn rejects_show_request_name_when_model_not_in_whitelist() -> io::Result<()> {
        let (state, db_path) = test_state("show_name_whitelist_reject", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["llama3".to_string()],
            Vec::new(),
        )?;
        let request =
            request_with_auth_to("POST", "/api/show", Some(&token), br#"{"name":"mistral"}"#);

        assert_eq!(authorize_request(&state, &request), Err(403));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn rejects_request_when_model_is_blacklisted() -> io::Result<()> {
        let (state, db_path) = test_state("blacklist_reject", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["llama3".to_string(), "mistral".to_string()],
            vec!["mistral".to_string()],
        )?;
        let request = request_with_auth(Some(&token), br#"{"model":"mistral"}"#);

        assert_eq!(authorize_request(&state, &request), Err(403));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn rejects_openai_request_when_model_not_in_whitelist() -> io::Result<()> {
        let (state, db_path) = test_state("openai_whitelist_reject", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["llama3".to_string()],
            Vec::new(),
        )?;
        let request = request_with_auth_to(
            "POST",
            "/v1/chat/completions",
            Some(&token),
            br#"{"model":"mistral","messages":[]}"#,
        );

        assert_eq!(authorize_request(&state, &request), Err(403));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn rejects_openai_responses_request_when_model_not_in_whitelist() -> io::Result<()> {
        let (state, db_path) = test_state("openai_responses_whitelist_reject", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["llama3".to_string()],
            Vec::new(),
        )?;
        let request = request_with_auth_to(
            "POST",
            "/v1/responses",
            Some(&token),
            br#"{"model":"mistral","input":"hello"}"#,
        );

        assert_eq!(authorize_request(&state, &request), Err(403));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn rejects_vllm_request_when_model_not_in_whitelist() -> io::Result<()> {
        let (state, db_path) = test_state("vllm_whitelist_reject", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["llama3".to_string()],
            Vec::new(),
        )?;
        let request = request_with_auth_to(
            "POST",
            "/rerank",
            Some(&token),
            br#"{"model":"reranker","query":"q","documents":["d"]}"#,
        );

        assert_eq!(authorize_request(&state, &request), Err(403));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn rejects_node_targeted_request_for_client_key() -> io::Result<()> {
        let (state, db_path) = test_state("node_client_reject", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            vec!["llama3".to_string()],
            Vec::new(),
        )?;
        let mut request = request_with_auth(Some(&token), br#"{"model":"llama3"}"#);
        request
            .headers
            .insert("node".to_string(), "worker-a".to_string());

        assert_eq!(authorize_request(&state, &request), Err(403));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn rejects_admin_only_proxy_route_for_client_key() -> io::Result<()> {
        let (state, db_path) = test_state("admin_only_client_reject", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;
        let request = request_with_auth_to(
            "DELETE",
            "/api/delete",
            Some(&token),
            br#"{"model":"llama3"}"#,
        );

        assert_eq!(authorize_request(&state, &request), Err(403));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn rejects_vllm_control_route_for_client_key() -> io::Result<()> {
        let (state, db_path) = test_state("vllm_control_client_reject", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Client,
            "alice".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;
        let request = request_with_auth_to("POST", "/sleep", Some(&token), br#"{"level":1}"#);

        assert_eq!(authorize_request(&state, &request), Err(403));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn accepts_admin_only_proxy_route_for_admin_key() -> io::Result<()> {
        let (state, db_path) = test_state("admin_only_admin_ok", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Admin,
            "root".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;
        let request = request_with_auth_to("GET", "/load", Some(&token), b"");

        assert_eq!(authorize_request(&state, &request), Ok(()));

        cleanup(&db_path);
        Ok(())
    }

    #[test]
    fn accepts_node_targeted_request_for_admin_key() -> io::Result<()> {
        let (state, db_path) = test_state("node_admin_ok", true)?;
        let token = Uuid::new_v4().to_string();
        state.keys.insert(
            token.clone(),
            Role::Admin,
            "root".to_string(),
            false,
            Vec::new(),
            Vec::new(),
        )?;
        let mut request = request_with_auth(Some(&token), br#"{"model":"llama3"}"#);
        request
            .headers
            .insert("node".to_string(), "worker-a".to_string());

        assert_eq!(authorize_request(&state, &request), Ok(()));

        cleanup(&db_path);
        Ok(())
    }
}
