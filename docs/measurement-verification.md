# Verification of measurement evidence

This feature is code, not proof of deployment. No historical signal prices are manufactured.

## Sources and scope

Canonical executions remain in accounting.sqlite3, read-only for this report. Identity is (trade_id, order_id); CID joins the durable benchmark. The private positions.benchmarks.json sidecar retains immutable decision_benchmarks durably written before the order intent and submission, for BUY and both SELL paths. Each contains decision_mts, reference_price, requested_quantity, side and benchmark_kind=signal_last_trade. It survives zero/partial/full settlement and restart. This is the signal last-trade price, not executable bid/ask, an IOC limit, or a reconstructed historical quote. Absent legacy benchmarks are INCOMPLETE. Local decision time after exchange fill time invalidates comparison; clock synchronization must be checked operationally.

Slippage positive means adverse: BUY(fill-reference), SELL(reference-fill), multiplied by quantity. Weighted bps divides summed adverse cost by summed reference notional. Fees are excluded from this execution-quality measure and remain canonical accounting expenses. All fills for the CID are checked against requested quantity, including fills outside the requested report window. Multiple order IDs per CID are ambiguous and rejected.

## Sampled equity

Runtime wallet reconciliation collects authenticated BTC and USD exchange balances under the existing execution-idle guard, with an atomic timestamped market mark. Blocking persistence runs after guard release, off the market path. Data goes to the risk-state parent directory/measurement_evidence (normally /opt/caslav/risk/measurement_evidence; inherited from the configured risk-state path); daily equity-<UTC epochday>.jsonl files carry schema, source, wallet/mark/observation time and boot/session identity. BTC/USD are a portfolio subset, not necessarily every asset in the account. sync_cursor_ms is null when no trustworthy cursor accompanies the observation.

Writer fsyncs retained daily streams. Bad/future/stale marks, disk failures, malformed streams and daily16MiB capacity stop evidence collection visibly; they do not silently generate zeroes or alter trading risk. Deployment must monitor these warnings and file freshness. Stream retention is indefinite; disk space and private backups require operational ownership. A single runtime writer is assumed, not cross-process hostile isolation.

Report derives sampled unadjusted maximum balance drawdown in USD and sats. It splits coverage at gaps over60seconds and session changes. One point is not proof of zero drawdown. It reports incomplete requested windows and never merges across an outage to claim continuous maxDD. Transfers and other-pair flows can change the balances; cashflow-adjusted returns remain UNVERIFIED without authenticated attribution. Highs/lows between15second observations are not measured.

## Read-only report

```
python3 scripts/pirana_measurements.py --db /var/lib/pirana/accounting.sqlite3 --positions /var/lib/pirana/positions.json --equity-dir /opt/caslav/risk/measurement_evidence --start-ms <UTC milliseconds>
```

The JSON separates history gaps, slippage verified subset and equity segments. Exit0 means the report ran against fresh complete synchronization, not that every metric is verified; inspect status and each section. Exit2 means canonical synchronization is stale/incomplete; malformed input also fails without producing a successful report. A report with no benchmark/equity history stays INCOMPLETE.

Historical missing acquisition basis is listed per exact execution with missing BTC/sats. Deposit proof is not purchase-price proof. Other-pair trades and external purchases require original records before basis can be added by a separately reviewed accounting migration. No production ledger migration or balance reset is part of this change.

## Deployment and recovery gate

No production restart or publication is implied. Before deployment: full native gates, independent review, production build, current no-pending-order and recovery proof, private consistent backup, and a concrete approved rollout.

**The unchanged positions.json schema remains compatible with the old production binary. Benchmark evidence is a separate sidecar preserved by that binary. Never restore an old positions/accounting snapshot over new executions.** Rollback must preserve the sidecar and use the CURRENT positions/accounting state after independent no-pending-order/recovery checks. The old binary will not collect new benchmarks/equity, so those subsequent intervals are explicitly incomplete. An orphan benchmark created before a failed intent write is retained/reserved and is not counted as an execution without a canonical fill. The two files are not one atomic transaction; write ordering prevents a submitted measured order without prior benchmark evidence.

Tests cover side/sign, partial fills, missing old evidence, CID ambiguity, clock reversal, excess quantity, restarts, corrupted streams, invalid marks, gaps and both numeraires. Live collection needs dated postdeployment evidence; synthetic tests are not proof of actual runtime capture.


## Owner-approved funding-date market marks

`scripts/pirana_funding_valuations.py /var/lib/pirana/funding_valuations.json`
validates a private evidence envelope containing original official Bitfinex
one-minute candle response bytes and their SHA-256 digests. It checks positive
finite Decimal quantities/prices, minute alignment, source endpoint/query,
OHLC ranges, the latest completed candle before the deposit (at most 120 seconds
old after candle completion), matching close, ledger identity and source time.
The runtime file is private evidence, not tracked account data.

