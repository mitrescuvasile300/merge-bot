# Merge Bot Development Status

## Current State: v12 — VARIANCE CRUSHED, EV SLIGHTLY NEGATIVE
- Code compiles cleanly (0 errors, 0 warnings)
- v12: mid-window stop-loss + tighter FV bands + fill_price + 1 order/side max
- HEAD: `e517b30` (v12: mid-window stop-loss + tighter FV bands + fill_price tracking)
- **Key finding: Variance reduced from ±$40 to ±$1.3/window, but average is -$0.91/window**

## Latest Test — 2026-02-16 ~07:16-07:30 UTC (v12, 3-window GBM)

```
Window  Orders  Merges  Pairs  Merge P&L   Salvage Loss   NET P&L
  #1      13      1       5    +$0.47      -$1.51         -$1.04
  #2      11      4      20    +$1.89      -$3.39         -$1.50
  #3      12      6      30    +$2.62      -$3.88         -$1.27
─────   ─────   ────   ────   ────────    ──────────     ────────
TOTAL    36      11      55    +$4.97      -$8.78         -$3.81

Capital: $215 → $212.26 (-1.27%)
100% merge win rate | 4 mid-window stop-losses triggered
```

### What v12 Changed
1. Two-tier mid-window stop-loss: HARD (FV<0.30 → sell immediately), SOFT (FV<0.40 → sell near close)
2. Fill_price tracking (actual fill with price improvement, not limit price)
3. Reduced max open orders per side: 2 → 1 (max 5 shares outstanding)
4. Tightened FV extreme threshold: 0.20-0.80 (was 0.12-0.88)

### Economics Breakdown
- **Merge profit per window**: ~$1.66 avg (11 merges, 55 pairs, $0.09/pair avg)
- **Salvage loss per window**: ~$2.93 avg (125 shares total, avg ~$0.07 loss/share)
- **Net per window**: -$0.91 avg → slightly negative EV
- **Merge efficiency**: 55 merged / ~180 total shares = 31% (need ~50%+ for break-even)

### Root Cause: Adverse Selection
When BTC trends, the losing side becomes cheap → more fills at our limit.
The winning side's ask rises → no fills. We accumulate excess losing-side tokens
that must be salvaged at a loss. The merge alpha is REAL (~9% per pair) but
the position waste ratio makes it net negative.

## Test Results History
| Run | Version | Merges | Pairs | Merge P&L | Salvage Net | TRUE NET | Variance |
|-----|---------|--------|-------|-----------|-------------|----------|----------|
| v12 3-win GBM | v12 | 11 | 55 | +$4.97 | -$8.78 | -$3.81 | ±$0.25/win |
| v9.1 3-win GBM | v9.1 | ? | 25 | ? | ? | +$2.70 | ±$3/win |
| v8 3-win GBM | v8 | 5 | 170 | +$6.80 | +$58.21 | +$65.01 | ±$40/win |
| v8 1-win GBM | v8 | 2 | 110 | +$4.40 | -$42.30 | -$37.90 | N/A |
| v8 3-win sine | Sine | 42 | 1560 | +$138.73 | N/A | +$91 | N/A |
| v8 1-win sine | Sine | 29 | 1200 | +$147.44 | N/A | +$147 | N/A |

## Architecture
- **Sim**: GBM with 45% annual vol, κ=0.001 mean-reversion
- **Orders**: 5 shares/order, max 1 open per side, $0.48 max side price
- **Stop-loss**: Two-tier (HARD at FV<0.30, SOFT at FV<0.40 near close)
- **Salvage**: Sells excess unmerged at bid before window close
- **FV threshold**: Time-scaled (20-80% early → 34-66% near expiry)
- **P&L**: merge_profit + salvage_proceeds - salvage_cost - remaining_cost

## Next Steps (Priority)
1. **🔴 Balanced buying constraint**: Only buy side B when holding side A → higher merge efficiency
2. **🟡 Earlier stop-loss**: FV<0.40 trigger (currently 0.30) → better salvage recovery
3. **🟡 Wider merge target**: $0.96 combined cost (not $0.98) → bigger per-pair edge
4. **🟡 Python v9 exit-sell strategy**: Shows more consistent profit, different economics
5. **🟢 Live test**: Only after merge efficiency > 50% or alternative strategy validated
