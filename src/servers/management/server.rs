use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use crate::app::{AppState, authorize_admin};
use crate::servers::management::routes;
use crate::shared::http::{HttpResponse, read_request};
use crate::shared::log;

pub fn run(state: Arc<AppState>) -> io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", state.config.management_connection_port))?;
    log::info(format!(
        "management listener bound on 0.0.0.0:{}",
        state.config.management_connection_port
    ));
    for connection in listener.incoming() {
        let state = Arc::clone(&state);
        match connection {
            Ok(stream) => {
                thread::spawn(move || {
                    let _ = handle_connection(state, stream);
                });
            }
            Err(err) => log::error(format!("management accept error: {err}")),
        }
    }
    Ok(())
}

fn handle_connection(state: Arc<AppState>, mut stream: TcpStream) -> io::Result<()> {
    let started_at = Instant::now();
    let request = match read_request(&stream) {
        Ok(request) => request,
        Err(err) => {
            log::warn(format!("management parse error: {err}"));
            return Ok(());
        }
    };

    if !authorize_admin(&state, &request) {
        log::warn(format!(
            "rejected unauthorized management request method={} uri={}",
            request.method, request.uri
        ));
        return HttpResponse::new(403, "Unauthorized", Vec::new()).write_to(&mut stream);
    }

    let response = match (request.method.as_str(), request.uri.as_str()) {
        ("GET", "/queue") => routes::queue::get_queue(&state),
        ("GET", "/worker/status") => routes::workers::get_status(&state),
        ("GET", "/worker/connections") => routes::workers::get_connections(&state),
        ("GET", "/worker/pings") => routes::workers::get_pings(&state),
        ("GET", "/worker/tags") => routes::workers::get_tags(&state),
        ("GET", "/worker/versions") => routes::workers::get_versions(&state),
        ("GET", "/key") => routes::keys::get_keys(&state),
        ("POST", "/key") => routes::keys::post_key(&state, &request),
        ("PATCH", "/key") => routes::keys::patch_key(&state, &request),
        ("POST", "/worker/command") => {
            routes::worker_command::post_worker_command(&state, &request)
        }
        _ => HttpResponse::new(404, "Not Found", Vec::new()),
    };

    let status_code = response.status_code;
    response.write_to(&mut stream)?;
    log::info(format!(
        "management request method={} uri={} status={} total={}",
        request.method,
        request.uri,
        log::bold(status_code.to_string()),
        log::bold(log::format_duration(started_at.elapsed()))
    ));
    Ok(())
}
