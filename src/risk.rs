//! Risk management: circuit breakers, position limits, daily stop loss.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{info, warn};

/// Risk manager enforces trading limits
pub struct RiskManager {
    /// Starting capital for the session
    starting_capital: Decimal,
    /// Current available capital
    available_capital: Decimal,
    /// Maximum position as fraction of capital
    max_position_pct: Decimal,
    /// Daily stop loss threshold (fraction)
    daily_stop_loss_pct: Decimal,
    /// Maximum consecutive losses before circuit breaker
    consecutive_loss_limit: u32,
    /// Current consecutive losses
    consecutive_losses: u32,
    /// Total P&L for the day
    daily_pnl: Decimal,
    /// Maximum exposure per market window
    max_exposure_per_market: Decimal,
    /// Current market window exposure
    current_market_exposure: Decimal,
    /// Maximum open orders
    max_open_orders: usize,
    /// Whether trading is halted
    halted: bool,
    /// Halt reason
    halt_reason: Option<String>,
}

impl RiskManager {
    pub fn new(
        starting_capital: Decimal,
        max_position_pct: Decimal,
        daily_stop_loss_pct: Decimal,
        consecutive_loss_limit: u32,
        max_exposure_per_market: Decimal,
        max_open_orders: usize,
    ) -> Self {
        Self {
            starting_capital,
            available_capital: starting_capital,
            max_position_pct,
            daily_stop_loss_pct,
            consecutive_loss_limit,
            consecutive_losses: 0,
            daily_pnl: Decimal::ZERO,
            max_exposure_per_market,
            current_market_exposure: Decimal::ZERO,
            max_open_orders,
            halted: false,
            halt_reason: None,
        }
    }

    /// Check if a new order is allowed
    pub fn can_place_order(
        &self,
        order_cost: Decimal,
        current_open_orders: usize,
    ) -> Result<(), String> {
        if self.halted {
            return Err(format!(
                "Trading halted: {}",
                self.halt_reason.as_deref().unwrap_or("unknown")
            ));
        }

        // Check daily stop loss
        if self.daily_pnl < -(self.starting_capital * self.daily_stop_loss_pct) {
            return Err(format!(
                "Daily stop loss hit: P&L ${:.2} exceeds {:.0}% limit",
                self.daily_pnl,
                self.daily_stop_loss_pct * dec!(100)
            ));
        }

        // Check consecutive losses
        if self.consecutive_losses >= self.consecutive_loss_limit {
            return Err(format!(
                "Circuit breaker: {} consecutive losses (limit: {})",
                self.consecutive_losses, self.consecutive_loss_limit
            ));
        }

        // Check available capital
        if order_cost > self.available_capital {
            return Err(format!(
                "Insufficient capital: need ${:.2}, have ${:.2}",
                order_cost, self.available_capital
            ));
        }

        // Check max position size
        let max_order = self.starting_capital * self.max_position_pct;
        if order_cost > max_order {
            return Err(format!(
                "Order ${:.2} exceeds max position {:.0}% = ${:.2}",
                order_cost,
                self.max_position_pct * dec!(100),
                max_order
            ));
        }

        // Check market exposure limit
        if self.current_market_exposure + order_cost > self.max_exposure_per_market {
            return Err(format!(
                "Market exposure ${:.2} + ${:.2} would exceed limit ${:.2}",
                self.current_market_exposure, order_cost, self.max_exposure_per_market
            ));
        }

        // Check open orders limit
        if current_open_orders >= self.max_open_orders {
            return Err(format!(
                "Too many open orders: {} (limit: {})",
                current_open_orders, self.max_open_orders
            ));
        }

        Ok(())
    }

    /// Record capital allocated to an order
    pub fn record_order(&mut self, cost: Decimal) {
        self.available_capital -= cost;
        self.current_market_exposure += cost;
    }

    /// Record a merge result. Releases exposure for the merged cost.
    pub fn record_merge(&mut self, profit: Decimal, cost_returned: Decimal) {
        self.daily_pnl += profit;
        self.available_capital += cost_returned + profit;

        // Release the exposure for merged pairs — those shares have been
        // cashed out and no longer represent risk.
        let release = cost_returned.min(self.current_market_exposure);
        self.current_market_exposure -= release;
        info!(
            released_exposure = %release,
            remaining_exposure = %self.current_market_exposure,
            "Exposure released after merge"
        );

        if profit > Decimal::ZERO {
            self.consecutive_losses = 0;
            info!(
                daily_pnl = %self.daily_pnl,
                available = %self.available_capital,
                "Profitable merge, consecutive losses reset"
            );
        } else {
            self.consecutive_losses += 1;
            warn!(
                consecutive_losses = self.consecutive_losses,
                daily_pnl = %self.daily_pnl,
                "Loss on merge, consecutive losses: {}",
                self.consecutive_losses
            );

            if self.consecutive_losses >= self.consecutive_loss_limit {
                self.halt("Circuit breaker triggered: too many consecutive losses");
            }
        }

        // Check daily stop loss
        if self.daily_pnl < -(self.starting_capital * self.daily_stop_loss_pct) {
            self.halt("Daily stop loss exceeded");
        }
    }

    /// Release exposure for cancelled orders that never filled
    pub fn release_cancelled_exposure(&mut self, cost: Decimal) {
        let release = cost.min(self.current_market_exposure);
        self.current_market_exposure -= release;
        self.available_capital += cost;
    }

    /// Reset for a new market window
    pub fn reset_market_exposure(&mut self) {
        self.current_market_exposure = Decimal::ZERO;
    }

    /// Halt trading
    pub fn halt(&mut self, reason: &str) {
        self.halted = true;
        self.halt_reason = Some(reason.to_string());
        warn!(reason = reason, "TRADING HALTED");
    }

    /// Check if trading is halted
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// Get halt reason
    pub fn halt_reason(&self) -> Option<&str> {
        self.halt_reason.as_deref()
    }

    /// Get available capital
    pub fn available_capital(&self) -> Decimal {
        self.available_capital
    }

    /// Get daily P&L
    pub fn daily_pnl(&self) -> Decimal {
        self.daily_pnl
    }

    /// Get current market exposure
    pub fn market_exposure(&self) -> Decimal {
        self.current_market_exposure
    }

    /// Get a summary string
    pub fn summary(&self) -> String {
        format!(
            "Capital: ${:.2} | Daily P&L: ${:.2} | Market Exp: ${:.2} | Consec Losses: {} | Halted: {}",
            self.available_capital,
            self.daily_pnl,
            self.current_market_exposure,
            self.consecutive_losses,
            self.halted
        )
    }
}
