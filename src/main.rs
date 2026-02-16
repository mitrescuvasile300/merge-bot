//! Polymarket BTC 5-Minute Merge Arbitrage Bot
//!
//! Implements the MuseumOfBees strategy: buy BOTH sides (Up + Down)
//! of BTC 5-minute binary options at different times during BTC oscillations,
//! then merge pairs (1 Up + 1 Down = $1 USDC) for guaranteed profit.
//!
//! DEFAULT MODE: Dry-run (paper trading, no real transactions)
//! To run live: --live flag + .env credentials

#![allow(dead_code)] // Many public items are part of the API but not yet called from main

#![allow(dead_code)] // Public API methods used by consumers, not all used internally yet

mod config;
mod feeds;
mod market;
mod merge;
mod orders;
mod pricing;
mod risk;
mod strategy;
mod types;

use anyhow::Result;
use clap::Parser;
use rust_decimal_macros::dec;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use config::{CliArgs, Config};
use feeds::binance::{BinanceFeed, SimulatedBinanceFeed};
use feeds::polymarket::{PolymarketFeed, SimulatedPolymarketFeed};
use market::MarketDiscovery;
use merge::MergeEngine;
use orders::OrderManager;
use risk::RiskManager;
use strategy::MergeStrategy;

const BANNER: &str = r#"
╔══════════════════════════════════════════════════════════╗
║                                                          ║
║   🦀  MERGE BOT  —  Polymarket BTC 5-min Arbitrage  🦀   ║
║                                                          ║
║   Strategy: Buy Up + Down at different times,            ║
║   merge pairs for guaranteed $1 payout.                  ║
║   Maker fees = ZERO. Profit = $1 - combined_cost.        ║
║                                                          ║
╚══════════════════════════════════════════════════════════╝
"#;

#[tokio::main]
async fn main() -> Result<()> {
    let args = CliArgs::parse();
    let config = Config::from_args(&args);

    // Initialize logging
    init_logging(&args);

    println!("{}", BANNER);

    if config.dry_run {
        info!("╔════════════════════════════════════╗");
        info!("║  MODE: DRY-RUN (Paper Trading)     ║");
        info!("║  No real transactions will be made  ║");
        info!("╚════════════════════════════════════╝");
    } else {
        config.validate_live_mode()?;
        warn!("╔════════════════════════════════════╗");
        warn!("║  MODE: *** LIVE TRADING ***         ║");
        warn!("║  Real USDC will be spent!           ║");
        warn!("╚════════════════════════════════════╝");
    }

    info!(
        capital = %config.initial_capital,
        target_edge = %config.target_edge,
        shares_per_order = %config.shares_per_order,
        entry_delay = config.entry_delay_secs,
        exit_buffer = config.exit_buffer_secs,
        "Configuration loaded"
    );

    // Run the bot
    if config.dry_run {
        run_dry_mode(config).await
    } else {
        run_live_mode(config).await
    }
}

