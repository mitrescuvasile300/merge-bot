# Merge Bot Development Status

## Current State: RISK MANAGEMENT v2 — Capital Recycling Fix ✅
- Code compiles cleanly (0 errors, 0 warnings)
- GBM random walk simulation with realistic order books
- **Emergency salvage** for unmerged positions at window close
- **Window investment cap** prevents capital recycling abuse
- **Accurate NET P&L** tracking including salvage losses
- **Tightened risk params**: 10 shares/order, 20 max imbalance, 2x ratio limit

## Latest Test Results (2026-02-16 ~05:30 UTC)

### Key Improvements Since Last Status
1. **Capital recycling cap** (`max_window_investment = 1.5x max_exposure`) — prevents unlimited position accumulation through merge→rebuy cycles
2. **Emergency salvage** — sells excess unmerged shares at bid before window close instead of losing 100%
3. **Fixed NET P&L reporting** — now correctly accounts for salvage loss (cost_basis - proceeds)
4. **Smaller orders** (10 shares vs 20) and tighter imbalance (20 max vs 60)
5. **Time-weighted wind-down** — graduated exit phases at 120s, 90s, 60s before close

### Test Results Summary (Random Walk)
```
Test          Merges  Pairs   Invested  Merge P&L  Salvage  NET P&L   Capital
────────────  ──────  ──────  ────────  ─────────  ───────  ────────  ────────
Oscillating    3       150    $148.80   +$6.00     $0       +$1.20    $216.20
Strong trend   2       60     $134.40   +$2.40     $1.60    -$72.80   $142.20
Flat trend     0       0      $16-26    $0         $0.55    -$25.85   $189.15
```

### Key Insight: Random Walk Variance Is EXTREME
- **Oscillating market**: Bot is profitable (merges work, small unmerged loss)
- **Trending market**: Major losses despite all protections (160 excess shares)
- **Flat trend**: Minimal activity, moderate losses from imbalanced fills

### Risk Parameter Evolution
```
Parameter         v1(sine)  v6(random)  v8(current)
─────────────     ────────  ──────────  ───────────
shares_per_order  20        20          10
max_imbalance     60        60          20
imbalance_ratio   3.0x      3.0x        2.0x
wind-down         none      30s stop    120/90/60s graduated
investment_cap    none      none        1.5x exposure ($150)
salvage           none      none        ✅ sells at bid
NET P&L accuracy  partial   partial     ✅ full (incl salvage)
```

## Architecture
- Simulated BTC feed: GBM with 45% annualized vol + weak mean-reversion (κ=0.001)
- Microstructure noise: ±$2-3 per tick
- Order books: 3-level depth with ±0.5c spread jitter, widening near expiry
- Strategy: 10 shares/order, 2% target edge, 2x imbalance ratio max

## Remaining Issues (Priority Order)
1. **🔴 Trending market exposure still too high** — $150 window cap allows ~310 shares, too much for $215 account
   - Consider: reduce max_exposure to $60 → max_window_investment = $90
   - Consider: reduce max_exposure to $50 per window (~23% of capital)
2. **🟡 Salvage bid of $0.01 recovers almost nothing** — near expiry, losing side's FV → 0
   - Consider: earlier salvage trigger (at 60s remaining, not window close)
   - Consider: time-based FV for salvage pricing
3. **🟡 Need 20+ window sample** to estimate true EV with random walks
4. **🟢 Live mode setup** — Polymarket API keys, Polygon wallet
5. **🟢 Live test** with tiny capital ($20-50) — real market conditions

## Git History (Recent)
```
af79e05 fix: accurate NET P&L after salvage (track salvage cost basis)
7ffffc7 v8: time-scaled FV extreme threshold
c136dc4 feat: emergency salvage for unmerged positions + window investment cap
4769eac v7: Fix two-phase buying + softer FV filter
7206619 docs: update CRON_STATUS with random walk test results
8ceee9d feat: replace sine-wave sim with GBM random walk + noisy order books
```
