use serde::Deserialize;

#[derive(Deserialize)]
pub struct KeyDeleteRequest {
    pub id: i64,
}
