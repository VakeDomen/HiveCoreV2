use std::fs;
use std::io;
use std::path::Path;

use crate::app::models::config::Config;

impl Config {
    pub fn load_or_create(path: &str) -> io::Result<Self> {
        let path_ref = Path::new(path);
        if !path_ref.exists() {
            let default = Self::default();
            fs::write(path_ref, default.to_ini())?;
            return Ok(default);
        }

        let contents = fs::read_to_string(path_ref)?;
        Ok(Self::from_ini(&contents))
    }

    fn from_ini(contents: &str) -> Self {
        let mut config = Self::default();
        let mut section = String::new();

        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].to_string();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();

            match (section.as_str(), key) {
                ("Server", "USER_AUTHENTICATION") => {
                    config.user_authentication = value.eq_ignore_ascii_case("true")
                }
                ("Server", "PROXY_PORT") => {
                    if let Ok(parsed) = value.parse() {
                        config.proxy_port = parsed;
                    }
                }
                ("Server", "NODE_CONNECTION_PORT") => {
                    if let Ok(parsed) = value.parse() {
                        config.node_connection_port = parsed;
                    }
                }
                ("Server", "MANAGEMENT_CONNECTION_PORT") => {
                    if let Ok(parsed) = value.parse() {
                        config.management_connection_port = parsed;
                    }
                }
                ("Connection", "POLLING_NODE_CONNECTION_TIMEOUT") => {
                    if let Ok(parsed) = value.parse() {
                        config.polling_node_connection_timeout = parsed;
                    }
                }
                ("Connection", "WORKING_NODE_CONNECTION_TIMEOUT") => {
                    if let Ok(parsed) = value.parse() {
                        config.working_node_connection_timeout = parsed;
                    }
                }
                ("Connection", "PROXY_TIMEOUT_MS") => {
                    if let Ok(parsed) = value.parse() {
                        config.proxy_timeout_ms = parsed;
                    }
                }
                ("Connection", "MESSAGE_CHUNK_BUFFER_SIZE") => {
                    if let Ok(parsed) = value.parse() {
                        config.message_chunk_buffer_size = parsed;
                    }
                }
                ("Database", "DATABASE_URL") => {
                    config.database_url = normalize_database_url(value);
                }
                _ => {}
            }
        }

        config
    }

    fn to_ini(&self) -> String {
        format!(
            "[Server]\nUSER_AUTHENTICATION = {}\nPROXY_PORT = {}\nNODE_CONNECTION_PORT = {}\nMANAGEMENT_CONNECTION_PORT = {}\n\n[Connection]\nPOLLING_NODE_CONNECTION_TIMEOUT = {}\nWORKING_NODE_CONNECTION_TIMEOUT = {}\nPROXY_TIMEOUT_MS = {}\nMESSAGE_CHUNK_BUFFER_SIZE = {}\n\n[Database]\nDATABASE_URL = {}\n",
            self.user_authentication,
            self.proxy_port,
            self.node_connection_port,
            self.management_connection_port,
            self.polling_node_connection_timeout,
            self.working_node_connection_timeout,
            self.proxy_timeout_ms,
            self.message_chunk_buffer_size,
            self.database_url
        )
    }
}

fn normalize_database_url(value: &str) -> String {
    value
        .strip_prefix("jdbc:sqlite:")
        .unwrap_or(value)
        .trim()
        .to_string()
}
