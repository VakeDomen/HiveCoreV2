use serde::Deserialize;

#[derive(Deserialize)]
pub struct KeyUpdateRequest {
    pub id: i64,
    pub capture: bool,
}
