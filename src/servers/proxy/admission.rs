use crate::app::AppState;
use crate::auth::Role;
use crate::shared::http::{HttpRequest, extract_json_value};
use crate::shared::log;

pub fn authorize_request(state: &AppState, request: &HttpRequest) -> Result<(), u16> {
    let verified_key = request
        .bearer_token()
        .and_then(|token| state.keys.verify(token, &[Role::Admin, Role::Client]));

    if state.config.user_authentication && verified_key.is_none() {
        log::warn(format!(
            "rejected unauthorized client request method={} uri={}",
            request.method, request.uri
        ));
        return Err(401);
    }

    if let Some(model) = extract_json_value(&request.body, "model") {
        if let Some(key) = verified_key.as_ref() {
            if !key.allows_model(&model) {
                log::warn(format!(
                    "rejected model by key policy key={} model={}",
                    key.name, model
                ));
                return Err(403);
            }
        }
    }

    Ok(())
}
