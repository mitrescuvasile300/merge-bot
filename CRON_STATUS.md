# Merge Bot Development Status

## Current State: v8 + P&L FIX — HIGH VARIANCE CONFIRMED
- Code compiles cleanly (0 errors, 0 warnings)
- v8: time-scaled FV threshold + salvage mechanism + accurate P&L
- HEAD: `af79e05` (fix: accurate NET P&L after salvage)
- **Critical finding: Same strategy produces +30% OR -18% depending on BTC path**

## Latest Tests — 2026-02-16 ~05:16-05:35 UTC

### Test A: 3-Window Random Walk (GBM)
```
Window  Merges  Pairs  Merge P&L   Salvage Net   TRUE NET
  #1      0       0    $0.00       -$9.40        -$9.40
  #2      1      30    $1.20      +$52.76       +$53.96
  #3      4     140    $5.60      +$14.85       +$20.45
──────  ──────  ────  ────────    ──────────    ────────
TOTAL     5     170    $6.80      +$58.21       +$65.01
```
- Capital: $215 → $280 (+30.2% ROI)
- 73 orders, 54 fills, 100% merge win rate
- ⚠️ $58 of $65 profit from salvage (directional luck), only $6.80 from merges

### Test B: 1-Window Verification (with P&L fix)
```
Window  Merges  Pairs  Merge P&L   Salvage Net   TRUE NET
  #1      2     110    $4.40      -$42.30       -$37.90
```
- Capital: $215 → $177 (-17.7%)
- BTC trended → 90 UP shares expired near-worthless
- P&L fix correctly shows -$37.90 (old formula would have shown ~+$5)

### Key Findings
1. **Merge alpha is REAL but small**: 100% win rate, 4.16% per pair, ~$2-5/window
2. **Position risk DOMINATES**: ±$40/window from unmerged tokens
3. **Salvage is directional**: can profit (+$52) or lose (-$42) depending on trend
4. **Variance is extremely high**: same code → +$65 or -$38
5. **P&L reporting now accurate**: salvage cost basis tracked correctly

## Test Results History
| Run | Sim | Merges | Pairs | Merge P&L | Salvage Net | TRUE NET | Notes |
|-----|-----|--------|-------|-----------|-------------|----------|-------|
| 3-win v8 GBM | GBM | 5 | 170 | +$6.80 | +$58.21 | +$65.01 | Lucky salvage |
| 1-win v8 GBM | GBM | 2 | 110 | +$4.40 | -$42.30 | -$37.90 | Unlucky trend |
| Random Walk v1 | GBM | 19 | 740 | +$42.53 | N/A | ~-$5 | First reality check |
| 3-win sine | Sine | 42 | 1560 | +$138.73 | N/A | +$91 | Unrealistic |

## Architecture
- **Sim**: GBM with 45% annual vol, κ=0.001 mean-reversion
- **Orders**: 10 shares/order, max 20 imbalance, $0.48 max side price
- **Salvage**: Sells excess unmerged at bid before window close
- **FV threshold**: Time-scaled (05-95% early → 17-83% near expiry)
- **P&L**: merge_profit - salvage_loss - remaining_unmerged_cost

## Next Steps (Priority)
1. **Run 20+ window sample** to estimate true expected value
2. **Reduce position risk**: smaller orders (5 shares), stricter imbalance (10)
3. **Explore exit sells**: Python v9 has promising exit mechanism
4. **Calibrate with real order books** when Polymarket launches BTC 5-min markets
5. **Live test**: Only after variance is understood (50+ window sample)
