//! Binance BTC/USDT WebSocket price feed.
//!
//! Connects to `wss://stream.binance.com:9443/ws/btcusdt@trade`
//! and streams real-time BTC price updates.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::{watch, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::types::{PriceSource, PriceTick};

/// Binance trade stream message
#[derive(Debug, Deserialize)]
struct BinanceTrade {
    /// Symbol (e.g., "BTCUSDT")
    s: String,
    /// Price
    p: String,
    /// Quantity
    q: String,
    /// Trade time (ms)
    #[serde(rename = "T")]
    trade_time: u64,
    /// Is buyer the maker?
    m: bool,
}

/// Manages the Binance WebSocket connection and price state
pub struct BinanceFeed {
    /// Current BTC price (shared state)
    price: Arc<RwLock<Option<PriceTick>>>,
    /// Watch channel for price updates
    price_tx: watch::Sender<Option<PriceTick>>,
    /// WebSocket URL
    ws_url: String,
}

impl BinanceFeed {
    pub fn new(ws_url: &str) -> (Self, watch::Receiver<Option<PriceTick>>) {
        let (price_tx, price_rx) = watch::channel(None);
        let feed = Self {
            price: Arc::new(RwLock::new(None)),
            price_tx,
            ws_url: ws_url.to_string(),
        };
        (feed, price_rx)
    }

    /// Get the current BTC price
    pub async fn current_price(&self) -> Option<PriceTick> {
        self.price.read().await.clone()
    }

    /// Start the WebSocket connection (runs forever, reconnects on failure)
    pub async fn run(&self) -> Result<()> {
        loop {
            match self.connect_and_stream().await {
                Ok(()) => {
                    warn!("Binance WebSocket disconnected cleanly, reconnecting...");
                }
                Err(e) => {
                    error!("Binance WebSocket error: {:?}, reconnecting in 5s...", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn connect_and_stream(&self) -> Result<()> {
        info!(url = %self.ws_url, "Connecting to Binance WebSocket");

        let (ws_stream, _) = connect_async(&self.ws_url)
            .await
            .context("Failed to connect to Binance WebSocket")?;

        info!("Connected to Binance BTC/USDT trade stream");

        let (mut _write, mut read) = ws_stream.split();

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Err(e) = self.handle_message(&text).await {
                        debug!("Failed to parse Binance message: {:?}", e);
                    }
                }
                Ok(Message::Ping(data)) => {
                    debug!("Binance ping received");
                    // tokio-tungstenite auto-responds to pings
                    let _ = data;
                }
                Ok(Message::Close(_)) => {
                    warn!("Binance WebSocket closed");
                    break;
                }
                Err(e) => {
                    error!("Binance WebSocket error: {:?}", e);
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn handle_message(&self, text: &str) -> Result<()> {
        let trade: BinanceTrade =
            serde_json::from_str(text).context("Failed to parse Binance trade")?;

        let price: Decimal = trade
            .p
            .parse()
            .context("Failed to parse Binance price")?;

        let timestamp = DateTime::from_timestamp_millis(trade.trade_time as i64)
            .unwrap_or_else(Utc::now);

        let tick = PriceTick {
            price,
            timestamp,
            source: PriceSource::Binance,
        };

        // Update shared state
        *self.price.write().await = Some(tick.clone());

        // Notify watchers
        let _ = self.price_tx.send(Some(tick));

        Ok(())
    }
}

/// Simulated Binance feed for dry-run mode.
/// Generates realistic BTC price movements using a random walk.
pub struct SimulatedBinanceFeed {
    price: Arc<RwLock<Option<PriceTick>>>,
    price_tx: watch::Sender<Option<PriceTick>>,
    initial_price: Decimal,
}

impl SimulatedBinanceFeed {
    pub fn new(initial_price: Decimal) -> (Self, watch::Receiver<Option<PriceTick>>) {
        let (price_tx, price_rx) = watch::channel(None);
        let feed = Self {
            price: Arc::new(RwLock::new(None)),
            price_tx,
            initial_price,
        };
        (feed, price_rx)
    }

    pub async fn current_price(&self) -> Option<PriceTick> {
        self.price.read().await.clone()
    }

    /// Run the simulated feed (generates prices every 500ms)
    pub async fn run(&self) -> Result<()> {
        info!(
            initial_price = %self.initial_price,
            "Starting simulated BTC price feed"
        );

        let mut step = 0u64;

        loop {
            // Simulate BTC oscillation: ±$10-50 random walk
            // Use a simple deterministic oscillation for reproducibility
            let phase = (step as f64 * 0.1).sin() * 30.0
                + (step as f64 * 0.23).sin() * 15.0
                + (step as f64 * 0.07).cos() * 20.0;

            let delta = Decimal::from_f64_retain(phase).unwrap_or(Decimal::ZERO);
            let current_price = self.initial_price + delta;

            let tick = PriceTick {
                price: current_price,
                timestamp: Utc::now(),
                source: PriceSource::Simulated,
            };

            *self.price.write().await = Some(tick.clone());
            let _ = self.price_tx.send(Some(tick));

            step += 1;
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    }
}