/// Dry-run mode: simulated feeds + simulated order matching
async fn run_dry_mode(config: Config) -> Result<()> {
    info!("Initializing dry-run mode...");

    let simulated_btc_price = dec!(97000); // Simulated BTC starting price

    // Initialize components
    let order_manager = Arc::new(OrderManager::new(true, &config.clob_url));
    let merge_engine = Arc::new(MergeEngine::new(true));
    let risk_manager = Arc::new(RwLock::new(RiskManager::new(
        config.initial_capital,
        config.max_position_pct,
        config.daily_stop_loss_pct,
        config.consecutive_loss_limit,
        config.max_exposure_per_market,
        config.max_open_orders,
    )));

    // Simulated BTC price feed
    let (sim_btc, mut btc_rx) = SimulatedBinanceFeed::new(simulated_btc_price);
    let btc_price_state = Arc::new(RwLock::new(None::<types::PriceTick>));
    let btc_price_state2 = btc_price_state.clone();

    // Simulated Polymarket order book
    let sim_polymarket = Arc::new(SimulatedPolymarketFeed::new(simulated_btc_price));
    let sim_books = sim_polymarket.books();

    // Start simulated BTC feed
    let sim_btc = Arc::new(sim_btc);
    let sim_btc_clone = sim_btc.clone();
    tokio::spawn(async move {
        if let Err(e) = sim_btc_clone.run().await {
            error!("Simulated BTC feed error: {:?}", e);
        }
    });

    // Forward price updates to shared state + simulated order book
    let sim_pm_clone = sim_polymarket.clone();
    tokio::spawn(async move {
        loop {
            if btc_rx.changed().await.is_ok() {
                let tick = btc_rx.borrow().clone();
                if let Some(ref tick) = tick {
                    *btc_price_state2.write().await = Some(tick.clone());
                    // Update simulated order books based on BTC price
                    let remaining = 150u64; // Approximate
                    sim_pm_clone
                        .update_from_btc_price(tick.price, remaining)
                        .await;
                }
            }
        }
    });

    // Wait for first price
    info!("Waiting for simulated BTC price feed...");
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Create strategy
    let mut strat = MergeStrategy::new(
        config.clone(),
        order_manager.clone(),
        merge_engine.clone(),
        risk_manager.clone(),
    );

    let mut windows_traded = 0u64;

    loop {
        // Create a simulated market
        let (window_start, window_end) = MarketDiscovery::current_window();
        let market = types::Market {
            condition_id: format!("sim-condition-{}", window_start),
            slug: MarketDiscovery::expected_slug(window_start),
            up_token_id: format!("sim-up-{}", window_start),
            down_token_id: format!("sim-down-{}", window_start),
            window_start,
            window_end,
            opening_price: btc_price_state.read().await.as_ref().map(|t| t.price),
            active: true,
        };

        info!(
            "╔══════════════════════════════════════╗"
        );
        info!(
            "║  Window #{}: {}",
            windows_traded + 1,
            market.slug
        );
        info!(
            "╚══════════════════════════════════════╝"
        );

        // Create price closure
        let price_state = btc_price_state.clone();
        let price_fn = move || {
            // Use try_read to avoid blocking in sync context
            price_state.try_read().ok().and_then(|guard| guard.clone())
        };

        // Run strategy for this window
        match strat.run_window(&market, price_fn, sim_books.clone()).await {
            Ok(merges) => {
                info!("Window completed with {} merges", merges);
            }
            Err(e) => {
                error!("Strategy error: {:?}", e);
            }
        }

        windows_traded += 1;

        // Check if we should stop
        if config.max_windows > 0 && windows_traded >= config.max_windows {
            info!(
                "Reached max windows limit ({}), stopping",
                config.max_windows
            );
            break;
        }

        // Check risk
        if risk_manager.read().await.is_halted() {
            warn!("Risk manager halted, stopping bot");
            break;
        }

        // Wait for next window
        let remaining = market.seconds_remaining();
        if remaining > 0 {
            info!("Waiting {} seconds for next window...", remaining);
            tokio::time::sleep(tokio::time::Duration::from_secs(remaining as u64)).await;
        }
    }

    // Final P&L report
    print_final_report(&merge_engine, &risk_manager).await;

    Ok(())
}

