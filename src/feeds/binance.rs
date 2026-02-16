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
///
/// Uses Geometric Brownian Motion (GBM) with mean-reversion — a realistic
/// model for BTC price movements within a 5-minute window:
///
/// - GBM component: random walk with 45% annualized vol (~$30 per 5-min window)
/// - Mean-reversion: weak pull back toward opening price (κ = 0.001)
///   This models the empirical observation that 5-min BTC moves are partially
///   mean-reverting, creating the oscillation the merge strategy exploits.
/// - Microstructure noise: small random tick-level noise (±$2-3)
///
/// Net effect: BTC typically oscillates ±$30-80 within a window, with
/// occasional trending periods — much more realistic than the old sine wave.
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

    /// Run the simulated feed (generates prices every 500ms via GBM + mean-reversion)
    pub async fn run(&self) -> Result<()> {
        use rand::SeedableRng;
        use rand_distr::{Distribution, Normal};

        info!(
            initial_price = %self.initial_price,
            "Starting simulated BTC price feed (GBM + mean-reversion)"
        );

        // Use StdRng (Send-safe) seeded from entropy, so it works across .await
        let mut rng = rand::rngs::StdRng::from_entropy();
        let normal = Normal::new(0.0, 1.0).unwrap();

        // GBM parameters calibrated to real BTC:
        // - 45% annualized vol → per 500ms step: σ√(dt) where dt = 0.5/31536000
        let sigma_annual = 0.45_f64;
        let dt: f64 = 0.5 / 31_536_000.0; // 500ms in years
        let sigma_step = sigma_annual * dt.sqrt(); // ~1.79e-4

        // Mean-reversion: Ornstein-Uhlenbeck component
        // κ = mean-reversion speed (small = weak pull, preserves randomness)
        let kappa = 0.001_f64;

        // Microstructure noise: small random tick jitter (σ_noise ~ $2)
        let noise_sigma = 2.0_f64;
        let noise_dist = Normal::new(0.0, noise_sigma).unwrap();

        let initial_f64 = self.initial_price.to_string().parse::<f64>().unwrap_or(97000.0);
        let mut current_f64 = initial_f64;
        let anchor = initial_f64; // mean-reversion target

        loop {
            // 1. GBM step: dS/S = σ * dW (zero drift for short-term)
            let z: f64 = normal.sample(&mut rng);
            let gbm_return = sigma_step * z;

            // 2. Mean-reversion: pull toward anchor price
            let deviation = (current_f64 - anchor) / anchor; // relative deviation
            let mr_pull = -kappa * deviation; // proportional pull back

            // 3. Combined move
            current_f64 *= 1.0 + gbm_return + mr_pull;

            // 4. Add microstructure noise (absolute, not proportional)
            let noise: f64 = noise_dist.sample(&mut rng);
            current_f64 += noise;

            // Sanity clamp: BTC shouldn't move more than 2% in a 5-min window
            let min_price = anchor * 0.98;
            let max_price = anchor * 1.02;
            current_f64 = current_f64.clamp(min_price, max_price);

            let current_price = Decimal::from_f64_retain(current_f64)
                .unwrap_or(self.initial_price);

            let tick = PriceTick {
                price: current_price,
                timestamp: Utc::now(),
                source: PriceSource::Simulated,
            };

            *self.price.write().await = Some(tick.clone());
            let _ = self.price_tx.send(Some(tick));

            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    }
}
