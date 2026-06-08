use crate::shared::http::HttpResponse;

use super::model_route_kind::ModelRouteKind;

pub enum RoutePlan {
    QueueByModel {
        model: String,
        kind: ModelRouteKind,
    },
    QueueByNode(String),
    Local(HttpResponse),
    Reject {
        status: u16,
        reason: &'static str,
        message: &'static str,
    },
}
