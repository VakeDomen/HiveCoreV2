use crate::app::AppState;
use crate::shared::http::{HttpResponse, escape_json};

pub fn get_queue(state: &AppState) -> HttpResponse {
    let snapshot = state.request_queue.snapshot();
    let model_queue = render_count_map(&snapshot.model_queue);
    let node_queue = render_count_map(&snapshot.node_queue);
    let body = format!("{{\"model_queue\":{model_queue},\"node_queue\":{node_queue}}}");
    HttpResponse::json(200, "OK", body)
}

fn render_count_map(map: &std::collections::HashMap<String, usize>) -> String {
    let entries: Vec<String> = map
        .iter()
        .map(|(key, value)| format!("\"{}\":{}", escape_json(key), value))
        .collect();
    format!("{{{}}}", entries.join(","))
}
