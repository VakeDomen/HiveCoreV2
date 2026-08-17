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
                ("Capture", "CAPTURE_DIR") => {
                    config.capture_dir = value.to_string();
                }
                ("Telegram", "BOT_TOKEN") => {
                    config.telegram_bot_token = normalize_optional_string(value);
                }
                ("Telegram", "USER_ID") => {
                    let target = parse_telegram_target(value);
                    config.telegram_user_id = target.map(|target| target.0);
                    config.telegram_message_thread_id = target.and_then(|target| target.1);
                }
                ("Telegram", "MESSAGE_THREAD_ID") => {
                    config.telegram_message_thread_id = value.parse::<i64>().ok();
                }
                _ => {}
            }
        }

        config
    }

    fn to_ini(&self) -> String {
        format!(
            "[Server]\nUSER_AUTHENTICATION = {}\nPROXY_PORT = {}\nNODE_CONNECTION_PORT = {}\nMANAGEMENT_CONNECTION_PORT = {}\n\n[Connection]\nPOLLING_NODE_CONNECTION_TIMEOUT = {}\nWORKING_NODE_CONNECTION_TIMEOUT = {}\nPROXY_TIMEOUT_MS = {}\nMESSAGE_CHUNK_BUFFER_SIZE = {}\n\n[Database]\nDATABASE_URL = {}\n\n[Capture]\nCAPTURE_DIR = {}\n\n[Telegram]\nBOT_TOKEN = {}\nUSER_ID = {}\n",
            self.user_authentication,
            self.proxy_port,
            self.node_connection_port,
            self.management_connection_port,
            self.polling_node_connection_timeout,
            self.working_node_connection_timeout,
            self.proxy_timeout_ms,
            self.message_chunk_buffer_size,
            self.database_url,
            self.capture_dir,
            self.telegram_bot_token.as_deref().unwrap_or(""),
            format_telegram_target(self.telegram_user_id, self.telegram_message_thread_id)
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

fn normalize_optional_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn parse_telegram_target(value: &str) -> Option<(i64, Option<i64>)> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Some((chat_id, thread_id)) = trimmed.rsplit_once(':') else {
        return trimmed.parse::<i64>().ok().map(|chat_id| (chat_id, None));
    };
    Some((
        chat_id.trim().parse::<i64>().ok()?,
        Some(thread_id.trim().parse::<i64>().ok()?),
    ))
}

fn format_telegram_target(chat_id: Option<i64>, message_thread_id: Option<i64>) -> String {
    match (chat_id, message_thread_id) {
        (Some(chat_id), Some(thread_id)) => format!("{chat_id}:{thread_id}"),
        (Some(chat_id), None) => chat_id.to_string(),
        (None, _) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, parse_telegram_target};

    #[test]
    fn parses_plain_telegram_chat_id() {
        assert_eq!(parse_telegram_target("12345"), Some((12345, None)));
    }

    #[test]
    fn parses_telegram_topic_target() {
        assert_eq!(
            parse_telegram_target("-1003996209253:2"),
            Some((-1003996209253, Some(2)))
        );
    }

    #[test]
    fn config_loads_telegram_topic_from_user_id() {
        let config =
            Config::from_ini("[Telegram]\nBOT_TOKEN = token\nUSER_ID = -1003996209253:2\n");

        assert_eq!(config.telegram_bot_token.as_deref(), Some("token"));
        assert_eq!(config.telegram_user_id, Some(-1003996209253));
        assert_eq!(config.telegram_message_thread_id, Some(2));
    }
}
