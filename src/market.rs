//! Market discovery via Polymarket Gamma API.
//!
//! Finds active BTC 5-minute binary option markets using the deterministic
//! slug pattern: `btc-updown-5m-{unix_timestamp}` aligned to 300s intervals.

use anyhow::{Context, Result};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::types::Market;

/// Gamma API response for market search
#[derive(Debug, Deserialize)]
struct GammaMarketResponse {
    #[serde(default)]
    data: Vec<GammaMarket>,
}

#[derive(Debug, Deserialize)]
struct GammaMarket {
    condition_id: Option<String>,
    question: Option<String>,
    slug: Option<String>,
    tokens: Option<Vec<GammaToken>>,
    active: Option<bool>,
    closed: Option<bool>,
    end_date_iso: Option<String>,
    start_date_iso: Option<String>,
    game_start_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GammaToken {
    token_id: Option<String>,
    outcome: Option<String>,
    price: Option<f64>,
}

/// Client for discovering BTC 5-min binary markets
pub struct MarketDiscovery {
    client: reqwest::Client,
    gamma_url: String,
    clob_url: String,
}

impl MarketDiscovery {
    pub fn new(gamma_url: &str, clob_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            gamma_url: gamma_url.to_string(),
            clob_url: clob_url.to_string(),
        }
    }

    /// Get the current 5-minute window timestamps
    pub fn current_window() -> (u64, u64) {
        let now = Utc::now().timestamp() as u64;
        let window_start = now - (now % 300);
        let window_end = window_start + 300;
        (window_start, window_end)
    }

    /// Get the next 5-minute window timestamps
    pub fn next_window() -> (u64, u64) {
        let (_, current_end) = Self::current_window();
        (current_end, current_end + 300)
    }

    /// Compute expected slug for a given window start timestamp
    pub fn expected_slug(window_start: u64) -> String {
        format!("btc-updown-5m-{}", window_start)
    }

    /// Find the active market for the current 5-minute window
    pub async fn find_current_market(&self) -> Result<Option<Market>> {
        let (window_start, window_end) = Self::current_window();
        info!(
            window_start = window_start,
            window_end = window_end,
            "Searching for current BTC 5-min market"
        );

        // Strategy 1: Try the deterministic slug
        let slug = Self::expected_slug(window_start);
        if let Some(market) = self.find_by_slug(&slug, window_start, window_end).await? {
            return Ok(Some(market));
        }

        // Strategy 2: Search Gamma API for active BTC 5-min markets
        if let Some(market) = self.search_gamma_markets(window_start, window_end).await? {
            return Ok(Some(market));
        }

        // Strategy 3: Try with slightly different timestamp alignments
        for offset in &[-1i64, 1, -2, 2] {
            let alt_start = (window_start as i64 + offset * 300) as u64;
            let alt_slug = Self::expected_slug(alt_start);
            if let Some(market) = self
                .find_by_slug(&alt_slug, alt_start, alt_start + 300)
                .await?
            {
                return Ok(Some(market));
            }
        }

        warn!("No active BTC 5-min market found for window {}", window_start);
        Ok(None)
    }

    /// Find market by its slug via Gamma API
    async fn find_by_slug(
        &self,
        slug: &str,
        window_start: u64,
        window_end: u64,
    ) -> Result<Option<Market>> {
        debug!(slug = slug, "Trying slug lookup");

        let url = format!("{}/markets?slug={}", self.gamma_url, slug);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to query Gamma API by slug")?;

        if !resp.status().is_success() {
            debug!("Slug {} not found (status: {})", slug, resp.status());
            return Ok(None);
        }

        let body: serde_json::Value = resp.json().await?;
        self.parse_market_response(&body, window_start, window_end)
    }

    /// Search for active BTC 5-min markets on Gamma
    async fn search_gamma_markets(
        &self,
        window_start: u64,
        window_end: u64,
    ) -> Result<Option<Market>> {
        debug!("Searching Gamma for active BTC 5-min markets");

        // Query for active, non-closed markets matching BTC up/down
        let url = format!(
            "{}/markets?active=true&closed=false&limit=20&tag=btc&order=startDate&ascending=false",
            self.gamma_url
        );

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to search Gamma API")?;

        if !resp.status().is_success() {
            warn!("Gamma search failed: {}", resp.status());
            return Ok(None);
        }

        let body: serde_json::Value = resp.json().await?;

        // Try parsing as array directly or as { data: [...] }
        let markets: Vec<serde_json::Value> = if body.is_array() {
            serde_json::from_value(body)?
        } else if let Some(data) = body.get("data") {
            serde_json::from_value(data.clone())?
        } else {
            vec![body]
        };

        for market_json in &markets {
            let slug = market_json
                .get("slug")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Match BTC 5-min binary markets
            if slug.contains("btc") && slug.contains("5m") {
                if let Ok(Some(market)) =
                    self.parse_market_response(market_json, window_start, window_end)
                {
                    return Ok(Some(market));
                }
            }
        }

        Ok(None)
    }

