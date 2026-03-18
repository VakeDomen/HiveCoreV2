mod auth;
mod config;
mod db;
mod http;
mod log;
mod server;
mod state;

use std::io;
use std::sync::Arc;
use std::thread;

use config::Config;
use state::AppState;

fn main() -> io::Result<()> {
    let config = Config::load_or_create("config.ini")?;
    log::info(format!(
        "starting hive_core_v2 proxy_port={} worker_port={} management_port={} database_url={}",
        config.proxy_port,
        config.node_connection_port,
        config.management_connection_port,
        config.database_url
    ));
    let state = Arc::new(AppState::new(config.clone())?);

    let client_state = Arc::clone(&state);
    let worker_state = Arc::clone(&state);
    let management_state = Arc::clone(&state);
    let overseer_state = Arc::clone(&state);

    let client_thread = thread::spawn(move || server::client::run(client_state));
    let worker_thread = thread::spawn(move || server::worker::run(worker_state));
    let management_thread = thread::spawn(move || server::management::run(management_state));
    let overseer_thread = thread::spawn(move || state::run_overseer(overseer_state));

    join_server("client", client_thread)?;
    join_server("worker", worker_thread)?;
    join_server("management", management_thread)?;
    join_server("overseer", overseer_thread)?;

    Ok(())
}

fn join_server(name: &str, handle: thread::JoinHandle<io::Result<()>>) -> io::Result<()> {
    match handle.join() {
        Ok(result) => result,
        Err(_) => {
            log::error(format!("{name} thread panicked"));
            Err(io::Error::other(format!("{name} thread panicked")))
        }
    }
}
