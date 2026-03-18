use std::net::TcpStream;
use std::sync::mpsc::Sender;

use crate::servers::proxy::models::worker_http_response::WorkerHttpResponse;

pub enum ResponseTarget {
    ProxyClient(TcpStream),
    Capture(Sender<WorkerHttpResponse>),
    Ignore,
}
