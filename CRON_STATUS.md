# Merge Bot Development Status

## Current State: INITIAL BUILD COMPLETE
- Code compiles cleanly (0 errors, 0 warnings)
- 6 unit tests pass (Black-Scholes pricing, Kelly, fees)
- Pushed to GitHub: https://github.com/mitrescuvasile300/merge-bot
- Dry-run mode implemented with simulated BTC price feed + simulated order books

## Next Steps
1. Run first dry-run test (1 window) and capture output
2. Verify strategy loop works end-to-end
3. Check order placement, fill simulation, merge execution
4. Fix any issues found
5. Optimize strategy parameters if needed

## Known Issues
- None yet (first test pending)

## Test History
(Will be populated by cron runs)

## Architecture Notes
- Simulated BTC feed oscillates ±$30-50 around $97,000 (deterministic sine waves)
- Simulated order books derive from Black-Scholes fair value with 2c spread
- Strategy waits 90s entry delay, stops 30s before window close
- Orders: 20 shares per order, limit BUY at fair value
- Target edge: 3% per merge pair (combined cost < $0.97)
