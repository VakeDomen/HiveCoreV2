use crate::app::AppState;
use crate::servers::proxy::models::client_task::ClientTask;
use crate::shared::http::{HttpRequest, HttpResponse, extract_json_value};
use crate::shared::log;

pub fn post_worker_command(state: &AppState, request: &HttpRequest) -> HttpResponse {
    let Some(worker) = extract_json_value(&request.body, "worker") else {
        return HttpResponse::new(400, "Bad Request", b"Missing worker".to_vec());
    };
    let Some(command) = extract_json_value(&request.body, "command") else {
        return HttpResponse::new(400, "Bad Request", b"Missing command".to_vec());
    };

    let synthetic = HttpRequest {
        method: match command.as_str() {
            "UPDATE" => "UPDATE_OLLAMA".to_string(),
            _ => command,
        },
        uri: "/".to_string(),
        protocol: "HIVE".to_string(),
        headers: Default::default(),
        body: Vec::new(),
    };

    let task = ClientTask {
        request: synthetic,
        client_stream: None,
    };
    if state.request_queue.enqueue_to_node(worker, task).is_ok() {
        log::info("accepted worker command");
        HttpResponse::new(202, "Accepted", Vec::new())
    } else {
        HttpResponse::new(500, "Internal Server Error", Vec::new())
    }
}
