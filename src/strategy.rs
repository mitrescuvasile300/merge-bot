//! Core merge arbitrage strategy loop.
//!
//! Implements the MuseumOfBees strategy:
//! 1. Wait ~90 seconds after market opens for price to establish
//! 2. Monitor BTC price oscillations within the 5-min window
//! 3. Buy Up tokens when BTC dips (Up tokens cheap)
//! 4. Buy Down tokens when BTC rises (Down tokens cheap)
//! 5. Combined cost of Up + Down < $1 → profit on merge
//! 6. Use LIMIT orders (maker = 0 fees + rebates)

use anyhow::Result;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::feeds::polymarket::MarketBooks;
use crate::merge::MergeEngine;
use crate::orders::OrderManager;
use crate::pricing::{BinaryPricer, VolatilityEstimator};
use crate::risk::RiskManager;
use crate::types::{Market, PriceTick, Side};

/// The merge arbitrage strategy
pub struct MergeStrategy {
    config: Config,
    pricer: BinaryPricer,
    vol_estimator: VolatilityEstimator,
    order_manager: Arc<OrderManager>,
    merge_engine: Arc<MergeEngine>,
    risk_manager: Arc<RwLock<RiskManager>>,
    /// v10: Tick counter for post-merge cooldown
    last_merge_tick: u64,
    /// v10: Current tick counter (incremented each loop iteration)
    current_tick: u64,
    /// v10: Running window P&L for MTM loss cap (cash flows only)
    window_cash_pnl: Decimal,
}

/// v10 constants
const POST_MERGE_COOLDOWN_TICKS: u64 = 2;

/// Snapshot of the current market state for decision making
#[derive(Debug)]
struct MarketSnapshot {
    btc_price: Decimal,
    opening_price: Decimal,
    remaining_secs: u64,
    up_best_ask: Option<Decimal>,
    down_best_ask: Option<Decimal>,
    up_best_bid: Option<Decimal>,
    down_best_bid: Option<Decimal>,
    fair_value_up: Decimal,
    fair_value_down: Decimal,
    sigma: Decimal,
}

impl MergeStrategy {
    pub fn new(
        config: Config,
        order_manager: Arc<OrderManager>,
        merge_engine: Arc<MergeEngine>,
        risk_manager: Arc<RwLock<RiskManager>>,
    ) -> Self {
        Self {
            config,
            pricer: BinaryPricer::new(),
            vol_estimator: VolatilityEstimator::new(200),
            order_manager,
            merge_engine,
            risk_manager,
            last_merge_tick: 0,
            current_tick: 0,
            window_cash_pnl: Decimal::ZERO,
        }
    }

