#[derive(Clone, Debug)]
pub struct Config {
    pub user_authentication: bool,
    pub proxy_port: u16,
    pub node_connection_port: u16,
    pub management_connection_port: u16,
    pub polling_node_connection_timeout: u64,
    pub working_node_connection_timeout: u64,
    pub proxy_timeout_ms: u64,
    pub message_chunk_buffer_size: usize,
    pub database_url: String,
    pub capture_dir: String,
    pub telegram_bot_token: Option<String>,
    pub telegram_user_id: Option<i64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            user_authentication: false,
            proxy_port: 6666,
            node_connection_port: 7777,
            management_connection_port: 6668,
            polling_node_connection_timeout: 10,
            working_node_connection_timeout: 300,
            proxy_timeout_ms: 60_000,
            message_chunk_buffer_size: 16_384,
            database_url: "sqlite.db".to_string(),
            capture_dir: "data/captures".to_string(),
            telegram_bot_token: None,
            telegram_user_id: None,
        }
    }
}