The owner-approved methodology is `funding_date_market_valuation`: market value
at deposit, **not proven original purchase cost**. `actual_acquisition_basis`
remains `UNKNOWN`; a mark cannot clear canonical FIFO gaps, modify fills or
change the existing trading epoch. The owner approval field references the
external authorization; the file cannot authorize itself. SHA-256 verifies
retained byte integrity, not independent source authenticity or approval.

The measurement CLI accepts optional `--funding-valuations PATH` and adds a
separate section. Invalid supplied evidence returns exit 2 without changing
canonical accounting. Omitting the flag retains the original behavior.
Full historical profit still requires authenticated opening balances and every
BTC/UST/USD flow, other-pair trade, fee, deposit and withdrawal. Funding marks
alone cannot establish historical profit or cash-flow-adjusted drawdown.


## Legacy reserve adjustment provenance

The historical `unlock_btc_reserve.py` writes trade1978200001,
order244505000001, CID28638000000001 as a synthetic 51000-sat reserve adjustment
at a reference price of81500USD/BTC. This row is **not evidence of an exchange
purchase**, original cost or owner authorization. Shared `execution_provenance`
classifies the exact tuple and conservatively flags any partial identity
collision. Snapshot reporting source becomes mixed. The underlying database
row, operational status, inventory, orders and recovery remain unchanged.
Financial consumers suppress affected account/active-period PnL verification;
the measurement CLI lists excluded identities and excludes them from actual
execution counts, slippage and fill-only inventory-gap analysis. This does not
resolve the missing original acquisition basis and does not silently remove
accounting records. Classification detects known markers, not every possible
past manual modification; independent venue reconciliation remains necessary.

The legacy mutation script is not invoked by this reporting repair. Any future
reserve migration needs a separately reviewed typed adjustment implementation;
the old synthetic-fill writer must not be reused as proof of acquisition or
current owner authorization. Operational projection metadata explicitly labels
its preserved opening reference as legacy-script input, not a confirmed cost.


## Recovery and calibration provenance (2026-09-29)

Raw canonical fill rows are retained. `fill_provenance` classifies immutable known operator entries using payload digests; `operational_opening_lots` holds separately typed, owner-authorized inventory references. Neither a typed lot nor an owner-declared acquisition reference is an authenticated venue execution. `pirana_accounting_repair.py` defaults to an in-memory preview and requires an exclusive backup for `--apply`; callers must stop writers and independently verify venue evidence, pending intents and inventory. Historical reports with incomplete basis remain unverified. Never remove a real venue execution to make a projection balance.

`pirana_calibration.py` reconstructs strategy roundtrips from authenticated entry orders, settled exit CIDs, signed fees and the position journal. Daily returns use actual opening equity observations with complete consecutive UTC-day coverage. Runtime independently validates the projection; stale or malformed input cannot fall back to the legacy diagnostic ledger. Existing brakes remain intact. WARMUP means the existing 50-roundtrip/5-day requirements have not been met; it does not prove that trading is broken. Accepted older risk settings are not retroactively certified by a fresh input report. Calibration does not invent historical VPIN, cashflow-adjusted account returns or missing price benchmarks.

The production helper runs from the separate 15-second wallet reconciliation task (every fourth reconciliation), outside the market WebSocket task. The helper is an asynchronous subprocess with a timeout; source evidence is published atomically. Completed-day alignment is pinned to report generation time; freshness is rechecked at use time. Equity files may append while a fixed validated prefix is read; changed/truncated prefixes remain invalid.

`funding_valuations.json` supports explicitly labeled owner-declared reference basis (including the owner's 99,999 USD reference). This is not venue proof of original purchase cost, a candle quotation, or repair of missing cashflow history.

The optional research capture is separate from the live execution feed. `scripts/research/hft_capture.py` is the tracked source of the installed collector. Its bounded asynchronous enqueue preserves raw frame order and receive timestamps during transient writer stalls; persistent stalls still fail visibly. Existing queue, storage and fsync limits are retained. Historical capture gaps remain gaps. Tests use fake stores/transports, not live exchange or Telegram requests.

A deployment must verify a fresh no-pending-order snapshot, current wallet/position reconciliation, an additive accounting preview against a consistent backup, and actual journal recovery before one controlled restart. Restore only the old executable if rollback is needed; never roll back newer fills or positions. Tests include an explicit private-fixture recovery check (`PIRANA_RECOVERY_FIXTURE`, ignored in ordinary public CI because it contains no public account data).
