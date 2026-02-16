# Merge Bot Development Status

## Current State: DRY-RUN WORKING — PROFITABLE
- Code compiles cleanly (0 errors, 0 warnings)
- Unit tests pass
- Dry-run mode end-to-end tested with full window completion
- **Latest run: 3 merges, 140 pairs, $19.23 profit, 100% win rate**

## Latest Test (2026-02-16 ~03:35 UTC)
### Test Parameters
- `--max-windows 1 --entry-delay 5 --exit-buffer 3`
- Simulated BTC feed, simulated order books
- Capital: $215, Target edge: 2%

### Results
```
Total orders placed:   14
Total merges:          3
Total pairs merged:    140
Total invested:        $120.77
Total merge payout:    $140.00
Total profit:          $19.23
Win rate:              100%
Final capital:         $234.23
Unmerged positions:    0 Up + 0 Down (clean close)
```

### Merge Details
| # | Pairs | Cost | Payout | Profit | Return |
|---|-------|------|--------|--------|--------|
| 1 | 80 | $74.30 | $80.00 | $5.70 | 7.67% |
| 2 | 40 | $37.63 | $40.00 | $2.37 | 6.30% |
| 3 | 20 | $8.84 | $20.00 | $11.16 | 126.17% |

## Recent Fixes (this session)
1. **Conservative bid floor (0.40)** — prevents overpaying one side when other side's fair value is temporarily low during BTC swings. Eliminated all losing merges.
2. **Market exposure release on merge** — fixes exposure accumulation that blocked ALL orders after first merge
3. **Position imbalance limit (60 shares max)** — prevents dangerous one-side accumulation
4. **Cancelled order exposure release** — releases risk budget when unfilled orders are cancelled at window close
5. **Increased max_exposure_per_market ($50 → $100)** — allows more trading activity per window

## Previous Tests
### Test 2 (pre-conservative-bid)
- 17 merges, $79.63 profit, 14W/3L — higher volume but 3 losing merges
- Losses caused by max_side_price (0.65) buys when other side ended up expensive

### Test 1 (pre-exposure-fix)
- 1 merge, $4.42 profit — exposure bug blocked all orders after first merge
- Discovered: market_exposure never decreased, capping at $50 permanently

## Known Issues / Next Steps
1. **Simulation is unrealistic** — BTC price sine-wave oscillates too predictably; real markets have random walk + microstructure noise
2. **Need multi-window test** — run 3+ consecutive windows to test window transitions, capital recycling
3. **Profit may be inflated by sim** — the ±$60 BTC swings in 5 min create larger-than-real-life edge
4. **Live mode untested** — needs Polymarket API keys, Polygon wallet setup
5. **Consider reducing max_side_price** from 0.65 to 0.58 to be even more conservative

## Architecture Notes
- Simulated BTC feed oscillates ±$30-50 around $97,000 (deterministic sine waves)
- Simulated order books derive from Black-Scholes fair value with 2.5c spread
- Strategy waits 5s entry delay, stops 3s before window close
- Orders: 20 shares per order, limit BUY at fair value
- Target edge: 2% per merge pair (combined cost < $0.98)
- Conservative bid: assumes other side costs ≥ $0.40 when no position held
