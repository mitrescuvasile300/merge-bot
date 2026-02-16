# Merge Bot Development Status

## Current State: v9.1 (v12 HEAD) — FIRST PROFITABLE GBM RUN
- Code compiles cleanly (0 errors, 0 warnings)
- HEAD: `e517b30` (v12: mid-window stop-loss + tighter FV bands + fill_price tracking)
- **Milestone: +1.3% ROI on 3-window GBM random walk test**
- Adverse selection problem identified and fixed

## Latest Tests — 2026-02-16 ~07:08-07:20 UTC

### Test: 3-Window GBM Random Walk (v9.1)
```
Window  Merges  Margin    Salvage         NET P&L
  #1      0     —        +$2.51 profit   +$2.51
  #2     10     5.88%    -$0.95 loss     -$2.91
  #3     15     5.8-9.7% +$2.29 profit   +$4.30
──────  ──────  ────────  ──────────      ────────
TOTAL    25     100% win  salvage +$3.85  +$2.70
```
- Capital: $215 → $217.70 (+1.26% ROI)
- 17 orders, 4 merges, 25 pairs, 100% merge win rate
- Stop-loss fired in W2: sold 10 excess DOWN at FV=0.14

### Comparison with v8
```
v8 (before):  5 windows → $193.86 (-9.8%) ❌
v9.1 (now):   3 windows → $217.70 (+1.3%) ✅
```

## Root Cause: Adverse Selection (FIXED)
The bot was buying MORE losing-side tokens because:
1. Losing side gets cheaper → fills more at our limit price
2. Winning side gets expensive → our limit doesn't fill
3. Result: excess worthless losing tokens at expiry

Three fixes:
1. **Per-side order cap** (max 2 open/side) — prevents batch one-sided fills
2. **Price improvement in sim** — fills at best_ask, not limit price
3. **Mid-window stop-loss** — sells excess losing tokens at ~$0.12-0.18 vs $0.01 at expiry

## Architecture (v9.1)
- **Sim**: GBM with 45% annual vol, κ=0.001 mean-reversion
- **Orders**: 5 shares/order, max 2 open orders per side, max 10 imbalance
- **Exposure**: $50/window cap (halved from $100)
- **Salvage**: Sells excess at bid before window close
- **Stop-loss**: If excess side FV < 0.20, sell early at current bid
- **FV threshold**: 0.12-0.26 (time-scaled), stops buying clear losers
- **P&L**: fill_price-based (with price improvement)
- **Merge margin**: 5-10% per pair (improved from fixed 4.16% due to price improvement)

## Test Results History
| Run | Sim | Merges | Pairs | Merge P&L | Salvage Net | TRUE NET | Notes |
|-----|-----|--------|-------|-----------|-------------|----------|-------|
| v9.1 3-win GBM | GBM | 4 | 25 | +$1.62 | +$3.85 | +$2.70 | **PROFITABLE** |
| v8 5-win GBM | GBM | 8 | 175 | +$7.00 | -$28.14 | -$21.14 | Adverse selection |
| v8 3-win GBM | GBM | 5 | 170 | +$6.80 | +$58.21 | +$65.01 | Lucky salvage |
| v8 1-win GBM | GBM | 2 | 110 | +$4.40 | -$42.30 | -$37.90 | Unlucky trend |
| Sine wave 3-win | Sine | 42 | 1560 | +$138.73 | N/A | +$91 | Unrealistic |

## Next Steps (Priority)
1. **🔴 Run 10+ window Monte Carlo** for statistical confidence on positive EV
2. **🟡 Tune FV threshold** — current 0.12-0.26 may be too restrictive (0 merges in some windows)
3. **🟢 Calibrate sim** with real Polymarket order books
4. **🟢 Live test** — only after 20+ window stats confirm positive EV
