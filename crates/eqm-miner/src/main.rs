//! Equium Miner Pro — Optimized multi-threaded $EQM miner.
//!
//! Modes:
//!   --mode cpu    Single-thread reference solver (proven, safe)
//!   --mode turbo  Multi-thread rayon solver (default, fastest CPU)
//!   --mode gpu    CUDA Equihash(96,5) kernel (coming soon)
//!
//! Principles:
//!   1. TX broadcast FIRST, log AFTER
//!   2. Telegram = fire-and-forget (never blocks)
//!   3. Receipt monitor = background thread
//!   4. Multi-RPC = parallel send_transaction (not send_and_confirm)

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use equihash_core::challenge::{build_input, solution_hash, I_LEN};
use equihash_core::solver::{solve, Solution};
use equihash_core::target::hash_under_target;
use rayon::prelude::*;
use rand::RngCore;
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{read_keypair_file, Keypair, Signature, Signer};
use solana_sdk::system_program;
use solana_sdk::sysvar;
use solana_sdk::transaction::Transaction;

mod logger;
mod receipt;
mod telegram;

use logger::Logger;
use receipt::ReceiptMonitor;
use telegram::TelegramBot;

// Equium program ID (mainnet verified)
const EQUIUM_PROGRAM_ID: &str = "ZKGMUfxiRCXFPnqz9zgqAnuqJy15jk7fKbR4o6FuEQM";
const CONFIG_SEED: &[u8] = b"equium-config";
const VAULT_SEED: &[u8] = b"equium-vault";

#[derive(Debug, Clone, ValueEnum)]
enum MineMode {
    /// Single-thread reference solver (proven, safe)
    Cpu,
    /// Multi-thread rayon solver (default, fastest CPU)
    Turbo,
    /// CUDA GPU kernel (coming soon)
    Gpu,
}

#[derive(Parser, Debug)]
#[command(version, about = "Equium Miner Pro — Multi-threaded $EQM miner")]
struct Args {
    /// Mining mode
    #[arg(long, env = "MINE_MODE", default_value = "turbo")]
    mode: MineMode,

    /// Primary RPC endpoint URL
    #[arg(long, env = "RPC_URL", default_value = "https://rpc.ankr.com/sol")]
    rpc_url: String,

    /// Additional RPC URLs for TX broadcast (comma-separated)
    #[arg(long, env = "RPC_URLS")]
    rpc_urls: Option<String>,

    /// Path to keypair JSON
    #[arg(long, env = "KEYPAIR_PATH")]
    keypair: PathBuf,

    /// Number of solver threads (0 = auto-detect CPU cores). Only for turbo mode.
    #[arg(long, env = "THREADS", default_value_t = 0)]
    threads: usize,

    /// Max nonce attempts per round before re-fetching state
    #[arg(long, env = "MAX_NONCES_PER_ROUND", default_value_t = 16384)]
    max_nonces_per_round: u64,

    /// Compute-unit limit per mine TX
    #[arg(long, env = "CU_LIMIT", default_value_t = 1_400_000)]
    cu_limit: u32,

    /// Stop after N successful blocks (0 = run forever)
    #[arg(long, default_value_t = 0)]
    max_blocks: u64,

