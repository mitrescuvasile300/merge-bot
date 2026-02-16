# Merge Bot Development Status

## Current State: RANDOM WALK SIMULATION IMPLEMENTED ✅ (Reality Check)
- Code compiles cleanly (0 errors, 0 warnings)
- Unit tests pass
- Simulation upgraded from deterministic sine-wave to GBM random walk
- **First realistic dry-run: merges are profitable but unmerged position risk dominates**

## Latest Test — Random Walk Dry-Run (2026-02-16 ~04:28 UTC)
### Test Parameters
- `--max-windows 2 --log-level info --entry-delay 5 --exit-buffer 5`
- **NEW**: GBM random walk BTC feed (not deterministic sine)
- **NEW**: Noisy order book spreads (±0.5c jitter, time-dependent widening)
- Capital: $215, Target edge: 2%

### Per-Window Results
```
Window  Merges  Pairs   Invested    Merge P&L   Win Rate    NET P&L
──────  ──────  ──────  ──────────  ─────────   ────────    ────────
  #1      12     380    $361.82     +$18.18     11/12       -$5.18 *
  #2       7     360    $335.65     +$24.35      7/7        (killed)
──────  ──────  ──────  ──────────  ─────────   ────────    ────────
TOTAL     19     740    $697.47     +$42.53     18/19       ~-$5
```
* Window 1 NET loss due to 60 unmerged Down shares ($23.36 lost)
* Window 2 was killed by timeout before summary — merges looked profitable

### Key Insights from Random Walk
1. **Merges ARE profitable**: 18/19 winning (95%), avg profit $2.24/merge
2. **Unmerged positions are the MAIN RISK**: BTC trending one way builds up one-sided shares
3. **Window 1 example**: BTC trended down → 60 Down shares accumulated → $23.36 lost at close
4. **Merge margins realistic**: 0.7% to 16.5% per merge (vs 2-14% with sine wave)
5. **Capital turnover lower**: 3.2x (vs 6.8x with sine) — fewer oscillations to exploit

### Comparison: Random Walk vs Old Sine Wave
```
Metric              Sine Wave (3-win)   Random Walk (this test)
────────────────    ─────────────────   ───────────────────────
Merges/window       ~14                 ~10
Pairs/window        ~520                ~370
Merge profit/win    ~$46                ~$21
Capital turnover    6.8x                3.2x
Win rate            93%                 95%
Unmerged risk       LOW                 HIGH ⚠️
NET profit/win      ~$30                ~-$3 (variable)
```

## Previous Test History
| Run | Sim Type | Merges | Pairs | Merge P&L | Win Rate | NET P&L | Notes |
|-----|----------|--------|-------|-----------|----------|---------|-------|
| Random Walk | GBM | 19 | 740 | +$42.53 | 95% | ~-$5 | **Reality check** |
| 3-win sine | Sine | 42 | 1560 | +$138.73 | 93% | +$91 | Multi-window milestone |
| #4 sine | Sine | 29 | 1200 | +$147.44 | 96.5% | +$147 | Capital turnover ~5.6x |

## All 10 Bugs Fixed ✅
(Same as before — see git history)

## Known Issues & Next Steps (Priority Order)
1. **⚠️ CRITICAL: Unmerged position risk** — main source of loss
   - Options: tighter imbalance limit (2x instead of 3x)
   - Time-weighted position wind-down in last 60s
   - Smaller order sizes (10 shares instead of 20) for finer control
   - Emergency exit: sell unmerged positions before window close
2. **Variance is HIGH** with random walks — need 10+ window sample
3. **Consider**: Entry delay > 5s to let price establish direction
4. **Consider**: Only buy when BTC is within ±0.1% of opening (tight range strategy)
5. **Live mode setup** — Polymarket API keys, Polygon wallet
6. **Live test** with tiny capital ($20-50) — real market conditions

## Architecture Notes
- Simulated BTC feed: GBM with 45% annualized vol + weak mean-reversion (κ=0.001)
- Microstructure noise: ±$2-3 per tick
- Order books: 3-level depth with ±0.5c spread jitter, widening near expiry
- Strategy: 20 shares/order, 2% target edge, 3x imbalance ratio max
