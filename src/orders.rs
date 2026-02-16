//! Order management for Polymarket CLOB.
//!
//! Handles order creation, placement, cancellation, and fill tracking.
//! In dry-run mode, simulates order matching against the order book.

use anyhow::{Context, Result};
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};
use uuid::Uuid;

use crate::types::{Order, OrderBook, OrderStatus, Side};

/// Order manager handles all order operations
pub struct OrderManager {
    /// All orders indexed by ID
    orders: Arc<RwLock<HashMap<String, Order>>>,
    /// Whether we're in dry-run mode
    dry_run: bool,
    /// CLOB API URL (for live mode)
    clob_url: String,
    /// HTTP client
    client: reqwest::Client,
    /// API credentials (live mode)
    api_key: Option<String>,
    api_secret: Option<String>,
    passphrase: Option<String>,
}

impl OrderManager {
    pub fn new(dry_run: bool, clob_url: &str) -> Self {
        Self {
            orders: Arc::new(RwLock::new(HashMap::new())),
            dry_run,
            clob_url: clob_url.to_string(),
            client: reqwest::Client::new(),
            api_key: None,
            api_secret: None,
            passphrase: None,
        }
    }

    /// Set API credentials for live mode
    pub fn with_credentials(
        mut self,
        api_key: Option<String>,
        api_secret: Option<String>,
        passphrase: Option<String>,
    ) -> Self {
        self.api_key = api_key;
        self.api_secret = api_secret;
        self.passphrase = passphrase;
        self
    }

    /// Place a limit BUY order for a side.
    /// Returns the order ID if successful.
    pub async fn place_limit_buy(
        &self,
        market_condition_id: &str,
        token_id: &str,
        side: Side,
        price: Decimal,
        size: Decimal,
    ) -> Result<String> {
        // Validate order parameters
        if size < dec!(5) {
            anyhow::bail!("Order size {} below minimum of 5 shares", size);
        }
        if price <= Decimal::ZERO || price >= Decimal::ONE {
            anyhow::bail!("Invalid price: {} (must be 0 < p < 1)", price);
        }

        let order_id = Uuid::new_v4().to_string();
        let order = Order {
            id: order_id.clone(),
            market_condition_id: market_condition_id.to_string(),
            token_id: token_id.to_string(),
            side,
            price,
            fill_price: price, // Will be updated on fill with price improvement
            size,
            filled: Decimal::ZERO,
            status: OrderStatus::Open,
            created_at: Utc::now(),
            is_dry_run: self.dry_run,
        };

        if self.dry_run {
            info!(
                order_id = %order_id,
                side = %side,
                price = %price,
                size = %size,
                "[DRY-RUN] Limit BUY order placed"
            );
        } else {
            // Live order placement via CLOB API
            self.submit_order_to_clob(&order).await?;
            info!(
                order_id = %order_id,
                side = %side,
                price = %price,
                size = %size,
                "[LIVE] Limit BUY order placed"
            );
        }

        self.orders.write().await.insert(order_id.clone(), order);
        Ok(order_id)
    }

