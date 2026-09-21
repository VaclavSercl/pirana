# SynthBit v2.3 — Universal Development Harness

## Goal
Implement complete SynthBit v2.3 harness covering all eight contract steps: canonical memory, verification gate, repository map, recall/pruning, worktree sandbox, authoritative ledger, multi-runtime dispatcher, and shell installer. Upgrade from partial v2.2.

## Non-goals
No production trading changes, deployment, package installation, remote publication, credentials collection, or home-directory Git initialization. No unrestricted runtime modes. Docker unavailable.

## Detected Environment
- Host: Linux aarch64 (caslav 7.0.0-1017-raspi, Ubuntu)
- Git 2.53.0, Python 3.14.4, Bash 5.3.9
- SQLite FTS5 available; Docker/Zsh absent
- Repository: unborn HEAD on branch feat/synthbit-v2_2
- Existing: gate.py, common.py, tests, config.json (partial v2.2)

## Impact Analysis
Owned paths: all .synthbit/*, .gauntlet.sh, .generate_repomap.sh, .gitignore, AGENTS.md, PLAN.md, README.md, Universal_Master_Prompt_v2_3.md. Dependencies restricted to existing Git, Bash, and Python standard library.

## File Checklist
- [ ] AGENTS.md: full canonical memory with v2.3 sections
- [ ] .synthbit/gate.py: verification gate (extend existing)
- [ ] .synthbit/repomap.py: deterministic AST repository map
- [ ] .generate_repomap.sh: launcher for repomap
- [ ] .synthbit/recall.py: fresh recall with FTS5 + fallback
- [ ] .synthbit/prune_memory.py: transactional memory pruning
- [ ] .synthbit/sandbox.sh: managed worktree lifecycle
- [ ] .synthbit/ledger.py: authoritative common-directory ledger + rollback
- [ ] .synthbit/dispatcher.py: capability-detected multi-runtime dispatcher
- [ ] .synthbit/bin/ai-run: canonical launcher
- [ ] .synthbit/install.py: idempotent shell installer
- [ ] .synthbit/config.json: full command + policy configuration
- [ ] .synthbit/.gitignore: runtime exclusions
- [ ] .synthbit/tests/: comprehensive regression tests
- [ ] .gauntlet.sh: integrated gate launcher

## Acceptance Criteria
1. python3 -m unittest discover -s .synthbit/tests -v passes all tests
2. ./.gauntlet.sh exits 0
3. ./.generate_repomap.sh produces deterministic .repomap.txt
4. recall.py works with both auto and fallback backends
5. ai-run doctor reports status
6. All components handle unsafe paths, missing tools, interrupted writers correctly

## Verification Commands
- python3 -m unittest discover -s .synthbit/tests -v
- ./.gauntlet.sh
- ./.generate_repomap.sh
- python3 .synthbit/recall.py --backend auto -- 'test query'
- python3 .synthbit/recall.py --backend fallback -- 'test query'
- python3 .synthbit/prune_memory.py
- .synthbit/bin/ai-run doctor
- python3 .synthbit/ledger.py doctor

## Failure Scenarios
Unsafe paths, missing Git identity, conflicting launchers, unknown dirty files, interrupted writers, corrupt history, stale recall, missing required tools, hooks changing candidates, and publication uncertainty all block dependent operation. At most three evidence-driven repair cycles per gate failure.

## Recovery Strategy
Preserve all pre-existing files. Atomic writes and protected backups. Keep authoritative intents and recovery refs/bundles before Git side effects. Revert managed checkpoints only with preview and explicit apply, never automatic reset/clean.

## Approval Requirements
Local implementation, tests and local commits authorized. No remote endpoint/branch authorized. No dependency installations or Docker image pulls authorized.
