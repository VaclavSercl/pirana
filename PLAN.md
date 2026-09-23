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


## Codex continuation: full audit reconciliation on Caslav, 2026-09-23
Goal: integrate audited F01-F09/A01-A05 repairs with server HEAD642ecd2, preserving unrelated server work. Work only in this isolated worktree. Linux ARM rustc/cargo1.93.1 available; no Windows tests required. No live orders, service deployment/restart or remote publication.
Instruction source: actual server master policy SHA2565564412cff064935fc715185d8d4fe2cd80b705d78176b61d60b9f81ab0cc2f8 fully read. Actual instruction pin and logs in ../pirana-evidence-20260923. Apply owner limit3 repair cycles over weaker repo7.
File scope: market routing/main, protected exits/client serialization, depth/slippage/callers, atomic position/fee accounting/recovery, Activity/equity/tick records/dashboard, integration tests. Preserve reports, operational scripts and existing helper sources; retire duplicate executable paths explicitly if superseded.
Verification order: cargo check --locked --all-targets; cargo clippy --locked --all-targets -- -D warnings; cargo test --locked --workspace; full Python suite/strategy/shell syntax; independent review and final content verification. One cargo process, CARGO_BUILD_JOBS=1, nice priority; separate build output from running release binary. Required missing capabilities report BLOCKED rather than bypass.
Recovery: registered clean baseline worktree; original service checkout untouched. Keep incoming files/three-way conflict evidence in private evidence directory. No discard/reset. Local commit only after required gate.

Repair cycle 1: Clippy found is_none_or incompatible with declared MSRV1.81; use map_or without lowering MSRV/lints. Independent review found exposure publication outside wallet generation guard; reacquire the same generation for the exposure calculation and publication. Re-run affected check/Clippy before full tests.

Repair cycle 2: the Linux suite exposed one obsolete gross-entry-cost assertion. The fixture pays 156 USD and receives 1.49 BTC after its base fee; assert net cost 156/1.49 and preserved TP/SL distances, consistent with runtime recovery regression. Restore 19 legacy helper tests through a separate integration target without restoring obsolete production routing. Re-run the full Rust gate.

Final native verification: PASS on Caslav Linux aarch64, existing rustup Rust/Cargo1.94.0 and Python3.14.4. cargo check --offline --locked --all-targets, cargo clippy --offline --locked --all-targets -- -D warnings, cargo test --offline --locked --workspace all exit0; 452 Rust tests, none ignored. Full pytest:210 passed plus2 subtests. Strategy validation,12 shell syntax checks, systemd graph and diff integrity passed. Independent read-only review accepted net-cost assertions,19 retained legacy tests and guarded exposure publication. Two repair cycles used. Source checks completed before this documentation-only finalization.
Complete CI status: BLOCKED because Docker compose/build are unavailable; pinned CI Rust1.85.1 was not exercised. No final checkpoint, deployment, service restart, push or PR. Preserve candidate for container checks on Caslav after specific Docker/image authorization or an existing approved container environment becomes available. Historical account migration and profitability validation remain separate evidence-dependent operational/research steps; no historical BTC counters were reclassified. Evidence: ../pirana-evidence-20260923/report.md, gate-final.json, tests-final.log and python-tests.xml. Production checkout remained clean at642ecd292d7dceef7a251d56dfdf7e67e968fa82 and service active.

Owner continuation 2026-09-23: explicitly requests commit, push to GitHub and deployment of this repair; hourly monitoring configured in Codex task. Resolve actual single GitHub endpoint and exact destination before publication. Production currently HALTED due to unresolved execution reconciliation; diagnose against authenticated evidence without clearing durable intents. Preserve baseline binary and runtime snapshots; build release into separate target directory to avoid replacing live binary during compilation. Mandatory predeployment review and postdeployment health/reconciliation. Docker install/images approval requested under A.5; pending. Do not bypass required checks.


Incident repair 2026-09-23: authenticated read-only evidence confirms pending BUY CID28639263577008/order244611273982 is terminal EXCHANGE IOC with status `IOC CANCELED was: PARTIALLY FILLED @ 81889.0(0.00012209)`, requested0.000465, remaining0.00034291, exact execution0.00012209 and zero USD fee. Existing status delimiter parser omits the observed ` was: ` form and repeatedly reports nonterminal. Scope: narrowly extend recognized terminal status suffix, preserve independent exact quantity/identity/fee proof, and add raw-status partial-fill/negative regressions in bitfinex_client.rs. Never clear intents or manufacture zero fills. Verify native check/Clippy/full tests before approved commit/publication/deployment. Owner approved Docker installation/images; parent will additionally add ARG CARGO_BUILD_JOBS=1 solely in infrastructure/docker/Dockerfile for bounded ARM build resource use and build from sanitized external context excluding Git metadata and secrets. Docker process will not compete with native Cargo.

Publication readiness: native455 Rust tests and210 Python tests, strict root Clippy, strategy and Compose passed. Docker production image built successfully on Caslav ARM64 with pinned Rust1.85.1; build inputs fingerprinted in private evidence. Added narrow progress entries to KANBAN.md and AGENT_STATE.md. Final staged candidate will repeat native gate and integrity checks before local commit. Authorized endpoint:https://github.com/VaclavSercl/pirana.git; destination:refs/heads/codex/audit-remediation-20260923; PR base:main. No force push or remote main merge. Deployment outcomes and exact image/binary/commit identities will be recorded outside tracked files in pirana-evidence-20260923. The earlier Docker BLOCKED state above is now resolved by owner-approved installation and successful build.
