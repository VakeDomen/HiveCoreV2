use std::collections::HashMap;

#[derive(Clone, Debug, serde::Serialize)]
pub struct RateLimitTier {
    /// 0 means unlimited for that window.
    pub requests_per_minute: u64,
    pub requests_per_hour: u64,
    pub requests_per_day: u64,
    pub requests_per_week: u64,
    pub requests_per_month: u64,
    /// Max concurrent in-flight requests. 0 = unlimited.
    pub max_concurrent: u64,
}

impl RateLimitTier {
    pub fn unlimited() -> Self {
        Self {
            requests_per_minute: 0,
            requests_per_hour: 0,
            requests_per_day: 0,
            requests_per_week: 0,
            requests_per_month: 0,
            max_concurrent: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub user_authentication: bool,
    pub proxy_port: u16,
    pub node_connection_port: u16,
    pub management_connection_port: u16,
    pub polling_node_connection_timeout: u64,
    pub working_node_connection_timeout: u64,
    /// Seconds after which a node-reported latency is considered stale.
    pub latency_report_timeout: u64,
    pub proxy_timeout_ms: u64,
    pub message_chunk_buffer_size: usize,
    pub database_url: String,
    pub capture_dir: String,
    pub telegram_bot_token: Option<String>,
    pub telegram_user_id: Option<i64>,
    pub telegram_message_thread_id: Option<i64>,
    pub rate_limit_tiers: HashMap<String, RateLimitTier>,
    pub default_rate_limit_tier: String,
}

impl Default for Config {
    fn default() -> Self {
        let mut rate_limit_tiers = HashMap::new();
        rate_limit_tiers.insert(
            "low".to_string(),
            RateLimitTier {
                requests_per_minute: 20,
                requests_per_hour: 100,
                requests_per_day: 500,
                requests_per_week: 2_000,
                requests_per_month: 8_000,
                max_concurrent: 3,
            },
        );
        rate_limit_tiers.insert(
            "medium".to_string(),
            RateLimitTier {
                requests_per_minute: 60,
                requests_per_hour: 300,
                requests_per_day: 1_500,
                requests_per_week: 6_000,
                requests_per_month: 24_000,
                max_concurrent: 8,
            },
        );
        rate_limit_tiers.insert(
            "high".to_string(),
            RateLimitTier {
                requests_per_minute: 200,
                requests_per_hour: 1_000,
                requests_per_day: 5_000,
                requests_per_week: 20_000,
                requests_per_month: 80_000,
                max_concurrent: 25,
            },
        );
        rate_limit_tiers.insert("unlimited".to_string(), RateLimitTier::unlimited());
        Self {
            user_authentication: false,
            proxy_port: 6666,
            node_connection_port: 7777,
            management_connection_port: 6668,
            polling_node_connection_timeout: 10,
            working_node_connection_timeout: 300,
            latency_report_timeout: 30,
            proxy_timeout_ms: 60_000,
            message_chunk_buffer_size: 16_384,
            database_url: "sqlite.db".to_string(),
            capture_dir: "data/captures".to_string(),
            telegram_bot_token: None,
            telegram_user_id: None,
            telegram_message_thread_id: None,
            rate_limit_tiers,
            default_rate_limit_tier: "low".to_string(),
        }
    }
}