    /// Run the strategy for one 5-minute market window.
    /// Returns the number of merge pairs completed.
    pub async fn run_window(
        &mut self,
        market: &Market,
        btc_price_fn: impl Fn() -> Option<PriceTick>,
        books: Arc<RwLock<MarketBooks>>,
    ) -> Result<u64> {
        let mode = if self.config.dry_run {
            "[DRY-RUN]"
        } else {
            "[LIVE]"
        };

        info!(
            "{} Starting strategy for market {} (window {}–{})",
            mode, market.slug, market.window_start, market.window_end
        );

        // Phase 1: Wait for entry delay (let market establish)
        let elapsed = market.seconds_elapsed();
        if elapsed < self.config.entry_delay_secs {
            let wait = self.config.entry_delay_secs - elapsed;
            info!(
                "Waiting {} seconds for market to establish (entry delay: {}s)",
                wait, self.config.entry_delay_secs
            );
            tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;
        }

        // Capture opening price from first BTC tick
        let opening_price = if let Some(op) = market.opening_price {
            op
        } else if let Some(tick) = btc_price_fn() {
            tick.price
        } else {
            warn!("No BTC price available, skipping window");
            return Ok(0);
        };

        info!(
            "BTC opening price for window: ${:.2}",
            opening_price
        );

        // Reset merge engine for new window
        self.merge_engine.reset_for_new_window().await;
        self.risk_manager.write().await.reset_market_exposure();

        // v10: Reset per-window tracking
        self.current_tick = 0;
        self.last_merge_tick = 0;
        self.window_cash_pnl = Decimal::ZERO;

        let mut total_orders = 0u64;

        // Phase 2: Active trading loop
        loop {
            let remaining = market.seconds_remaining();

            // Stop trading before window close
            if remaining <= self.config.exit_buffer_secs as i64 {
                info!(
                    "Exit buffer reached ({} secs remaining), stopping orders",
                    remaining
                );
                break;
            }

            // Always sleep between iterations (core rate limit)
            tokio::time::sleep(self.config.order_interval).await;

            // Get current BTC price
            let btc_tick = match btc_price_fn() {
                Some(tick) => tick,
                None => {
                    debug!("No BTC price, waiting...");
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    continue;
                }
            };

            // Feed volatility estimator
            self.vol_estimator.add_price(
                btc_tick.timestamp.timestamp() as u64,
                crate::pricing::decimal_to_f64_pub(btc_tick.price),
            );

            // Build market snapshot
            let snapshot = {
                let book_state = books.read().await;
                let sigma = self
                    .vol_estimator
                    .annualized_volatility()
                    .unwrap_or(VolatilityEstimator::default_volatility());

                let fair_up = self.pricer.fair_value_up(
                    btc_tick.price,
                    opening_price,
                    sigma,
                    remaining as u64,
                );
                let fair_down = Decimal::ONE - fair_up;

                // In dry-run mode, derive book prices from our own fair value
                // to avoid the async opening-price mismatch with the simulated feed.
                // In live mode, use the actual Polymarket order book.
                let (up_best_ask, down_best_ask, up_best_bid, down_best_bid) =
                    if self.config.dry_run {
                        let spread = dec!(0.025); // 2.5c spread from fair (realistic)
                        (
                            Some((fair_up + spread).min(dec!(0.99))),
                            Some((fair_down + spread).min(dec!(0.99))),
                            Some((fair_up - spread).max(dec!(0.01))),
                            Some((fair_down - spread).max(dec!(0.01))),
                        )
                    } else {
                        (
                            book_state.up_book.best_ask(),
                            book_state.down_book.best_ask(),
                            book_state.up_book.best_bid(),
                            book_state.down_book.best_bid(),
                        )
                    };

                MarketSnapshot {
                    btc_price: btc_tick.price,
                    opening_price,
                    remaining_secs: remaining as u64,
                    up_best_ask,
                    down_best_ask,
                    up_best_bid,
                    down_best_bid,
                    fair_value_up: fair_up,
                    fair_value_down: fair_down,
                    sigma,
                }
            };

            debug!(
                btc = %snapshot.btc_price,
                fv_up = %snapshot.fair_value_up,
                fv_down = %snapshot.fair_value_down,
                remaining = snapshot.remaining_secs,
                "Market snapshot"
            );

            // v10: Increment tick counter
            self.current_tick += 1;

            // Check risk limits
            if self.risk_manager.read().await.is_halted() {
                warn!("Risk manager halted trading");
                break;
            }

            // v10: Post-merge cooldown — skip order placement for N ticks after a merge
            if self.current_tick.saturating_sub(self.last_merge_tick) < POST_MERGE_COOLDOWN_TICKS
                && self.last_merge_tick > 0
            {
                debug!(
                    "Post-merge cooldown: {} ticks since last merge",
                    self.current_tick - self.last_merge_tick
                );
                // Still process fills/merges below, just skip new orders
            }

            // v10: MTM loss cap — stop new orders if mark-to-market P&L is too negative
            let mtm_pnl = {
                let (up_pos, down_pos) = self.merge_engine.positions().await;
                let up_mtm = up_pos.shares * snapshot.fair_value_up;
                let down_mtm = down_pos.shares * snapshot.fair_value_down;
                self.window_cash_pnl + up_mtm + down_mtm
            };
            let mtm_blocked = mtm_pnl < dec!(-3); // v13: tighter MTM loss cap ($3 vs $5)
            if mtm_blocked {
                debug!(
                    mtm_pnl = %mtm_pnl,
                    cash_pnl = %self.window_cash_pnl,
                    "Window MTM loss cap reached — no new orders"
                );

                // v12: DYNAMIC STOP-LOSS — if MTM breaches cap AND we're in the
                // second half of the window (remaining < 90s), sell one-sided
                // position immediately instead of waiting for salvage phase.
                // This cuts losses faster in directional windows.
                const STOPLOSS_WINDOW_SECS: u64 = 90;
                if snapshot.remaining_secs <= STOPLOSS_WINDOW_SECS {
                    let (up_pos, down_pos) = self.merge_engine.positions().await;
                    let excess_up = up_pos.shares.saturating_sub(down_pos.shares);
                    let excess_down = down_pos.shares.saturating_sub(up_pos.shares);

                    if excess_up > Decimal::ZERO {
                        if let Some(up_bid) = snapshot.up_best_bid {
                            let sell_price = up_bid.max(dec!(0.01));
                            warn!(
                                "STOPLOSS: Selling {} excess Up @ ${:.4} (MTM={:.2}, remaining={}s)",
                                excess_up, sell_price, mtm_pnl, snapshot.remaining_secs
                            );
                            let proceeds = self.merge_engine
                                .record_salvage(Side::Up, excess_up, sell_price)
                                .await;
                            if proceeds > Decimal::ZERO {
                                self.risk_manager.write().await.record_salvage_revenue(proceeds);
                                self.window_cash_pnl += proceeds;
                            }
                            // Cancel all pending orders
                            self.order_manager.cancel_all().await?;
                        }
                    } else if excess_down > Decimal::ZERO {
                        if let Some(down_bid) = snapshot.down_best_bid {
                            let sell_price = down_bid.max(dec!(0.01));
                            warn!(
                                "STOPLOSS: Selling {} excess Down @ ${:.4} (MTM={:.2}, remaining={}s)",
                                excess_down, sell_price, mtm_pnl, snapshot.remaining_secs
                            );
                            let proceeds = self.merge_engine
                                .record_salvage(Side::Down, excess_down, sell_price)
                                .await;
                            if proceeds > Decimal::ZERO {
                                self.risk_manager.write().await.record_salvage_revenue(proceeds);
                                self.window_cash_pnl += proceeds;
                            }
                            self.order_manager.cancel_all().await?;
                        }
                    }
                }
            }

            let cooldown_active = self.last_merge_tick > 0
                && self.current_tick.saturating_sub(self.last_merge_tick) < POST_MERGE_COOLDOWN_TICKS;

            // Try to place orders on each side (unless blocked by cooldown or MTM cap)
            let placed = if !cooldown_active && !mtm_blocked {
                self.evaluate_and_place_orders(market, &snapshot).await?
            } else {
                0
            };
            if placed > 0 {
                total_orders += placed;
                info!(
                    "Orders placed this cycle: {} | Total: {} | Open: {}",
                    placed,
                    total_orders,
                    self.order_manager.open_order_count().await,
                );
            } else {
                debug!(
                    "No orders placed | Open: {} | BTC: {} | FV up: {} | FV down: {}",
                    self.order_manager.open_order_count().await,
                    snapshot.btc_price,
                    snapshot.fair_value_up,
                    snapshot.fair_value_down,
                );
            }

            // Cancel stale orders that are unlikely to fill.
            // This prevents phantom imbalance from sitting unfilled orders blocking new ones.
            // An order is stale if it's been open > 30s and its bid is > 3c below current ask.
            {
                let (cancelled, released_exp) = self.order_manager.cancel_stale_orders(
                    30, // max age: 30 seconds
                    snapshot.up_best_ask,
                    snapshot.down_best_ask,
                    dec!(0.03), // min gap: 3c between our bid and current ask
                ).await;
                if cancelled > 0 {
                    self.risk_manager.write().await.release_cancelled_exposure(released_exp);
                    info!(
                        "Cancelled {} stale orders (released ${:.2} exposure)",
                        cancelled, released_exp
                    );
                }
            }

            // In dry-run mode, simulate fills using strategy-consistent fair values
            if self.config.dry_run {
                use crate::types::{BookLevel, OrderBook};

                // Build order books from our own fair values (not the async-lagged feed)
                let spread = dec!(0.025);
                let sim_up_book = OrderBook {
                    bids: vec![BookLevel {
                        price: (snapshot.fair_value_up - spread).max(dec!(0.01)),
                        size: dec!(500),
                    }],
                    asks: vec![BookLevel {
                        price: (snapshot.fair_value_up + spread).min(dec!(0.99)),
                        size: dec!(500),
                    }],
                    timestamp: Some(chrono::Utc::now()),
                };
                let sim_down_book = OrderBook {
                    bids: vec![BookLevel {
                        price: (snapshot.fair_value_down - spread).max(dec!(0.01)),
                        size: dec!(500),
                    }],
                    asks: vec![BookLevel {
                        price: (snapshot.fair_value_down + spread).min(dec!(0.99)),
                        size: dec!(500),
                    }],
                    timestamp: Some(chrono::Utc::now()),
                };

                let up_fills = self
                    .order_manager
                    .simulate_fills(&sim_up_book, &market.up_token_id)
                    .await;
                let down_fills = self
                    .order_manager
                    .simulate_fills(&sim_down_book, &market.down_token_id)
                    .await;

                // Record fills in merge engine — use fill_price (with price improvement)
                for fill_id in &up_fills {
                    let orders = self.order_manager.all_orders().await;
                    if let Some(order) = orders.iter().find(|o| &o.id == fill_id) {
                        self.merge_engine
                            .record_fill(Side::Up, order.filled, order.fill_price)
                            .await;
                        // v10: Track cash outflow
                        self.window_cash_pnl -= order.filled * order.fill_price;
                    }
                }
                for fill_id in &down_fills {
                    let orders = self.order_manager.all_orders().await;
                    if let Some(order) = orders.iter().find(|o| &o.id == fill_id) {
                        self.merge_engine
                            .record_fill(Side::Down, order.filled, order.fill_price)
                            .await;
                        // v10: Track cash outflow
                        self.window_cash_pnl -= order.filled * order.fill_price;
                    }
                }
            }

            // MID-WINDOW STOP-LOSS: Sell excess losing-side tokens before they expire worthless.
            // v11: Raised FV threshold from 0.20 → 0.30 and extended time window.
            // At FV=0.30 bid is ~$0.28 vs $0.01 at expiry — saves ~$0.27/share.
            // Two-tier system:
            //   - FV < 0.30: sell ALL excess immediately (clear loser)
            //   - FV < 0.40 + time < 180s: sell excess (probably losing, not worth holding)
            if snapshot.remaining_secs > 30 && snapshot.remaining_secs < 280 {
                let (up_pos_sl, down_pos_sl) = self.merge_engine.positions().await;
                let min_excess = self.config.shares_per_order; // 5 shares minimum to trigger

                // Two-tier FV thresholds: aggressive early exit if clearly losing
                let fv_hard_stop = dec!(0.30);  // Below this = definite loser, exit immediately
                let fv_soft_stop = dec!(0.40);  // Below this near close = probably loser
                let soft_stop_active = snapshot.remaining_secs < 180; // Soft stop in last 3 min

                // Check UP side
                let up_losing = snapshot.fair_value_up < fv_hard_stop
                    || (soft_stop_active && snapshot.fair_value_up < fv_soft_stop);
                if up_pos_sl.shares > down_pos_sl.shares + min_excess && up_losing {
                    let excess = up_pos_sl.shares - down_pos_sl.shares;
                    let up_bid = (snapshot.fair_value_up - dec!(0.02)).max(dec!(0.01));
                    info!("STOP-LOSS: Selling {} excess UP shares (FV={:.4}, bid={:.4}, tier={})",
                        excess, snapshot.fair_value_up, up_bid,
                        if snapshot.fair_value_up < fv_hard_stop { "HARD" } else { "SOFT" });
                    let proceeds = self.merge_engine.record_salvage(Side::Up, excess, up_bid).await;
                    if proceeds > Decimal::ZERO {
                        self.risk_manager.write().await.record_salvage_revenue(proceeds);
                        // v11: Track salvage in cash P&L
                        self.window_cash_pnl += proceeds;
                    }
                }

                // Check DOWN side
                let down_losing = snapshot.fair_value_down < fv_hard_stop
                    || (soft_stop_active && snapshot.fair_value_down < fv_soft_stop);
                if down_pos_sl.shares > up_pos_sl.shares + min_excess && down_losing {
                    let excess = down_pos_sl.shares - up_pos_sl.shares;
                    let down_bid = (snapshot.fair_value_down - dec!(0.02)).max(dec!(0.01));
                    info!("STOP-LOSS: Selling {} excess DOWN shares (FV={:.4}, bid={:.4}, tier={})",
                        excess, snapshot.fair_value_down, down_bid,
                        if snapshot.fair_value_down < fv_hard_stop { "HARD" } else { "SOFT" });
                    let proceeds = self.merge_engine.record_salvage(Side::Down, excess, down_bid).await;
                    if proceeds > Decimal::ZERO {
                        self.risk_manager.write().await.record_salvage_revenue(proceeds);
                        // v11: Track salvage in cash P&L
                        self.window_cash_pnl += proceeds;
                    }
                }
            }

            // Try to merge any available pairs
            if let Some(merge_result) = self.merge_engine.try_merge().await {
                let mut risk = self.risk_manager.write().await;
                risk.record_merge(merge_result.profit, merge_result.total_cost);
                // v10: Track merge for cooldown + P&L
                self.last_merge_tick = self.current_tick;
                self.window_cash_pnl += merge_result.total_cost + merge_result.profit;
                info!(
                    "v10: Merge at tick {} — cooldown active for {} ticks",
                    self.current_tick, POST_MERGE_COOLDOWN_TICKS
                );
            }
        }

        // Phase 3: End of window — final merge attempt
        if let Some(merge_result) = self.merge_engine.try_merge().await {
            let mut risk = self.risk_manager.write().await;
            risk.record_merge(merge_result.profit, merge_result.total_cost);
        }

        // Phase 4: SALVAGE — sell excess unmerged shares before window close.
        //
        // Instead of losing 100% of unmerged shares (they expire worthless),
        // sell them back at the current bid price. This is the single biggest
        // risk reduction: a share bought at $0.47 can be sold for ~$0.20-0.40
        // instead of going to $0.
        //
        // In dry-run mode, we compute the bid from the last known fair value.
        // In live mode, we'd place actual sell orders on the CLOB.
        {
            let (up_pos, down_pos) = self.merge_engine.positions().await;

            // Only salvage if there's a meaningful imbalance
            if up_pos.shares > Decimal::ZERO || down_pos.shares > Decimal::ZERO {
                // Get fresh snapshot for bid prices
                let remaining = market.seconds_remaining();
                let btc_tick = btc_price_fn();

                if let Some(tick) = btc_tick {
                    let sigma = self
                        .vol_estimator
                        .annualized_volatility()
                        .unwrap_or(crate::pricing::VolatilityEstimator::default_volatility());

                    let fair_up = self.pricer.fair_value_up(
                        tick.price,
                        opening_price,
                        sigma,
                        remaining.max(0) as u64,
                    );
                    let fair_down = Decimal::ONE - fair_up;
                    let spread = dec!(0.025);

                    // Compute bid prices (what we'd sell at)
                    let up_bid = (fair_up - spread).max(dec!(0.01));
                    let down_bid = (fair_down - spread).max(dec!(0.01));

                    // Salvage excess Up shares
                    if up_pos.shares > down_pos.shares {
                        let excess = up_pos.shares - down_pos.shares;
                        info!(
                            "SALVAGE: {} excess Up shares (bid ${:.4}) — selling to avoid expiry loss",
                            excess, up_bid
                        );
                        let proceeds = self.merge_engine.record_salvage(Side::Up, excess, up_bid).await;
                        if proceeds > Decimal::ZERO {
                            self.risk_manager.write().await.record_salvage_revenue(proceeds);
                        }
                    }

                    // Salvage excess Down shares
                    if down_pos.shares > up_pos.shares {
                        let excess = down_pos.shares - up_pos.shares;
                        info!(
                            "SALVAGE: {} excess Down shares (bid ${:.4}) — selling to avoid expiry loss",
                            excess, down_bid
                        );
                        let proceeds = self.merge_engine.record_salvage(Side::Down, excess, down_bid).await;
                        if proceeds > Decimal::ZERO {
                            self.risk_manager.write().await.record_salvage_revenue(proceeds);
                        }
                    }

                    // Also salvage any balanced remaining pairs (they'd both expire worthless too)
                    let (up_after, down_after) = self.merge_engine.positions().await;
                    let remaining_pairs = up_after.shares.min(down_after.shares);
                    if remaining_pairs > Decimal::ZERO {
                        // For balanced remaining pairs, sell both sides
                        info!(
                            "SALVAGE: {} balanced pairs remaining — selling both sides",
                            remaining_pairs
                        );
                        let up_proceeds = self.merge_engine.record_salvage(Side::Up, remaining_pairs, up_bid).await;
                        let down_proceeds = self.merge_engine.record_salvage(Side::Down, remaining_pairs, down_bid).await;
                        let total_proceeds = up_proceeds + down_proceeds;
                        if total_proceeds > Decimal::ZERO {
                            self.risk_manager.write().await.record_salvage_revenue(total_proceeds);
                        }
                    }
                }
            }
        }

        // Cancel any remaining open orders and release their exposure
        let open_orders = self.order_manager.all_orders().await;
        let mut cancelled_exposure = rust_decimal::Decimal::ZERO;
        for order in &open_orders {
            if order.status == crate::types::OrderStatus::Open
                || order.status == crate::types::OrderStatus::PartialFill
            {
                let unfilled = order.size - order.filled;
                cancelled_exposure += unfilled * order.price;
            }
        }
        let cancelled = self.order_manager.cancel_all().await?;
        if cancelled > 0 {
            info!("Cancelled {} remaining open orders at window close", cancelled);
            self.risk_manager
                .write()
                .await
                .release_cancelled_exposure(cancelled_exposure);
            info!(
                "Released ${:.2} exposure from cancelled orders",
                cancelled_exposure
            );
        }

        // Report results
        let pnl = self.merge_engine.pnl_snapshot().await;
        let (up_pos, down_pos) = self.merge_engine.positions().await;
        let risk = self.risk_manager.read().await;

        // Calculate unmerged share cost (these expire worthless at window close)
        let unmerged_cost = up_pos.total_cost + down_pos.total_cost;
        // Salvage loss = cost of salvaged shares - revenue received
        let salvage_loss = pnl.total_salvage_cost_basis - pnl.total_salvage_revenue;
        // NET P&L = merge profit - salvage loss - remaining unmerged losses
        // This correctly reflects the TRUE window performance
        let net_pnl = pnl.total_profit - salvage_loss - unmerged_cost;

        info!("╔══════════════════════════════════════╗");
        info!("║    WINDOW SUMMARY                    ║");
        info!("╠══════════════════════════════════════╣");
        info!("║ {} Market: {}", mode, market.slug);
        info!(
            "║ Orders placed: {} | Merges: {} | Pairs: {}",
            total_orders, pnl.total_merges, pnl.total_pairs
        );
        info!(
            "║ Total invested: ${:.4} | Merged payout: ${:.4}",
            pnl.total_invested, pnl.total_merged_payout
        );
        info!("║ Merge profit: ${:.4}", pnl.total_profit);
        if pnl.total_salvage_shares > Decimal::ZERO {
            info!(
                "║ Salvage: {} shares | cost ${:.4} → sold for ${:.4} (loss: ${:.4})",
                pnl.total_salvage_shares, pnl.total_salvage_cost_basis,
                pnl.total_salvage_revenue, salvage_loss
            );
        }
        info!(
            "║ Unmerged: {} Up (${:.4}) + {} Down (${:.4})",
            up_pos.shares, up_pos.total_cost, down_pos.shares, down_pos.total_cost
        );
        info!("║ Unmerged loss: -${:.4}", unmerged_cost);
        info!("║ NET P&L: ${:.4}", net_pnl);
        info!("║ {}", risk.summary());
        info!("╚══════════════════════════════════════╝");

        Ok(pnl.total_merges)
    }

