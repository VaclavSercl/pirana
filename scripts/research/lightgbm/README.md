# Isolated LightGBM research for Pirana

This optional research tool reads public Bitfinex capture files. It cannot place orders. The first frozen experiment on Caslav (2026-09-30) did not demonstrate an advantage: on three held-out UTC dates LightGBM selected220/28280 opportunities with mean executable60-second markout -2.7154bps before additional costs; all-opportunity benchmark -2.0341bps, logistic -2.3464bps. These overlapping opportunities are not actual Pirana trades or account PnL. No confidence interval is justified by three dates. No live trading integration is included.

## Required verification and isolated installation

Owner authorization is required before dependency installation. The lock is specifically for tested LinuxARM64/Python3.14 wheels (LightGBM4.7.0, NumPy2.5.3, SciPy1.18.1, Narwhals2.26.0). It is not a universal requirements file. Use a dedicated virtual environment, never global Python or Hermes, and pip with --require-hashes --only-binary=:all:. Do not silently upgrade or substitute versions.

Run with that environment, from the repository:

    /approved/venv/bin/python -m pip check
    /approved/venv/bin/python -B -m unittest discover -s scripts/research/lightgbm -p 'checks_*.py'

The31 research checks, including actual native model loading and invalid-input rejection, are a separate REQUIRED research gate. They are not part of application pytest and do not silently skip absent dependencies. Application CI remains unchanged. Run actual train/save/reload/retrain equality and bounded forward inference too; parser fixtures alone do not verify an installed ML library.

## Data and frozen experiment

book_dataset.py verifies SEQ_ALL/OB_CHECKSUM, signedCRC32, session continuity, P0BTCUSD25level books, trade subscription, sane clocks and executable depth. It creates23causal receipt-time features every5seconds after65seconds of warmup. Corruption invalidates the session. The nominal0.00046BTC is solely a research benchmark. Private capture, derived data and model files must stay outside Git.

    python -B scripts/research/lightgbm/book_dataset.py --directory /public/capture --output /new/audit
    python -B scripts/research/lightgbm/order_observations.py --source /audit --destination /new/chronological
    python -B scripts/research/lightgbm/experiment.py --audit-dir /chronological --output /new/experiment

migrate_feature_contract.py exists only to preserve provenance of the initial misnamed65-second volatility field; it does not transform numeric data. New replays already produce feature contract2. A derived chronological view is mandatory: filename order alone did not match historical wall-clock order. Duplicate timestamps and inputdigest mismatches fail closed.

Training uses fixed100 shallow trees and a logistic benchmark, chronological date partitions,300-second purge,60-second executablebid/ask labels, maximum5-second lateness, and no cross-session/gap labels. All losses remain included. No hyperparameter search or adaptive test tuning. Cost stress is additional roundtripbps; report both selection coverage and markout of selected opportunities, never just a diluted average over all opportunities. PnL anddrawdown require faithful inventory/exit simulation. Observed dates do not certify complete days. The4–8week data objective remains unfulfilled.

## Bounded forward shadow

shadow.py reconstructs the last session from at most256MiB of recent append-only segments, then follows rotation. It deliberately drops bootstrap/history observations older than10seconds and anything within the frozen experiment. It requires exact manifest/model hashes, native feature names/order, versions, finitefeatures and unchanged benchmarkquantity. CRC/sequence failure waits for a newvalidsession; insufficient bootstrap blocks. Unexpected files, replacement, truncation and partial rotated records fail closed. Oldrows are never relabeledaslive.

Run only in an independently verified OS namespace: private home, account/config/runtime and otherprocess data hidden; source/model/code readonly; ownoutput writable; allnetwork syscalls denied with EPERM; oneCPUthread,512MiB,16tasks; explicit600second run with a hard660second outer timeout. The extra60seconds is shutdown slack, not renewed observation. isolation_probe.py verifies these boundaries and native model loading at fixed namespace paths /app,/model,/data,/output. Do not execute it against unisolated hostpaths. The source directory must be an immutable owned copy for the run.

    python -B shadow.py --source /data --model /model --output /output --manifest-sha VERIFIED_MANIFEST_SHA256 --model-sha VERIFIED_MODEL_SHA256 --duration 600

