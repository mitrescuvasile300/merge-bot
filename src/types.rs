use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Which side of the binary option
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Up,
    Down,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Side::Up => write!(f, "UP"),
            Side::Down => write!(f, "DOWN"),
        }
    }
}

impl Side {
    pub fn opposite(&self) -> Self {
        match self {
            Side::Up => Side::Down,
            Side::Down => Side::Up,
        }
    }
}

/// A Polymarket BTC 5-minute binary market
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    /// Polymarket condition ID
    pub condition_id: String,
    /// Question / slug
    pub slug: String,
    /// Token ID for the Up outcome
    pub up_token_id: String,
    /// Token ID for the Down outcome
    pub down_token_id: String,
    /// Window start timestamp (Unix seconds)
    pub window_start: u64,
    /// Window end timestamp (Unix seconds)
    pub window_end: u64,
    /// BTC opening price for this window
    pub opening_price: Option<Decimal>,
    /// Whether the market is still active
    pub active: bool,
}

impl Market {
    /// Seconds remaining until window close
    pub fn seconds_remaining(&self) -> i64 {
        let now = Utc::now().timestamp() as u64;
        if now >= self.window_end {
            0
        } else {
            (self.window_end - now) as i64
        }
    }

    /// Seconds elapsed since window open
    pub fn seconds_elapsed(&self) -> u64 {
        let now = Utc::now().timestamp() as u64;
        if now <= self.window_start {
            0
        } else {
            now - self.window_start
        }
    }

    /// Get token ID for a given side
    pub fn token_id(&self, side: Side) -> &str {
        match side {
            Side::Up => &self.up_token_id,
            Side::Down => &self.down_token_id,
        }
    }
}

/// An order book level (price + size)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: Decimal,
    pub size: Decimal,
}

/// Full order book for one side of a market
#[derive(Debug, Clone, Default)]
pub struct OrderBook {
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    pub timestamp: Option<DateTime<Utc>>,
}

impl OrderBook {
    /// Best bid price (highest buy order)
    pub fn best_bid(&self) -> Option<Decimal> {
        self.bids.first().map(|l| l.price)
    }

    /// Best ask price (lowest sell order)
    pub fn best_ask(&self) -> Option<Decimal> {
        self.asks.first().map(|l| l.price)
    }

    /// Midpoint price
    pub fn midpoint(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / Decimal::TWO),
            (Some(bid), None) => Some(bid),
            (None, Some(ask)) => Some(ask),
            _ => None,
        }
    }

    /// Spread in absolute terms
    pub fn spread(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some(ask - bid),
            _ => None,
        }
    }
}

/// A trade order (placed or simulated)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub market_condition_id: String,
    pub token_id: String,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub filled: Decimal,
    pub status: OrderStatus,
    pub created_at: DateTime<Utc>,
    pub is_dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    Pending,
    Open,
    PartialFill,
    Filled,
    Cancelled,
    Rejected,
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderStatus::Pending => write!(f, "PENDING"),
            OrderStatus::Open => write!(f, "OPEN"),
            OrderStatus::PartialFill => write!(f, "PARTIAL"),
            OrderStatus::Filled => write!(f, "FILLED"),
            OrderStatus::Cancelled => write!(f, "CANCELLED"),
            OrderStatus::Rejected => write!(f, "REJECTED"),
        }
    }
}

/// Position for one side of a market
#[derive(Debug, Clone, Default)]
pub struct Position {
    pub shares: Decimal,
    pub avg_cost: Decimal,
    pub total_cost: Decimal,
}

/// Merge result: 1 Up + 1 Down = $1 USDC
#[derive(Debug, Clone, Serialize)]
pub struct MergeResult {
    pub pairs_merged: Decimal,
    pub total_cost: Decimal,
    pub total_payout: Decimal,
    pub profit: Decimal,
    pub profit_pct: Decimal,
    pub timestamp: DateTime<Utc>,
}

/// Running P&L tracker
#[derive(Debug, Clone, Default, Serialize)]
pub struct PnlSnapshot {
    pub total_invested: Decimal,
    pub total_merged_payout: Decimal,
    pub total_profit: Decimal,
    pub total_merges: u64,
    pub total_pairs: Decimal,
    pub total_orders: u64,
    pub markets_traded: u64,
    pub win_rate: Decimal,
}

/// BTC price tick from a feed
#[derive(Debug, Clone)]
pub struct PriceTick {
    pub price: Decimal,
    pub timestamp: DateTime<Utc>,
    pub source: PriceSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSource {
    Binance,
    Chainlink,
    Simulated,
}
