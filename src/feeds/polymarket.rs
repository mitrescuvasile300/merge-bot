//! Polymarket order book WebSocket feed.
//!
//! Connects to `wss://ws-subscriptions-clob.polymarket.com/ws/market`
//! and streams real-time order book updates for Up and Down tokens.

use anyhow::{Context, Result};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::types::{BookLevel, OrderBook, Side};

/// Subscription message for the Polymarket WebSocket
#[derive(Debug, Serialize)]
struct WsSubscription {
    r#type: String,
    assets_ids: Vec<String>,
}

/// WebSocket message from Polymarket
#[derive(Debug, Deserialize)]
struct WsBookMessage {
    #[serde(default)]
    event_type: String,
    #[serde(default)]
    asset_id: String,
    #[serde(default)]
    market: String,
    #[serde(default)]
    bids: Vec<WsBookLevel>,
    #[serde(default)]
    asks: Vec<WsBookLevel>,
    #[serde(default)]
    hash: String,
    #[serde(default)]
    timestamp: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WsBookLevel {
    price: String,
    size: String,
}

/// Shared order book state for both sides of a market
#[derive(Debug, Clone)]
pub struct MarketBooks {
    pub up_book: OrderBook,
    pub down_book: OrderBook,
}

impl Default for MarketBooks {
    fn default() -> Self {
        Self {
            up_book: OrderBook::default(),
            down_book: OrderBook::default(),
        }
    }
}

/// Manages WebSocket connections to Polymarket order book
pub struct PolymarketFeed {
    /// Shared order book state
    books: Arc<RwLock<MarketBooks>>,
    /// WebSocket URL
    ws_url: String,
    /// Up token ID
    up_token_id: String,
    /// Down token ID
    down_token_id: String,
}

impl PolymarketFeed {
    pub fn new(
        ws_url: &str,
        up_token_id: &str,
        down_token_id: &str,
    ) -> Self {
        Self {
            books: Arc::new(RwLock::new(MarketBooks::default())),
            ws_url: ws_url.to_string(),
            up_token_id: up_token_id.to_string(),
            down_token_id: down_token_id.to_string(),
        }
    }

    /// Get shared reference to the order books
    pub fn books(&self) -> Arc<RwLock<MarketBooks>> {
        self.books.clone()
    }

    /// Get the current best ask (lowest sell price) for a side
    pub async fn best_ask(&self, side: Side) -> Option<Decimal> {
        let books = self.books.read().await;
        match side {
            Side::Up => books.up_book.best_ask(),
            Side::Down => books.down_book.best_ask(),
        }
    }

    /// Get the current best bid (highest buy price) for a side
    pub async fn best_bid(&self, side: Side) -> Option<Decimal> {
        let books = self.books.read().await;
        match side {
            Side::Up => books.up_book.best_bid(),
            Side::Down => books.down_book.best_bid(),
        }
    }

    /// Get the midpoint price for a side
    pub async fn midpoint(&self, side: Side) -> Option<Decimal> {
        let books = self.books.read().await;
        match side {
            Side::Up => books.up_book.midpoint(),
            Side::Down => books.down_book.midpoint(),
        }
    }

