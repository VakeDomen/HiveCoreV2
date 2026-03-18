use std::io;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::app::AppState;
use crate::servers::worker::models::worker_phase::WorkerPhase;
use crate::shared::log;

pub fn run(state: Arc<AppState>) -> io::Result<()> {
    loop {
        thread::sleep(Duration::from_millis(500));
        let now = Instant::now();
        let polling_timeout = Duration::from_secs(state.config.polling_node_connection_timeout);
        let working_timeout = Duration::from_secs(state.config.working_node_connection_timeout);

        let stale_workers = state
            .workers
            .read()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .filter_map(|(name, worker)| {
                        let timeout = match worker.state {
                            WorkerPhase::Working => working_timeout,
                            _ => polling_timeout,
                        };
                        (now.duration_since(worker.last_ping) > timeout
                            && now.duration_since(worker.last_poll) > timeout)
                            .then(|| name.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if stale_workers.is_empty() {
            continue;
        }

        if let Ok(mut guard) = state.workers.write() {
            for name in stale_workers {
                guard.remove(&name);
                log::warn(format!("removed stale worker={name}"));
            }
        }
    }
}
