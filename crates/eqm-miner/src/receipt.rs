//! Background receipt monitor — polls getSignatureStatuses without blocking mining.
//!
//! Flow:
//!   1. Main thread calls track(signature) after broadcast
//!   2. Background thread polls every 2-3 seconds
//!   3. On confirmed → log + Telegram notify
//!   4. On reverted → log warning + Telegram notify
//!   5. On dropped (60s timeout) → log warning

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use solana_client::rpc_client::RpcClient;
use solana_sdk::signature::Signature;

use crate::telegram::TelegramBot;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const TX_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct PendingTx {
    signature: Signature,
    block_height: u64,
    reward: u64,
    submitted_at: Instant,
}

#[derive(Clone)]
pub struct ReceiptMonitor {
    rpc_urls: Vec<String>,
    telegram: TelegramBot,
    pending: Arc<Mutex<Vec<PendingTx>>>,
}

impl ReceiptMonitor {
    pub fn new(rpc_urls: Vec<String>, telegram: TelegramBot) -> Self {
        let monitor = Self {
            rpc_urls,
            telegram,
            pending: Arc::new(Mutex::new(Vec::new())),
        };

        // Spawn background polling thread
        let monitor_clone = monitor.clone();
        std::thread::spawn(move || {
            monitor_clone.poll_loop();
        });

        monitor
    }

    /// Track a new TX signature for background monitoring.
    /// Non-blocking — returns immediately.
    pub fn track(&self, signature: Signature, block_height: u64, reward: u64) {
        let mut pending = self.pending.lock().unwrap();
        pending.push(PendingTx {
            signature,
            block_height,
            reward,
            submitted_at: Instant::now(),
        });
    }

    fn poll_loop(&self) {
        let rpc = RpcClient::new(self.rpc_urls[0].clone());

        loop {
            std::thread::sleep(POLL_INTERVAL);

            let pending = self.pending.lock().unwrap();
            if pending.is_empty() {
                drop(pending);
                continue;
            }

            let sigs: Vec<Signature> = pending.iter().map(|p| p.signature).collect();
            drop(pending); // Release lock during RPC call

            let statuses = match rpc.get_signature_statuses(&sigs) {
                Ok(response) => response.value,
                Err(_) => continue, // RPC error — try again next poll
            };

            let mut pending = self.pending.lock().unwrap();
            let mut to_remove = Vec::new();

            for (i, status) in statuses.iter().enumerate() {
                if i >= pending.len() { break; }

                match status {
                    Some(s) => {
                        if let Some(err) = &s.err {
                            // TX reverted — fee charged but program error
                            self.telegram.send(&format!(
                                "x TX reverted\nBlock #{}\nError: {:?}",
                                pending[i].block_height, err,
                            ));
                            to_remove.push(i);
                        } else if s.confirmations.unwrap_or(0) > 0 || s.confirmation_status.is_some() {
                            // Confirmed
                            self.telegram.send(&format!(
                                "v TX confirmed\nBlock #{}\n+{} EQM",
                                pending[i].block_height,
                                format_reward(pending[i].reward),
                            ));
                            to_remove.push(i);
                        }
                    }
                    None => {
                        // Still pending — check timeout
                        if pending[i].submitted_at.elapsed() > TX_TIMEOUT {
                            self.telegram.send(&format!(
                                "! TX dropped (timeout)\nBlock #{}",
                                pending[i].block_height,
                            ));
                            to_remove.push(i);
                        }
                    }
                }
            }

            // Remove processed entries (reverse order to maintain indices)
            for &i in to_remove.iter().rev() {
                pending.remove(i);
            }
        }
    }
}

fn format_reward(base_units: u64) -> String {
    let whole = base_units / 1_000_000;
    let frac = base_units % 1_000_000;
    if frac == 0 { format!("{}", whole) }
    else { format!("{}.{:06}", whole, frac).trim_end_matches('0').to_string() }
}
