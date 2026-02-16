//! Merge engine: converts matched Up + Down positions into USDC profit.
//!
//! Core mechanism: 1 Up share + 1 Down share = $1 USDC (guaranteed)
//! If combined purchase cost < $1, the difference is profit.

use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::types::{MergeResult, PnlSnapshot, Position, Side};

/// The merge engine tracks positions and executes merges
pub struct MergeEngine {
    /// Current position in Up shares
    up_position: Arc<RwLock<Position>>,
    /// Current position in Down shares
    down_position: Arc<RwLock<Position>>,
    /// Running P&L
    pnl: Arc<RwLock<PnlSnapshot>>,
    /// Whether we're in dry-run mode
    dry_run: bool,
    /// All merge results for this session
    merge_history: Arc<RwLock<Vec<MergeResult>>>,
}

impl MergeEngine {
    pub fn new(dry_run: bool) -> Self {
        Self {
            up_position: Arc::new(RwLock::new(Position::default())),
            down_position: Arc::new(RwLock::new(Position::default())),
            pnl: Arc::new(RwLock::new(PnlSnapshot::default())),
            dry_run,
            merge_history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Record a fill: shares purchased at a given price
    pub async fn record_fill(&self, side: Side, shares: Decimal, price: Decimal) {
        let cost = shares * price;

        let position = match side {
            Side::Up => &self.up_position,
            Side::Down => &self.down_position,
        };

        let mut pos = position.write().await;
        let new_total_shares = pos.shares + shares;
        let new_total_cost = pos.total_cost + cost;

        pos.avg_cost = if new_total_shares > Decimal::ZERO {
            new_total_cost / new_total_shares
        } else {
            Decimal::ZERO
        };
        pos.shares = new_total_shares;
        pos.total_cost = new_total_cost;

        // Update P&L snapshot
        let mut pnl = self.pnl.write().await;
        pnl.total_invested += cost;
        pnl.total_orders += 1;

        let mode = if self.dry_run { "[DRY-RUN]" } else { "[LIVE]" };
        info!(
            "{} Filled {} {} shares @ ${} (total: {} shares, avg cost: ${})",
            mode, shares, side, price, pos.shares, pos.avg_cost
        );
    }

    /// Check if we can merge and execute merges.
    /// Returns the merge result if any pairs were merged.
    pub async fn try_merge(&self) -> Option<MergeResult> {
        let mut up_pos = self.up_position.write().await;
        let mut down_pos = self.down_position.write().await;

        // We can merge min(up_shares, down_shares) pairs
        let mergeable_pairs = up_pos.shares.min(down_pos.shares);

        if mergeable_pairs < dec!(1) {
            return None;
        }

        // Calculate costs for the merged pairs
        let up_cost = up_pos.avg_cost * mergeable_pairs;
        let down_cost = down_pos.avg_cost * mergeable_pairs;
        let total_cost = up_cost + down_cost;

        // Each merged pair pays out $1
        let total_payout = mergeable_pairs; // $1 per pair
        let profit = total_payout - total_cost;
        let profit_pct = if total_cost > Decimal::ZERO {
            (profit / total_cost) * dec!(100)
        } else {
            Decimal::ZERO
        };

        let result = MergeResult {
            pairs_merged: mergeable_pairs,
            total_cost,
            total_payout,
            profit,
            profit_pct,
            timestamp: Utc::now(),
        };

        // Deduct merged shares from positions
        up_pos.shares -= mergeable_pairs;
        up_pos.total_cost -= up_cost;
        down_pos.shares -= mergeable_pairs;
        down_pos.total_cost -= down_cost;

        // Update P&L
        let mut pnl = self.pnl.write().await;
        pnl.total_merged_payout += total_payout;
        pnl.total_profit += profit;
        pnl.total_merges += 1;
        pnl.total_pairs += mergeable_pairs;

        // Track win rate
        if profit > Decimal::ZERO {
            let total = pnl.total_merges as f64;
            let wins = (pnl.win_rate * Decimal::from_f64_retain(total - 1.0).unwrap_or(Decimal::ZERO)
                + Decimal::ONE)
                / Decimal::from_f64_retain(total).unwrap_or(Decimal::ONE);
            pnl.win_rate = wins;
        } else {
            let total = pnl.total_merges as f64;
            let wins = pnl.win_rate
                * Decimal::from_f64_retain(total - 1.0).unwrap_or(Decimal::ZERO)
                / Decimal::from_f64_retain(total).unwrap_or(Decimal::ONE);
            pnl.win_rate = wins;
        }

        // Store merge history
        self.merge_history.write().await.push(result.clone());

        let mode = if self.dry_run { "[DRY-RUN]" } else { "[LIVE]" };
        info!(
            "{} MERGED {} pairs | Cost: ${:.4} | Payout: ${:.4} | Profit: ${:.4} ({:.2}%)",
            mode,
            result.pairs_merged,
            result.total_cost,
            result.total_payout,
            result.profit,
            result.profit_pct
        );

        if profit <= Decimal::ZERO {
            warn!(
                "Merge resulted in LOSS: ${:.4} ({:.2}%)",
                profit, profit_pct
            );
        }

        Some(result)
    }

    /// Get current position snapshot
    pub async fn positions(&self) -> (Position, Position) {
        let up = self.up_position.read().await.clone();
        let down = self.down_position.read().await.clone();
        (up, down)
    }

    /// Get current P&L snapshot
    pub async fn pnl_snapshot(&self) -> PnlSnapshot {
        self.pnl.read().await.clone()
    }

    /// Get the number of mergeable pairs
    pub async fn mergeable_pairs(&self) -> Decimal {
        let up = self.up_position.read().await;
        let down = self.down_position.read().await;
        up.shares.min(down.shares)
    }

    /// Get the current combined average cost per pair
    pub async fn avg_pair_cost(&self) -> Option<Decimal> {
        let up = self.up_position.read().await;
        let down = self.down_position.read().await;

        if up.avg_cost > Decimal::ZERO && down.avg_cost > Decimal::ZERO {
            Some(up.avg_cost + down.avg_cost)
        } else {
            None
        }
    }

    /// Record a salvage sale: sell excess unmerged shares at the bid price before
    /// the window closes. This recovers partial value instead of losing 100% of
    /// the position when shares expire worthless.
    ///
    /// Returns the salvage proceeds (shares × bid_price).
    pub async fn record_salvage(&self, side: Side, shares: Decimal, bid_price: Decimal) -> Decimal {
        let position = match side {
            Side::Up => &self.up_position,
            Side::Down => &self.down_position,
        };

        let mut pos = position.write().await;

        // Sanity check: don't sell more than we hold
        let sell_shares = shares.min(pos.shares);
        if sell_shares <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let proceeds = sell_shares * bid_price;
        let cost_basis = sell_shares * pos.avg_cost;

        // Reduce position
        pos.shares -= sell_shares;
        pos.total_cost -= cost_basis;
        // avg_cost stays the same (selling doesn't change avg cost of remaining)

        // Update P&L: the salvage "loss" is (cost_basis - proceeds), but it's
        // better than losing cost_basis entirely. Track separately.
        let mut pnl = self.pnl.write().await;
        pnl.total_salvage_revenue += proceeds;
        pnl.total_salvage_cost_basis += cost_basis;
        pnl.total_salvage_shares += sell_shares;

        let mode = if self.dry_run { "[DRY-RUN]" } else { "[LIVE]" };
        info!(
            "{} SALVAGE: Sold {} {} shares @ bid ${:.4} | Proceeds: ${:.4} | Cost basis: ${:.4} | Saved: ${:.4}",
            mode, sell_shares, side, bid_price, proceeds, cost_basis, proceeds
        );

        proceeds
    }

    /// Reset positions for a new market window
    pub async fn reset_for_new_window(&self) {
        let (up, down) = self.positions().await;
        if up.shares > Decimal::ZERO || down.shares > Decimal::ZERO {
            warn!(
                "Resetting positions with {} Up and {} Down shares remaining (unmerged!)",
                up.shares, down.shares
            );
        }
        *self.up_position.write().await = Position::default();
        *self.down_position.write().await = Position::default();

        // Increment markets_traded counter
        let mut pnl = self.pnl.write().await;
        pnl.markets_traded += 1;
    }

    /// Get merge history
    pub async fn merge_history(&self) -> Vec<MergeResult> {
        self.merge_history.read().await.clone()
    }

    /// Generate a per-window summary report as a formatted string
    pub async fn window_report(&self, market_slug: &str, window_num: u64) -> String {
        let pnl = self.pnl_snapshot().await;
        let (up, down) = self.positions().await;
        let history = self.merge_history().await;

        let mut report = String::new();
        report.push_str(&format!("═══════════════════════════════════════════\n"));
        report.push_str(&format!("  MERGE BOT — Window #{} Report\n", window_num));
        report.push_str(&format!("  Market: {}\n", market_slug));
        report.push_str(&format!("  Time:   {} UTC\n", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")));
        report.push_str(&format!("═══════════════════════════════════════════\n\n"));

        report.push_str(&format!("POSITIONS AT WINDOW END:\n"));
        report.push_str(&format!("  Up:   {} shares @ avg ${:.4}\n", up.shares, up.avg_cost));
        report.push_str(&format!("  Down: {} shares @ avg ${:.4}\n", down.shares, down.avg_cost));
        if up.avg_cost > rust_decimal::Decimal::ZERO && down.avg_cost > rust_decimal::Decimal::ZERO {
            report.push_str(&format!("  Combined avg cost/pair: ${:.4}\n", up.avg_cost + down.avg_cost));
        }
        report.push_str("\n");

        report.push_str(&format!("MERGES THIS WINDOW:\n"));
        if history.is_empty() {
            report.push_str("  No merges executed\n");
        } else {
            for (i, m) in history.iter().enumerate() {
                report.push_str(&format!(
                    "  #{}: {} pairs | cost ${:.4} | payout ${:.4} | profit ${:.4} ({:.2}%)\n",
                    i + 1, m.pairs_merged, m.total_cost, m.total_payout, m.profit, m.profit_pct
                ));
            }
        }
        report.push_str("\n");

        report.push_str(&format!("SESSION TOTALS:\n"));
        report.push_str(&format!("  Total orders:       {}\n", pnl.total_orders));
        report.push_str(&format!("  Total merges:       {}\n", pnl.total_merges));
        report.push_str(&format!("  Total pairs merged: {}\n", pnl.total_pairs));
        report.push_str(&format!("  Total invested:     ${:.4}\n", pnl.total_invested));
        report.push_str(&format!("  Total payout:       ${:.4}\n", pnl.total_merged_payout));
        report.push_str(&format!("  Total profit:       ${:.4}\n", pnl.total_profit));
        report.push_str(&format!("  Win rate:           {:.1}%\n", pnl.win_rate * rust_decimal_macros::dec!(100)));
        report.push_str(&format!("  Markets traded:     {}\n", pnl.markets_traded));
        report.push_str(&format!("═══════════════════════════════════════════\n"));

        report
    }
}
