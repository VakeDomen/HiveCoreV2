use crate::app::AppState;
use crate::servers::management::models::worker_command::WorkerCommandRequest;
use crate::servers::proxy::models::client_task::ClientTask;
use crate::servers::proxy::models::response_target::ResponseTarget;
use crate::shared::http::{HttpRequest, HttpResponse};
use crate::shared::log;

pub fn post_worker_command(state: &AppState, request: &HttpRequest) -> HttpResponse {
    let payload = match serde_json::from_slice::<WorkerCommandRequest>(&request.body) {
        Ok(payload) => payload,
        Err(err) => {
            log::warn(format!("invalid worker command json: {err}"));
            return HttpResponse::new(400, "Bad Request", b"Invalid JSON body".to_vec());
        }
    };

    let synthetic = HttpRequest {
        method: payload.command.hive_method().to_string(),
        uri: "/".to_string(),
        protocol: "HIVE".to_string(),
        headers: Default::default(),
        body: Vec::new(),
    };

    let task = ClientTask {
        request: synthetic,
        response_target: ResponseTarget::Ignore,
    };
    if state.request_queue.enqueue_to_node(payload.worker, task).is_ok() {
        log::info("accepted worker command");
        HttpResponse::new(202, "Accepted", Vec::new())
    } else {
        HttpResponse::new(500, "Internal Server Error", Vec::new())
    }
}
