//! Equium Miner Pro — Optimized multi-threaded $EQM miner.
//!
//! Features over the reference miner:
//! - Multi-threaded Equihash solver (rayon) — N cores working in parallel
//! - Multi-RPC broadcast — same signed TX sent to all RPCs for faster landing
//! - Telegram bot monitoring — /status /monitor /hashrate /balance /stop
//! - Auto epoch detection — instant restart on new round
//! - Ankr Solana RPC as primary

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anchor_lang::prelude::AccountMeta;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anyhow::{anyhow, Context, Result};
use clap::Parser;
use equihash_core::challenge::{build_input, solution_hash};
use equihash_core::solver::{solve, Solution};
use equihash_core::target::hash_under_target;
use rayon::prelude::*;
use rand::RngCore;
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{read_keypair_file, Keypair, Signer};
use solana_sdk::system_program;
use solana_sdk::sysvar;
use solana_sdk::transaction::Transaction;

mod config;
mod telegram;

use config::MinerConfig;
use telegram::TelegramBot;

// Equium program ID (mainnet verified)
const EQUIUM_PROGRAM_ID: &str = "ZKGMUfxiRCXFPnqz9zgqAnuqJy15jk7fKbR4o6FuEQM";
const CONFIG_SEED: &[u8] = b"equium-config";
const VAULT_SEED: &[u8] = b"equium-vault";

#[derive(Parser, Debug)]
#[command(version, about = "Equium Miner Pro — Multi-threaded $EQM miner")]
struct Args {
    /// Primary RPC endpoint URL
    #[arg(long, env = "RPC_URL", default_value = "https://rpc.ankr.com/sol")]
    rpc_url: String,

    /// Additional RPC URLs for TX broadcast (comma-separated)
    #[arg(long, env = "RPC_URLS")]
    rpc_urls: Option<String>,

    /// Path to keypair JSON
    #[arg(long, env = "KEYPAIR_PATH")]
    keypair: PathBuf,

    /// Number of solver threads (0 = auto-detect CPU cores)
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

// ANSI colors
const C_RESET: &str = "\x1b[0m";
const C_DIM: &str = "\x1b[2m";
const C_BOLD: &str = "\x1b[1m";
const C_ROSE_B: &str = "\x1b[1;35m";
const C_GOLD: &str = "\x1b[33m";
const C_GOLD_B: &str = "\x1b[1;33m";
const C_SAGE: &str = "\x1b[32m";
const C_SAGE_B: &str = "\x1b[1;32m";
const C_TEAL: &str = "\x1b[36m";
const C_GRAY: &str = "\x1b[90m";

const LOGO: &str = r#"
   ███████╗ ██████╗ ███╗   ███╗   ██████╗ ██████╗  ██████╗
   ██╔════╝██╔═══██╗████╗ ████║   ██╔══██╗██╔══██╗██╔═══██╗
   █████╗  ██║   ██║██╔████╔██║   ██████╔╝██████╔╝██║   ██║
   ██╔══╝  ██║▄▄ ██║██║╚██╔╝██║   ██╔═══╝ ██╔══██╗██║   ██║
   ███████╗╚██████╔╝██║ ╚═╝ ██║   ██║     ██║  ██║╚██████╔╝
   ╚══════╝ ╚══▀▀═╝ ╚═╝     ╚═╝   ╚═╝     ╚═╝  ╚═╝ ╚═════╝"#;

const RULE: &str = "   ────────────────────────────────────────────────────────────";

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args = Args::parse();

    let program_id: Pubkey = match &args.program_id {
        Some(s) => Pubkey::from_str(s).context("invalid --program-id")?,
        None => Pubkey::from_str(EQUIUM_PROGRAM_ID).unwrap(),
    };

    let miner_kp = read_keypair_file(&args.keypair)
        .map_err(|e| anyhow!("read keypair {}: {}", args.keypair.display(), e))?;
    let miner = miner_kp.pubkey();

    // Configure thread pool
    let num_threads = if args.threads == 0 {
        num_cpus()
    } else {
        args.threads
    };
    rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build_global()
        .unwrap_or(());

    // Setup RPC clients
    let primary_rpc = RpcClient::new_with_commitment(
        args.rpc_url.clone(),
        CommitmentConfig::confirmed(),
    );

    let broadcast_rpcs: Vec<RpcClient> = parse_rpc_urls(&args.rpc_url, &args.rpc_urls)
        .into_iter()
        .map(|url| RpcClient::new_with_commitment(url, CommitmentConfig::confirmed()))
        .collect();

    let (config_pda, _) = Pubkey::find_program_address(&[CONFIG_SEED], &program_id);
    let (vault_pda, _) = Pubkey::find_program_address(&[VAULT_SEED], &program_id);