    /// Run the WebSocket connection (reconnects on failure)
    pub async fn run(&self) -> Result<()> {
        loop {
            match self.connect_and_stream().await {
                Ok(()) => {
                    warn!("Polymarket WebSocket disconnected, reconnecting...");
                }
                Err(e) => {
                    error!("Polymarket WebSocket error: {:?}, reconnecting in 3s", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                }
            }
        }
    }

    async fn connect_and_stream(&self) -> Result<()> {
        info!(
            url = %self.ws_url,
            up_token = %self.up_token_id,
            down_token = %self.down_token_id,
            "Connecting to Polymarket order book WebSocket"
        );

        let (ws_stream, _) = connect_async(&self.ws_url)
            .await
            .context("Failed to connect to Polymarket WebSocket")?;

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to both token order books
        let sub_msg = serde_json::json!({
            "type": "subscribe",
            "assets_ids": [&self.up_token_id, &self.down_token_id],
        });

        write
            .send(Message::Text(sub_msg.to_string()))
            .await
            .context("Failed to send subscription")?;

        info!("Subscribed to Polymarket order books");

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Err(e) = self.handle_message(&text).await {
                        debug!("Failed to parse Polymarket WS message: {:?}", e);
                    }
                }
                Ok(Message::Ping(_)) => {
                    debug!("Polymarket ping");
                }
                Ok(Message::Close(_)) => {
                    warn!("Polymarket WebSocket closed by server");
                    break;
                }
                Err(e) => {
                    error!("Polymarket WebSocket read error: {:?}", e);
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn handle_message(&self, text: &str) -> Result<()> {
        let msg: WsBookMessage =
            serde_json::from_str(text).context("Failed to parse WS book message")?;

        // Determine which side this update is for
        let side = if msg.asset_id == self.up_token_id {
            Some(Side::Up)
        } else if msg.asset_id == self.down_token_id {
            Some(Side::Down)
        } else {
            None
        };

        if let Some(side) = side {
            let bids: Vec<BookLevel> = msg
                .bids
                .iter()
                .filter_map(|l| {
                    Some(BookLevel {
                        price: l.price.parse().ok()?,
                        size: l.size.parse().ok()?,
                    })
                })
                .collect();

            let asks: Vec<BookLevel> = msg
                .asks
                .iter()
                .filter_map(|l| {
                    Some(BookLevel {
                        price: l.price.parse().ok()?,
                        size: l.size.parse().ok()?,
                    })
                })
                .collect();

            let book = OrderBook {
                bids,
                asks,
                timestamp: Some(Utc::now()),
            };

            let mut books = self.books.write().await;
            match side {
                Side::Up => books.up_book = book,
                Side::Down => books.down_book = book,
            }

            debug!(
                side = %side,
                best_bid = ?match side {
                    Side::Up => books.up_book.best_bid(),
                    Side::Down => books.down_book.best_bid(),
                },
                best_ask = ?match side {
                    Side::Up => books.up_book.best_ask(),
                    Side::Down => books.down_book.best_ask(),
                },
                "Order book updated"
            );
        }

        Ok(())
    }
}

/// Simulated order book feed for dry-run mode.
/// Generates realistic order book data based on BTC price movements.
pub struct SimulatedPolymarketFeed {
    books: Arc<RwLock<MarketBooks>>,
    /// BTC opening price for the window (updatable per-window)
    opening_price: Arc<RwLock<Decimal>>,
}

impl SimulatedPolymarketFeed {
    pub fn new(opening_price: Decimal) -> Self {
        Self {
            books: Arc::new(RwLock::new(MarketBooks::default())),
            opening_price: Arc::new(RwLock::new(opening_price)),
        }
    }

    pub fn books(&self) -> Arc<RwLock<MarketBooks>> {
        self.books.clone()
    }

    /// Update opening price at the start of each new window
    pub async fn set_opening_price(&self, price: Decimal) {
        *self.opening_price.write().await = price;
    }

    /// Update simulated books based on current BTC price.
    ///
    /// Uses a realistic 5-cent spread (real Polymarket 5-min options have 3-8c spreads).
    /// The book has 3 levels of depth at increasing spreads.
    pub async fn update_from_btc_price(
        &self,
        btc_price: Decimal,
        remaining_secs: u64,
    ) {
        let pricer = crate::pricing::BinaryPricer::new();
        let sigma = crate::pricing::VolatilityEstimator::default_volatility();
        let opening = *self.opening_price.read().await;

        let fair_up = pricer.fair_value_up(btc_price, opening, sigma, remaining_secs);
        let fair_down = Decimal::ONE - fair_up;

        // Realistic spread: 5c total (2.5c each side of fair value)
        // With deeper levels at 4c and 6c from fair
        let spread_l1 = rust_decimal_macros::dec!(0.025); // Tight: 2.5c from fair
        let spread_l2 = rust_decimal_macros::dec!(0.04);  // Mid: 4c from fair
        let spread_l3 = rust_decimal_macros::dec!(0.06);  // Wide: 6c from fair

        let up_book = OrderBook {
            bids: vec![
                BookLevel {
                    price: (fair_up - spread_l1).max(rust_decimal_macros::dec!(0.01)),
                    size: rust_decimal_macros::dec!(50),
                },
                BookLevel {
                    price: (fair_up - spread_l2).max(rust_decimal_macros::dec!(0.01)),
                    size: rust_decimal_macros::dec!(150),
                },
                BookLevel {
                    price: (fair_up - spread_l3).max(rust_decimal_macros::dec!(0.01)),
                    size: rust_decimal_macros::dec!(300),
                },
            ],
            asks: vec![
                BookLevel {
                    price: (fair_up + spread_l1).min(rust_decimal_macros::dec!(0.99)),
                    size: rust_decimal_macros::dec!(50),
                },
                BookLevel {
                    price: (fair_up + spread_l2).min(rust_decimal_macros::dec!(0.99)),
                    size: rust_decimal_macros::dec!(150),
                },
                BookLevel {
                    price: (fair_up + spread_l3).min(rust_decimal_macros::dec!(0.99)),
                    size: rust_decimal_macros::dec!(300),
                },
            ],
            timestamp: Some(Utc::now()),
        };

        let down_book = OrderBook {
            bids: vec![
                BookLevel {
                    price: (fair_down - spread_l1).max(rust_decimal_macros::dec!(0.01)),
                    size: rust_decimal_macros::dec!(50),
                },
                BookLevel {
                    price: (fair_down - spread_l2).max(rust_decimal_macros::dec!(0.01)),
                    size: rust_decimal_macros::dec!(150),
                },
                BookLevel {
                    price: (fair_down - spread_l3).max(rust_decimal_macros::dec!(0.01)),
                    size: rust_decimal_macros::dec!(300),
                },
            ],
            asks: vec![
                BookLevel {
                    price: (fair_down + spread_l1).min(rust_decimal_macros::dec!(0.99)),
                    size: rust_decimal_macros::dec!(50),
                },
                BookLevel {
                    price: (fair_down + spread_l2).min(rust_decimal_macros::dec!(0.99)),
                    size: rust_decimal_macros::dec!(150),
                },
                BookLevel {
                    price: (fair_down + spread_l3).min(rust_decimal_macros::dec!(0.99)),
                    size: rust_decimal_macros::dec!(300),
                },
            ],
            timestamp: Some(Utc::now()),
        };

        let mut books = self.books.write().await;
        books.up_book = up_book;
        books.down_book = down_book;
    }
}
