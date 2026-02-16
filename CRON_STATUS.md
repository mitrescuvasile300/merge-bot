# Merge Bot Development Status

## Current State: 3-WINDOW MULTI-WINDOW TEST PASSED ✅✅✅
- Code compiles cleanly (0 errors, 0 warnings)
- Unit tests pass
- Dry-run mode end-to-end tested across 3 consecutive 5-min windows
- **3-Window Test: 42 merges, 1560 pairs, +$138.73 profit, 92.9% win rate**
- Window transitions, capital carry-forward, position resets all working

## Latest Test — 3-Window Multi-Window (2026-02-16 ~04:09 UTC)
### Test Parameters
- `--max-windows 3 --log-level info --entry-delay 5 --exit-buffer 5`
- Simulated BTC feed, simulated order books
- Capital: $215, Target edge: 2%

### Per-Window Results
```
Window  Merges  Pairs   Profit    Win Rate
──────  ──────  ──────  ────────  ────────
  #1      15     500    +$61.12   14/15 (93%)
  #2      19     760    +$57.71   18/19 (95%)
  #3       8     300    +$19.89    7/8  (88%)
──────  ──────  ──────  ────────  ────────
TOTAL     42    1560   +$138.73   39/42 (93%)
```

### Session Summary
```
Total orders placed:   162
Total merges:          42
Total pairs merged:    1560
Total invested:        $1,468.99 (6.8x capital turnover)
Total merge payout:    $1,560.00
Merge profit:          $138.73
NET P&L:               $100.50 (after unmerged losses)
Win rate:              92.9% (39/42)
Final capital:         $306.01 (from $215 start, +42.3%)
Avg merge profit:      $3.30 (9.76% margin)
```

### Key Observations
- Window transitions work cleanly — positions reset, capital carries forward
- Capital recycled through merges allows >$1400 invested from $215 base
- Window 3 was less active (8 merges) — likely timing/cycle effect
- 3 losing merges total (max: -$2.49 on one merge, -11% on 20 pairs)
- Unmerged positions: 20 Up shares after W1, 20 Down after W2 close
- Position balance limit (3x ratio) continues to prevent heavy one-sided exposure

## Previous Tests
| Run | Merges | Pairs | Profit | Win Rate | Notes |
|-----|--------|-------|--------|----------|-------|
| 3-win | 42 | 1560 | $138.73 | 92.9% | **Multi-window milestone** |
| #4 | 29 | 1200 | $147.44 net | 96.5% | Capital turnover ~5.6x |
| #3 | 3 | 140 | $19.23 | 100% | Clean close |
| #2 | 17 | 560 | $79.63 | 82% | 3 losing merges |
| #1 | 1 | 40 | $4.42 | 100% | First successful dry-run |

## All 10 Bugs Fixed ✅
1. RwLock Deadlock — scoped read guard to drop before write
2. should_buy() too restrictive — replaced market_ask check with bid-only
3. Opening price mismatch — fixed with set_opening_price() per window
4. Tight strategy loop — added unconditional sleep(order_interval)
5. Window startup timing — wait for fresh window if < 64s remaining
6. Market exposure release on merge — fixed accumulation blocking orders
7. Position imbalance limit — prevents one-sided accumulation (3x ratio max)
8. Cancelled order exposure release — releases risk budget at window close
9. Conservative bid floor (0.40) — prevents overpaying during extreme swings
10. NET P&L reporting — includes unmerged share losses in window summary

## Known Limitations & Next Steps
1. **Simulation is unrealistic** — BTC sine-wave oscillation + predictable order book
   - Real markets: random walk + noise + competition + wider spreads
   - Real-world profit will be significantly lower
2. **Next: Make simulation realistic** — random walk BTC, stochastic books
3. **Next: Live mode setup** — Polymarket API keys, Polygon wallet
4. **Next: Live test with tiny capital** — $20-50 to validate in real conditions
5. **Consider**: reduce max_side_price from 0.65 to 0.58 for extra safety
6. **Consider**: per-window file logging (`--log-dir` flag)

## Architecture Notes
- Simulated BTC feed oscillates ±$50-100 around $97,000 (deterministic sine waves)
- Simulated order books derive from Black-Scholes fair value with 2.5c spread
- Strategy waits 5s entry delay, stops 5s before window close
- Orders: 20 shares per order, limit BUY at fair value
- Target edge: 2% per merge pair (combined cost < $0.98)
- Conservative bid: assumes other side costs ≥ $0.40 when no position held
- Position balance: pauses buying if one side > 3x the other (and > 40 shares)
- Max exposure: $100 per market window
