use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::shared::http::HttpRequest;

use super::response_target::ResponseTarget;

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

pub struct ClientTask {
    pub id: u64,
    pub request: HttpRequest,
    pub response_target: ResponseTarget,
    pub queued_at: Instant,
}

impl ClientTask {
    pub fn new(request: HttpRequest, response_target: ResponseTarget) -> Self {
        Self {
            id: NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed),
            request,
            response_target,
            queued_at: Instant::now(),
        }
    }

    pub fn queue_wait(&self) -> Duration {
        self.queued_at.elapsed()
    }
}