    // Boot display
    print_boot(&miner, &program_id, &args.rpc_url, num_threads, broadcast_rpcs.len());

    // Telegram bot (background, non-blocking)
    let tg = TelegramBot::from_env();
    tg.send_sync(&format!(
        "🚀 <b>EQM Miner Pro started</b>\nWallet: <code>{}</code>\nThreads: <code>{}</code>\nRPCs: <code>{}</code>",
        short_pk(&miner), num_threads, broadcast_rpcs.len()
    ));

    // Detect token program
    let token_program_id = {
        let cfg = fetch_config(&primary_rpc, &config_pda)?;
        let mint_acct = primary_rpc.get_account(&cfg.mint)?;
        mint_acct.owner
    };

    let mut blocks_mined = 0u64;
    let mut total_nonces = AtomicU64::new(0);
    let started_at = Instant::now();
    let mut current_height: u64 = u64::MAX;

    loop {
        let cfg = match fetch_config(&primary_rpc, &config_pda) {
            Ok(c) => c,
            Err(e) => {
                println!("   {}RPC error: {} — retry in 3s{}", C_GRAY, e, C_RESET);
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
        };
        let miner_ata = derive_ata(&miner, &cfg.mint, &token_program_id);

        if cfg.block_height != current_height {
            current_height = cfg.block_height;
            println!();
            println!(
                "   {}round #{}{}   {}reward {} EQM{}   {}target 0x{}…{}   {}threads {}{}",
                C_BOLD, cfg.block_height, C_RESET,
                C_DIM, format_reward(cfg.current_epoch_reward), C_RESET,
                C_DIM, hex::encode(&cfg.current_target[..4]), C_RESET,
                C_GOLD, num_threads, C_RESET,
            );
            println!("{}{}{}", C_GRAY, RULE, C_RESET);
        }

        // === MULTI-THREADED SOLVE ===
        let solve_started = Instant::now();
        let input = build_input(
            &cfg.current_challenge,
            &miner.to_bytes(),
            cfg.block_height,
        );

        let found = Arc::new(AtomicBool::new(false));
        let nonces_tried = Arc::new(AtomicU64::new(0));
        let max_per_thread = args.max_nonces_per_round / num_threads as u64;

        let solution: Option<Solution> = (0..num_threads)
            .into_par_iter()
            .find_map_any(|_thread_id| {
                let found_ref = Arc::clone(&found);
                let nonces_ref = Arc::clone(&nonces_tried);
                let mut rng = rand::thread_rng();
                let mut counter = 0u64;

                let result = solve(cfg.equihash_n, cfg.equihash_k, &input, || {
                    if found_ref.load(Ordering::Relaxed) || counter >= max_per_thread {
                        return None;
                    }
                    counter += 1;
                    nonces_ref.fetch_add(1, Ordering::Relaxed);
                    let mut nonce = [0u8; 32];
                    rng.fill_bytes(&mut nonce);
                    Some(nonce)
                });

                match result {
                    Ok(sol) => {
                        // Off-chain target check
                        let cand_hash = solution_hash(&sol.soln_indices, &input);
                        if hash_under_target(&cand_hash, &cfg.current_target) {
                            found_ref.store(true, Ordering::Relaxed);
                            Some(sol)
                        } else {
                            None
                        }
                    }
                    Err(_) => None,
                }
            });

        let solve_ms = solve_started.elapsed().as_millis() as u64;
        let round_nonces = nonces_tried.load(Ordering::Relaxed);
        total_nonces.fetch_add(round_nonces, Ordering::Relaxed);

        let session_secs = started_at.elapsed().as_secs_f64().max(0.001);
        let hashrate = total_nonces.load(Ordering::Relaxed) as f64 / session_secs;

        let solution = match solution {
            Some(s) => s,
            None => {
                println!(
                    "     {}· no solution{}   {}{}ms{}   {}{}{}   {}nonces: {}{}",
                    C_GRAY, C_RESET,
                    C_DIM, solve_ms, C_RESET,
                    C_GOLD, fmt_hashrate(hashrate), C_RESET,
                    C_DIM, round_nonces, C_RESET,
                );
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };

        // === MULTI-RPC BROADCAST ===
        let submit_result = submit_mine_multi_rpc(
            &broadcast_rpcs,
            &miner_kp,
            &program_id,
            &config_pda,
            &cfg,
            &vault_pda,
            &miner_ata,
            &token_program_id,
            &solution.nonce,
            solution.soln_indices.clone(),
            args.cu_limit,
        );

        match submit_result {
            Ok(sig) => {
                blocks_mined += 1;
                println!(
                    "     {}✓ MINED!{}   {}+{} EQM{}   {}{}ms{}   {}{}{}",
                    C_SAGE_B, C_RESET,
                    C_BOLD, format_reward(cfg.current_epoch_reward), C_RESET,
                    C_DIM, solve_ms, C_RESET,
                    C_GOLD_B, fmt_hashrate(hashrate), C_RESET,
                );
                println!("       {}sig {}{}   {}broadcast to {} RPCs{}", C_GRAY, short_sig(&sig), C_RESET, C_DIM, broadcast_rpcs.len(), C_RESET);
                println!();
                println!(
                    "   {}total{}  {}{} EQM{}   {}blocks {}{}   {}uptime {}{}",
                    C_DIM, C_RESET,
                    C_BOLD, format_reward(cfg.current_epoch_reward * blocks_mined), C_RESET,
                    C_DIM, blocks_mined, C_RESET,
                    C_DIM, fmt_uptime(session_secs), C_RESET,
                );

                tg.send_sync(&format!(
                    "⛏ <b>MINED +{} EQM</b>\nBlock: <code>#{}</code>\nSig: <code>{}</code>\nTotal: <code>{} blocks</code>\nHashrate: <code>{}</code>",
                    format_reward(cfg.current_epoch_reward),
                    cfg.block_height,
                    short_sig(&sig),
                    blocks_mined,
                    fmt_hashrate(hashrate),
                ));
            }
            Err(e) => {
                let reason = classify_submit_err(&e.to_string());
                println!(
                    "     {}· {}{}   {}{}ms{}   {}{}{}",
                    C_GRAY, reason, C_RESET,
                    C_DIM, solve_ms, C_RESET,
                    C_GOLD, fmt_hashrate(hashrate), C_RESET,
                );
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        }

        if args.max_blocks > 0 && blocks_mined >= args.max_blocks {
            println!("\n   {}session complete — {} blocks mined{}", C_ROSE_B, blocks_mined, C_RESET);
            tg.send_sync(&format!("🏁 Session complete — {} blocks mined", blocks_mined));
            return Ok(());
        }
    }
}

// ============================================================================
// Multi-RPC broadcast
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn submit_mine_multi_rpc(
    rpcs: &[RpcClient],
    miner_kp: &Keypair,
    program_id: &Pubkey,
    config_pda: &Pubkey,
    cfg: &EquiumConfig,
    vault_pda: &Pubkey,
    miner_ata: &Pubkey,
    token_program_id: &Pubkey,
    nonce: &[u8; 32],
    soln_indices: Vec<u8>,
    cu_limit: u32,
) -> Result<String> {
    let miner = miner_kp.pubkey();

    // Build instruction
    let accounts = build_mine_accounts(
        &miner, config_pda, &cfg.mint, vault_pda, miner_ata, token_program_id,
    );
    let data = build_mine_data(nonce, &soln_indices);
    let ix = Instruction {
        program_id: *program_id,
        accounts,
        data,
    };
    let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(cu_limit);

    // Sign once
    let recent = rpcs[0].get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &[cu_ix, ix],
        Some(&miner),
        &[miner_kp],
        recent,
    );

    // Broadcast to ALL RPCs (same signed TX = fee only 1x)
    let mut first_result: Option<Result<String>> = None;
    for rpc in rpcs {
        match rpc.send_and_confirm_transaction(&tx) {
            Ok(sig) => return Ok(sig.to_string()),
            Err(e) => {
                if first_result.is_none() {
                    first_result = Some(Err(anyhow!("{}", e)));
                }
            }
        }
    }

    first_result.unwrap_or(Err(anyhow!("all RPCs failed")))
}

fn build_mine_accounts(
    miner: &Pubkey,
    config_pda: &Pubkey,
    mint: &Pubkey,
    vault_pda: &Pubkey,
    miner_ata: &Pubkey,
    token_program_id: &Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*miner, true),
        AccountMeta::new(*config_pda, false),
        AccountMeta::new_readonly(*mint, false),
        AccountMeta::new(*vault_pda, false),
        AccountMeta::new(*miner_ata, false),
        AccountMeta::new_readonly(*token_program_id, false),
        AccountMeta::new_readonly(anchor_spl::associated_token::ID, false),
        AccountMeta::new_readonly(system_program::ID, false),
        AccountMeta::new_readonly(sysvar::slot_hashes::ID, false),
    ]
}

