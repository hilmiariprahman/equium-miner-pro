//! Configuration loading from .env and CLI args.

pub struct MinerConfig {
    pub rpc_url: String,
    pub rpc_urls: Vec<String>,
    pub keypair_path: String,
    pub threads: usize,
    pub max_nonces_per_round: u64,
    pub cu_limit: u32,
    pub telegram_enabled: bool,
    pub telegram_bot_token: Option<String>,
    pub telegram_chat_id: Option<String>,
}