    /// Override program ID
    #[arg(long)]
    program_id: Option<String>,
}

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args = Args::parse();

    // GPU mode — placeholder
    if matches!(args.mode, MineMode::Gpu) {
        println!("\n   CUDA GPU mode is not yet implemented.");
        println!("   Equihash(96,5) CUDA kernel is planned for a future release.");
        println!("   Use --mode turbo for multi-threaded CPU mining.\n");
        std::process::exit(0);
    }

    let program_id: Pubkey = match &args.program_id {
        Some(s) => Pubkey::from_str(s).context("invalid --program-id")?,
        None => Pubkey::from_str(EQUIUM_PROGRAM_ID).unwrap(),
    };

    let miner_kp = read_keypair_file(&args.keypair)
        .map_err(|e| anyhow!("read keypair {}: {}", args.keypair.display(), e))?;
    let miner = miner_kp.pubkey();

    // Thread count
    let num_threads = match args.mode {
        MineMode::Cpu => 1,
        MineMode::Turbo => if args.threads == 0 { num_cpus() } else { args.threads },
        MineMode::Gpu => 1, // placeholder
    };

    rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build_global()
        .unwrap_or(());

    // RPC clients
    let rpc_urls = parse_rpc_urls(&args.rpc_url, &args.rpc_urls);
    let primary_rpc = RpcClient::new_with_commitment(
        args.rpc_url.clone(),
        CommitmentConfig::confirmed(),
    );
    let broadcast_rpcs: Vec<RpcClient> = rpc_urls.iter()
        .map(|url| RpcClient::new_with_commitment(url.clone(), CommitmentConfig::confirmed()))
        .collect();

    let (config_pda, _) = Pubkey::find_program_address(&[CONFIG_SEED], &program_id);
    let (vault_pda, _) = Pubkey::find_program_address(&[VAULT_SEED], &program_id);

    // Logger + Telegram
    let log = Logger::new("hilmiariprahman");
    let tg = TelegramBot::from_env();
    let receipt_monitor = ReceiptMonitor::new(rpc_urls.clone(), tg.clone());

    let mode_label = match args.mode {
        MineMode::Cpu => "cpu (single-thread)",
        MineMode::Turbo => "turbo (multi-thread)",
        MineMode::Gpu => "gpu (cuda)",
    };

    // Boot
    log.banner();
    log.info("miner", &short_pk(&miner));
    log.info("program", &short_pk(&program_id));
    log.info("mode", mode_label);
    log.info("threads", &num_threads.to_string());
    log.info("RPCs", &broadcast_rpcs.len().to_string());
    log.info("primary", &args.rpc_url);
    log.rule();

    tg.send(&format!(
        "Miner started\nWallet: {}\nMode: {}\nThreads: {}\nRPCs: {}",
        short_pk(&miner), mode_label, num_threads, broadcast_rpcs.len()
    ));

    // Detect token program
    let token_program_id = {
        let cfg = fetch_config(&primary_rpc, &config_pda)?;
        let mint_acct = primary_rpc.get_account(&cfg.mint)?;
        mint_acct.owner
    };

    let mut blocks_mined = 0u64;
    let total_nonces = Arc::new(AtomicU64::new(0));
    let started_at = Instant::now();
    let mut current_height: u64 = u64::MAX;

    loop {
        let cfg = match fetch_config(&primary_rpc, &config_pda) {
            Ok(c) => c,
            Err(e) => {
                log.warn(&format!("RPC error: {} — retry 3s", e));
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
        };
        let miner_ata = derive_ata(&miner, &cfg.mint, &token_program_id);

        if cfg.block_height != current_height {
            current_height = cfg.block_height;
            println!();
            log.round(cfg.block_height, &format_reward(cfg.current_epoch_reward), &cfg.current_target);
        }

        // === SOLVE ===
        let solve_started = Instant::now();
        let input = build_input(&cfg.current_challenge, &miner.to_bytes(), cfg.block_height);

        let solution = match args.mode {
            MineMode::Cpu => solve_single_thread(&input, cfg.equihash_n, cfg.equihash_k, &cfg.current_target, args.max_nonces_per_round, &total_nonces),
            MineMode::Turbo => solve_multi_thread(&input, cfg.equihash_n, cfg.equihash_k, &cfg.current_target, args.max_nonces_per_round, num_threads, &total_nonces),
            MineMode::Gpu => unreachable!(),
        };

        let solve_ms = solve_started.elapsed().as_millis() as u64;
        let session_secs = started_at.elapsed().as_secs_f64().max(0.001);
        let hashrate = total_nonces.load(Ordering::Relaxed) as f64 / session_secs;

        let solution = match solution {
            Some(s) => s,
            None => {
                log.no_solution(solve_ms, hashrate);
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };

        // === BROADCAST (parallel, non-blocking confirm) ===
        // Sign TX once
        let tx = build_mine_tx(
            &broadcast_rpcs[0], &miner_kp, &program_id, &config_pda, &cfg,
            &vault_pda, &miner_ata, &token_program_id, &solution.nonce,
            &solution.soln_indices, args.cu_limit,
        );

        let tx = match tx {
            Ok(t) => t,
            Err(e) => {
                log.warn(&format!("TX build failed: {}", e));
                continue;
            }
        };

        // Parallel broadcast — send_transaction WITHOUT confirm
        let sig = parallel_broadcast(&broadcast_rpcs, &tx);

        match sig {
            Some(signature) => {
                // MINED — log AFTER broadcast is done
                blocks_mined += 1;
                let sig_str = signature.to_string();

                log.mined(&format_reward(cfg.current_epoch_reward), solve_ms, hashrate, &sig_str, broadcast_rpcs.len());
                log.total(blocks_mined, cfg.current_epoch_reward * blocks_mined, session_secs);

                // Background receipt monitor (non-blocking)
                receipt_monitor.track(signature, cfg.block_height, cfg.current_epoch_reward);

                tg.send(&format!(
                    "MINED +{} EQM\nBlock #{}\nSig: {}\nTotal: {} blocks\n{}",
                    format_reward(cfg.current_epoch_reward),
                    cfg.block_height, short_sig(&sig_str),
                    blocks_mined, fmt_hashrate(hashrate),
                ));
            }
            None => {
                log.warn("All RPCs rejected TX");
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        }

        if args.max_blocks > 0 && blocks_mined >= args.max_blocks {
            log.info("session", &format!("complete — {} blocks mined", blocks_mined));
            tg.send(&format!("Session complete — {} blocks mined", blocks_mined));
            return Ok(());
        }
    }
}

// ============================================================================
// Solvers
// ============================================================================

fn solve_single_thread(
    input: &[u8; I_LEN],
    n: u32, k: u32,
    target: &[u8; 32],
    max_nonces: u64,
    total_nonces: &Arc<AtomicU64>,
) -> Option<Solution> {
    let mut rng = rand::thread_rng();
    let mut counter = 0u64;

    let result = solve(n, k, input, || {
        if counter >= max_nonces { return None; }
        counter += 1;
        total_nonces.fetch_add(1, Ordering::Relaxed);
        let mut nonce = [0u8; 32];
        rng.fill_bytes(&mut nonce);
        Some(nonce)
    });

    match result {
        Ok(sol) => {
            let cand_hash = solution_hash(&sol.soln_indices, input);
            if hash_under_target(&cand_hash, target) { Some(sol) } else { None }
        }
        Err(_) => None,
    }
}

fn solve_multi_thread(
    input: &[u8; I_LEN],
    n: u32, k: u32,
    target: &[u8; 32],
    max_nonces: u64,
    num_threads: usize,
    total_nonces: &Arc<AtomicU64>,
) -> Option<Solution> {
    let found = Arc::new(AtomicBool::new(false));
    let max_per_thread = max_nonces / num_threads as u64;

    (0..num_threads).into_par_iter().find_map_any(|_| {
        let found_ref = Arc::clone(&found);
        let nonces_ref = Arc::clone(total_nonces);
        let mut rng = rand::thread_rng();
        let mut counter = 0u64;

        let result = solve(n, k, input, || {
            if found_ref.load(Ordering::Relaxed) || counter >= max_per_thread { return None; }
            counter += 1;
            nonces_ref.fetch_add(1, Ordering::Relaxed);
            let mut nonce = [0u8; 32];
            rng.fill_bytes(&mut nonce);
            Some(nonce)
        });

        match result {
            Ok(sol) => {
                let cand_hash = solution_hash(&sol.soln_indices, input);
                if hash_under_target(&cand_hash, target) {
                    found_ref.store(true, Ordering::Relaxed);
                    Some(sol)
                } else { None }
            }
            Err(_) => None,
        }
    })
}

// ============================================================================
// TX Build + Parallel Broadcast
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn build_mine_tx(
    rpc: &RpcClient,
    miner_kp: &Keypair,
    program_id: &Pubkey,
    config_pda: &Pubkey,
    cfg: &EquiumConfig,
    vault_pda: &Pubkey,
    miner_ata: &Pubkey,
    token_program_id: &Pubkey,
    nonce: &[u8; 32],
    soln_indices: &[u8],
    cu_limit: u32,
) -> Result<Transaction> {
    let miner = miner_kp.pubkey();

    let accounts = vec![
        AccountMeta::new(miner, true),
        AccountMeta::new(*config_pda, false),
        AccountMeta::new_readonly(cfg.mint, false),
        AccountMeta::new(*vault_pda, false),
        AccountMeta::new(*miner_ata, false),
        AccountMeta::new_readonly(*token_program_id, false),
        AccountMeta::new_readonly(anchor_spl::associated_token::ID, false),
        AccountMeta::new_readonly(system_program::ID, false),
        AccountMeta::new_readonly(sysvar::slot_hashes::ID, false),
    ];

    let data = build_mine_instruction_data(nonce, soln_indices);
    let ix = Instruction { program_id: *program_id, accounts, data };
    let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(cu_limit);

    let recent = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &[cu_ix, ix], Some(&miner), &[miner_kp], recent,
    );
    Ok(tx)
}

fn build_mine_instruction_data(nonce: &[u8; 32], soln_indices: &[u8]) -> Vec<u8> {
    use sha2::{Sha256, Digest};
    let hash = Sha256::digest(b"global:mine");
    let mut data = Vec::with_capacity(8 + 32 + 4 + soln_indices.len());
    data.extend_from_slice(&hash[..8]); // Anchor discriminator
    data.extend_from_slice(nonce);
    data.extend_from_slice(&(soln_indices.len() as u32).to_le_bytes());
    data.extend_from_slice(soln_indices);
    data
}

/// Broadcast signed TX to ALL RPCs in parallel using threads.
/// Returns the first successful signature, or None if all fail.
/// Uses send_transaction (NO confirm) — receipt checked in background.
fn parallel_broadcast(rpcs: &[RpcClient], tx: &Transaction) -> Option<Signature> {
    use std::sync::Mutex;

    let result: Arc<Mutex<Option<Signature>>> = Arc::new(Mutex::new(None));

    std::thread::scope(|s| {
        for rpc in rpcs {
            let result_ref = Arc::clone(&result);
            s.spawn(move || {
                // send_transaction = fire without waiting for confirm
                match rpc.send_transaction(tx) {
                    Ok(sig) => {
                        let mut lock = result_ref.lock().unwrap();
                        if lock.is_none() {
                            *lock = Some(sig);
                        }
                    }
                    Err(_) => {} // RPC rejected — likely "already processed" or error
                }
            });
        }
    });

    Arc::try_unwrap(result).ok()?.into_inner().ok()?
}

// ============================================================================
// Config Fetch
// ============================================================================

#[derive(Debug)]
struct EquiumConfig {
    mint: Pubkey,
    equihash_n: u32,
    equihash_k: u32,
    current_target: [u8; 32],
    block_height: u64,
    current_challenge: [u8; 32],
    current_epoch_reward: u64,
}

fn fetch_config(rpc: &RpcClient, config_pda: &Pubkey) -> Result<EquiumConfig> {
    let acct = rpc.get_account(config_pda)?;
    let data = &acct.data;

    if data.len() < 200 {
        return Err(anyhow!("config account too small: {} bytes", data.len()));
    }

    let mut offset = 8; // skip Anchor discriminator

    let mint = Pubkey::try_from(&data[offset..offset + 32]).unwrap();
    offset += 32;
    offset += 32; // mineable_vault
    offset += 1;  // mineable_vault_bump
    offset += 1;  // config_bump
    offset += 8;  // genesis_slot
    offset += 8;  // genesis_unix_ts
    let equihash_n = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    offset += 4;
    let equihash_k = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    offset += 4;
    let current_target: [u8; 32] = data[offset..offset + 32].try_into().unwrap();
    offset += 32;
    let block_height = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let current_challenge: [u8; 32] = data[offset..offset + 32].try_into().unwrap();
    offset += 32;
    offset += 8;  // current_round_open_slot
    offset += 8;  // current_round_open_unix_ts
    offset += 32; // last_winner
    let current_epoch_reward = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());

    Ok(EquiumConfig {
        mint, equihash_n, equihash_k, current_target,
        block_height, current_challenge, current_epoch_reward,
    })
}

// ============================================================================
// Helpers
// ============================================================================

fn derive_ata(owner: &Pubkey, mint: &Pubkey, token_program_id: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(owner, mint, token_program_id)
}

fn parse_rpc_urls(primary: &str, extra: &Option<String>) -> Vec<String> {
    let mut urls: Vec<String> = vec![primary.to_string()];
    if let Some(extra_str) = extra {
        for url in extra_str.split(',') {
            let url = url.trim().to_string();
            if !url.is_empty() && !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    urls
}

fn num_cpus() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}

fn short_pk(pk: &Pubkey) -> String {
    let s = pk.to_string();
    format!("{}...{}", &s[..4], &s[s.len() - 4..])
}

fn short_sig(s: &str) -> String {
    if s.len() <= 12 { return s.to_string(); }
    format!("{}...{}", &s[..6], &s[s.len() - 6..])
}

fn format_reward(base_units: u64) -> String {
    let whole = base_units / 1_000_000;
    let frac = base_units % 1_000_000;
    if frac == 0 { format!("{}", whole) }
    else { format!("{}.{:06}", whole, frac).trim_end_matches('0').to_string() }
}

fn fmt_hashrate(hashes_per_sec: f64) -> String {
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