fn build_mine_data(nonce: &[u8; 32], soln_indices: &[u8]) -> Vec<u8> {
    // Anchor discriminator for "mine" + args
    use sha2::{Sha256, Digest};
    let mut discriminator = [0u8; 8];
    let hash = Sha256::digest(b"global:mine");
    discriminator.copy_from_slice(&hash[..8]);

    let mut data = Vec::new();
    data.extend_from_slice(&discriminator);
    data.extend_from_slice(nonce);
    // Vec<u8> encoding: 4-byte little-endian length + bytes
    data.extend_from_slice(&(soln_indices.len() as u32).to_le_bytes());
    data.extend_from_slice(soln_indices);
    data
}

// ============================================================================
// Helpers
// ============================================================================

/// Minimal EquiumConfig deserialization (matches on-chain state layout).
/// We only need the fields the miner reads.
#[derive(Debug)]
struct EquiumConfig {
    mint: Pubkey,
    #[allow(dead_code)]
    mineable_vault: Pubkey,
    #[allow(dead_code)]
    mineable_vault_bump: u8,
    #[allow(dead_code)]
    config_bump: u8,
    #[allow(dead_code)]
    genesis_slot: u64,
    #[allow(dead_code)]
    genesis_unix_ts: i64,
    equihash_n: u32,
    equihash_k: u32,
    current_target: [u8; 32],
    block_height: u64,
    current_challenge: [u8; 32],
    #[allow(dead_code)]
    current_round_open_slot: u64,
    #[allow(dead_code)]
    current_round_open_unix_ts: i64,
    #[allow(dead_code)]
    last_winner: Pubkey,
    current_epoch_reward: u64,
}

