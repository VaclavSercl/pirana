# F05-F09 Repair Plan

## Goal
Fix five findings (F05-F09) in the pirana trading system related to receive timeout handling, kappa parameter separation, BTC reserve accounting, execution activity generation, and VWAP depth coverage.

## Impact
- F05: `src/main.rs` (run_market_data_feed, DashboardState in pirana-dashboard)
- F06: `src/main.rs` (process_ws_message — ticker & trade handlers)
- F07: `src/main.rs` (profit skimmer blocks), `crates/pirana-dashboard/src/state.rs`
- F08: `src/main.rs` (process_ws_message, EXECUTION_ACTIVITY usage)
- F09: `crates/pirana-core/src/order_book.rs` (vwap), `crates/pirana-core/src/slippage.rs`, `src/main.rs` (callers)

## File Checklist
- [ ] `crates/pirana-dashboard/src/state.rs` — add `market_data_available`, `pending_skim_usd` fields
- [ ] `src/main.rs` — F05 timeout logic, F06 kappa separation, F07 pending_skim_usd, F08 activity only on economic mutations
- [ ] `crates/pirana-core/src/order_book.rs` — VwapResult struct
- [ ] `crates/pirana-core/src/slippage.rs` — adapt VwapResult usage
- [ ] `crates/pirana-core/src/lib.rs` — export VwapResult
- [ ] Add regression tests in each modified crate

## Failure Scenarios
1. `cargo check` fails due to missing field initializers in DashboardState
2. Slippage guard signature change breaks existing tests
3. VwapResult::None vs Option<f64> mismatch at call sites
4. EXECUTION_ACTIVITY.begin() in async block drops guard immediately if not stored