    /// Evaluate current market conditions and place orders if favorable.
    /// Returns the number of orders placed.
    async fn evaluate_and_place_orders(
        &self,
        market: &Market,
        snapshot: &MarketSnapshot,
    ) -> Result<u64> {
        let mut placed = 0u64;

        // Get current positions
        let (up_pos, down_pos) = self.merge_engine.positions().await;
        let open_orders = self.order_manager.open_order_count().await;

        // Get pending (unfilled) orders per side — critical for imbalance calculation
        let (up_pending, down_pending) = self.order_manager.pending_shares_per_side().await;
        let (up_open_count, down_open_count) = self.order_manager.open_orders_per_side().await;

        // Per-side open order cap: prevent accumulating many one-sided orders
        // that could all fill simultaneously creating massive imbalance.
        // v11: Reduced from 2 to 1 — max 5 shares outstanding per side.
        // With 1 open per side, worst-case one-sided fill creates only 5-share excess.
        let max_open_per_side: usize = 1;
        let up_order_capped = up_open_count >= max_open_per_side;
        let down_order_capped = down_open_count >= max_open_per_side;

        // Dynamic order sizing: reduce order size as window progresses.
        // Full size in first 3 minutes, half size in last 2 minutes.
        // This naturally reduces exposure accumulation near close.
        let effective_shares = if snapshot.remaining_secs < 120 {
            // Last 2 min: half size (but at least min_order_shares)
            (self.config.shares_per_order / dec!(2)).max(self.config.min_order_shares)
        } else {
            self.config.shares_per_order
        };

        // CRITICAL FIX: Include BOTH filled positions AND pending orders in imbalance.
        // Without this, 6 UP orders can be placed while UP=0, then all fill at once
        // creating UP=30 vs DOWN=0. Now we count pending orders as virtual positions.
        let effective_up = up_pos.shares + up_pending;
        let effective_down = down_pos.shares + down_pending;

        // Position balance: don't let one side get too far ahead.
        // If we have 2x more of one side, stop buying it until the other catches up.
        // This prevents massive unmerged positions at window close.
        let max_imbalance_ratio = dec!(2.0);
        let up_heavy = effective_up > effective_down * max_imbalance_ratio
            && effective_up > self.config.shares_per_order * dec!(2);
        let down_heavy = effective_down > effective_up * max_imbalance_ratio
            && effective_down > self.config.shares_per_order * dec!(2);

        if up_heavy {
            debug!("Position imbalance: {} Up ({}+{} pending) >> {} Down, pausing Up buys",
                effective_up, up_pos.shares, up_pending, effective_down);
        }
        if down_heavy {
            debug!("Position imbalance: {} Down ({}+{} pending) >> {} Up, pausing Down buys",
                effective_down, down_pos.shares, down_pending, effective_up);
        }

        // Calculate target buy prices based on merge profitability
        // We want: up_price + down_price < 1.0 - target_edge
        let target_combined = Decimal::ONE - self.config.target_edge;

        // Position imbalance check: include pending orders (virtual position)
        // This prevents placing orders that would create a massive one-sided fill
        let up_excess = effective_up - effective_down;
        let down_excess = effective_down - effective_up;
        let up_blocked = up_excess >= self.config.max_side_imbalance || up_order_capped;
        let down_blocked = down_excess >= self.config.max_side_imbalance || down_order_capped;

        if up_order_capped {
            debug!(
                up_open = up_open_count, max = max_open_per_side,
                "UP blocked — per-side open order cap reached"
            );
        }
        if down_order_capped {
            debug!(
                down_open = down_open_count, max = max_open_per_side,
                "DOWN blocked — per-side open order cap reached"
            );
        }
        if up_excess >= self.config.max_side_imbalance {
            debug!(
                up_filled = %up_pos.shares, up_pending = %up_pending,
                down_filled = %down_pos.shares, down_pending = %down_pending,
                max_imbalance = %self.config.max_side_imbalance,
                "Skipping UP orders — position+pending imbalance limit reached"
            );
        }
        if down_excess >= self.config.max_side_imbalance {
            debug!(
                up_filled = %up_pos.shares, up_pending = %up_pending,
                down_filled = %down_pos.shares, down_pending = %down_pending,
                max_imbalance = %self.config.max_side_imbalance,
                "Skipping DOWN orders — position+pending imbalance limit reached"
            );
        }

        // === Evaluate buying Up tokens ===
        // When BTC is below opening price, Up tokens are cheaper → good time to buy
        if !up_blocked {
        if let Some(up_ask) = snapshot.up_best_ask {
            let raw_bid = self.calculate_bid_price(
                Side::Up,
                snapshot,
                &down_pos,
                target_combined,
            );

            // CRITICAL: If our bid would cross the ask, cap it at the ask price.
            // This ensures we buy at the ask (immediate fill) rather than overpaying.
            // Posting at exactly the ask makes us a taker on Polymarket, so use ask - 0.01
            // to stay maker when possible, or use the ask itself if it's already below our target.
            let our_up_bid = if raw_bid > up_ask {
                up_ask // Buy at the ask (the token is cheap enough)
            } else {
                raw_bid
            };

            debug!(
                side = "UP",
                bid = %our_up_bid,
                raw_bid = %raw_bid,
                ask = %up_ask,
                fv = %snapshot.fair_value_up,
                down_pos_shares = %down_pos.shares,
                "Bid calculation"
            );

            // v11: Other-side affordability check.
            // If we already have Up tokens but no Down, check if Down's current
            // ask would allow a profitable merge. If combined > $1.05, don't buy more Up.
            // v12: Relaxed from $1.02 to $1.05 — 5c buffer for BTC oscillation.
            let up_affordable = if up_pos.shares > Decimal::ZERO && down_pos.shares.is_zero() {
                if let Some(down_ask_price) = snapshot.down_best_ask {
                    let projected = our_up_bid + down_ask_price;
                    if projected > dec!(1.05) {
                        debug!(
                            side = "UP",
                            our_bid = %our_up_bid,
                            down_ask = %down_ask_price,
                            projected = %projected,
                            "Skipping: other side unaffordable for merge"
                        );
                        false
                    } else {
                        true
                    }
                } else {
                    true // No Down order book data → allow
                }
            } else {
                true // No one-sided risk → allow
            };

            if !up_heavy && up_affordable && self.should_buy(Side::Up, our_up_bid, up_ask, snapshot) {
                let order_cost = effective_shares * our_up_bid;
                // Check risk THEN drop the read guard before potentially writing
                let can_place = {
                    let risk = self.risk_manager.read().await;
                    risk.can_place_order(order_cost, open_orders)
                }; // read guard dropped here

                match can_place {
                    Ok(()) => {
                        match self
                            .order_manager
                            .place_limit_buy(
                                &market.condition_id,
                                &market.up_token_id,
                                Side::Up,
                                our_up_bid,
                                effective_shares,
                            )
                            .await
                        {
                            Ok(_) => {
                                self.risk_manager.write().await.record_order(order_cost);
                                placed += 1;
                            }
                            Err(e) => debug!("Failed to place Up order: {:?}", e),
                        }
                    }
                    Err(reason) => {
                        debug!(side = "UP", reason = %reason, "Risk check rejected order");
                    }
                }
            }
        }
        } // end if !up_blocked

        // === Evaluate buying Down tokens ===
        // When BTC is above opening price, Down tokens are cheaper → good time to buy
        if !down_blocked {
        if let Some(down_ask) = snapshot.down_best_ask {
            let raw_bid = self.calculate_bid_price(
                Side::Down,
                snapshot,
                &up_pos,
                target_combined,
            );

            // Same crossing fix for Down side
            let our_down_bid = if raw_bid > down_ask {
                down_ask
            } else {
                raw_bid
            };

            debug!(
                side = "DOWN",
                bid = %our_down_bid,
                raw_bid = %raw_bid,
                ask = %down_ask,
                fv = %snapshot.fair_value_down,
                up_pos_shares = %up_pos.shares,
                "Bid calculation"
            );

            // v12: Other-side affordability check for Down side (relaxed to $1.05).
            let down_affordable = if down_pos.shares > Decimal::ZERO && up_pos.shares.is_zero() {
                if let Some(up_ask_price) = snapshot.up_best_ask {
                    let projected = our_down_bid + up_ask_price;
                    if projected > dec!(1.05) {
                        debug!(
                            side = "DOWN",
                            our_bid = %our_down_bid,
                            up_ask = %up_ask_price,
                            projected = %projected,
                            "Skipping: other side unaffordable for merge"
                        );
                        false
                    } else {
                        true
                    }
                } else {
                    true
                }
            } else {
                true
            };

            if !down_heavy && down_affordable && self.should_buy(Side::Down, our_down_bid, down_ask, snapshot) {
                let order_cost = effective_shares * our_down_bid;
                let can_place = {
                    let risk = self.risk_manager.read().await;
                    risk.can_place_order(order_cost, open_orders + placed as usize)
                }; // read guard dropped

                match can_place {
                    Ok(()) => {
                        match self
                            .order_manager
                            .place_limit_buy(
                                &market.condition_id,
                                &market.down_token_id,
                                Side::Down,
                                our_down_bid,
                                effective_shares,
                            )
                            .await
                        {
                            Ok(_) => {
                                self.risk_manager.write().await.record_order(order_cost);
                                placed += 1;
                            }
                            Err(e) => debug!("Failed to place Down order: {:?}", e),
                        }
                    }
                    Err(reason) => {
                        debug!(side = "DOWN", reason = %reason, "Risk check rejected order");
                    }
                }
            }
        }
        } // end if !down_blocked

        Ok(placed)
    }

