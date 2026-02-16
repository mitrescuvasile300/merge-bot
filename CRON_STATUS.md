# Merge Bot Development Status

## Current State: FIRST DRY-RUN COMPLETE ✅
- Full 5-minute window tested end-to-end
- Orders placed, fills simulated, merges executed, P&L tracked
- Position imbalance limit added (max 60 shares ahead)
- Markets traded counter fixed
- Pushed to GitHub: https://github.com/mitrescuvasile300/merge-bot

## Latest Test Results (2026-02-16 03:33 UTC)
| Metric | Value |
|--------|-------|
| Orders placed | 41 |
| Merges executed | 7 |
| Pairs merged | 260 |
| Total invested | $216.23 |
| Total payout | $260.00 |
| Realized profit | $47.54 |
| Win rate | 85.7% (1 losing merge) |
| Unmerged position | 60 Up + 0 Down |
| Final capital | $258.77 |

## Known Issues / Next Steps
1. **Simulated P&L is unrealistically high** — BTC price feed oscillates too aggressively; real markets have tighter spreads and more competition. Need to calibrate the simulated feed.
2. **Still heavy UP bias** — The simulated BTC often drops below opening, making UP cheap. Strategy correctly buys, but the price recovery creates large one-sided positions even with the 60-share imbalance cap.
3. **Fill simulation too generous** — Orders fill instantly when bid >= ask. Need partial fills, competition, and slippage modeling.
4. **Need `--sim-speed` flag** — Full 5-min windows take 5+ minutes real time. Add time compression for faster testing.
5. **Multi-window test** — Run 3+ consecutive windows to test position carry-over and daily P&L tracking.
6. **Tighten simulated spread** — Currently 2.5c; real markets may have 1-2c spreads.

## Architecture Notes
- Simulated BTC feed oscillates ±$30-50 around $97,000 (deterministic sine waves)
- Simulated order books derive from Black-Scholes fair value with 2.5c spread
- Strategy uses entry_delay=5s, exit_buffer=3s for testing (prod: 90s, 30s)
- Max side imbalance: 60 shares (prevents dangerous one-sided exposure)
- Orders: 20 shares per order, limit BUY at fair value
- Target edge: 2% per merge pair (combined cost < $0.98)

## Test Command
```bash
timeout 420 ./target/release/merge-bot --max-windows 1 --log-level info --entry-delay 5 --exit-buffer 3
```
