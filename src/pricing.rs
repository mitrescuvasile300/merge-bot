//! Black-Scholes pricing for BTC 5-minute binary options.
//!
//! P_up ≈ N(d₂) where d₂ = ln(S/K) / (σ√T)
//! T = remaining_seconds / 31_536_000 (seconds in a year)
//!
//! Also includes Kelly criterion sizing and fee calculations.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use statrs::distribution::{ContinuousCDF, Normal};
use std::collections::VecDeque;

/// Black-Scholes pricer for binary options
pub struct BinaryPricer {
    /// Standard normal distribution
    normal: Normal,
}

impl BinaryPricer {
    pub fn new() -> Self {
        Self {
            normal: Normal::new(0.0, 1.0).unwrap(),
        }
    }

    /// Calculate fair value of the Up token.
    ///
    /// - `spot`: Current BTC price (S)
    /// - `strike`: BTC price at window open (K)
    /// - `sigma`: Annualized volatility (e.g., 0.50 for 50%)
    /// - `remaining_secs`: Seconds until window close
    pub fn fair_value_up(
        &self,
        spot: Decimal,
        strike: Decimal,
        sigma: Decimal,
        remaining_secs: u64,
    ) -> Decimal {
        if remaining_secs == 0 {
            // At expiry: binary payoff
            return if spot > strike {
                Decimal::ONE
            } else if spot < strike {
                Decimal::ZERO
            } else {
                dec!(0.5)
            };
        }

        if strike.is_zero() || sigma.is_zero() {
            return dec!(0.5);
        }

        let t_years = remaining_secs as f64 / 31_536_000.0;
        let sigma_f = decimal_to_f64(sigma);
        let sigma_sqrt_t = sigma_f * t_years.sqrt();

        if sigma_sqrt_t < 1e-12 {
            // Extremely close to expiry — binary payoff
            return if spot > strike {
                Decimal::ONE
            } else if spot < strike {
                Decimal::ZERO
            } else {
                dec!(0.5)
            };
        }

        let s = decimal_to_f64(spot);
        let k = decimal_to_f64(strike);
        let d2 = (s / k).ln() / sigma_sqrt_t;
        let p_up = self.normal.cdf(d2);

        Decimal::from_f64_retain(p_up).unwrap_or(dec!(0.5))
    }

    /// Calculate fair value of the Down token (= 1 - P_up)
    pub fn fair_value_down(
        &self,
        spot: Decimal,
        strike: Decimal,
        sigma: Decimal,
        remaining_secs: u64,
    ) -> Decimal {
        Decimal::ONE - self.fair_value_up(spot, strike, sigma, remaining_secs)
    }
}

/// Rolling realized volatility estimator using 5-minute returns
pub struct VolatilityEstimator {
    /// Recent price observations (timestamp_secs, price)
    observations: VecDeque<(u64, f64)>,
    /// Maximum number of observations to keep
    max_observations: usize,
    /// Sampling interval in seconds
    sample_interval_secs: u64,
    /// Last sample timestamp
    last_sample: u64,
}

impl VolatilityEstimator {
    pub fn new(max_observations: usize) -> Self {
        Self {
            observations: VecDeque::with_capacity(max_observations),
            max_observations,
            sample_interval_secs: 60, // Sample every 60 seconds
            last_sample: 0,
        }
    }

    /// Add a price observation. Only records at the sampling interval.
    pub fn add_price(&mut self, timestamp_secs: u64, price: f64) {
        if timestamp_secs < self.last_sample + self.sample_interval_secs && !self.observations.is_empty() {
            return;
        }

        self.last_sample = timestamp_secs;
        self.observations.push_back((timestamp_secs, price));

        while self.observations.len() > self.max_observations {
            self.observations.pop_front();
        }
    }

    /// Calculate annualized realized volatility from recent observations.
    /// Returns None if insufficient data.
    pub fn annualized_volatility(&self) -> Option<Decimal> {
        if self.observations.len() < 10 {
            return None;
        }

        // Calculate log returns
        let returns: Vec<f64> = self
            .observations
            .iter()
            .zip(self.observations.iter().skip(1))
            .map(|((_, p1), (_, p2))| (p2 / p1).ln())
            .collect();

        if returns.is_empty() {
            return None;
        }

        let n = returns.len() as f64;
        let mean = returns.iter().sum::<f64>() / n;
        let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let std_dev = variance.sqrt();

        // Annualize: each observation is ~60 seconds apart
        // There are 525,600 minutes/year, so 525,600/1 = 525,600 samples/year
        // Annualization factor = sqrt(525600 / sample_interval_in_minutes)
        let samples_per_year = 525_600.0 / (self.sample_interval_secs as f64 / 60.0);
        let annualized = std_dev * samples_per_year.sqrt();

        // Clamp to reasonable range (10% - 200% annualized)
        let clamped = annualized.clamp(0.10, 2.0);

        Decimal::from_f64_retain(clamped)
    }

