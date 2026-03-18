use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use crate::auth::Role;
use crate::http::{HttpResponse, extract_json_value, read_request};
use crate::log;
use crate::state::{AppState, ClientTask};

pub fn run(state: Arc<AppState>) -> io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", state.config.proxy_port))?;
    log::info(format!(
        "client listener bound on 0.0.0.0:{}",
        state.config.proxy_port
    ));
    for connection in listener.incoming() {
        let state = Arc::clone(&state);
        match connection {
            Ok(stream) => {
                thread::spawn(move || {
                    let _ = handle_connection(state, stream);
                });
            }
            Err(err) => log::error(format!("client accept error: {err}")),
        }
    }
    Ok(())
}

fn handle_connection(state: Arc<AppState>, mut stream: TcpStream) -> io::Result<()> {
    let request = match read_request(&stream) {
        Ok(request) => request,
        Err(err) => {
            log::warn(format!("client request parse error: {err}"));
            return Ok(());
        }
    };

    let verified_key = request
        .bearer_token()
        .and_then(|token| state.keys.verify(token, &[Role::Admin, Role::Client]));

    if state.config.user_authentication && verified_key.is_none() {
        log::warn(format!(
            "rejected unauthorized client request method={} uri={}",
            request.method, request.uri
        ));
        return HttpResponse::new(403, "Unauthorized", Vec::new()).write_to(&mut stream);
    }

    if let Some(model) = extract_json_value(&request.body, "model") {
        if let Some(key) = verified_key.as_ref() {
            if !key.allows_model(&model) {
                log::warn(format!(
                    "rejected model by key policy key={} model={}",
                    key.name, model
                ));
                return HttpResponse::new(403, "Forbidden", Vec::new()).write_to(&mut stream);
            }
        }
    }

    let request_method = request.method.clone();
    let request_uri = request.uri.clone();
    let task = ClientTask {
        request,
        client_stream: Some(stream.try_clone()?),
    };

    if let Err(message) = state.request_queue.enqueue(task) {
        log::warn(format!(
            "rejecting client request method={} uri={} reason={}",
            request_method, request_uri, message
        ));
        return HttpResponse::new(405, "Method Not Allowed", message.as_bytes().to_vec())
            .write_to(&mut stream);
    }

    log::info(format!(
        "accepted client request method={} uri={}",
        request_method, request_uri
    ));
    Ok(())
}
