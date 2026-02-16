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
}

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

            // Check risk limits
            if self.risk_manager.read().await.is_halted() {
                warn!("Risk manager halted trading");
                break;
            }

            // Try to place orders on each side
            let placed = self.evaluate_and_place_orders(market, &snapshot).await?;
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

                // Record fills in merge engine
                for fill_id in &up_fills {
                    let orders = self.order_manager.all_orders().await;
                    if let Some(order) = orders.iter().find(|o| &o.id == fill_id) {
                        self.merge_engine
                            .record_fill(Side::Up, order.filled, order.price)
                            .await;
                    }
                }
                for fill_id in &down_fills {
                    let orders = self.order_manager.all_orders().await;
                    if let Some(order) = orders.iter().find(|o| &o.id == fill_id) {
                        self.merge_engine
                            .record_fill(Side::Down, order.filled, order.price)
                            .await;
                    }
                }
            }

            // Try to merge any available pairs
            if let Some(merge_result) = self.merge_engine.try_merge().await {
                let mut risk = self.risk_manager.write().await;
                risk.record_merge(merge_result.profit, merge_result.total_cost);
            }
        }

        // Phase 3: End of window — final merge attempt
        if let Some(merge_result) = self.merge_engine.try_merge().await {
            let mut risk = self.risk_manager.write().await;
            risk.record_merge(merge_result.profit, merge_result.total_cost);
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
        let net_pnl = pnl.total_profit - unmerged_cost;

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

        // Position balance: don't let one side get too far ahead.
        // If we have 3x more of one side, stop buying it until the other catches up.
        // This prevents massive unmerged positions at window close.
        let max_imbalance_ratio = dec!(3.0);
        let up_heavy = up_pos.shares > down_pos.shares * max_imbalance_ratio
            && up_pos.shares > self.config.shares_per_order * dec!(2);
        let down_heavy = down_pos.shares > up_pos.shares * max_imbalance_ratio
            && down_pos.shares > self.config.shares_per_order * dec!(2);

        if up_heavy {
            debug!("Position imbalance: {} Up >> {} Down, pausing Up buys", up_pos.shares, down_pos.shares);
        }
        if down_heavy {
            debug!("Position imbalance: {} Down >> {} Up, pausing Down buys", down_pos.shares, up_pos.shares);
        }

        // Calculate target buy prices based on merge profitability
        // We want: up_price + down_price < 1.0 - target_edge
        let target_combined = Decimal::ONE - self.config.target_edge;

        // Position imbalance check: don't let one side get too far ahead
        let up_excess = up_pos.shares - down_pos.shares;
        let down_excess = down_pos.shares - up_pos.shares;
        let up_blocked = up_excess >= self.config.max_side_imbalance;
        let down_blocked = down_excess >= self.config.max_side_imbalance;

        if up_blocked {
            debug!(
                up = %up_pos.shares, down = %down_pos.shares,
                max_imbalance = %self.config.max_side_imbalance,
                "Skipping UP orders — position imbalance limit reached"
            );
        }
        if down_blocked {
            debug!(
                up = %up_pos.shares, down = %down_pos.shares,
                max_imbalance = %self.config.max_side_imbalance,
                "Skipping DOWN orders — position imbalance limit reached"
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

            if !up_heavy && self.should_buy(Side::Up, our_up_bid, up_ask, snapshot) {
                let order_cost = self.config.shares_per_order * our_up_bid;
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
                                self.config.shares_per_order,
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

            if !down_heavy && self.should_buy(Side::Down, our_down_bid, down_ask, snapshot) {
                let order_cost = self.config.shares_per_order * our_down_bid;
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
                                self.config.shares_per_order,
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

        // If we already have the other side, use our actual avg cost
        let other_cost = if other_side_pos.shares > Decimal::ZERO {
            other_side_pos.avg_cost
        } else {
            // Estimate from fair value of the other side, but use a conservative
            // floor of 0.40. When BTC swings, the other side's fair value can be
            // very low (e.g. 0.15), making us bid too high on this side. By assuming
            // the other side will cost at least 0.40, we keep combined cost < $1.
            let fv_other = match side {
                Side::Up => snapshot.fair_value_down,
                Side::Down => snapshot.fair_value_up,
            };
            fv_other.max(dec!(0.40))
        };

        // Our max bid = target_combined - other_side_cost
        let max_bid = target_combined - other_cost;

        // Don't bid above fair value (we want edge, not overpay)
        let bid = max_bid.min(fair_value);

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

        // Fair value check: don't bid more than 2c above Black-Scholes fair value.
        // This prevents overpaying for tokens. The bid is already capped at fair_value
        // in calculate_bid_price, but the ask-cap in evaluate_and_place may have lowered it
        // further, so this check should almost always pass.
        let fair_value = match side {
            Side::Up => snapshot.fair_value_up,
            Side::Down => snapshot.fair_value_down,
        };
        if our_bid > fair_value + dec!(0.02) {
            debug!(
                side = %side,
                bid = %our_bid,
                fv = %fair_value,
                "Skipping: bid {:.3} > fv {:.3} + 0.02",
                our_bid, fair_value,
            );
            return false;
        }

        // CRITICAL FIX: Don't buy at extreme fair values (> 0.80 or < 0.20).
        // When one side is at an extreme, the combined cost of Up + Down > $1.00,
        // making merges unprofitable. Only buy when prices are balanced.
        if fair_value > dec!(0.80) || fair_value < dec!(0.20) {
            debug!(
                side = %side,
                fv = %fair_value,
                "Skipping: extreme fair value — merge would be unprofitable",
            );
            return false;
        }

        // CRITICAL FIX: Don't buy in the last 30 seconds of the window.
        // Near expiry, fair values go to extremes (0.99 or 0.01) making
        // any purchase likely to be on the wrong side of a binary outcome.
        if snapshot.remaining_secs < 30 {
            debug!(
                side = %side,
                remaining = snapshot.remaining_secs,
                "Skipping: too close to expiry (< 30s)",
            );
            return false;
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