    /// Get a default volatility estimate if we don't have enough data
    pub fn default_volatility() -> Decimal {
        dec!(0.45) // 45% annualized — reasonable BTC default
    }
}

/// Polymarket taker fee calculation
pub fn taker_fee(price: Decimal) -> Decimal {
    // fee = 0.25 × (p × (1-p))²
    let p = decimal_to_f64(price);
    let fee = 0.25 * (p * (1.0 - p)).powi(2);
    Decimal::from_f64_retain(fee).unwrap_or(Decimal::ZERO)
}

/// Kelly criterion for binary options: f* = (p - c) / (1 - c)
/// Uses quarter-Kelly for safety
pub fn kelly_fraction(true_prob: Decimal, market_price: Decimal) -> Decimal {
    let edge = true_prob - market_price;
    if edge <= Decimal::ZERO || market_price >= Decimal::ONE {
        return Decimal::ZERO;
    }

    let full_kelly = edge / (Decimal::ONE - market_price);
    // Quarter Kelly for safety
    full_kelly * dec!(0.25)
}

/// Calculate optimal limit order price for a side.
///
/// We want to buy at a price where the combined cost of Up + Down < $1.
/// Given the other side's expected cost, calculate what we should bid.
pub fn optimal_bid_price(
    other_side_expected_cost: Decimal,
    target_edge: Decimal,
) -> Decimal {
    // Combined cost should be: 1.0 - target_edge
    // So our price should be: (1.0 - target_edge) - other_side_cost
    let target_combined = Decimal::ONE - target_edge;
    let our_price = target_combined - other_side_expected_cost;

    // Clamp to valid range
    our_price.max(dec!(0.01)).min(dec!(0.99))
}

fn decimal_to_f64(d: Decimal) -> f64 {
    use std::str::FromStr;
    f64::from_str(&d.to_string()).unwrap_or(0.0)
}

/// Public version of decimal_to_f64 for use in other modules
pub fn decimal_to_f64_pub(d: Decimal) -> f64 {
    decimal_to_f64(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_fair_value_atm() {
        let pricer = BinaryPricer::new();
        let fv = pricer.fair_value_up(dec!(97000), dec!(97000), dec!(0.50), 150);
        // ATM should be ~0.50
        assert!(fv > dec!(0.45) && fv < dec!(0.55), "ATM fair value: {}", fv);
    }

    #[test]
    fn test_fair_value_itm() {
        let pricer = BinaryPricer::new();
        // Spot > Strike → Up is ITM
        let fv = pricer.fair_value_up(dec!(97200), dec!(97000), dec!(0.50), 150);
        assert!(fv > dec!(0.60), "ITM fair value should be > 0.60: {}", fv);
    }

    #[test]
    fn test_fair_value_otm() {
        let pricer = BinaryPricer::new();
        // Spot < Strike → Up is OTM
        let fv = pricer.fair_value_up(dec!(96800), dec!(97000), dec!(0.50), 150);
        assert!(fv < dec!(0.40), "OTM fair value should be < 0.40: {}", fv);
    }

    #[test]
    fn test_fair_value_at_expiry() {
        let pricer = BinaryPricer::new();
        let fv_itm = pricer.fair_value_up(dec!(97001), dec!(97000), dec!(0.50), 0);
        assert_eq!(fv_itm, Decimal::ONE);

        let fv_otm = pricer.fair_value_up(dec!(96999), dec!(97000), dec!(0.50), 0);
        assert_eq!(fv_otm, Decimal::ZERO);
    }

    #[test]
    fn test_taker_fee() {
        let fee_50 = taker_fee(dec!(0.50));
        assert!(fee_50 > dec!(0.015) && fee_50 < dec!(0.016), "Fee at 50: {}", fee_50);

        let fee_90 = taker_fee(dec!(0.90));
        assert!(fee_90 < dec!(0.003), "Fee at 90: {}", fee_90);
    }

    #[test]
    fn test_kelly() {
        let f = kelly_fraction(dec!(0.645), dec!(0.58));
        // Edge = 0.065, full Kelly = 0.065/0.42 = 0.155, quarter = 0.039
        assert!(f > dec!(0.03) && f < dec!(0.05), "Kelly fraction: {}", f);
    }
}
