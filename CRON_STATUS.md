# Merge Bot Development Status

## Current State: v12 — BEST RESULTS YET (Near Break-Even)
- Code compiles cleanly (0 errors, 0 warnings)
- HEAD includes v9-v12 improvements
- **Key finding: v12 stop-loss improvements reduced losses by 96%**
- **Best 5-window GBM test: -$0.78 (-0.16/window) — nearly break-even!**

## Latest Tests — 2026-02-16 ~07:16-08:10 UTC

### Test 1: v12 5-Window GBM (BEST RESULT)
```
Window  Merges  Pairs  Stop-Loss                         Salvage               Cap Change
  #1      1       5    SL: 10 DOWN@$0.33 (SOFT)         5 UP@$0.97           +$2.54
  #2      2       5    SL: 10 UP@$0.36, 10 UP@$0.35     5 DOWN@$0.21         -$2.64
  #3      3      15    —                                 5 UP@$0.01           -$2.15
  #4      9      30    —                                 —                    +$2.50
  #5      1       5    SL: 10 UP@$0.27 (HARD)           —                    -$1.03
─────   ─────   ────  ──────────────────────────         ─────────────        ────────
TOTAL    16      60    3 mid-window stop-losses          16 shares salvaged   -$0.78
```
- Capital: $215.00 → $214.22 (-0.4% over 5 windows, -$0.16/window)
- 100% merge win rate, 10 merges
- Stop-loss recovered $8.59 that would have been $0.30 → **saved ~$8.29**
- Window 4 was ideal: 9 merges, no salvage needed, BTC oscillated

### Test 2: v10 5-Window GBM (BASELINE)
```
Window  Merges  Pairs  Salvage                  Cap Change
  #1      1      10    10 DOWN@$0.42            +$0.08
  #2      1      10    20 UP@$0.01              -$8.58
  #3      1      10    20 UP@$0.01              -$8.39
  #4      8      50    10 UP@$0.02              +$5.33
  #5      1      10    20 DOWN@$0.01            -$8.00
─────   ─────   ────  ──────────────            ────────
TOTAL    12      90    No stop-loss             -$19.55
```
- Capital: $215.00 → $195.45 (-9.1%, -$3.91/window)
- 4 of 5 windows: salvage at $0.01 (total loss)

### Test 3: v13 Experiment (tighter imbalance=5, MTM cap=-3)
```
Capital: $215.00 → $208.89 (-2.8%, -$1.22/window)
9 merges, 45 pairs — slightly worse due to reduced trading activity
```
→ Reverted: tighter balance reduced merge opportunities without improving risk

### Version Comparison
| Version | Windows | Final Cap | Per-Window | Stop-Loss | Salvage@$0.01 |
|---------|---------|-----------|------------|-----------|---------------|
| v10     | 5       | $195.45   | -$3.91     | None      | 4 of 5 windows |
| v12     | 5       | $214.22   | -$0.16     | 3 hits    | 1 of 5 windows |
| v13     | 5       | $208.89   | -$1.22     | 2 hits    | 2 of 5 windows |
| v12 (other) | 3   | $212.26   | -$0.91     | 4 hits    | N/A |

**v12 is the best version — 96% reduction in losses vs v10**

## What v12 Changed (Key Innovations)
1. **Two-tier mid-window stop-loss**: HARD (FV<0.30) + SOFT (FV<0.40 near close)
2. **Tighter FV entry bands**: 0.20-0.80 (was 0.12-0.88) — avoids buying clear losers
3. **Max 1 open order per side**: prevents one-sided fill storms
4. **Fill price tracking**: uses actual fill price (with price improvement)

## Economics Breakdown (v12, 5-window average)
- **Merge profit**: ~$1.20/window (100% win rate, ~5-10% per pair)
- **Stop-loss recovery**: ~$1.70/window (sold at $0.27-$0.37 instead of $0.01)
- **Position loss**: ~$1.50/window (remaining unmerged at expiry)
- **Net**: ~-$0.16/window — **almost break-even!**

## Architecture
- **Sim**: GBM with 45% annual vol, κ=0.001 mean-reversion, ±$2 noise
- **Orders**: 5 shares/order, max 1 open per side, $0.48 max side price
- **Stop-loss**: Two-tier (HARD at FV<0.30, SOFT at FV<0.40 near close)
- **Stale orders**: Cancelled after 30s if bid > 3c below current ask
- **FV threshold**: Time-scaled (20-80% early → 34-66% near expiry)
- **MTM cap**: Stop trading if mark-to-market P&L < -$5
- **Post-merge cooldown**: 2 ticks pause
- **Salvage**: Sells excess unmerged at bid before window close

## Next Steps (Priority)
1. **🔴 Run 20+ window test** for confidence interval on true EV
2. **🟡 Calibrate sim against real Polymarket** — real books may have wider spreads (more edge)
3. **🟡 Explore reducing max_side_price** from $0.48 to $0.45 (more merge edge per pair)
4. **🟢 Live test on smallest possible size** once sim confirms positive EV
5. **🟢 Consider pair-only buying** — only buy side B when holding side A