    /// Parse a Gamma API market response into our Market type
    fn parse_market_response(
        &self,
        json: &serde_json::Value,
        window_start: u64,
        window_end: u64,
    ) -> Result<Option<Market>> {
        let condition_id = json
            .get("condition_id")
            .or_else(|| json.get("conditionId"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if condition_id.is_empty() {
            return Ok(None);
        }

        let slug = json
            .get("slug")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Extract token IDs from the tokens array
        let tokens = json
            .get("tokens")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut up_token_id = String::new();
        let mut down_token_id = String::new();

        for token in &tokens {
            let outcome = token
                .get("outcome")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();

            let token_id = token
                .get("token_id")
                .or_else(|| token.get("tokenId"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if outcome.contains("up") || outcome.contains("yes") || outcome.contains("higher") {
                up_token_id = token_id;
            } else if outcome.contains("down")
                || outcome.contains("no")
                || outcome.contains("lower")
            {
                down_token_id = token_id;
            }
        }

        // If we couldn't determine token IDs from outcomes, use positional
        if up_token_id.is_empty() && tokens.len() >= 2 {
            up_token_id = tokens[0]
                .get("token_id")
                .or_else(|| tokens[0].get("tokenId"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            down_token_id = tokens[1]
                .get("token_id")
                .or_else(|| tokens[1].get("tokenId"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }

        if up_token_id.is_empty() || down_token_id.is_empty() {
            debug!("Could not extract token IDs from market {}", condition_id);
            return Ok(None);
        }

        let active = json
            .get("active")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let closed = json
            .get("closed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if closed || !active {
            return Ok(None);
        }

        info!(
            condition_id = %condition_id,
            slug = %slug,
            up_token = %up_token_id,
            down_token = %down_token_id,
            "Found active BTC 5-min market"
        );

        Ok(Some(Market {
            condition_id,
            slug,
            up_token_id,
            down_token_id,
            window_start,
            window_end,
            opening_price: None,
            active: true,
        }))
    }

    /// Fetch the current order book for a token from the CLOB
    pub async fn fetch_order_book(
        &self,
        token_id: &str,
    ) -> Result<crate::types::OrderBook> {
        let url = format!("{}/book?token_id={}", self.clob_url, token_id);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to fetch order book")?;

        let body: serde_json::Value = resp.json().await?;

        let parse_levels = |key: &str| -> Vec<crate::types::BookLevel> {
            body.get(key)
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|level| {
                            let price = level
                                .get("price")
                                .and_then(|p| p.as_str().or_else(|| p.as_f64().map(|_| "")).and_then(|_| None).or_else(|| {
                                    p.as_str()
                                }))
                                .and_then(|s| s.parse::<Decimal>().ok())
                                .or_else(|| {
                                    level
                                        .get("price")
                                        .and_then(|p| p.as_f64())
                                        .and_then(|f| Decimal::from_f64_retain(f))
                                })?;
                            let size = level
                                .get("size")
                                .and_then(|s| {
                                    s.as_str()
                                        .and_then(|s| s.parse::<Decimal>().ok())
                                        .or_else(|| {
                                            s.as_f64().and_then(Decimal::from_f64_retain)
                                        })
                                })?;
                            Some(crate::types::BookLevel { price, size })
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        let mut bids = parse_levels("bids");
        let mut asks = parse_levels("asks");

        // Ensure sorted: bids descending, asks ascending
        bids.sort_by(|a, b| b.price.cmp(&a.price));
        asks.sort_by(|a, b| a.price.cmp(&b.price));

        Ok(crate::types::OrderBook {
            bids,
            asks,
            timestamp: Some(Utc::now()),
        })
    }

    /// Fetch current price for a token
    pub async fn fetch_price(&self, token_id: &str) -> Result<Option<Decimal>> {
        let url = format!(
            "{}/price?token_id={}&side=BUY",
            self.clob_url, token_id
        );
        let resp = self.client.get(&url).send().await?;
        let body: serde_json::Value = resp.json().await?;

        let price = body
            .get("price")
            .and_then(|p| {
                p.as_str()
                    .and_then(|s| s.parse::<Decimal>().ok())
                    .or_else(|| p.as_f64().and_then(Decimal::from_f64_retain))
            });

        Ok(price)
    }
}