    /// Cancel an open order
    pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
        let mut orders = self.orders.write().await;
        if let Some(order) = orders.get_mut(order_id) {
            if order.status == OrderStatus::Open || order.status == OrderStatus::PartialFill {
                if !self.dry_run {
                    self.cancel_order_on_clob(order_id).await?;
                }
                order.status = OrderStatus::Cancelled;
                debug!(order_id = %order_id, "Order cancelled");
            }
        }
        Ok(())
    }

    /// Cancel all open orders
    pub async fn cancel_all(&self) -> Result<u32> {
        let mut orders = self.orders.write().await;
        let mut cancelled = 0u32;

        for order in orders.values_mut() {
            if order.status == OrderStatus::Open || order.status == OrderStatus::PartialFill {
                order.status = OrderStatus::Cancelled;
                cancelled += 1;
            }
        }

        if !self.dry_run && cancelled > 0 {
            // Batch cancel on CLOB
            let _ = self
                .client
                .delete(format!("{}/cancel-all", self.clob_url))
                .send()
                .await;
        }

        if cancelled > 0 {
            info!(count = cancelled, "Cancelled all open orders");
        }

        Ok(cancelled)
    }

    /// Cancel stale orders that are unlikely to fill.
    ///
    /// An order is "stale" if it's been open for > `max_age_secs` seconds AND
    /// the current best ask for its token is > `min_gap` above our bid price.
    /// This prevents phantom imbalance from unfillable orders blocking new ones.
    ///
    /// Returns (cancelled_count, cancelled_exposure)
    pub async fn cancel_stale_orders(
        &self,
        max_age_secs: u64,
        up_best_ask: Option<Decimal>,
        down_best_ask: Option<Decimal>,
        min_gap: Decimal,
    ) -> (u32, Decimal) {
        let now = chrono::Utc::now();
        let mut orders = self.orders.write().await;
        let mut cancelled = 0u32;
        let mut released_exposure = Decimal::ZERO;

        for order in orders.values_mut() {
            if order.status != OrderStatus::Open && order.status != OrderStatus::PartialFill {
                continue;
            }

            let age = (now - order.created_at).num_seconds() as u64;
            if age < max_age_secs {
                continue;
            }

            // Check if the current ask is far enough above our bid
            let current_ask = match order.side {
                Side::Up => up_best_ask,
                Side::Down => down_best_ask,
            };

            let should_cancel = match current_ask {
                Some(ask) => ask > order.price + min_gap,
                None => true, // No ask available → cancel
            };

            if should_cancel {
                let unfilled = order.size - order.filled;
                released_exposure += unfilled * order.price;
                order.status = OrderStatus::Cancelled;
                cancelled += 1;
                debug!(
                    order_id = %order.id,
                    side = %order.side,
                    age_secs = age,
                    bid = %order.price,
                    current_ask = ?current_ask,
                    "Cancelled stale order (unlikely to fill)"
                );
            }
        }

        (cancelled, released_exposure)
    }

    /// Simulate order fills against the current order book.
    /// In dry-run mode, this checks if our limit orders would have been filled.
    pub async fn simulate_fills(&self, book: &OrderBook, token_id: &str) -> Vec<String> {
        let mut filled_ids = Vec::new();
        let mut orders = self.orders.write().await;

        for order in orders.values_mut() {
            if order.token_id != token_id {
                continue;
            }
            if order.status != OrderStatus::Open && order.status != OrderStatus::PartialFill {
                continue;
            }

            // For a BUY limit order, it fills when there's a seller at or below our price
            // Check if best ask <= our bid price
            if let Some(best_ask) = book.best_ask() {
                if best_ask <= order.price {
                    // Full fill — price improvement: execute at ask, not our limit
                    // On a real CLOB, if our limit is $0.48 and ask is $0.40,
                    // we buy at $0.40 (price improvement). This matters for P&L accuracy.
                    order.filled = order.size;
                    order.fill_price = best_ask; // Execute at the better price
                    order.status = OrderStatus::Filled;
                    filled_ids.push(order.id.clone());

                    info!(
                        order_id = %order.id,
                        side = %order.side,
                        price = %order.fill_price,
                        size = %order.size,
                        "[DRY-RUN] Order FILLED (best_ask={} <= our_bid={})",
                        best_ask, order.price
                    );
                }
            }
        }

        filled_ids
    }

    /// Get all filled orders for a specific side and market
    pub async fn filled_shares(&self, market_condition_id: &str, side: Side) -> (Decimal, Decimal) {
        let orders = self.orders.read().await;
        let mut total_shares = Decimal::ZERO;
        let mut total_cost = Decimal::ZERO;

        for order in orders.values() {
            if order.market_condition_id != market_condition_id {
                continue;
            }
            if order.side != side {
                continue;
            }
            if order.status != OrderStatus::Filled {
                continue;
            }

            total_shares += order.filled;
            total_cost += order.filled * order.fill_price;
        }

        (total_shares, total_cost)
    }

    /// Get count of open orders
    pub async fn open_order_count(&self) -> usize {
        let orders = self.orders.read().await;
        orders
            .values()
            .filter(|o| o.status == OrderStatus::Open || o.status == OrderStatus::PartialFill)
            .count()
    }

    /// Get pending (unfilled) shares per side — used for imbalance checks
    /// Returns (up_pending_shares, down_pending_shares)
    pub async fn pending_shares_per_side(&self) -> (Decimal, Decimal) {
        let orders = self.orders.read().await;
        let mut up_pending = Decimal::ZERO;
        let mut down_pending = Decimal::ZERO;

        for order in orders.values() {
            if order.status == OrderStatus::Open || order.status == OrderStatus::PartialFill {
                let remaining = order.size - order.filled;
                match order.side {
                    Side::Up => up_pending += remaining,
                    Side::Down => down_pending += remaining,
                }
            }
        }

        (up_pending, down_pending)
    }

    /// Get count of open orders per side — used for per-side order caps
    /// Returns (up_open_count, down_open_count)
    pub async fn open_orders_per_side(&self) -> (usize, usize) {
        let orders = self.orders.read().await;
        let mut up_count = 0usize;
        let mut down_count = 0usize;

        for order in orders.values() {
            if order.status == OrderStatus::Open || order.status == OrderStatus::PartialFill {
                match order.side {
                    Side::Up => up_count += 1,
                    Side::Down => down_count += 1,
                }
            }
        }

        (up_count, down_count)
    }

    /// Get all orders as a snapshot
    pub async fn all_orders(&self) -> Vec<Order> {
        self.orders.read().await.values().cloned().collect()
    }

    /// Submit order to Polymarket CLOB (live mode only)
    async fn submit_order_to_clob(&self, order: &Order) -> Result<()> {
        let payload = json!({
            "order": {
                "tokenID": order.token_id,
                "price": order.price.to_string(),
                "size": order.size.to_string(),
                "side": "BUY",
                "type": "GTC",
            }
        });

        let mut req = self.client.post(format!("{}/order", self.clob_url));

        // Add auth headers if available
        if let (Some(key), Some(secret), Some(pass)) = (
            &self.api_key,
            &self.api_secret,
            &self.passphrase,
        ) {
            req = req
                .header("POLY_API_KEY", key)
                .header("POLY_API_SECRET", secret)
                .header("POLY_PASSPHRASE", pass);
        }

        let resp = req
            .json(&payload)
            .send()
            .await
            .context("Failed to submit order to CLOB")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("CLOB order rejected: {} - {}", status, body);
        }

        Ok(())
    }

    /// Cancel order on Polymarket CLOB (live mode only)
    async fn cancel_order_on_clob(&self, order_id: &str) -> Result<()> {
        let mut req = self
            .client
            .delete(format!("{}/order/{}", self.clob_url, order_id));

        if let (Some(key), Some(secret), Some(pass)) = (
            &self.api_key,
            &self.api_secret,
            &self.passphrase,
        ) {
            req = req
                .header("POLY_API_KEY", key)
                .header("POLY_API_SECRET", secret)
                .header("POLY_PASSPHRASE", pass);
        }

        req.send().await.context("Failed to cancel order on CLOB")?;
        Ok(())
    }
}
