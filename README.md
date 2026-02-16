# 🦀 Merge Bot — Polymarket BTC 5-Min Merge Arbitrage

A Rust-based arbitrage bot for Polymarket's BTC 5-minute binary options markets. Implements the **merge arbitrage strategy**: buy both Up and Down tokens at different times during BTC price oscillations, then merge pairs (1 Up + 1 Down = $1 USDC) for guaranteed profit.

## Strategy Overview

The MuseumOfBees strategy exploits BTC price oscillations within each 5-minute window:

1. **Wait ~90 seconds** after market opens for price to establish
2. **When BTC dips** → Up tokens get cheaper → BUY Up tokens
3. **When BTC rises** → Down tokens get cheaper → BUY Down tokens
4. **Merge pairs**: 1 Up + 1 Down = $1 USDC (guaranteed by Polymarket's CTF contract)
5. **Profit** = $1 - (cost of Up + cost of Down)

Key advantages:
- **Maker fees = ZERO** (limit orders only, never taker)
- **No directional risk** — profit regardless of where BTC ends up
- **Deterministic payout** — merge is guaranteed by smart contract
- **~250 trades per 5-min window**, every 2-5 seconds

## Architecture

```
src/
├── main.rs           # Entry point, tokio runtime, mode selection
├── config.rs         # CLI args + configuration
├── types.rs          # Core types: Market, Order, Position, PriceTick
├── market.rs         # Market discovery via Gamma API
├── pricing.rs        # Black-Scholes fair value + Kelly sizing + fees
├── strategy.rs       # Core merge-arb strategy loop
├── orders.rs         # Order management (placement, cancellation, fills)
├── merge.rs          # Merge engine + position tracking + P&L
├── risk.rs           # Risk management (circuit breakers, stop loss)
└── feeds/
    ├── mod.rs
    ├── binance.rs    # Binance BTC/USDT WebSocket price feed
    └── polymarket.rs # Polymarket order book WebSocket feed
```

## Quick Start

### Dry-Run Mode (Default — No Real Money)

```bash
# Clone and build
git clone https://github.com/mitrescuvasile300/merge-bot
cd merge-bot
cargo build --release

# Run in dry-run mode (paper trading with simulated feeds)
cargo run --release

# Run 3 windows then stop
cargo run --release -- --max-windows 3

# Verbose logging
cargo run --release -- --log-level debug
```

### Live Mode (⚠️ Real Money)

```bash
# 1. Copy and fill in credentials
cp .env.example .env
# Edit .env with your Polymarket API keys + Polygon wallet

# 2. Run with --live flag
cargo run --release -- --live --capital 215
```

## Configuration

| Flag | Default | Description |
|------|---------|-------------|
| `--dry-run` | `true` | Paper trading mode (no real transactions) |
| `--live` | `false` | Enable real trading (requires .env credentials) |
| `--capital` | `215.0` | Initial capital in USDC |
| `--target-edge` | `0.03` | Target profit per merge pair ($0.03 = 3%) |
| `--shares-per-order` | `20` | Shares per individual order (~$10) |
| `--max-windows` | `0` | Max 5-min windows to trade (0 = unlimited) |
| `--log-level` | `info` | Logging: trace/debug/info/warn/error |
| `--json-logs` | `false` | Output structured JSON logs |

## Environment Variables (.env)

```bash
# Required for live mode
POLYMARKET_API_KEY=your_api_key
POLYMARKET_API_SECRET=your_secret
POLYMARKET_PASSPHRASE=your_passphrase
POLYGON_PRIVATE_KEY=0x_your_private_key

# Optional
INITIAL_CAPITAL=215.0
POLYGON_RPC_URL=https://polygon.drpc.org
RUST_LOG=merge_bot=info
```

## How It Works

### Market Discovery
- Finds active BTC 5-min markets via Polymarket's Gamma API
- Deterministic slug pattern: `btc-updown-5m-{unix_timestamp}` (aligned to 300s)
- Falls back to API search if slug lookup fails

### Pricing
- **Black-Scholes** fair value: `P_up = N(ln(S/K) / σ√T)`
- **Rolling realized volatility** from 1-minute BTC samples
- **Kelly criterion** for optimal sizing: `f* = 0.25 × (p - c) / (1 - c)`

### Order Execution
- **Limit orders only** (maker = zero fees + daily rebates)
- Target: combined cost of Up + Down < $1 - target_edge
- Places orders at Black-Scholes fair value, below the ask

### Merge Mechanism
- On-chain: calls `mergePositions()` on CTF contract
  - Contract: `0x4D97DCd97eC945f40cF65F87097ACe5EA0476045`
  - Burns 1 Up + 1 Down → receives 1 USDC.e
- In dry-run: simulated merge with same P&L tracking

### Risk Management
- Maximum 10% of capital per single trade
- Maximum $50 exposure per market window
- 15% daily stop-loss circuit breaker
- 5 consecutive loss limit → halt
- Maximum 10 concurrent open orders

## APIs Used

| API | URL | Purpose |
|-----|-----|---------|
| CLOB | `https://clob.polymarket.com` | Order placement, book data |
| Gamma | `https://gamma-api.polymarket.com` | Market discovery |
| WS Book | `wss://ws-subscriptions-clob.polymarket.com/ws/market` | Real-time order book |
| Binance WS | `wss://stream.binance.com:9443/ws/btcusdt@trade` | BTC price feed |

## RPC Requirements

A Polygon RPC is needed for on-chain merge operations:
- **Free**: `https://polygon.drpc.org`, `https://1rpc.io/matic`
- **Recommended**: QuickNode or Alchemy free tier
- Gas costs: ~$0.001-$0.02 per merge transaction

## Fee Structure

| Order Type | Fee | Notes |
|-----------|-----|-------|
| Maker (limit) | **0%** | + daily rebates from taker fees |
| Taker at p=0.50 | 1.56% | AVOID — kills merge profitability |
| Taker at p=0.80 | 0.64% | Still expensive |
| Taker at p=0.95 | 0.06% | Negligible |

This bot uses **maker orders exclusively** to avoid fees entirely.

## Performance Expectations

Based on MuseumOfBees analysis (60.6% win rate, 3.42% edge):

| Capital | Est. Monthly Profit | Notes |
|---------|-------------------|-------|
| $215 | $0-50 | Educational/testing |
| $2,000 | $200-500 | Minimum competitive |
| $5,000+ | $500-2,000 | Serious operation |

## License

MIT
