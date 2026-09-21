# SynthBit development harness — v2_2

Status: implementation in progress. No complete-harness acceptance yet. The existing
home-directory v2.1 implementation is neither imported as trusted code nor an
attestation for this repository.

## Architecture
The standalone repository is the release source. Each adopted repository receives
an explicitly pinned copy and its own command policy; updates produce reviewable
changes rather than changing running projects through a global symlink. The user
launcher resolves the actual current Git root, including linked worktrees. It must
not fall back to a home-directory harness when the current repository lacks one.

## Trust and filesystem boundaries
This is a cooperative harness for trusted local developers and coding agents.
Instructions and advisory locks do not confine hostile processes. Native commands
have the invoking user's permissions. Git worktrees isolate changes, not credentials,
network or processes. Cross-host locks and network-filesystem durability are untested.
Python atomic replacement and fsync are used for owned files; multiple replaced
files are not one atomic transaction. Consumers verify generation/digest metadata.
No third-party packages, runtime downloads or container pulls are performed.

## Verification policy
The harness adds required standard-library regression tests and syntax validation.
There was no pre-existing application or CI configuration in this new repository.
No measured coverage percentage is claimed. Project adopters must explicitly bind
existing CI commands and environments; missing required tools block acceptance.
Optional unavailable tools are visibly skipped. Every command is an argv array,
with a repository-contained cwd and finite timeout. Reports exclude raw command
output, prompt arguments and environment contents.

Gauntlet exit codes: 0 required checks passed; 1 verified finding; 2 blocked or
infrastructure/configuration failure. A syntax pass is not a whole-harness pass.

## History and recovery contract
`<git-common-dir>/synthbit/ledger.jsonl` is the sole event authority. Worktree-local
`.synthbit/ledger.jsonl` is an ignored projection. Recovery references use the private
`refs/synthbit/recovery/` namespace and are never part of publication refspecs.
Shared operations acquire the worktree lock before the common Git-operation lock;
authoritative appends take a separate short ledger lock. Do not reacquire a lock
already owned by the caller. A corrupt authority blocks execution, resume and undo.

## Validation coverage
Live environment: Linux aarch64, Git 2.53.0, Python 3.14.4, Bash 5.3.9, SQLite FTS5.
macOS, WSL, Zsh and Bash 3.2 runtime testing remain unavailable. Fake adapters do not
establish paid-session access or actual agent context delivery. The final acceptance
report will list exact tested capabilities and unresolved requirements.
