#[derive(Clone, Debug)]
pub struct WorkerHttpResponse {
    pub status_code: u16,
    pub body: Vec<u8>,
}
