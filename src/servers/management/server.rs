use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use crate::app::{AppState, authorize_management};
use crate::auth::Role;
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

    let route_path = request
        .uri
        .split('?')
        .next()
        .unwrap_or(request.uri.as_str());
    let Some(permission) = management_permission(&request.method, route_path) else {
        return HttpResponse::new(404, "Not Found", Vec::new()).write_to(&mut stream);
    };

    let allowed_roles = permission.allowed_roles();
    let Some(requester_role) = authorize_management(&state, &request, allowed_roles) else {
        log::warn(format!(
            "rejected unauthorized management request method={} uri={}",
            request.method, request.uri
        ));
        return HttpResponse::new(403, "Unauthorized", Vec::new()).write_to(&mut stream);
    };

    let response = match (request.method.as_str(), route_path) {
        ("GET", "/queue") => routes::queue::get_queue(&state),
        ("GET", "/usage") => routes::usage::get_usage(&state, &request),
        ("GET", "/worker/status") => routes::workers::get_status(&state),
        ("GET", "/worker/connections") => routes::workers::get_connections(&state),
        ("GET", "/worker/pings") => routes::workers::get_pings(&state),
        ("GET", "/worker/tags") => routes::workers::get_tags(&state),
        ("GET", "/worker/versions") => routes::workers::get_versions(&state),
        ("GET", "/key") => routes::keys::get_keys(&state, requester_role),
        ("POST", "/key") => routes::keys::post_key(&state, &request),
        ("PATCH", "/key") => routes::keys::patch_key(&state, &request),
        ("DELETE", "/key") => routes::keys::delete_key(&state, &request),
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

#[derive(Clone, Copy)]
enum ManagementPermission {
    Read,
    Write,
}

impl ManagementPermission {
    fn allowed_roles(self) -> &'static [Role] {
        match self {
            Self::Read => &[Role::Admin, Role::Analytics],
            Self::Write => &[Role::Admin],
        }
    }
}

fn management_permission(method: &str, route_path: &str) -> Option<ManagementPermission> {
    match (method, route_path) {
        ("GET", "/queue")
        | ("GET", "/usage")
        | ("GET", "/worker/status")
        | ("GET", "/worker/connections")
        | ("GET", "/worker/pings")
        | ("GET", "/worker/tags")
        | ("GET", "/worker/versions")
        | ("GET", "/key") => Some(ManagementPermission::Read),
        ("POST", "/key") | ("PATCH", "/key") | ("DELETE", "/key") | ("POST", "/worker/command") => {
            Some(ManagementPermission::Write)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ManagementPermission, management_permission};
    use crate::auth::Role;

    #[test]
    fn analytics_can_read_management_routes() {
        let permission = management_permission("GET", "/usage").expect("usage permission");

        assert!(matches!(permission, ManagementPermission::Read));
        assert!(permission.allowed_roles().contains(&Role::Analytics));
    }

    #[test]
    fn analytics_cannot_write_management_routes() {
        let permission = management_permission("POST", "/key").expect("key permission");

        assert!(matches!(permission, ManagementPermission::Write));
        assert!(!permission.allowed_roles().contains(&Role::Analytics));
        assert!(permission.allowed_roles().contains(&Role::Admin));
    }
}
