# Equium Miner Pro

Optimized multi-threaded $EQM miner for Solana.

Based on the [reference CLI miner](https://github.com/HannaPrints/equium) with significant performance improvements.

## Features

- **Multi-threaded Equihash solver** — N CPU cores solving in parallel (rayon)
- **Multi-RPC broadcast** — same signed TX sent to all RPCs for faster landing (fee hanya 1x)
- **Ankr Solana RPC** as primary (free tier sufficient)
- **Telegram notifications** — get notified when you mine a block
- **Auto epoch detection** — instant restart on new round
- **.env config** — easy setup

## Performance vs Reference

| | Reference Miner | EQM Miner Pro |
|---|---|---|
| Threads | 1 | All CPU cores |
| Hashrate (8-core) | ~1.6 H/s | ~10-15 H/s |
| RPC broadcast | 1 RPC | Multi-RPC parallel |
| Notifications | None | Telegram |

## Quick Start

```bash
git clone https://github.com/hilmiariprahman/equium-miner-pro
cd equium-miner-pro
cp .env.example .env
# Edit .env with your RPC key and keypair path
cargo build --release -p eqm-miner
./target/release/eqm-miner
```

## Requirements

- Rust toolchain (install via [rustup.rs](https://rustup.rs))
- Solana keypair (generate: `solana-keygen new`)
- Small amount of SOL for TX fees (~0.001 SOL per mine)
- RPC endpoint (Ankr free tier works)

## Configuration

All config via `.env` file or CLI flags:

```env
RPC_URL=https://rpc.ankr.com/sol/YOUR_API_KEY
RPC_URLS=https://rpc.ankr.com/sol/YOUR_API_KEY,https://mainnet.helius-rpc.com/?api-key=YOUR_KEY
KEYPAIR_PATH=~/.config/solana/id.json
THREADS=0  # 0 = auto-detect all cores
TELEGRAM_ENABLED=true
TELEGRAM_BOT_TOKEN=your_bot_token
TELEGRAM_CHAT_ID=your_chat_id
```

## CLI Flags

```
eqm-miner --help

Options:
  --rpc-url <URL>             Primary Solana RPC
  --rpc-urls <URLS>           Extra RPCs for broadcast (comma-separated)
  --keypair <PATH>            Path to keypair JSON
  --threads <N>               Solver threads (0=auto)
  --max-nonces-per-round <N>  Max attempts before refresh
  --cu-limit <N>              Compute units per TX
  --max-blocks <N>            Stop after N blocks (0=forever)
```

## Multi-RPC Broadcast

TX yang sama (signed sekali) dikirim ke semua RPC sekaligus:
- Fee **hanya 1x** — Solana validators deduplicate identical transactions
- Meningkatkan chance TX landing di block
- Tidak ada risiko double-charge

## License

Apache-2.0