/// Live mode: real Binance + Polymarket feeds, real order placement
async fn run_live_mode(config: Config) -> Result<()> {
    info!("Initializing live trading mode...");

    let market_discovery = MarketDiscovery::new(&config.gamma_url, &config.clob_url);

    // Initialize order manager with credentials
    let order_manager = Arc::new(
        OrderManager::new(false, &config.clob_url).with_credentials(
            config.polymarket_api_key.clone(),
            config.polymarket_api_secret.clone(),
            config.polymarket_passphrase.clone(),
        ),
    );

    let merge_engine = Arc::new(MergeEngine::new(false));
    let risk_manager = Arc::new(RwLock::new(RiskManager::new(
        config.initial_capital,
        config.max_position_pct,
        config.daily_stop_loss_pct,
        config.consecutive_loss_limit,
        config.max_exposure_per_market,
        config.max_open_orders,
    )));

    // Start Binance BTC price feed
    let (binance_feed, mut btc_rx) = BinanceFeed::new(&config.binance_ws_url);
    let btc_price_state = Arc::new(RwLock::new(None::<types::PriceTick>));
    let btc_price_state2 = btc_price_state.clone();

    let binance_feed = Arc::new(binance_feed);
    let binance_clone = binance_feed.clone();
    tokio::spawn(async move {
        if let Err(e) = binance_clone.run().await {
            error!("Binance feed fatal error: {:?}", e);
        }
    });

    // Forward price updates
    tokio::spawn(async move {
        loop {
            if btc_rx.changed().await.is_ok() {
                let tick = btc_rx.borrow().clone();
                if let Some(tick) = tick {
                    *btc_price_state2.write().await = Some(tick);
                }
            }
        }
    });

    // Wait for BTC price
    info!("Waiting for Binance BTC price feed...");
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        if btc_price_state.read().await.is_some() {
            let price = btc_price_state
                .read()
                .await
                .as_ref()
                .unwrap()
                .price;
            info!("BTC price connected: ${:.2}", price);
            break;
        }
    }

    let mut strat = MergeStrategy::new(
        config.clone(),
        order_manager.clone(),
        merge_engine.clone(),
        risk_manager.clone(),
    );

    let mut windows_traded = 0u64;

    loop {
        // Find current active market
        let market = match market_discovery.find_current_market().await? {
            Some(m) => m,
            None => {
                warn!("No active market found, waiting for next window...");
                let (_, window_end) = MarketDiscovery::current_window();
                let now = chrono::Utc::now().timestamp() as u64;
                if window_end > now {
                    tokio::time::sleep(tokio::time::Duration::from_secs(window_end - now + 1))
                        .await;
                }
                continue;
            }
        };

        // Start Polymarket order book feed for this market
        let pm_feed = PolymarketFeed::new(
            &config.clob_ws_url,
            &market.up_token_id,
            &market.down_token_id,
        );
        let pm_books = pm_feed.books();

        let pm_handle = tokio::spawn(async move {
            if let Err(e) = pm_feed.run().await {
                error!("Polymarket feed error: {:?}", e);
            }
        });

        // Wait a moment for order book to populate
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        info!(
            "╔══════════════════════════════════════╗"
        );
        info!(
            "║  Window #{}: {}",
            windows_traded + 1,
            market.slug
        );
        info!(
            "╚══════════════════════════════════════╝"
        );

        let price_state = btc_price_state.clone();
        let price_fn = move || {
            price_state.try_read().ok().and_then(|guard| guard.clone())
        };

        match strat.run_window(&market, price_fn, pm_books).await {
            Ok(merges) => {
                info!("Window completed with {} merges", merges);
            }
            Err(e) => {
                error!("Strategy error: {:?}", e);
            }
        }

        // Stop the order book feed for this market
        pm_handle.abort();

        windows_traded += 1;

        if config.max_windows > 0 && windows_traded >= config.max_windows {
            info!(
                "Reached max windows limit ({}), stopping",
                config.max_windows
            );
            break;
        }

        if risk_manager.read().await.is_halted() {
            warn!("Risk manager halted, stopping bot");
            break;
        }

        // Wait for next window
        let remaining = market.seconds_remaining();
        if remaining > 0 {
            info!("Waiting {} seconds for next window...", remaining);
            tokio::time::sleep(tokio::time::Duration::from_secs(remaining as u64)).await;
        }
    }

    print_final_report(&merge_engine, &risk_manager).await;

    Ok(())
}

/// Print the final session report
async fn print_final_report(
    merge_engine: &MergeEngine,
    risk_manager: &RwLock<RiskManager>,
) {
    let pnl = merge_engine.pnl_snapshot().await;
    let risk = risk_manager.read().await;

    info!("╔══════════════════════════════════════════════════╗");
    info!("║              FINAL SESSION REPORT                ║");
    info!("╠══════════════════════════════════════════════════╣");
    info!("║ Total orders:         {}", pnl.total_orders);
    info!("║ Total merges:         {}", pnl.total_merges);
    info!("║ Total pairs merged:   {}", pnl.total_pairs);
    info!("║ Total invested:       ${:.4}", pnl.total_invested);
    info!("║ Total merge payout:   ${:.4}", pnl.total_merged_payout);
    info!("║ Total profit:         ${:.4}", pnl.total_profit);
    info!("║ Win rate:             {:.1}%", pnl.win_rate * dec!(100));
    info!("║ Markets traded:       {}", pnl.markets_traded);
    info!("║ Daily P&L:            ${:.4}", risk.daily_pnl());
    info!("║ Final capital:        ${:.4}", risk.available_capital());
    info!("╚══════════════════════════════════════════════════╝");
}

fn init_logging(args: &CliArgs) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&format!("merge_bot={}", args.log_level)));

    if args.json_logs {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_thread_ids(false)
            .with_file(false)
            .with_line_number(false)
            .init();
    }
}
