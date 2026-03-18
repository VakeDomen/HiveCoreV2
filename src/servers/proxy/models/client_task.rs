use crate::shared::http::HttpRequest;

use super::response_target::ResponseTarget;

pub struct ClientTask {
    pub request: HttpRequest,
    pub response_target: ResponseTarget,
}