Private SQLite output has model/session/time uniqueness, FULL synchronous commits and a singlewriter lock; duplicate replay does not duplicate observations.128MiB limit fails without deleting history. Status is durable and says order_authority=false. Permission/malformed/storage/native failures remainvisible. Exit0 means at leastone fresh observation completed, not strategyquality or continuoushealthycoverage; evaluate counts, stale exclusions, timing and gaps separately. No boot enable or automatic retraining/promotion. Stop only the owned unit; preserve evidence. Namespace is a containment boundary; the trustedhost owner/root can still alter mounted artifacts.

## Promotion requirements

A different validated model, sufficient fresh holdout data across regimes, all actual Pirana candidate decisions with as-of features, faithful position/exit/risk replay, cost/latency analysis and independent review are required before considering a filter. The current negative model has no authorization or capability to trade. Local research deployment does not change live risk, orders, accounting, calibration or Telegram scheduling.

## Actual entry-decision evidence

The production Rust recorder observes every admitted trade that reaches baseline evaluation, including no-signal, shadow and rejected live candidates. The earlier execution, cooldown and VPIN guards are counted separately because they prevent candidate evaluation. Rejected raw wire messages are outside this scope. Inputs are frozen before entry side effects; the recorder never scores a model or changes an order/risk decision. A CID and `intent_handoff` mean a durable intent was handed to the asynchronous execution task, not that the venue received or filled an order.

The producer uses a bounded 1024-record channel without waiting for capacity. One worker writes private append-only segments under the sibling `decision_evidence` directory of the existing risk-state path. Segments are limited to 64 MiB and retained evidence to 2 GiB. No automatic deletion occurs. Loss, write failure, unknown outcomes, a missing segment or stale final evidence prevents research qualification; the trading policy is unaffected. Capacity duration depends on observed traffic. This worker shares the trader process; it is not the isolated public-data model sandbox.

Each segment identifies the actual running binary SHA-256. The offline reader requires a separately pinned deployment manifest containing its verified source commit/tree and verification-evidence hash. It verifies segment order, monotonic sequence/counters, frozen baseline predicate and intent timestamps. Canonical fills join by CID and a unique venue order, including partial quantities and signed USD/BTC fees. An unmatched CID remains `NOT_YET_MATCHED`, never an invented cancellation. Acknowledgement receipt is not assumed to precede an exchange fill timestamp.

    python3 scripts/research/lightgbm/decision_dataset.py --directory /private/decision_evidence --manifest /private/deployment.json --manifest-sha256 VERIFIED_MANIFEST_SHA256 --canonical-db /var/lib/pirana/accounting.sqlite3 --output /new/private/audit

Use a new output directory each time. `rows.unqualified.jsonl` is deliberately unqualified; `BLOCKED.json` marks failed assessments and any partial output must not be trained on. Successful validation describes only the recorded prefix, with source hashes and coverage counters. Live or abruptly stopped sessions retain an unverified tail. It does not prove complete continuous capture, a faithful position/exit simulation, full historical returns or a profitable model. No automatic training/promotion is provided. The original holdout is frozen; collect 4–8 weeks of new evidence and implement independently verified inventory/exit replay before reconsidering live filtering.

Dependency assessment on 2026-09-30 queried OSV for the four exact installed package versions: no advisories were listed. Official wheel hashes and native tests establish provenance and tested behavior, not absence of unknown vulnerabilities. Agy and Hermes completed text-only reviews; their opinions do not replace the tests or future performance evidence.

The research auditor validates a recorded prefix (`VERIFIED_RECORDED_PREFIX`), not an independently observed disk-flush acknowledgement. The writer syncs each record, but a concurrent reader cannot attest an in-flight sync completion. The live tail and full strategy replay remain unqualified.