    /// Calculate our limit bid price for a side, given the other side's position
    fn calculate_bid_price(
        &self,
        side: Side,
        snapshot: &MarketSnapshot,
        other_side_pos: &crate::types::Position,
        target_combined: Decimal,
    ) -> Decimal {
        let fair_value = match side {
            Side::Up => snapshot.fair_value_up,
            Side::Down => snapshot.fair_value_down,
        };

        // v7 TWO-PHASE BUYING:
        // Phase 1 (no other position): Use max_side_price as the cap.
        //   We plan to buy the other side later when BTC reverses.
        //   Old logic used fv_other.max(0.40) which set max_bid = 0.32
        //   → 5c below ask → 0% fills. This was the #1 bug.
        // Phase 2 (have other position): Combined check with real avg_cost.
        let max_bid = if other_side_pos.shares > Decimal::ZERO {
            // Phase 2: constrained by existing position cost
            target_combined - other_side_pos.avg_cost
        } else {
            // Phase 1: just use the per-side cap
            self.config.max_side_price
        };

        // Bid up to fair_value + 0.03 (merge edge covers individual-side overpay)
        let bid = max_bid.min(fair_value + dec!(0.03));

        // Clamp: minimum 1c (Polymarket floor), max per config
        // NOTE: We intentionally use 0.01 as floor, NOT min_side_price.
        // The old min_side_price (0.20-0.35) would force bids above fair value
        // when a token is cheap, causing the fair_value check in should_buy to reject.
        bid.max(dec!(0.01))
            .min(self.config.max_side_price)
    }

