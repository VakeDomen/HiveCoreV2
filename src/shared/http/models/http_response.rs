#[derive(Debug)]
pub struct HttpResponse {
    pub status_code: u16,
    pub reason: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
