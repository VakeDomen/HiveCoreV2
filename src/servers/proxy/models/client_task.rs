use std::net::TcpStream;

use crate::shared::http::HttpRequest;

pub struct ClientTask {
    pub request: HttpRequest,
    pub client_stream: Option<TcpStream>,
}
