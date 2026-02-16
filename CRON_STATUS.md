# Merge Bot Development Status

## Current State: DRY-RUN WORKING — 2 SUCCESSFUL WINDOWS ✅
- Code compiles cleanly (0 errors, 0 warnings)
- Unit tests pass
- Dry-run mode end-to-end tested across multiple runs
- **Run 3 (latest): 29 merges, 1200 pairs, NET $147.44 profit, 96.5% win rate**
- **Run 2: 3 merges, 140 pairs, $19.23 profit, 100% win rate, clean close**

## Latest Test — Run 3 (2026-02-16 ~03:39 UTC)
### Test Parameters
- `--max-windows 1 --entry-delay 5 --exit-buffer 5`
- Simulated BTC feed, simulated order books
- Capital: $215, Target edge: 2%

### Results
```
Total orders placed:   129
Total merges:          29
Total pairs merged:    1200
Total invested:        $1052.56
Total merge payout:    $1200.00
Merge profit:          $154.22
Unmerged positions:    0 Up + 40 Down ($6.78 cost)
Unmerged loss:         -$6.78
NET P&L:               $147.44  (+68.6% on $215 capital)
Win rate:              96.5%
Final capital:         $362.44
Capital turnover:      ~5.6x in single 5-min window
```

### Key Observations
- Capital recycled through merges allows >$1000 invested from $215 base
- Only 40 unmerged Down shares at close (position balance limit working)
- 1/29 merges at slight loss (96.5% win rate)
- Real-world results would be MUCH more modest (sim conditions are favorable)

## Previous Test — Run 2 (2026-02-16 ~03:35 UTC)
```
Total orders placed:   14
Total merges:          3
Total pairs merged:    140
Total invested:        $120.77
Total merge payout:    $140.00
Total profit:          $19.23
Unmerged positions:    0 Up + 0 Down (clean close)
Win rate:              100%
Final capital:         $234.23
```

## All Bugs Fixed
1. **RwLock Deadlock** — scoped read guard to drop before write
2. **should_buy() too restrictive** — replaced market_ask check with bid-only checks
3. **Opening price mismatch** — fixed with set_opening_price() per window
4. **Tight strategy loop** — added unconditional sleep(order_interval) each iteration
5. **Window startup timing** — wait for fresh window if current has < 64s remaining
6. **Market exposure release on merge** — fixed accumulation that blocked all orders
7. **Position imbalance limit** — prevents one-sided accumulation (3x ratio max)
8. **Cancelled order exposure release** — releases risk budget at window close
9. **Conservative bid floor (0.40)** — prevents overpaying during extreme BTC swings
10. **NET P&L reporting** — includes unmerged share losses in window summary

## Known Issues / Next Steps
1. **Simulation is unrealistic** — BTC sine-wave oscillation + predictable order book. Real markets have random walk + noise + competition.
2. **Need multi-window test** — run 3+ consecutive windows to test window transitions
3. **Need 3+ successful test windows before live mode** — currently at 2 ✅
4. **Live mode untested** — needs Polymarket API keys, Polygon wallet
5. **Real-world profit will be lower** — less predictable fills, wider spreads, competition
6. **Consider per-window log files** — `--log-dir` flag exists but needs testing

## Architecture Notes
- Simulated BTC feed oscillates ±$50-100 around $97,000 (deterministic sine waves)
- Simulated order books derive from Black-Scholes fair value with 2.5c spread
- Strategy waits 5s entry delay, stops 5s before window close
- Orders: 20 shares per order, limit BUY at fair value
- Target edge: 2% per merge pair (combined cost < $0.98)
- Conservative bid: assumes other side costs ≥ $0.40 when no position held
- Position balance: pauses buying if one side > 3x the other (and > 40 shares)
- Max exposure: $100 per market window