    /// Determine if we should place a buy order for this side.
    ///
    /// The bid has already been adjusted in evaluate_and_place_orders (capped at ask
    /// to prevent crossing). So here we only check value and directional filters.
    fn should_buy(
        &self,
        side: Side,
        our_bid: Decimal,
        _market_ask: Decimal,
        snapshot: &MarketSnapshot,
    ) -> bool {
        // Basic validity: bid must be in Polymarket's range [0.01, 0.99]
        if our_bid < dec!(0.01) || our_bid > dec!(0.99) {
            return false;
        }

        // Don't overpay: bid must not exceed max_side_price
        if our_bid > self.config.max_side_price {
            return false;
        }

        // Fair value check: don't bid more than 3c above Black-Scholes fair value.
        // This prevents overpaying for tokens. We use 3c (matching the spread in
        // calculate_bid_price) so that ask-capped bids aren't rejected by a tighter
        // tolerance. The real protection is max_side_price ($0.48).
        let fair_value = match side {
            Side::Up => snapshot.fair_value_up,
            Side::Down => snapshot.fair_value_down,
        };
        if our_bid > fair_value + dec!(0.03) {
            debug!(
                side = %side,
                bid = %our_bid,
                fv = %fair_value,
                "Skipping: bid {:.3} > fv {:.3} + 0.03",
                our_bid, fair_value,
            );
            return false;
        }

        // v11: Tightened FV extreme threshold.
        // Data shows that buying tokens with FV < 0.20 almost always results in
        // unmerged waste that salvages at $0.01. Tighter bands reduce position risk.
        // Window is 300s. time_fraction = remaining / 300.
        // At full time: allow FV 0.20-0.80 (avoids clear losers)
        // At mid time:  allow FV ~0.26-0.74
        // At end (60s):  allow FV ~0.32-0.68 (conservative near expiry)
        // This sacrifices ~15% of merge opportunities but avoids ~40% of position losses.
        {
            let time_fraction = (snapshot.remaining_secs as f64 / 300.0).clamp(0.0, 1.0);
            let min_fv_f64 = 0.20 + 0.14 * (1.0 - time_fraction);
            let max_fv_f64 = 1.0 - min_fv_f64;
            let min_fv = Decimal::from_f64_retain(min_fv_f64)
                .unwrap_or(dec!(0.15))
                .round_dp(4);
            let max_fv = Decimal::from_f64_retain(max_fv_f64)
                .unwrap_or(dec!(0.85))
                .round_dp(4);
            if fair_value > max_fv || fair_value < min_fv {
                debug!(
                    side = %side,
                    fv = %fair_value,
                    min_fv = %min_fv,
                    max_fv = %max_fv,
                    remaining = snapshot.remaining_secs,
                    "Skipping: FV extreme for current time (need {}-{})",
                    min_fv, max_fv,
                );
                return false;
            }
        }

        // TIME-WEIGHTED WIND-DOWN: Graduated exit as window approaches close.
        // This is the primary defense against unmerged position risk.
        //
        // Phase 1 (remaining < 60s): HARD STOP — no new orders at all.
        //   Near expiry, fair values go to extremes making any purchase risky.
        // Phase 2 (remaining < 90s): Only buy if we're balancing an imbalance.
        //   We can still buy the lighter side to create merge opportunities.
        // Phase 3 (remaining < 120s): Only buy if token is a true bargain
        //   (2c+ below fair value). This naturally reduces volume.
        if snapshot.remaining_secs < 60 {
            debug!(
                side = %side,
                remaining = snapshot.remaining_secs,
                "WIND-DOWN: hard stop — no new orders (< 60s)",
            );
            return false;
        }

        if snapshot.remaining_secs < 90 {
            // Phase 2: Only allow balancing buys.
            // Proxy: only buy if this side's fair value < 0.45 (the "cheap" underdog side).
            // When BTC trends, one side's FV goes high (>0.55) and the other low (<0.45).
            // Buying only the cheap side helps balance positions for more merges.
            if fair_value >= dec!(0.45) {
                debug!(
                    side = %side,
                    remaining = snapshot.remaining_secs,
                    fv = %fair_value,
                    "WIND-DOWN: only balancing buys allowed (< 90s), fv too high",
                );
                return false;
            }
        }

        if snapshot.remaining_secs < 120 {
            // Phase 3: Only buy true bargains (2c below fair value)
            if our_bid >= fair_value - dec!(0.02) {
                debug!(
                    side = %side,
                    remaining = snapshot.remaining_secs,
                    bid = %our_bid,
                    fv = %fair_value,
                    "WIND-DOWN: only bargains allowed (< 120s), bid not cheap enough",
                );
                return false;
            }
        }

        // Directional preference: buy each side when it's the "cheap" side.
        // BTC below opening → Up is cheap; BTC above opening → Down is cheap.
        // Use 0.03% tolerance so both sides can be bought near the opening price.
        let btc_delta_pct = if snapshot.opening_price > Decimal::ZERO {
            (snapshot.btc_price - snapshot.opening_price) / snapshot.opening_price
        } else {
            Decimal::ZERO
        };

        let is_cheap_side = match side {
            Side::Up => btc_delta_pct < dec!(0.0003),    // BTC flat or dipping
            Side::Down => btc_delta_pct > dec!(-0.0003),  // BTC flat or rising
        };

        if !is_cheap_side {
            // Even if not the preferred side, accept bargains below fair value
            if our_bid < fair_value - dec!(0.02) {
                return true;
            }
            return false;
        }

        true
    }
}

// Helper to expose decimal_to_f64 for strategy
impl crate::pricing::VolatilityEstimator {
    pub fn add_price_from_tick(&mut self, tick: &PriceTick) {
        let ts = tick.timestamp.timestamp() as u64;
        let price = crate::pricing::decimal_to_f64_pub(tick.price);
        self.add_price(ts, price);
    }
}
