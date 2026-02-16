use clap::Parser;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::Duration;

/// Polymarket BTC 5-min merge arbitrage bot
#[derive(Parser, Debug, Clone)]
#[command(name = "merge-bot", about = "Polymarket BTC 5-min merge arbitrage bot")]
pub struct CliArgs {
    /// Run in dry-run mode (paper trading, no real transactions)
    #[arg(long, default_value_t = true)]
    pub dry_run: bool,

    /// Run in LIVE mode (real transactions — use with caution!)
    #[arg(long, default_value_t = false)]
    pub live: bool,

    /// Initial capital in USDC
    #[arg(long, default_value_t = 215.0)]
    pub capital: f64,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Number of market windows to trade (0 = unlimited)
    #[arg(long, default_value_t = 0)]
    pub max_windows: u64,

    /// Target edge per merge pair (combined cost below $1 by this amount)
    #[arg(long, default_value_t = 0.02)]
    pub target_edge: f64,

    /// Shares per order
    #[arg(long, default_value_t = 5)]
    pub shares_per_order: u64,

    /// Output JSON logs
    #[arg(long, default_value_t = false)]
    pub json_logs: bool,

    /// Directory to write per-window log files (one .log per 5-min market)
    #[arg(long, default_value = "")]
    pub log_dir: String,

    /// Entry delay in seconds (wait after market opens before trading)
    #[arg(long, default_value_t = 90)]
    pub entry_delay: u64,

    /// Exit buffer in seconds (stop trading this many secs before window close)
    #[arg(long, default_value_t = 30)]
    pub exit_buffer: u64,

    /// Simulation speed multiplier for fast Monte Carlo testing (dry-run only).
    /// E.g., --sim-speed 10 runs 10x faster (each window takes ~30s instead of 300s).
    /// GBM vol is scaled to preserve statistical properties per window.
    #[arg(long, default_value_t = 1)]
    pub sim_speed: u64,
}

/// Full bot configuration derived from CLI args + env vars
#[derive(Debug, Clone)]
pub struct Config {
    // === Mode ===
    pub dry_run: bool,

    // === Capital ===
    pub initial_capital: Decimal,
    pub max_position_pct: Decimal,

    // === Strategy ===
    /// Target edge per pair (how much below $1 we aim for combined cost)
    pub target_edge: Decimal,
    /// Shares per individual order
    pub shares_per_order: Decimal,
    /// Minimum order size (Polymarket minimum is 5 shares)
    pub min_order_shares: Decimal,
    /// Delay after window opens before starting (seconds)
    pub entry_delay_secs: u64,
    /// Stop trading this many seconds before window close
    pub exit_buffer_secs: u64,
    /// Interval between order placement attempts
    pub order_interval: Duration,
    /// Max price to pay for any single side (never buy above this)
    pub max_side_price: Decimal,
    /// Min price to pay for any single side (ignore dust levels)
    pub min_side_price: Decimal,
    /// Max shares of one side that can exceed the other side's position.
    /// Prevents dangerous position imbalance (e.g., 280 Up vs 0 Down).
    pub max_side_imbalance: Decimal,

    // === Risk ===
    pub daily_stop_loss_pct: Decimal,
    pub consecutive_loss_limit: u32,
    pub max_open_orders: usize,
    pub max_exposure_per_market: Decimal,

    // === API Credentials (live mode only) ===
    pub polymarket_api_key: Option<String>,
    pub polymarket_api_secret: Option<String>,
    pub polymarket_passphrase: Option<String>,
    pub polygon_private_key: Option<String>,

    // === Endpoints ===
    pub clob_url: String,
    pub gamma_url: String,
    pub clob_ws_url: String,
    pub binance_ws_url: String,

    // === Limits ===
    pub max_windows: u64,
    pub json_logs: bool,

    // === Simulation speed ===
    pub sim_speed: u64,
    /// Effective window duration in seconds (300 / sim_speed)
    pub window_secs: u64,

    // === Per-window file logging ===
    pub log_dir: Option<String>,
}

impl Config {
    pub fn from_args(args: &CliArgs) -> Self {
        // Live mode requires --live flag AND dry_run must be explicitly disabled
        let dry_run = !args.live;

        // Load env vars for live mode credentials
        let _ = dotenvy::dotenv();

        let sim_speed = args.sim_speed.max(1);
        let window_secs = 300 / sim_speed;

        Config {
            dry_run,

            // Capital
            initial_capital: Decimal::from_f64_retain(args.capital)
                .unwrap_or(dec!(215.0)),
            max_position_pct: dec!(0.10),

            // Strategy — tuned to MuseumOfBees parameters
            target_edge: Decimal::from_f64_retain(args.target_edge)
                .unwrap_or(dec!(0.03)),
            shares_per_order: Decimal::from(args.shares_per_order),
            min_order_shares: dec!(5),
            entry_delay_secs: args.entry_delay / sim_speed,
            exit_buffer_secs: (args.exit_buffer / sim_speed).max(1),
            order_interval: Duration::from_millis((2000 / sim_speed).max(200)), // Scale but min 200ms
            max_side_price: dec!(0.48),  // v7: Lowered from 0.65 to ensure combined < $0.97
            min_side_price: dec!(0.01),  // Polymarket minimum tick (not used as bid floor anymore)
            max_side_imbalance: dec!(5), // v13: Max 5 shares ahead (one order's worth — forces alternating fills)

            // Risk
            daily_stop_loss_pct: dec!(0.15),
            consecutive_loss_limit: 5,
            max_open_orders: 20,
            max_exposure_per_market: dec!(50.0), // Max $50 per market window (~23% of capital, halved for variance reduction)

            // API Credentials
            polymarket_api_key: std::env::var("POLYMARKET_API_KEY").ok(),
            polymarket_api_secret: std::env::var("POLYMARKET_API_SECRET").ok(),
            polymarket_passphrase: std::env::var("POLYMARKET_PASSPHRASE").ok(),
            polygon_private_key: std::env::var("POLYGON_PRIVATE_KEY").ok(),

            // Endpoints
            clob_url: "https://clob.polymarket.com".to_string(),
            gamma_url: "https://gamma-api.polymarket.com".to_string(),
            clob_ws_url: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
            binance_ws_url: "wss://stream.binance.com:9443/ws/btcusdt@trade".to_string(),

            // Limits
            max_windows: args.max_windows,
            json_logs: args.json_logs,

            // Simulation speed
            sim_speed,
            window_secs,

            // Per-window file logging
            log_dir: if args.log_dir.is_empty() {
                None
            } else {
                Some(args.log_dir.clone())
            },
        }
    }

    /// Validate that live mode has all required credentials
    pub fn validate_live_mode(&self) -> anyhow::Result<()> {
        if !self.dry_run {
            if self.polymarket_api_key.is_none() {
                anyhow::bail!("POLYMARKET_API_KEY required for live trading");
            }
            if self.polymarket_api_secret.is_none() {
                anyhow::bail!("POLYMARKET_API_SECRET required for live trading");
            }
            if self.polymarket_passphrase.is_none() {
                anyhow::bail!("POLYMARKET_PASSPHRASE required for live trading");
            }
            if self.polygon_private_key.is_none() {
                anyhow::bail!("POLYGON_PRIVATE_KEY required for live trading");
            }
        }
        Ok(())
    }
}
