//! Logger — rapih, non-blocking, execution-first.
//! Style inspired by logger.js (hash256-mine-multirpc).
//! NEVER blocks TX broadcast. All output is AFTER execution.

const C_RESET: &str = "\x1b[0m";
const C_DIM: &str = "\x1b[2m";
const C_BOLD: &str = "\x1b[1m";
const C_ROSE_B: &str = "\x1b[1;35m";
const C_GOLD: &str = "\x1b[33m";
const C_GOLD_B: &str = "\x1b[1;33m";
const C_SAGE_B: &str = "\x1b[1;32m";
const C_TEAL: &str = "\x1b[36m";
const C_GRAY: &str = "\x1b[90m";
const C_RED: &str = "\x1b[31m";
const C_YELLOW: &str = "\x1b[33m";

const LOGO: &str = r#"
   ███████╗ ██████╗ ███╗   ███╗   ██████╗ ██████╗  ██████╗
   ██╔════╝██╔═══██╗████╗ ████║   ██╔══██╗██╔══██╗██╔═══██╗
   █████╗  ██║   ██║██╔████╔██║   ██████╔╝██████╔╝██║   ██║
   ██╔══╝  ██║▄▄ ██║██║╚██╔╝██║   ██╔═══╝ ██╔══██╗██║   ██║
   ███████╗╚██████╔╝██║ ╚═╝ ██║   ██║     ██║  ██║╚██████╔╝
   ╚══════╝ ╚══▀▀═╝ ╚═╝     ╚═╝   ╚═╝     ╚═╝  ╚═╝ ╚═════╝"#;

const RULE: &str = "   ────────────────────────────────────────────────────────────";

pub struct Logger {
    author: String,
}

impl Logger {
    pub fn new(author: &str) -> Self {
        Self { author: author.to_string() }
    }

    fn timestamp(&self) -> String {
        let now = chrono_lite_time();
        format!("{}{}{}", C_GRAY, now, C_RESET)
    }

    pub fn banner(&self) {
        println!("{}{}{}", C_ROSE_B, LOGO, C_RESET);
        println!(
            "   {}multi-threaded solana miner{}       {}by {}{}       {}$EQM{}",
            C_DIM, C_RESET, C_GRAY, self.author, C_RESET, C_GOLD_B, C_RESET
        );
        println!();
    }

    pub fn rule(&self) {
        println!("{}{}{}", C_GRAY, RULE, C_RESET);
    }

    pub fn info(&self, key: &str, value: &str) {
        println!(
            "   {}{:<10}{}  {}{}{}",
            C_DIM, key, C_RESET, C_TEAL, value, C_RESET
        );
    }

    pub fn round(&self, height: u64, reward: &str, target: &[u8; 32]) {
        println!(
            "   {}round #{}{}   {}reward {} EQM{}   {}target 0x{}...{}",
            C_BOLD, height, C_RESET,
            C_DIM, reward, C_RESET,
            C_DIM, hex::encode(&target[..4]), C_RESET,
        );
        println!("{}{}{}", C_GRAY, RULE, C_RESET);
    }

    pub fn no_solution(&self, solve_ms: u64, hashrate: f64) {
        println!(
            "     {}[{}]{}  {}no solution{}  {}{}ms{}  {}{}{}",
            C_GRAY, chrono_lite_time(), C_RESET,
            C_DIM, C_RESET,
            C_DIM, solve_ms, C_RESET,
            C_GOLD, fmt_hr(hashrate), C_RESET,
        );
    }

    pub fn mined(&self, reward: &str, solve_ms: u64, hashrate: f64, sig: &str, rpc_count: usize) {
        println!(
            "     {}[{}]{}  {}v MINED{}  {}+{} EQM{}  {}{}ms{}  {}{}{}",
            C_GRAY, chrono_lite_time(), C_RESET,
            C_SAGE_B, C_RESET,
            C_BOLD, reward, C_RESET,
            C_DIM, solve_ms, C_RESET,
            C_GOLD_B, fmt_hr(hashrate), C_RESET,
        );
        println!(
            "       {}sig {}  broadcast {}x{}",
            C_GRAY, short_sig(sig), rpc_count, C_RESET,
        );
    }

    pub fn total(&self, blocks: u64, total_reward: u64, session_secs: f64) {
        let reward_str = format_reward_base(total_reward);
        println!(
            "   {}total{}  {}{} EQM{}  {}blocks {}{}  {}uptime {}{}",
            C_DIM, C_RESET,
            C_BOLD, reward_str, C_RESET,
            C_DIM, blocks, C_RESET,
            C_DIM, fmt_uptime(session_secs), C_RESET,
        );
        println!();
    }

    pub fn warn(&self, msg: &str) {
        println!(
            "     {}[{}]{}  {}! {}{}",
            C_GRAY, chrono_lite_time(), C_RESET,
            C_YELLOW, msg, C_RESET,
        );
    }

    pub fn error(&self, msg: &str) {
        println!(
            "     {}[{}]{}  {}x {}{}",
            C_GRAY, chrono_lite_time(), C_RESET,
            C_RED, msg, C_RESET,
        );
    }

    pub fn receipt_confirmed(&self, sig: &str, block: u64) {
        println!(
            "     {}[{}]{}  {}v TX confirmed{}  {}block {}{}  {}sig {}{}",
            C_GRAY, chrono_lite_time(), C_RESET,
            C_SAGE_B, C_RESET,
            C_DIM, block, C_RESET,
            C_GRAY, short_sig(sig), C_RESET,
        );
    }

    pub fn receipt_failed(&self, sig: &str) {
        println!(
            "     {}[{}]{}  {}x TX reverted{}  {}sig {}{}",
            C_GRAY, chrono_lite_time(), C_RESET,
            C_RED, C_RESET,
            C_GRAY, short_sig(sig), C_RESET,
        );
    }

    pub fn receipt_dropped(&self, sig: &str) {
        println!(
            "     {}[{}]{}  {}! TX dropped{}  {}sig {}{}",
            C_GRAY, chrono_lite_time(), C_RESET,
            C_YELLOW, C_RESET,
            C_GRAY, short_sig(sig), C_RESET,
        );
    }
}

fn chrono_lite_time() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

fn short_sig(s: &str) -> String {
    if s.len() <= 12 { return s.to_string(); }
    format!("{}...{}", &s[..6], &s[s.len() - 6..])
}

fn format_reward_base(base_units: u64) -> String {
    let whole = base_units / 1_000_000;
    let frac = base_units % 1_000_000;
    if frac == 0 { format!("{}", whole) }
    else { format!("{}.{:06}", whole, frac).trim_end_matches('0').to_string() }
}

fn fmt_hr(hashes_per_sec: f64) -> String {
    if hashes_per_sec >= 1000.0 { format!("{:.1} kH/s", hashes_per_sec / 1000.0) }
    else { format!("{:.1} H/s", hashes_per_sec) }
}

fn fmt_uptime(seconds: f64) -> String {
    let total = seconds as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 { format!("{}:{:02}:{:02}", h, m, s) }
    else { format!("{}:{:02}", m, s) }
}
