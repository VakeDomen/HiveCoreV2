use crate::shared::http::HttpResponse;

pub enum RoutePlan {
    QueueByModel(String),
    QueueByNode(String),
    Local(HttpResponse),
    Reject {
        status: u16,
        reason: &'static str,
        message: &'static str,
    },
}
