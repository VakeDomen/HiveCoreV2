use std::io;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::app::AppState;
use crate::servers::proxy::models::response_target::ResponseTarget;
use crate::servers::worker::models::worker_phase::WorkerPhase;
use crate::shared::http::HttpResponse;
use crate::shared::log;

pub fn run(state: Arc<AppState>) -> io::Result<()> {
    loop {
        thread::sleep(Duration::from_millis(500));
        let now = Instant::now();
        let polling_timeout = Duration::from_secs(state.config.polling_node_connection_timeout);
        let working_timeout = Duration::from_secs(state.config.working_node_connection_timeout);
        expire_queued_requests(&state);

        let stale_connections = state
            .workers
            .read()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .flat_map(|(name, worker)| {
                        worker.connections.iter().filter_map(move |connection| {
                            let timeout = match connection.state {
                                WorkerPhase::Working => working_timeout,
                                _ => polling_timeout,
                            };
                            (now.duration_since(connection.last_ping) > timeout
                                && now.duration_since(connection.last_poll) > timeout)
                                .then(|| (name.clone(), connection.index))
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if stale_connections.is_empty() {
            continue;
        }

        if let Ok(mut guard) = state.workers.write() {
            for (name, connection_index) in stale_connections {
                let mut remove_worker = false;
                if let Some(worker) = guard.get_mut(&name) {
                    if worker.remove_connection(connection_index) {
                        remove_worker = worker.connections.is_empty();
                    }
                }
                if remove_worker {
                    guard.remove(&name);
                    log::warn(format!("removed stale worker={name}"));
                } else {
                    log::warn(format!(
                        "removed stale worker connection name={} index={}",
                        name, connection_index
                    ));
                }
            }
        }
    }
}

fn expire_queued_requests(state: &AppState) {
    let max_age = Duration::from_millis(state.config.proxy_timeout_ms);
    for task in state.request_queue.expire_older_than(max_age) {
        match task.response_target {
            ResponseTarget::ProxyClient(mut stream) => {
                let _ = HttpResponse::new(
                    504,
                    "Gateway Timeout",
                    b"request expired while waiting for a compatible worker".to_vec(),
                )
                .write_to(&mut stream);
            }
            ResponseTarget::Capture(_) | ResponseTarget::Ignore => {}
        }
    }
}
