//! Telegram notification — fire-and-forget, NEVER blocks mining.
//! Spawns a thread per message. If Telegram is down, messages are silently dropped.

use std::env;
use std::time::Duration;

#[derive(Clone)]
pub struct TelegramBot {
    enabled: bool,
    token: String,
    chat_id: String,
}

impl TelegramBot {
    pub fn from_env() -> Self {
        let enabled = env::var("TELEGRAM_ENABLED")
            .unwrap_or_default()
            .to_lowercase() == "true";
        let token = env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default();
        let chat_id = env::var("TELEGRAM_CHAT_ID").unwrap_or_default();
        Self { enabled, token, chat_id }
    }

    /// Send message — non-blocking. Spawns a thread and returns immediately.
    /// NEVER call this before TX broadcast.
    pub fn send(&self, message: &str) {
        if !self.enabled || self.token.is_empty() || self.chat_id.is_empty() {
            return;
        }

        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        let body = serde_json::json!({
            "chat_id": &self.chat_id,
            "text": message,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        }).to_string();

        std::thread::spawn(move || {
            let client = match reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
            {
                Ok(c) => c,
                Err(_) => return,
            };
            let _ = client.post(&url)
                .header("content-type", "application/json")
                .body(body)
                .send();
        });
    }
}