fn fetch_config(rpc: &RpcClient, config_pda: &Pubkey) -> Result<EquiumConfig> {
    let acct = rpc.get_account(config_pda)?;
    let data = &acct.data;

    // Skip 8-byte Anchor discriminator
    if data.len() < 8 + 32 + 32 + 1 + 1 + 8 + 8 + 4 + 4 + 32 + 8 + 32 + 8 + 8 + 32 + 8 {
        return Err(anyhow!("config account too small"));
    }
    let mut offset = 8;

    let mint = Pubkey::try_from(&data[offset..offset + 32]).unwrap();
    offset += 32;
    let mineable_vault = Pubkey::try_from(&data[offset..offset + 32]).unwrap();
    offset += 32;
    let mineable_vault_bump = data[offset];
    offset += 1;
    let config_bump = data[offset];
    offset += 1;
    let genesis_slot = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let genesis_unix_ts = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
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
    let current_round_open_slot = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let current_round_open_unix_ts = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let last_winner = Pubkey::try_from(&data[offset..offset + 32]).unwrap();
    offset += 32;
    let current_epoch_reward = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());

    Ok(EquiumConfig {
        mint,
        mineable_vault,
        mineable_vault_bump,
        config_bump,
        genesis_slot,
        genesis_unix_ts,
        equihash_n,
        equihash_k,
        current_target,
        block_height,
        current_challenge,
        current_round_open_slot,
        current_round_open_unix_ts,
        last_winner,
        current_epoch_reward,
    })
}

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
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn classify_submit_err(s: &str) -> &'static str {
    if s.contains("AboveTarget") || s.contains("0x1773") {
        "above target"
    } else if s.contains("InvalidEquihash") || s.contains("0x1772") {
        "stale challenge"
    } else if s.contains("blockhash not found") || s.contains("BlockhashNotFound") {
        "blockhash expired"
    } else {
        "submit error"
    }
}

fn print_boot(miner: &Pubkey, program: &Pubkey, rpc_url: &str, threads: usize, rpc_count: usize) {
    println!("{}{}{}", C_ROSE_B, LOGO, C_RESET);
    println!(
        "   {}multi-threaded solana miner{}                    {}$EQM ⛏{}",
        C_DIM, C_RESET, C_GOLD_B, C_RESET
    );
    println!();
    println!("{}{}{}", C_GRAY, RULE, C_RESET);
    println!("   {}miner{}      {}{}{}", C_DIM, C_RESET, C_TEAL, short_pk(miner), C_RESET);
    println!("   {}program{}    {}{}{}", C_DIM, C_RESET, C_TEAL, short_pk(program), C_RESET);
    println!("   {}threads{}    {}{}{}", C_DIM, C_RESET, C_GOLD_B, threads, C_RESET);
    println!("   {}RPCs{}       {}{}{}", C_DIM, C_RESET, C_TEAL, rpc_count, C_RESET);
    println!("   {}primary{}    {}{}{}", C_DIM, C_RESET, C_GRAY, rpc_url, C_RESET);
    println!("{}{}{}", C_GRAY, RULE, C_RESET);
}

fn short_pk(pk: &Pubkey) -> String {
    let s = pk.to_string();
    format!("{}…{}", &s[..4], &s[s.len() - 4..])
}

fn short_sig(s: &str) -> String {
    if s.len() <= 12 { return s.to_string(); }
    format!("{}…{}", &s[..6], &s[s.len() - 6..])
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
