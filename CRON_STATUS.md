# Merge Bot Development Status

## Current State: v11 — STALE ORDER FIX + AFFORDABILITY CHECK
- Code compiles cleanly (0 errors, 0 warnings)
- HEAD: `ff673cb` (v11: other-side affordability check)
- **Critical bug fixed**: stale order handling prevents bot freeze (was idle 4+ min/window)
- **v9-v11**: anti-adverse-selection, MTM loss cap, post-merge cooldown, affordability check
- **Key finding: merge alpha positive but position risk still dominates**

## Latest Test — v11 5-Window GBM (2026-02-16 ~07:12-07:35 UTC)

### Per-Window Breakdown
```
Window  Merges  Pairs  Salvage         Cap Change
  #1      2       15   10 DOWN@$0.17   -$2.40
  #2      3       15   10 UP @$0.15    -$2.61
  #3      1       10   15 UP @$0.14    -$4.47
  #4      5       25   10 DOWN@$0.17   -$2.00
  #5      1       10   10 DOWN@$0.17   -$2.32
──────  ──────  ────  ──────────────   ────────
TOTAL    12       75   55 shares        -$13.80
```

### Summary Statistics
- Capital: $215.00 → $201.20 (-6.4% over 5 windows, -$2.76/window)
- Merge profit: +$5.86 (100% win rate, ~8% per pair)
- Salvage loss: ~$16.45 (10-15 shares/window at ~$0.15 bid)
- Merge alpha: +$1.17/window
- Position risk: -$3.93/window
- **NET EV is negative: merge alpha doesn't cover position risk in sim**

### Previous Results Comparison
| Version | Windows | Merges | Merge P&L | NET/Window | Notes |
|---------|---------|--------|-----------|------------|-------|
| v8 stale fix only | 5 | 28 | +$8.08 | -$2.54 | More merges, same NET |
| v11 (all fixes) | 5 | 12 | +$5.86 | -$2.76 | Fewer merges, tighter risk |
| v8 GBM 3-win | 3 | 5 | +$6.80 | +$21.67 | Lucky salvage |
| v8 GBM 1-win | 1 | 2 | +$4.40 | -$37.90 | Unlucky trend |
| v8 sine 3-win | 3 | 42 | +$138.73 | +$30.33 | Unrealistic sim |

## Architecture
- **Sim**: GBM with 45% annual vol, κ=0.001 mean-reversion, ±$2 noise
- **Orders**: 5 shares/order, max 10 imbalance, $0.48 max side price
- **Stale orders**: Cancelled after 30s if bid > 3c below current ask
- **Anti-adverse**: Per-side order cap (2), pending imbalance tracking
- **Affordability**: Skip orders if projected merge cost > $1.02
- **MTM loss cap**: Stop trading if mark-to-market P&L too negative
- **Post-merge cooldown**: 2-tick pause after merge to prevent aggressive re-entry
- **Salvage**: Sells excess unmerged at bid before window close
- **FV threshold**: Time-scaled (aggressive early, conservative near expiry)
- **Wind-down**: Hard stop at <60s, balancing only at <90s, bargains at <120s

## Key Insights
1. **Merge alpha is REAL**: 100% win rate, ~4-10% per pair, ~$1/window
2. **Position risk DOMINATES**: ~$3-4/window from unmerged tokens expiring
3. **Simulation NET EV is negative**: -$2.76/window (merge alpha < position risk)
4. **But sim may be pessimistic**: real Polymarket books could have:
   - Wider spreads → more merge edge per pair
   - More oscillation → more merge opportunities
   - Better fill rates on both sides
5. **Stale order fix was critical**: without it, bot froze for 4+ min/window

## Next Steps (Priority)
1. **🔴 Calibrate sim against real Polymarket data** — test with actual BTC 5-min books
2. **🟡 Improve salvage timing** — sell imbalanced positions EARLY (at $0.30+) not at close ($0.15)
3. **🟡 Explore mid-window exits** — sell tokens when conditions turn against merge viability
4. **🟢 Run 50+ window Monte Carlo** for confidence interval on true EV
5. **🟢 Live test** — only after sim calibration confirms positive EV
