use serde_json::json;

use crate::app::AppState;
use crate::shared::http::HttpResponse;

pub fn get_queue(state: &AppState) -> HttpResponse {
    let snapshot = state.request_queue.snapshot();
    match serde_json::to_string(&json!({
        "model_queue": snapshot.model_queue,
        "node_queue": snapshot.node_queue
    })) {
        Ok(body) => HttpResponse::json(200, "OK", body),
        Err(_) => HttpResponse::new(500, "Internal Server Error", Vec::new()),
    }
}
